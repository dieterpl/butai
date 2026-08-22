// The daemon's push channel, and the pure reductions that turn it into the
// state the chrome renders.
//
// Before this the client asked `/api/state` every 1.5s, and the bridge answered
// each ask with N+2 sequential round trips over the Unix socket — one for the
// workspace list, one *per workspace* for its rails, one for the machine — per
// browser, forever. The daemon has pushed all of it since 0.5 (`GET /v1/events`,
// six tags); nothing here derives anything the daemon did not say.
//
// Two halves, deliberately separate:
//
//   * `DaemonEvents` is transport. It owns the EventSource, the fallback
//     decision, and nothing about what an event means.
//   * `mergeWorkspaces` / `applyWorkspaceDetail` / `acceptNotification` are pure
//     functions of (state, event) -> state. They are where every rule about how
//     a tag lands lives, and they are unit-tested under node by `check.py` —
//     which is the only way to test them without a browser and a live daemon.

import type {
  AgentDto,
  ApiEvent,
  GitOpDto,
  NotificationDto,
  PaneId,
  ProcessDto,
  RemoteAnnounceDto,
  SysDto,
  WorkspaceDetail,
  WorkspaceSummary,
} from "../protocol/generated/protocol.ts";

// Where the bridge relays `GET /v1/events`. Bridge-local, like `/api/state`
// and `/ws`: the daemon's own route is `/v1/events`.
//
// One stream per daemon: `?daemon=<key>`. The records on it are that daemon's,
// so nothing has to be un-multiplexed and the bridge never has to look inside
// one to say where it came from.
export const EVENTS_PATH = "/api/events";

// How long a dropped stream may stay dropped before we start polling behind it.
// The browser retries on its own (the bridge asks for 2s), so this is not the
// retry interval — it is how long the user may look at stale chrome before
// something else starts refreshing it.
export const FALLBACK_AFTER_MS = 6000;

// A stream the browser gave up on entirely (a 404 from a daemon too old to
// serve the route, say) is retried this often, so a daemon that gets restarted
// under a browser that is already open comes back to push without a reload.
export const RETRY_CLOSED_MS = 30000;

// How long an *open* stream may go without delivering a baseline before we poll
// behind it. A connection that opens and then says nothing is the worst of the
// failures here, because every other one announces itself: the browser reports
// no error, so without this the client would sit on the one snapshot it drew at
// startup and quietly never update again.
export const BASELINE_BY_MS = 8000;

/**
 * The bridge's `hello` record: which daemon this connection is attached to.
 *
 * Not a generated DTO — it is the bridge's own greeting, written by
 * `server/events.ts` (and by `server.py`), and it never crosses the daemon
 * socket. `resumable` is false and `last_event_id` is echoed rather than
 * honoured: the daemon's stream has no cursor and no history.
 */
export interface BridgeHello {
  daemon: string;
  label: string;
  primary: boolean;
  socket: string;
  last_event_id: string | null;
  resumable: boolean;
}

/**
 * The `snapshot` record: one daemon's slice of `/api/state`, ids already
 * qualified by the bridge.
 *
 * The twin of `server/events.ts`'s `DaemonSnapshot`, which is what writes it.
 * `error` set (with the lists empty) is a machine that did not answer — a fact
 * about one daemon, reported rather than thrown.
 */
export interface DaemonSnapshotRecord {
  daemon: string;
  error: string | null;
  workspaces: QualifiedWorkspace[];
  system: SysDto | null;
}

/**
 * Callbacks for one subscription. Each is given the daemon key as a trailing
 * argument so a handler shared between several streams can tell them apart.
 */
export interface DaemonEventHandlers {
  /** The bridge's greeting; `h.daemon` names the daemon. */
  onHello?: (hello: BridgeHello, daemon: string | null) => void;
  /** That daemon's `/api/state` slice, once per connection. */
  onSnapshot?: (snapshot: DaemonSnapshotRecord, daemon: string | null) => void;
  /** One daemon record, `{event, data}`, unparsed by the bridge. */
  onEvent?: (event: ApiEvent, daemon: string | null) => void;
  /** The stream is carrying records. */
  onUp?: (daemon: string | null) => void;
  /** It is not, and is not expected to be soon — poll instead. */
  onDown?: (why: string, daemon: string | null) => void;
}

