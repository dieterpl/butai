// The HOME page's row model: every agent on every daemon, in one list.
//
// The port of `all_agent_rows`, `machine_rows`, `home_rows` and `home_tray`
// from `crates/butai-client/`. Pure — no DOM, no network, no app — so
// `test/fleet.test.ts` runs the lot against hand-written multi-daemon state,
// which is the only way to see a namespacing mistake without a browser.
//
// ## Why this list is not sorted by urgency
//
// `home_rows`'s comment is the reasoning, and it is a measurement rather than a
// preference: a list re-sorted by state on the daemon's ~2s sampler tick
// travels ~174 positions per ten ticks at 24 agents, and banding plus
// hysteresis only brought that to 169. So the order here is a pure function of
// *identity* — daemon, then workspace, then spawn order — and reads no agent
// state at all. An agent's row is where it was an hour ago; a state change
// redraws the glyph in place. Attention is surfaced by [`homeTray`] copying
// rows upward instead.
//
// ## Why `daemon` is a key and not an index
//
// The TUI's `AllAgentRow.daemon` is an index into its `daemons` vec. Here it is
// the daemon's *key* — the same string the qualified ids carry — because an
// index compares equal to another machine's index and a key does not. That is
// the whole reason stage 5 chose `"<daemon>:<n>"` over `{id, daemon}`: the
// mistake has to render nothing rather than render the wrong machine's agent.

import type { QualifiedAgent, QualifiedWorkspace, Qid } from "./events.ts";
import type { SysDto } from "../protocol/generated/protocol.ts";

/// A workspace as the client holds it — the qualified detail, ids and all.
export type Workspace = QualifiedWorkspace;

/// One configured daemon, as `/api/state` and the `hello` record describe it.
///
/// The bridge's own shape (`server/roster.ts`'s `DaemonDto`), not a generated
/// DTO: the roster is a property of the bridge and never crosses the daemon
/// socket. Only the four fields this page reads are named.
export interface DaemonEntry {
  key: string;
  label?: string;
  system?: SysDto | null;
  error?: string | null;
}

/// One agent, wherever it is: which machine, which project, which pane.
export interface AgentRow {
  /// The workspace this agent is in, qualified — what "go there" needs.
  ws: Qid;
  workspace: string;
  daemon: string | null;
  /// The badge saying which machine, or null when there is only one.
  host: string | null;
  /// Qualified: `<daemon>:<pane>`.
  pane: Qid;
  agent: QualifiedAgent;
}

/// What a fleet row is. Headers are in the same list as agents so the drawing
/// and the cursor walk one sequence and cannot disagree about which row is
/// which — the bug every "draw it twice" list eventually has.
export const HomeRowKind = Object.freeze({ Machine: "machine", Space: "space", Agent: "agent" } as const);

/// A row of the fleet column: a machine header, a project header, or an agent.
export type HomeRow =
  | { kind: typeof HomeRowKind.Machine; daemon: string | null; label: string | null; agents: number }
  | { kind: typeof HomeRowKind.Space; ws: Qid; name: string; daemon: string | null }
  | { kind: typeof HomeRowKind.Agent; row: AgentRow; sel: number };

/// One machine's block in the COMPUTE column.
export interface MachineRow {
  daemon: string;
  label: string;
  sys: SysDto | null;
  error: string | null;
  agents: number;
}

/// A tray entry: an agent's row, carrying the index it has in the fleet.
export interface TrayRow {
  row: AgentRow;
  sel: number;
}

/// Every agent on every daemon, machine by machine in configured order.
///
/// `workspaces` is the client's flat cross-machine list (`snap.workspaces`) and
/// `daemons` is the roster. The daemons are walked on the *outside* rather than
/// trusting the workspace list to already be grouped: the grouping is what the
/// headers below are built from, and a list that arrived ungrouped would
/// produce a machine header per workspace.
///
/// `host` is the badge a row draws to say which machine it is on, and it is
/// null with one daemon — there is nothing to qualify it against, which is the
/// same rule the tab bar's badge follows.
export function allAgentRows(
  workspaces: readonly Workspace[] | null | undefined,
  daemons: readonly DaemonEntry[] | null | undefined,
): AgentRow[] {
  const list = workspaces || [];
  const roster = daemons || [];
  const many = roster.length > 1;
  const label = new Map<string | null, string>(roster.map((d): [string, string] => [d.key, d.label || d.key]));
  // Roster order first, then any daemon that has workspaces but is not on the
  // roster. Dropping those would be a machine's agents vanishing from the one
  // page that exists to show every machine's agents.
  const order: (string | null)[] = roster.map((d) => d.key);
  for (const w of list) if (w && !order.includes(w.daemon)) order.push(w.daemon);

  const out: AgentRow[] = [];
  for (const key of order) {
    for (const w of list) {
      if (!w || w.daemon !== key) continue;
      for (const a of w.agents || []) {
        out.push({
          // The workspace this agent is in, qualified — what "go there" needs.
          ws: w.id,
          workspace: w.name,
          daemon: key,
          host: many ? (label.get(key) || key) : null,
          // Qualified: `<daemon>:<pane>`. This is what the preview streams and
          // what makes the middle column reach the right machine's socket.
          pane: a.pane,
          agent: a,
        });
      }
    }
  }
  return out;
}

