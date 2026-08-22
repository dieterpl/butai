// The whole world, across every configured daemon.
//
// Transliterated from `server.py`'s `qualify_workspace` / `unavailable_detail` /
// `daemon_snapshot` / `snapshot`. The one structural change is the fan-out:
// Python spawned a thread per daemon and joined them against a deadline, which
// here is `Promise.all` over async calls with a timeout — same shape, no threads.

import { butaiJson } from "./proxy.ts";
import { qid } from "./routing.ts";
import type { DaemonDto, DaemonRef, RosterView } from "./roster.ts";

/** How long the whole-world read waits on one daemon before calling it down. */
const SNAPSHOT_TIMEOUT_MS = 20_000;

type Json = Record<string, unknown>;

/**
 * Stamp a daemon's key onto every id in one `WorkspaceDetail`.
 *
 * Four ids, and missing any one of them is a wrong-machine bug: the workspace
 * itself, the pane the daemon has staged, and the pane of every agent and every
 * process. `daemon` is carried alongside so the client can group and badge
 * without re-splitting the string on every render.
 *
 * **This has a twin in the client's `events.ts` (`qualifyWorkspace`).** It has
 * to: the bridge builds `/api/state` and can qualify it, while the daemon's own
 * records on the event stream are relayed verbatim and can only be qualified
 * after they arrive.
 */
export function qualifyWorkspace(key: string, det: Json): Json {
  const out: Json = { ...det, daemon: key, id: qid(key, det.id as number | null) };
  if (det.stage !== null && det.stage !== undefined) out.stage = qid(key, det.stage as number);
  for (const rail of ["agents", "processes"] as const) {
    const items = det[rail];
    if (Array.isArray(items)) {
      out[rail] = items.map((p) =>
        p && typeof p === "object" ? { ...(p as Json), pane: qid(key, (p as Json).pane as number | null) } : p,
      );
    }
  }
  return out;
}

/**
 * A `WorkspaceDetail`-shaped stand-in for a workspace whose detail fetch failed.
 *
 * Appending the `WorkspaceSummary` instead is the bug this replaced: in the
 * summary `agents` and `processes` are *counts* and `changes` is a count, while
 * the detail has them as lists and an object. The client `.some()`s and `.map()`s
 * over them, so one transient failure on one workspace threw a TypeError and took
 * the whole chrome down. Two different types under one key is never serveable:
 * keep the summary's identity, synthesise the detail's shape, and say plainly
 * that it is a stand-in via `detail_error`.
 */
export function unavailableDetail(ws: Json, err: unknown): Json {
  return {
    id: ws.id,
    name: ws.name,
    cwd: ws.cwd ?? "",
    agents: [],
    processes: [],
    changes: null,
    stage: null,
    attached_clients: ws.attached_clients ?? 0,
    detail_error: describe(err),
  };
}

function describe(err: unknown): string {
  if (err instanceof Error) return err.message || err.name;
  return String(err);
}

export interface DaemonSnapshot {
  daemon: string;
  error: string | null;
  workspaces: Json[];
  system: unknown;
}

/**
 * One daemon's whole world, with its ids qualified.
 *
 * This is the `snapshot` record on that daemon's event stream, and one slice of
 * `/api/state`. It costs N+2 round trips, which is why it is no longer on a
 * timer: `workspace_detail` on the event stream carries the same per-workspace
 * payload, pushed when it changes rather than fetched when a clock says so.
 * Deltas are meaningless without a baseline, though, so this stays.
 *
 * `error` is set (and the lists left empty) when this daemon is unreachable —
 * which is a fact about one machine, so it is reported rather than thrown.
 */
