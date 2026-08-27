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

/// One project on one machine — *including* the ones with nothing running.
///
/// The port of `SpaceRow`. The fleet used to be built by walking the agents and
/// emitting a header whenever the workspace changed, which meant a project with
/// no agents produced no rows at all: the one page listing every project on
/// every machine could not show you the ones you had not started anything in,
/// which are exactly the ones you would want to start something in.
export interface SpaceRow {
  ws: Qid;
  name: string;
  daemon: string | null;
  /// This project's agents, as a window onto the fleet list — a folded row
  /// draws their sprites, so it needs the agents and not a count.
  agents: AgentRow[];
  /// Where those agents start in the fleet list.
  first: number;
  /// The agent `+` starts here: the project's own `[agents] autostart`, then
  /// the client's pin. See `preferredAgent`.
  preferred: string | null;
}

/// A row of the fleet column: a machine header, a project header, or an agent.
export type HomeRow =
  | {
      kind: typeof HomeRowKind.Machine;
      daemon: string | null;
      label: string | null;
      agents: number;
      folded: boolean;
    }
  | { kind: typeof HomeRowKind.Space; space: SpaceRow; machine: string; folded: boolean }
  | { kind: typeof HomeRowKind.Agent; row: AgentRow; sel: number };

/// What HOME has folded away, and which machines have their gauges out.
///
/// Keyed by *name*, not by position, for the reason the TUI's `Folds` is: a
/// machine's place in the roster moves when an earlier one drops, and folding
/// by position would hand the fold belonging to the machine that went away to
/// whichever took its slot. A project's key is its qualified id, which already
/// carries its machine.
export interface Folds {
  machines: ReadonlySet<string>;
  spaces: ReadonlySet<Qid>;
  expanded: ReadonlySet<string>;
}

/// Nothing folded, and every machine a summary.
export const NO_FOLDS: Folds = Object.freeze({
  machines: new Set<string>(),
  spaces: new Set<Qid>(),
  expanded: new Set<string>(),
});

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

/// Every project on every daemon, in the roster's order and then tab order.
///
/// The port of `fleet_spaces`. Built from the *workspace list* rather than from
/// the agents, which is the point: a project with nothing running in it has a
/// summary and no agents, and HOME is where you would go to start something in
/// it.
///
/// `agents` is a window onto `all`, so the two lists must be walked in one
/// order — `allAgentRows` walks the roster and then the workspaces, and so does
/// this. `test/fleet.test.ts` is what keeps that one property one.
export function fleetSpaces(
  workspaces: readonly Workspace[] | null | undefined,
  daemons: readonly DaemonEntry[] | null | undefined,
  all: readonly AgentRow[] | null | undefined,
  pinned: string | null,
): SpaceRow[] {
  const list = workspaces || [];
  const rows = all || [];
  const roster = daemons || [];
  const order: (string | null)[] = roster.map((d) => d.key);
  for (const w of list) if (w && !order.includes(w.daemon)) order.push(w.daemon);

  const out: SpaceRow[] = [];
  for (const key of order) {
    for (const w of list) {
      if (!w || w.daemon !== key) continue;
      const first = rows.findIndex((r) => r.ws === w.id);
      const mine = rows.filter((r) => r.ws === w.id);
      out.push({
        ws: w.id,
        name: w.name,
        daemon: key,
        agents: mine,
        first: first < 0 ? rows.length : first,
        // The project's own declaration first, then the client's pin. Two
        // steps and no third: a project that wants a different agent says so
        // in the file it already has for exactly that, which lives with the
        // project and travels to the machine it runs on.
        preferred: preferredAgent(w, pinned),
      });
    }
  }
  return out;
}

/// The agent this project starts, or null when nothing names one and the
/// picker is the answer.
export function preferredAgent(
  ws: { autostart?: readonly string[] | null } | null | undefined,
  pinned: string | null,
): string | null {
  const declared = ws && ws.autostart && ws.autostart.length ? ws.autostart[0] : null;
  return declared || pinned || null;
}

/// The fleet column's rows: machine, then project, then that project's agents.
///
/// `sel` on an agent row is its index in the **fleet list**, which is what the
/// tray's copies carry. The cursor counts rows of *this* list, folds and all —
/// see `homeSelected` and `homePreview`.
///
/// Driven by the machine and project lists rather than by the agents, so an
/// empty project and a machine with nothing open both have a row. Folding is a
/// filter over that order and never a second ordering: a folded row is simply
/// not emitted, and the rows around it keep the positions they had.
export function homeRows(
  spaces: readonly SpaceRow[] | null | undefined,
  machines: readonly MachineRow[] | null | undefined,
  folds: Folds = NO_FOLDS,
): HomeRow[] {
  const all = spaces || [];
  const out: HomeRow[] = [];
  for (const m of machines || []) {
    const mine = all.filter((s) => s.daemon === m.daemon);
    const folded = folds.machines.has(m.label);
    out.push({
      kind: HomeRowKind.Machine,
      daemon: m.daemon,
      label: m.label,
      agents: mine.reduce((n, s) => n + s.agents.length, 0),
      folded,
    });
    if (folded) continue;
    for (const space of mine) {
      const shut = folds.spaces.has(space.ws);
      out.push({ kind: HomeRowKind.Space, space, machine: m.label, folded: shut });
      if (shut) continue;
      space.agents.forEach((row, i) => {
        out.push({ kind: HomeRowKind.Agent, row, sel: space.first + i });
      });
    }
  }
  return out;
}