/** How much of the stream's work is being done by push, for the checker. */
export interface DaemonEventStats {
  connects: number;
  records: number;
  snapshots: number;
  drops: number;
}

/**
 * One live subscription to one daemon's event stream.
 *
 * 'onUp'/'onDown' are edges, not levels: each fires only on a change.
 *
 * Holds no global state, so N daemons is N of these and nothing else.
 */
export class DaemonEvents {
  h: DaemonEventHandlers;
  daemon: string | null;
  path: string;
  es: EventSource | null;
  up: boolean;
  stopped: boolean;
  stats: DaemonEventStats;
  // Timer handles, `0` when not armed — the falsy value `_downTimer` is tested
  // for below, and what `clearTimeout` is given when nothing is pending.
  _downTimer: ReturnType<typeof setTimeout> | 0;
  _retryTimer: ReturnType<typeof setTimeout> | 0;
  _baselineTimer: ReturnType<typeof setTimeout> | 0;
  // The last reason we reported. Undefined until we report one, which is what
  // makes the first `_down` always get through.
  _told?: string;

  constructor(handlers: DaemonEventHandlers = {}, daemon: string | null = null) {
    this.h = handlers;
    // Which daemon this subscription belongs to. Every id in every record that
    // arrives on it is that daemon's, and this is the key they get qualified
    // with — attribution by connection, which is what lets the bridge relay the
    // daemon's bytes without reading them.
    this.daemon = daemon;
    this.path = daemon == null ? EVENTS_PATH : EVENTS_PATH + "?daemon=" + encodeURIComponent(daemon);
    this.es = null;
    this.up = false;
    this._downTimer = 0;
    this._retryTimer = 0;
    this._baselineTimer = 0;
    this.stopped = true;
    // Counted for the checker and for anyone wondering whether push is actually
    // doing the work: a client that fell back silently looks identical to one
    // that never needed to.
    this.stats = { connects: 0, records: 0, snapshots: 0, drops: 0 };
  }

  start(): void {
    this.stopped = false;
    this._open();
  }

  stop(): void {
    this.stopped = true;
    clearTimeout(this._downTimer);
    clearTimeout(this._retryTimer);
    clearTimeout(this._baselineTimer);
    if (this.es) {
      this.es.close();
      this.es = null;
    }
  }