export async function daemonSnapshot(daemon: DaemonRef): Promise<DaemonSnapshot> {
  const key = daemon.key;
  let workspaces: Json[];
  try {
    workspaces = (await butaiJson<Json[]>(daemon, "GET", "/v1/workspaces")) ?? [];
  } catch (e) {
    // Daemon not up yet, socket missing, tunnel down.
    return { daemon: key, error: describe(e), workspaces: [], system: null };
  }

  // Started here, awaited last: telemetry is the largest single payload the
  // daemon serves and nothing below depends on it, so it rides alongside the
  // details instead of queueing behind them. `.catch` is attached now rather
  // than at the `await` — an unhandled rejection on a promise nobody is
  // watching yet is a crash, and telemetry is the one part of this that may
  // simply be missing.
  const systemP: Promise<unknown> = butaiJson(daemon, "GET", "/v1/system").catch(() => null);

  // Fanned out, not looped. This was sequential — "firing them all at once only
  // queues them at the far end" — which is true and is also the point: when the
  // wire is the bottleneck, queuing at the far end is where you want the queue.
  // Measured on a Unix socket, eight of these in flight together finish 2.4x
  // faster than one after another (131us against 313us); the daemon saturates a
  // core at ~37k req/s, so N open workspaces never trouble it. Over a forwarded
  // socket it is the difference that matters: N round trips become one, so a
  // 30ms link costs 30ms here instead of 30ms per workspace.
  //
  // The failure mode stays readable because each request keeps its own
  // `try`/`catch` and its own slot — one workspace that will not answer still
  // becomes one `unavailableDetail`, and now it does so beside the others
  // rather than in front of them.
  const details = await Promise.all(
    workspaces.map(async (ws): Promise<Json> => {
      try {
        const det = await butaiJson<Json>(daemon, "GET", `/v1/workspaces/${ws.id}`);
        if (det.attached_clients === undefined) det.attached_clients = ws.attached_clients ?? 0;
        return qualifyWorkspace(key, det);
      } catch (e) {
        return qualifyWorkspace(key, unavailableDetail(ws, e));
      }
    }),
  );

  return { daemon: key, error: null, workspaces: details, system: await systemP };
}

export interface WorldState {
  daemons: DaemonDto[];
  workspaces: Json[];
  system: unknown;
  error?: string;
}

/**
 * The union across every configured daemon: the client's state baseline.
 *
 * Fanned out rather than looped. Sequentially, one machine on a tunnel that has
 * gone away would spend its whole connect timeout in front of every other
 * machine's rails — a per-source failure taking the others with it, only in the
 * time domain instead of the exception domain.
 *
 * `error` at the top level keeps its old meaning — *nothing* answered — so a
 * single-daemon bridge serves exactly the document it served before. A daemon
 * that is down among others is a marker in `daemons[]`, which is what the client
 * draws in place of its tabs.
 */
export async function snapshot(view: RosterView): Promise<WorldState> {
  const timedOut = (d: DaemonRef): DaemonSnapshot => ({
    daemon: d.key,
    error: `no answer within ${SNAPSHOT_TIMEOUT_MS / 1000}s`,
    workspaces: [],
    system: null,
  });

  const parts = await Promise.all(
    view.daemons.map((d) =>
      Promise.race([
        daemonSnapshot(d),
        new Promise<DaemonSnapshot>((resolve) => setTimeout(() => resolve(timedOut(d)), SNAPSHOT_TIMEOUT_MS)),
      ]),
    ),
  );

  const daemons: DaemonDto[] = [];
  const workspaces: Json[] = [];
  const errors: string[] = [];
  for (const [i, d] of view.daemons.entries()) {
    const part = parts[i]!;
    daemons.push(d.dto(part.error, part.system));
    // Grouped by daemon, in configured order: the tab bar is one row of projects
    // with the machine as a badge, exactly as the TUI's `tab_index` flattens it.
    workspaces.push(...part.workspaces);
    if (part.error) errors.push(`${d.key}: ${part.error}`);
  }

  const primaryAt = view.daemons.indexOf(view.primary);
  const state: WorldState = {
    daemons,
    workspaces,
    // The primary's, so a single-daemon client reads the field it always read.
    // Per-daemon telemetry is on each `daemons[]` entry, which is what the
    // SYSTEM rail follows when you look at another machine's workspace.
    system: parts[primaryAt]?.system ?? null,
  };
  if (errors.length === view.length) state.error = errors.join("; ");
  return state;
}
