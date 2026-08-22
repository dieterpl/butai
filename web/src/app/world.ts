// The world, and how it stays current.
//
// This is the client's whole data layer. It is deliberately not a query cache:
// `logic/events.ts` already *is* the state management — it holds the reducers
// that turn the daemon's `ApiEvent` records into a workspace list, and it has
// the push/poll fallback logic that took two incidents to get right. Wrapping it
// in a second cache would mean two answers to "what is true now".
//
// ## Push first, poll as a fallback, and the fallback is the interesting half
//
// One `DaemonEvents` per configured daemon (`/api/events?daemon=<key>`), because
// the bridge relays each daemon's records verbatim and attributes them *by
// connection* rather than by wrapping them in an envelope. When a stream cannot
// carry records the client falls back to polling `/api/state`, and goes back to
// push when it recovers — `FALLBACK_AFTER_MS` and `RETRY_CLOSED_MS` are the
// constants that decide when, and they live with the reducers rather than here.
//
// ## Ids are qualified before they reach a component
//
// A bare pane id matches on three machines. `qualifyWorkspace` stamps the daemon
// onto every id in a record as it arrives, so nothing downstream can compare an
// unqualified one by accident — see `logic/events.ts`.

import { useEffect, useRef, useState } from "react";
import { api } from "../logic/api.ts";
import {
  DaemonEvents,
  applyWorkspaceDetail,
  qualifyGitOp,
  qualifyNotification,
  qualifyWorkspace,
  type QualifiedGitOp,
  type QualifiedNotification,
  type QualifiedWorkspace,
  type Qid,
} from "../logic/events.ts";
import type { ApiEvent, SysDto } from "../protocol/generated/protocol.ts";

/** One configured daemon, as `/api/daemons` describes it. */
export interface DaemonEntry {
  key: string;
  label: string;
  socket: string;
  primary: boolean;
  source: string;
  error: string | null;
  system: SysDto | null;
}

export interface World {
  /** Every daemon this bridge speaks for, in configured order. */
  daemons: DaemonEntry[];
  /** Every workspace on every daemon, grouped by daemon in roster order. */
  workspaces: QualifiedWorkspace[];
  /** The primary daemon's telemetry — what a single-daemon client reads. */
  system: SysDto | null;
  notifications: QualifiedNotification[];
  /**
   * The last git operation any daemon reported — running, or the one that just
   * finished.
   *
   * Here rather than in the page that draws it, because a page cannot subscribe
   * to a stream: GIT's operation banner takes `op` as a prop and was dead
   * without this, and `cancelOp` — the one route the daemon has served since 0.8
   * with no client calling it — is only reachable while an operation is known to
   * be running. One at a time per workspace is the daemon's own rule, and the
   * consumer checks `op.ws` against the workspace it is drawing, so one slot is
   * enough for a bridge serving several machines.
   */
  gitOp: QualifiedGitOp | null;
  /** Which daemons are currently pushing, by key. */
  pushing: Record<string, boolean>;
  /** Set only when *nothing* answered, so a one-daemon client reads as before. */
  error?: string | undefined;
  /** False until the first snapshot lands, so the shell can say "connecting…". */
  loaded: boolean;
}

const EMPTY: World = {
  daemons: [],
  workspaces: [],
  system: null,
  notifications: [],
  gitOp: null,
  pushing: {},
  loaded: false,
};

// How often to re-read `/api/state` while any daemon is *not* pushing. The push
// path makes this the exception; a client with every stream up never runs it.
const POLL_MS = 2000;

/**
 * Subscribe to every daemon and keep one `World` current.
 *
 * Returns the world and a `refresh()` an action can call. That second half
 * matters more than it looks: after staging a file the daemon already knows the
 * answer, and waiting up to `POLL_MS` to see the row move reads as the click
 * having missed.
 */