  _open(): void {
    if (this.stopped) return;
    // A browser without EventSource is not a failure to recover from; it is a
    // browser that polls. Say so once and stay there.
    if (typeof EventSource === "undefined") {
      this._down("this browser has no EventSource");
      return;
    }
    this.stats.connects++;
    let es: EventSource;
    try {
      es = new EventSource(this.path);
    } catch (e) {
      const err = e as { message?: string } | null;
      this._down("event stream: " + (err && err.message ? err.message : String(e)));
      return;
    }
    this.es = es;
    // A stream that opens and then says nothing raises no error anywhere, so it
    // is the one failure that has to be timed rather than caught.
    clearTimeout(this._baselineTimer);
    this._baselineTimer = setTimeout(() => this._down("no baseline on the event stream"), BASELINE_BY_MS);

    es.addEventListener("hello", (e: Event) => {
      // A named record: the bridge's, and never the daemon's. `addEventListener`
      // types a custom name as a bare `Event`, so the cast is what says this one
      // carries data.
      const h = parse<BridgeHello>((e as MessageEvent<string>).data);
      if (h) this.h.onHello?.(h, this.daemon);
    });
    es.addEventListener("snapshot", (e: Event) => {
      const s = parse<DaemonSnapshotRecord>((e as MessageEvent<string>).data);
      if (!s) return;
      this.stats.snapshots++;
      // Up on the *snapshot*, not on `open`: an open connection that has not
      // delivered a baseline yet cannot render anything, and calling that "up"
      // would stop the fallback poll a beat before there was anything to
      // replace it.
      this._up();
      this.h.onSnapshot?.(s, this.daemon);
    });
    // Unnamed records are the daemon's, relayed verbatim by the bridge.
    es.onmessage = (e: MessageEvent<string>) => {
      const ev = parse<ApiEvent>(e.data);
      if (!ev || typeof ev.event !== "string") return;
      this.stats.records++;
      this.h.onEvent?.(ev, this.daemon);
    };
    es.onerror = () => {
      if (this.stopped) return;
      this.stats.drops++;
      if (es.readyState === EventSource.CLOSED) {
        // The browser will not retry this one. Either the bridge answered
        // something that is not a 200 text/event-stream — which is what a
        // daemon with no `/v1/events` route produces — or the origin is gone.
        this.es = null;
        this._down("the daemon is not pushing events");
        clearTimeout(this._retryTimer);
        this._retryTimer = setTimeout(() => this._open(), RETRY_CLOSED_MS);
        return;
      }
      // CONNECTING: the browser is already retrying on its own. Give it a
      // window before starting a poll behind it, so an ordinary bridge restart
      // does not flip the client into fallback for one second.
      if (this.up && !this._downTimer) {
        this._downTimer = setTimeout(() => {
          this._downTimer = 0;
          this._down("event stream dropped");
        }, FALLBACK_AFTER_MS);
      } else if (!this.up) {
        this._down("event stream unavailable");
      }
    };
  }

  _up(): void {
    clearTimeout(this._downTimer);
    clearTimeout(this._baselineTimer);
    this._downTimer = 0;
    if (this.up) return;
    this.up = true;
    this.h.onUp?.(this.daemon);
  }

  _down(why: string): void {
    if (!this.up && this._told === why) return;
    this._told = why;
    this.up = false;
    this.h.onDown?.(why, this.daemon);
  }
}

// What the bridge relayed, or null when it was not JSON. The caller says what
// it expects: these records are the daemon's or the bridge's, and neither is
// re-validated here — a record that is not what it claims is a bug on the
// writing side, and pretending otherwise would put a schema in the transport.
function parse<T>(text: string): T | null {
  try {
    return JSON.parse(text) as T;
  } catch (_) {
    return null;
  }
}

// ---------------------------------------------------------------------------
// Qualified ids
//
// Workspace ids and pane ids are per-daemon integers: two daemons both have a
// workspace 1, and both have a pane 5. Everything that crosses to the browser
// therefore carries its daemon — `<daemon-key>:<n>` — and a bare integer is
// never a valid id here.
//
// A string rather than an `{id, daemon}` pair for one reason above all: a bare
// integer compared against `"gpu:1"` never matches. Code that forgot to qualify
// renders nothing, which you see; code that dropped a `daemon` field beside an
// int renders the *other machine's* pane, which you do not.
//
// The bridge does this to its own aggregates (`/api/state`, the snapshot record
// on each stream) — see `qualifyWorkspace` in `server/snapshot.ts`. It cannot do
// it to the daemon's own records, which it relays byte for byte, so those are
// qualified here on arrival. `check.py` compares the two sides against each
// other rather than trusting them to stay in step.
// ---------------------------------------------------------------------------

/**
 * A workspace or pane id as the browser holds it: `'<daemon>:<n>'`.
 *
 * `number` is in the union for one case, and it is the one [`qid`] documents: a
 * null daemon, which is a world with no keys in it and is how the reducers are
 * exercised on their own. Every id the bridge produces is a string.
 */
export type Qid = string | number;

/**
 * '<daemon>:<n>'. Idempotent: an already-qualified id is returned unchanged.
 *
 * A null daemon means a world with no keys in it, and returns the id untouched
 * rather than inventing a key called 'null' — which is what the reducers are
 * given when they are exercised on their own.
 */