/// Every configured daemon and its own telemetry, for the COMPUTE column.
///
/// **One entry per machine, and the telemetry stays per machine.** `SysDto`
/// describes one box, and averaging four machines' CPU produces a number that
/// is true of nothing.
///
/// Unlike the TUI's `machine_rows` this includes a daemon that is **down**: it
/// is a marker, not an absence, and HOME is the page where the difference
/// between "the gpu box has nothing open" and "the gpu box is unreachable" is
/// most useful. A down machine has no `system` and no agents, and says why.
export function machineRows(
  daemons: readonly DaemonEntry[] | null | undefined,
  all: readonly AgentRow[] | null | undefined,
): MachineRow[] {
  const rows = all || [];
  return (daemons || []).map((d) => ({
    daemon: d.key,
    label: d.label || d.key,
    sys: d.system === undefined ? null : d.system,
    error: d.error || null,
    agents: rows.filter((r) => r.daemon === d.key).length,
  }));
}

/// The fleet column's rows: machine, then space, then that space's agents.
///
/// `sel` is the row's index among **agents only**, which is what the cursor
/// counts and what `j`/`k` walk. A header is not a row you can select — clicking
/// a machine's name is not a request to open somebody's agent.
///
/// Spaces are grouped by the *qualified workspace id* rather than by name. The
/// TUI groups by name and would merge two projects that happen to share one;
/// here they are two headers, because the id is the thing that is actually
/// unique and the name is only what the header prints.
export function homeRows(
  all: readonly AgentRow[] | null | undefined,
  machines: readonly MachineRow[] | null | undefined,
): HomeRow[] {
  const rows = all || [];
  const ms = machines || [];
  const out: HomeRow[] = [];
  let daemon: string | null = null;
  let ws: Qid | null = null;
  rows.forEach((row, sel) => {
    if (daemon !== row.daemon) {
      daemon = row.daemon;
      ws = null;
      const m = ms.find((x) => x.daemon === row.daemon);
      out.push({
        kind: HomeRowKind.Machine,
        daemon: row.daemon,
        label: m ? m.label : row.daemon,
        agents: rows.filter((r) => r.daemon === row.daemon).length,
      });
    }
    if (ws !== row.ws) {
      ws = row.ws;
      out.push({ kind: HomeRowKind.Space, ws: row.ws, name: row.workspace, daemon: row.daemon });
    }
    out.push({ kind: HomeRowKind.Agent, row, sel });
  });
  return out;
}

/// How loudly a tray row is asking, lowest first; `null` keeps it out entirely.
/// The port of `tray_rank` — see `crates/butai-client/src/chrome/mod.rs` for why
/// the order is blocked, then died-unread, then landed-unread.
function trayRank(a: QualifiedAgent | null | undefined): number | null {
  if (!a) return null;
  if (a.state === "waiting") return 0;
  // `unread` is absent on an older daemon, where it reads as false and this
  // client degrades to the waiting-only tray it had before the field existed.
  if (!a.unread) return null;
  if (a.state === "exited" && a.exited) return 1;
  return 2;
}

/// The agents that need you: blocked, or holding news you have not read.
///
/// **Copies, not moves.** The originals stay exactly where they are, which is
/// what lets the tray sort attention to the top without anything below it
/// shifting — the same trick the tab bar plays with its `!` marker. Each copy
/// carries the original's `sel`, so the tray highlights the *selected agent's*
/// copy rather than owning a cursor of its own; otherwise every waiting agent
/// would be two things you can select.
///
/// Sorted by `trayRank`; `Array.prototype.sort` is stable per spec, so rows of
/// equal rank keep fleet order rather than re-shuffling on every push.
export function homeTray(all: readonly AgentRow[] | null | undefined): TrayRow[] {
  const out: TrayRow[] = [];
  (all || []).forEach((row, sel) => {
    if (trayRank(row.agent) !== null) out.push({ row, sel });
  });
  // Every row in here ranked, so the `?? 0` is unreachable — it is what the
  // arithmetic did with a null in JavaScript, written out.
  out.sort((a, b) => (trayRank(a.row.agent) ?? 0) - (trayRank(b.row.agent) ?? 0));
  return out;
}

/// A machine that is answering has telemetry to draw; one that is not has a
/// reason. Split out so the column and the checks read one rule.
export function machineIsDown(m: MachineRow | null | undefined): boolean {
  return !!(m && m.error);
}