export function useWorld(): [World, () => void] {
  const [world, setWorld] = useState<World>(EMPTY);
  // The mutable copy the reducers write into. React state is replaced from it
  // on each change; reducing *into* React state would mean a render per record,
  // and a busy pane produces records faster than frames.
  const live = useRef<World>(EMPTY);
  const refreshRef = useRef<() => void>(() => {});

  useEffect(() => {
    let alive = true;
    const streams: DaemonEvents[] = [];

    const publish = () => {
      if (alive) setWorld({ ...live.current });
    };

    const readState = () =>
      api
        .state()
        .then((s) => {
          if (!alive) return;
          live.current = {
            ...live.current,
            daemons: (s.daemons ?? []) as DaemonEntry[],
            workspaces: (s.workspaces ?? []) as unknown as QualifiedWorkspace[],
            system: (s.system ?? null) as SysDto | null,
            error: s.error,
            loaded: true,
          };
          publish();
        })
        .catch(() => {});

    refreshRef.current = readState;

    const onEvent = (ev: ApiEvent, daemon: string | null) => {
      const w = live.current;
      switch (ev.event) {
        case "system":
          // The primary's telemetry is the top-level `system`; every daemon's is
          // on its own roster entry, which is what the SYSTEM rail follows when
          // you are looking at another machine's workspace.
          live.current = {
            ...w,
            system: w.daemons.find((d) => d.key === daemon)?.primary ? ev.data : w.system,
            daemons: w.daemons.map((d) => (d.key === daemon ? { ...d, system: ev.data } : d)),
          };
          break;
        case "workspaces": {
          const mine = ev.data.map((s) => qualifyWorkspace(daemon, s));
          const others = w.workspaces.filter((x) => daemonOfWorkspace(x) !== daemon);
          // A summary carries counts where a detail carries lists, so a summary
          // may only *add* a workspace or update its counts — never replace a
          // detail already held. `applyWorkspaceDetail` owns that merge.
          live.current = { ...w, workspaces: mergeSummaries(others, mine, w.workspaces, daemon) };
          break;
        }
        case "workspace_detail":
          live.current = {
            ...w,
            workspaces: applyWorkspaceDetail(w.workspaces, qualifyWorkspace(daemon, ev.data)),
          };
          break;
        case "notification":
          live.current = {
            ...w,
            notifications: [qualifyNotification(daemon, ev.data), ...w.notifications].slice(0, 200),
          };
          break;
        case "git_op":
          live.current = { ...w, gitOp: qualifyGitOp(daemon, ev.data) };
          break;
        default:
          // `remote_announce` is handled by the pages that care, through
          // `refresh()`. Ignoring an unknown tag is the contract: the bridge
          // relays records it has never heard of and so does this.
          break;
      }
      publish();
    };

    // The roster first: the client needs to know how many streams to open before
    // it opens any, and `/api/daemons` contacts no daemon to answer.
    api
      .daemons()
      .then((r) => {
        if (!alive) return;
        const entries = (r.daemons ?? []) as DaemonEntry[];
        live.current = { ...live.current, daemons: entries };
        for (const d of entries) {
          const s = new DaemonEvents(
            {
              onSnapshot: () => readState(),
              onEvent,
              onUp: (key) => {
                live.current = { ...live.current, pushing: { ...live.current.pushing, [key ?? ""]: true } };
                publish();
              },
              onDown: (_why, key) => {
                live.current = { ...live.current, pushing: { ...live.current.pushing, [key ?? ""]: false } };
                publish();
              },
            },
            d.key,
          );
          s.start();
          streams.push(s);
        }
        void readState();
      })
      .catch(() => void readState());

    // The poll only does work while something is not pushing.
    const timer = setInterval(() => {
      const p = live.current.pushing;
      const anyDown = live.current.daemons.some((d) => !p[d.key]);
      if (anyDown) void readState();
    }, POLL_MS);

    return () => {
      alive = false;
      clearInterval(timer);
      for (const s of streams) s.stop();
    };
  }, []);

  return [world, () => refreshRef.current()];
}

function daemonOfWorkspace(w: QualifiedWorkspace): string | null {
  return (w as { daemon?: string | null }).daemon ?? null;
}

/**
 * Fold a daemon's summary list into the workspaces already held.
 *
 * A summary and a detail disagree about their own shape — `agents` is a count in
 * one and a list in the other — so a summary must never overwrite a detail. It
 * adds workspaces that are new and leaves the rest alone; the detail arrives on
 * its own record.
 */
function mergeSummaries(
  others: QualifiedWorkspace[],
  incoming: ReturnType<typeof qualifyWorkspace>[],
  all: QualifiedWorkspace[],
  daemon: string | null,
): QualifiedWorkspace[] {
  const held = new Map(all.filter((w) => daemonOfWorkspace(w) === daemon).map((w) => [String(w.id), w]));
  const kept: QualifiedWorkspace[] = [];
  for (const s of incoming) {
    const id = String((s as { id: Qid }).id);
    const have = held.get(id);
    kept.push(have ?? placeholderFor(s));
  }
  return [...others, ...kept];
}

/**
 * A detail-shaped stand-in for a workspace we have only a summary of.
 *
 * **The summary cannot be used as-is**, and this is the bug the bridge's
 * `unavailable_detail` was written for years ago: in a `WorkspaceSummary`
 * `agents` and `processes` are *counts* and `changes` is a count, while in a
 * `WorkspaceDetail` they are lists and an object. Pushing the summary through
 * meant every consumer that `.find`s or `.map`s over a rail threw — which is
 * exactly what happened here, as `ws.agents.find is not a function` on the first
 * live run, taking the whole page down rather than showing one workspace short.
 *
 * Two different types under one key is never serveable. Keep the summary's
 * identity, synthesise the detail's shape, and let the real detail replace it
 * when its own record arrives.
 */
function placeholderFor(s: ReturnType<typeof qualifyWorkspace>): QualifiedWorkspace {
  const sum = s as unknown as { id: Qid; name: string; cwd?: string; daemon?: string | null };
  return {
    ...sum,
    cwd: sum.cwd ?? "",
    agents: [],
    processes: [],
    changes: null,
    stage: null,
  } as unknown as QualifiedWorkspace;
}