export function qid(daemon: string | null | undefined, n: Qid): Qid;
export function qid(daemon: string | null | undefined, n: Qid | null | undefined): Qid | null;
export function qid(daemon: string | null | undefined, n: Qid | null | undefined): Qid | null {
  if (n === null || n === undefined) return null;
  if (daemon === null || daemon === undefined) return n;
  if (typeof n === "string" && n.indexOf(":") >= 0) return n;
  return daemon + ":" + n;
}

/** The daemon a qualified id belongs to, or null if it is not one. */
export function daemonOf(id: Qid | null | undefined): string | null {
  if (typeof id !== "string") return null;
  const i = id.indexOf(":");
  return i > 0 ? id.slice(0, i) : null;
}

/** The daemon's own integer, for the wire. null if 'id' is not qualified. */
export function localId(id: Qid | null | undefined): PaneId | null {
  if (typeof id !== "string") return null;
  const i = id.indexOf(":");
  if (i < 0) return null;
  const n = Number(id.slice(i + 1));
  return Number.isFinite(n) ? n : null;
}

/**
 * One agent row, as the client holds it: the daemon's, with a qualified pane.
 *
 * `unread` is `| undefined` where the DTO has it required, because it was added
 * after 0.6 and an older daemon sends a row without it — which reads as false,
 * and is the degradation `trayRank` in `fleet.ts` is written for.
 */
export type QualifiedAgent = Omit<AgentDto, "pane" | "unread"> & { pane: Qid; unread?: boolean };

/** One process row, with a qualified pane. */
export type QualifiedProcess = Omit<ProcessDto, "pane"> & { pane: Qid };

/**
 * A workspace as the client's list holds it: a `WorkspaceDetail` with its four
 * ids qualified, plus the three fields the *bridge* adds and the daemon does
 * not — `attached_clients` (which the detail payload does not always carry),
 * `pending` (a stand-in built from a summary, see [`mergeWorkspaces`]) and
 * `detail_error` (a workspace whose detail fetch failed).
 */
export interface QualifiedWorkspace extends Omit<WorkspaceDetail, "id" | "stage" | "agents" | "processes"> {
  daemon: string | null;
  id: Qid;
  stage: Qid | null;
  agents: QualifiedAgent[];
  processes: QualifiedProcess[];
  attached_clients?: number;
  pending?: boolean;
  detail_error?: string;
}

/** A `WorkspaceSummary` with its id qualified. Its rails stay counts. */
export interface QualifiedSummary extends Omit<WorkspaceSummary, "id"> {
  daemon: string | null;
  id: Qid;
}

/**
 * Qualify every id in a workspace — summary or detail.
 *
 * Four of them, and missing any one is a wrong-machine bug: the workspace, the
 * staged pane, and the pane of every agent and every process. Summaries carry
 * counts where details carry arrays, so the rails are only walked when they
 * *are* arrays; nothing here converts one shape into the other.
 */
export function qualifyWorkspace(daemon: string | null, w: WorkspaceDetail): QualifiedWorkspace;
export function qualifyWorkspace(daemon: string | null, w: WorkspaceSummary): QualifiedSummary;
export function qualifyWorkspace(
  daemon: string | null,
  w: WorkspaceDetail | WorkspaceSummary,
): QualifiedWorkspace | QualifiedSummary {
  if (!w || typeof w !== "object") return w;
  // The two shapes disagree about every field below this line — `stage` is on
  // one of them, and the rails are counts on one and arrays on the other — so
  // the body reads them off the record and the overloads above are the contract
  // callers see.
  const src = w as Record<string, unknown>;
  const out: Record<string, unknown> = Object.assign({}, src, { daemon, id: qid(daemon, w.id) });
  if (src.stage !== null && src.stage !== undefined) out.stage = qid(daemon, src.stage as PaneId);
  for (const rail of ["agents", "processes"] as const) {
    const items: unknown = src[rail];
    if (Array.isArray(items)) {
      out[rail] = (items as unknown[]).map((p) =>
        p && typeof p === "object" ? Object.assign({}, p, { pane: qid(daemon, (p as { pane?: PaneId }).pane) }) : p);
    }
  }
  return out as unknown as QualifiedWorkspace | QualifiedSummary;
}