/// The agent the cursor is *on*, as an index into the fleet list — null on a
/// machine or a project. What ending a session and the row menu act on.
export function homeSelected(rows: readonly HomeRow[], sel: number): number | null {
  const row = rows[sel];
  return row && row.kind === HomeRowKind.Agent ? row.sel : null;
}

/// The agent the stage shows.
///
/// On an agent row, that agent. **On a project row, the one in it that most
/// needs you** — so walking the fleet is a fly-over of each project's screen
/// rather than a cursor that keeps pointing the pane somewhere it has left. A
/// project with nothing running, and a machine row, preview nothing.
export function homePreview(rows: readonly HomeRow[], sel: number): number | null {
  const row = rows[sel];
  if (!row) return null;
  if (row.kind === HomeRowKind.Agent) return row.sel;
  if (row.kind === HomeRowKind.Machine) return null;
  let best: number | null = null;
  let rank = Infinity;
  row.space.agents.forEach((a, i) => {
    const r = trayRank(a.agent);
    // Strictly better, so a tie keeps fleet order — the pick decides which of a
    // project's screens you see, and one that re-broke its ties every tick
    // would flick between two agents while you read.
    if ((r === null ? Infinity : r) < rank) {
      rank = r === null ? Infinity : r;
      best = row.space.first + i;
    }
  });
  return best;
}

/// Fold every project, or open every one — whichever leaves more of the fleet
/// visible. The DIFF page's `Z`, against a two-level tree: it folds *projects*
/// and leaves the machines open, because that is the reading the key exists for
/// — every machine, every project, what is running in each, in one screen.
export function toggleAllSpaces(folds: Folds, spaces: readonly SpaceRow[]): Folds {
  const every = spaces.map((s) => s.ws);
  const allShut = every.length > 0 && every.every((k) => folds.spaces.has(k));
  return {
    machines: new Set<string>(),
    spaces: allShut ? new Set<Qid>() : new Set<Qid>(every),
    expanded: folds.expanded,
  };
}

/// Add or remove one key, without mutating the set that was handed in.
export function toggleFold<T>(set: ReadonlySet<T>, key: T): Set<T> {
  const next = new Set(set);
  if (!next.delete(key)) next.add(key);
  return next;
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

/// The one reading that answers "is this machine in trouble".
///
/// **Not the CPU.** A box at 30% CPU with a full root filesystem is in trouble
/// and its CPU number says it is fine, so this is the *worst* of the four
/// things that can run out — and it carries which one it was, because "97%"
/// without a name is a number you have to go and investigate.
///
/// The port of `machine_pressure`. Ties go to whichever comes first in CPU,
/// RAM, GPU, disk order: a strict `>`, so a machine sitting at 40% everywhere
/// reports its CPU every tick instead of flickering between four labels that
/// are all equally true.
export interface Pressure {
  /// The SYSTEM rail's own label for whatever won — three cells every time, so
  /// the reading beside it lands in one column down the whole list.
  label: "CPU" | "RAM" | "GPU" | "DSK";
  pct: number;
}

/// A used/total pair as a percentage, with a zero total reading as zero rather
/// than as a division by it. A machine reporting no RAM has not run out.
function pctOf(used: number, total: number): number {
  return total > 0 ? (used / total) * 100 : 0;
}

/// The worst-off resource on a machine.
///
/// Disks come from `sys.disks` unfiltered here, where the TUI asks its
/// configured `disk_mounts`: this client has no per-mount setting to honour
/// yet, so it takes every local filesystem and nothing else — an overlay is the
/// image under a container rather than a disk that can fill, and a tmpfs is RAM
/// the RAM reading already counts.
export function machinePressure(sys: SysDto | null | undefined): Pressure {
  if (!sys) return { label: "CPU", pct: 0 };
  const gpu = (sys.gpus || []).reduce(
    (worst, g) => Math.max(worst, g.pct, pctOf(g.mem_used_gb, g.mem_total_gb)),
    -Infinity,
  );
  const dsk = (sys.disks || [])
    .filter((d) => d.kind === "local")
    .reduce((worst, d) => Math.max(worst, pctOf(d.used_gb, d.total_gb)), -Infinity);

  let out: Pressure = { label: "CPU", pct: sys.cpu_pct || 0 };
  const rest: Pressure[] = [
    { label: "RAM", pct: pctOf(sys.ram_used_gb, sys.ram_total_gb) },
    { label: "GPU", pct: gpu },
    { label: "DSK", pct: dsk },
  ];
  for (const p of rest) if (p.pct > out.pct) out = p;
  return out;
}