/** 'ApiEvent::Notification' — its workspace and its pane are both ids. */
export interface QualifiedNotification extends Omit<NotificationDto, "ws" | "pane"> {
  daemon: string | null;
  ws: Qid;
  pane: Qid;
}

export function qualifyNotification(daemon: string | null, n: NotificationDto): QualifiedNotification {
  if (!n || typeof n !== "object") return n;
  return Object.assign({}, n, { daemon, ws: qid(daemon, n.ws), pane: qid(daemon, n.pane) });
}

/** 'ApiEvent::GitOp' — 'ws' is an id, and the waiter matches on it. */
export interface QualifiedGitOp extends Omit<GitOpDto, "ws"> {
  daemon: string | null;
  ws: Qid;
}

export function qualifyGitOp(daemon: string | null, op: GitOpDto): QualifiedGitOp {
  if (!op || typeof op !== "object") return op;
  return Object.assign({}, op, { daemon, ws: qid(daemon, op.ws) });
}

/**
 * 'ApiEvent::RemoteAnnounce' — a 'butai' run over ssh inside one of this
 * daemon's panes, saying which machine it is on and where its socket is.
 *
 * The pane is qualified like any other; 'socket' deliberately is not, because it
 * is a path on the *far* machine and means nothing on this one until something
 * forwards it.
 */
export interface QualifiedAnnounce extends Omit<RemoteAnnounceDto, "pane"> {
  daemon: string | null;
  pane: Qid;
}

export function qualifyAnnounce(daemon: string | null, a: RemoteAnnounceDto): QualifiedAnnounce {
  if (!a || typeof a !== "object") return a;
  return Object.assign({}, a, { daemon, pane: qid(daemon, a.pane) });
}

/** The two commands that would put an announced machine in the tab bar. */
export interface AnnouncementPlan {
  target: string;
  socket: string;
  name: string;
  local: string;
  forward: string;
  configure: string;
}

/**
 * What it would take to put an announced machine in this bridge's tab bar.
 *
 * The daemon reports and does not act — it is the only party that can *detect* a
 * far machine (it reads every byte a pane writes) and the wrong party to connect
 * one. The TUI acts on this by running 'ssh -L' itself. This bridge does not:
 * it is usually a container with no keys and no agent, and 'butai proxy' — the
 * other way in — is a single stream, which is enough to drive one pane and not
 * enough to be a client that needs REST, an event stream and a pane socket at
 * once ('crates/butai-client/src/ssh.rs' says the same thing about the TUI).
 *
 * So the announcement becomes an instruction instead, and the two halves of it
 * are exactly '[[remote]] socket = ...' in the TUI's config: forward the far
 * socket, then name the local path in 'BUTAI_SOCKETS'.
 */
export function announcementPlan(
  a: QualifiedAnnounce | RemoteAnnounceDto | null | undefined,
  key?: string | null,
): AnnouncementPlan | null {
  const target = (a && (a.ssh_target || a.hint)) || "";
  const socket = (a && a.socket) || "";
  if (!target || !socket) return null;
  const name = key || (target.split("@").pop() || "remote").replace(/[^A-Za-z0-9._-]/g, "-").slice(0, 24);
  const local = "/tmp/butai-" + name + ".sock";
  // `a` is non-null by the guard above — an announcement with no target is one
  // with no `a` — but the guard reads its fields rather than the object.
  const args = (a?.ssh_args || []).join(" ");
  return {
    target,
    socket,
    name,
    local,
    forward: ("ssh -N -T -L " + local + ":" + socket + (args ? " " + args : "") + " " + target).replace(/\s+/g, " "),
    configure: 'BUTAI_SOCKETS="' + name + "=" + local + '"',
  };
}

// ---------------------------------------------------------------------------
// Pure reductions
//
// Each takes the whole workspace list — every daemon's — and replaces exactly
// one daemon's slice. `daemon` null means a single unnamed world and replaces
// all of it, which is what a bridge with one daemon and no keys would mean.
// ---------------------------------------------------------------------------

/**
 * Keep the list grouped by daemon, in 'order' (the configured order).
 *
 * The tab bar is one row of projects with the machine as a badge rather than a
 * level of hierarchy — the same flattening the TUI's 'tab_index' does — so the
 * grouping is the only thing that keeps a machine's tabs together as its
 * workspaces come and go. 'Array.prototype.sort' is stable, so within a daemon
 * the daemon's own order survives.
 */
function regroup(
  list: QualifiedWorkspace[],
  order: readonly string[] | null | undefined,
): QualifiedWorkspace[] {
  if (!order || !order.length) return list;
  const rank = new Map(order.map((k, i): [string, number] => [k, i]));
  // `rank.get` is only undefined for a daemon that is not in `order`, which is
  // the `order.length` case the `has` check named.
  const at = (w: QualifiedWorkspace) => (w.daemon == null ? order.length : rank.get(w.daemon) ?? order.length);
  return list.slice().sort((a, b) => at(a) - at(b));
}

/**
 * Replace one daemon's whole slice — the 'snapshot' record on its stream.
 *
 * A reconnect re-delivers a baseline for *that* daemon only, and it must not
 * touch the others: they have their own streams, their own snapshots, and no
 * reason to flicker because a different machine's tunnel came back.
 */
export function mergeDaemonSnapshot(
  current: QualifiedWorkspace[] | null | undefined,
  daemon: string | null,
  workspaces: QualifiedWorkspace[] | null | undefined,
  order: readonly string[] | null | undefined,
): QualifiedWorkspace[] {
  const mine = workspaces || [];
  if (daemon == null) return mine.slice();
  return regroup((current || []).filter((w) => w.daemon !== daemon).concat(mine), order);
}

/**
 * Reconcile the workspace list against an 'ApiEvent::Workspaces' payload.
 *
 * The payload is a list of 'WorkspaceSummary', where 'agents' and 'processes'
 * are **counts** and 'changes' is a count — while the list we render holds
 * 'WorkspaceDetail', where they are arrays and an object. Assigning a summary
 * into that list is the exact shape of the bug stage 2 fixed in the bridge, and
 * it would crash the chrome the same way here, so this event is used for one
 * thing only: which workspaces exist, in what order, under what name. Contents
 * come from 'workspace_detail' and from the connection's snapshot.
 *
 * A workspace we have never seen gets a detail-shaped stand-in with empty
 * rails and 'pending: true', so the tab appears at once and nothing downstream
 * has to guard against a missing array. 'pending' is the caller's cue to fetch
 * the detail once; the daemon pushes it shortly anyway, and the one-shot fetch
 * is what makes "shortly" not depend on the daemon choosing to.
 *
 * The payload is one daemon's list, so it replaces that daemon's slice and
 * leaves every other machine's tabs alone. Getting that wrong is not subtle —
 * one daemon's ordinary heartbeat would delete every other daemon's workspaces
 * a few times a second.
 *
 * Returns a new array; 'current' is not modified.
 */
export function mergeWorkspaces(
  current: QualifiedWorkspace[] | null | undefined,
  summaries: WorkspaceSummary[] | null | undefined,
  daemon: string | null = null,
  order: readonly string[] | null = null,
): QualifiedWorkspace[] {
  const list = current || [];
  const have = new Map(list.map((w): [Qid, QualifiedWorkspace] => [w.id, w]));
  const mine = (summaries || []).map((raw) => {
    const s = qualifyWorkspace(daemon, raw);
    const w = have.get(s.id);
    if (w) {
      // Identity can change under us (a rename); rails cannot come from here.
      return Object.assign({}, w, { name: s.name ?? w.name, cwd: s.cwd ?? w.cwd });
    }
    return {
      id: s.id,
      daemon: s.daemon,
      name: s.name,
      cwd: s.cwd || "",
      agents: [],
      processes: [],
      changes: null,
      stage: null,
      attached_clients: s.attached_clients || 0,
      pending: true,
    };
  });
  if (daemon == null) return mine;
  return regroup(list.filter((w) => w.daemon !== daemon).concat(mine), order);
}

/**
 * Apply an 'ApiEvent::WorkspaceDetail' payload to the workspace list.
 *
 * This is the event the whole stage turns on: it is the per-workspace poll,
 * pushed. The daemon only sends it when the rails actually differ from the last
 * one it sent, so applying it wholesale is right.
 *
 * A detail for an id we have never seen is **appended**, not dropped. That
 * happens on the ordinary race where a workspace is created and its detail
 * arrives before the 'workspaces' list naming it — dropping it would blank a
 * live workspace until something else changed.
 *
 * Returns a new array; 'current' is not modified.
 */
export function applyWorkspaceDetail(
  current: QualifiedWorkspace[] | null | undefined,
  detail: QualifiedWorkspace | null | undefined,
  order: readonly string[] | null = null,
): QualifiedWorkspace[] {
  const list = current || [];
  if (!detail || detail.id == null) return list;
  let found = false;
  const next = list.map((w) => {
    if (w.id !== detail.id) return w;
    found = true;
    // `attached_clients` is on the summary and on the snapshot but not always
    // on this payload; keeping the last known value beats flashing it to 0.
    return Object.assign({}, detail, {
      attached_clients: detail.attached_clients != null ? detail.attached_clients : w.attached_clients || 0,
    });
  });
  return found ? next : regroup(next.concat([Object.assign({}, detail)]), order);
}

/** What [`acceptNotification`] answers: show it, and the cursor to store. */
export interface NotificationVerdict {
  show: boolean;
  cursor: number | null | undefined;
}

/**
 * Should this pushed notification be shown, given the cursor we have?
 *
 * **One cursor per daemon.** The seq is a daemon's own counter, so two daemons
 * are both at 7 and mean different things; a single cursor would silence one
 * machine every time the other got ahead of it. The caller keeps a cursor per
 * key and passes the right one — this rule itself is the same either way.
 *
 * The cursor model is the daemon's, and it is the one stage 3 built for
 * 'GET /v1/notifications?since=N' — deliberately reused rather than duplicated,
 * because two cursors for one feed disagree the first time a client sees the
 * same transition down both paths, which is exactly what happens here: a
 * reconnect drains the feed *and* starts receiving pushes.
 *
 *   cursor === null  a browser that has never looked. Adopt the seq silently:
 *                    the first look shows nothing, by the same rule that makes
 *                    a fresh client jump to 'head' instead of replaying.
 *   seq <= cursor    already shown — a replay after a reconnect, or the drain
 *                    and the push racing over the same item.
 *   otherwise        new.
 *
 * Returns '{show, cursor}': the caller shows the item if 'show', and always
 * stores 'cursor'.
 */
export function acceptNotification(
  n: Pick<NotificationDto, "seq"> | null | undefined,
  cursor: number | null | undefined,
): NotificationVerdict {
  if (!n || typeof n.seq !== "number") return { show: false, cursor };
  if (cursor === null || cursor === undefined) return { show: false, cursor: n.seq };
  // A daemon restart begins the seq again at 1 and so leaves our cursor in the
  // future, which this rule alone would answer by silencing us forever. It is
  // not fixed here on purpose: a restart necessarily drops the stream, and the
  // resync every reconnect performs drains `GET /v1/notifications`, whose
  // `head < since` arm is where that reset already lives. One place, one rule.
  if (n.seq <= cursor) return { show: false, cursor };
  return { show: true, cursor: n.seq };
}
