// Commit lanes: turning a page of history into something with edges.
//
// The port of `crates/butai-client/src/graph.rs`, glyph for glyph. Pure — no
// DOM, no network, no repository — so `test/graph.test.ts` walks every shape a
// history takes without a daemon, which is the standing the Rust original has
// and for the same reason.
//
// **One row per commit, and that is the whole design constraint.** `git log
// --graph` emits connector rows between commits, which is prettier and makes
// the list stop being a list: the HISTORY cursor indexes commits, so a row that
// is not a commit is a row the cursor has to skip, and every "diff what I am
// on" becomes off-by-the-number-of-connectors-above-it. So joins and forks are
// drawn *on* the commit's row, beside its node.
//
// The topological order this depends on comes from the daemon: `git/log` walks
// `--topo-order` precisely so a child is never listed after its parent, which
// is what lets lanes be assigned in one pass down the page.

import type { LogEntryDto } from "../protocol/generated/protocol.ts";

/// How many lanes the page will draw before it collapses the rest into `…`.
export const MAX_LANES = 6;

/// What the lanes need of a commit: its id and its parents.
///
/// `parents` is optional where [`LogEntryDto`] has it required, because it
/// landed after 0.8 and an older daemon sends a page of commits without it.
export type GraphCommit = Pick<LogEntryDto, "id"> & { parents?: readonly string[] };

/// One commit's row: where its node sits, and every line that touches the row.
export interface GraphRow {
  /// The column the node is drawn in.
  lane: number;
  /// Lanes passing this row untouched.
  through: number[];
  /// Lanes converging on the node here, and closing.
  merged: number[];
  /// Lanes opened here for this commit's second and later parents.
  forked: number[];
  /// Wider than the page draws, so the rest collapses into `…`.
  overflow: boolean;
  /// More than one parent.
  merge: boolean;
}

/// How many columns a row actually needs.
export function rowWidth(row: Pick<GraphRow, "lane" | "through" | "merged" | "forked">): number {
  let w = row.lane;
  for (const set of [row.through, row.merged, row.forked]) {
    for (const i of set) if (i > w) w = i;
  }
  return w + 1;
}

/// Assign every commit a lane, in one pass down the page.
///
/// `commits` is `[{id, parents}]`, newest first. `maxLanes` bounds the
/// *drawing*, not the walk: a repository with thirty live branches still gets
/// correct rows, but anything past the limit is reported as `overflow` rather
/// than allowed to squeeze the summary off the screen.
///
/// `parents` may be empty on every commit — an older daemon does not send them
/// — and that is not an error: every row comes back in lane 0 with no edges,
/// which draws as the plain list of dots the page had before the graph existed.
export function graphRows(commits: readonly GraphCommit[] | null | undefined, maxLanes = MAX_LANES): GraphRow[] {
  // Each open lane is waiting for one commit id. That is the whole state.
  const lanes: (string | null)[] = [];
  const out: GraphRow[] = [];

  for (const c of commits || []) {
    const id = c.id;
    const parents = c.parents || [];
    // Every lane expecting this commit converges on it. The leftmost is where
    // the node goes, so a long-running branch keeps its column instead of
    // drifting right every time something merges into it.
    const waiting: number[] = [];
    for (let i = 0; i < lanes.length; i++) if (lanes[i] === id) waiting.push(i);
    const first = waiting[0];
    const lane = first === undefined ? freeLane(lanes) : first;

    // Lines passing this row untouched: open, not the node, not merging.
    const through: number[] = [];
    for (let i = 0; i < lanes.length; i++) {
      if (lanes[i] != null && i !== lane && !waiting.includes(i)) through.push(i);
    }

    const merged = waiting.slice(1);
    for (const i of merged) lanes[i] = null;

    // The first parent inherits the node's lane — that is what makes a branch a
    // straight line down the page rather than a staircase.
    lanes[lane] = parents[0] ?? null;

    const forked: number[] = [];
    for (const p of parents.slice(1)) {
      // If something is already waiting for this parent, join it rather than
      // opening a second lane for the same commit: two lanes that converge
      // immediately are a fork nobody drew on purpose.
      let at = lanes.indexOf(p);
      if (at < 0) {
        at = freeLane(lanes);
        lanes[at] = p;
      }
      forked.push(at);
    }

    // A lane that ended is only free once nothing to its right needs the width;
    // trimming here keeps the graph from growing a permanent margin after one
    // long-dead branch.
    while (lanes.length && lanes[lanes.length - 1] == null) lanes.pop();

    const row: GraphRow = { lane, through, merged, forked, overflow: false, merge: parents.length > 1 };
    row.overflow = rowWidth(row) > maxLanes;
    out.push(row);
  }
  return out;
}

/// The leftmost unused column, widening the graph only when it must.
function freeLane(lanes: (string | null)[]): number {
  const i = lanes.indexOf(null);
  if (i >= 0) return i;
  lanes.push(null);
  return lanes.length - 1;
}

/// The glyphs for one row, left to right, clamped to `maxLanes` columns.
///
/// Box-drawing and `●`, both of which the interface already uses. **No arrows
/// or pointing glyphs**: those are East-Asian-ambiguous width and render two
/// cells wide in some terminals. That is a terminal hazard rather than a
/// browser one, but the two clients draw the same graph and a glyph that is
/// wrong in one of them is a glyph that is wrong.
export function glyphs(row: GraphRow, maxLanes = MAX_LANES): string {
  const width = Math.min(rowWidth(row), maxLanes);
  const ends = row.merged.concat(row.forked);
  // How far the connectors reach, so the run of `─` joining them to the node is
  // drawn and nothing beyond it is.
  let reach = row.lane;
  for (const i of ends) if (i > reach) reach = i;
  reach = Math.min(reach, maxLanes - 1);
  let near = row.lane;
  for (const i of ends) if (i < near) near = i;

  let s = "";
  for (let i = 0; i < width; i++) {
    if (i === row.lane) s += row.merge ? "◆" : "●";
    else if (row.merged.includes(i)) s += "╯";
    else if (row.forked.includes(i)) s += "╮";
    // A line passing through wins over the horizontal run crossing it: losing a
    // branch's continuity is worse than a join that looks like it steps around
    // one.
    else if (row.through.includes(i)) s += "│";
    else if (i > near && i < reach) s += "─";
    else s += " ";
  }
  if (row.overflow) s += "…";
  return s;
}

/// How wide the whole page's graph column is, so the shas and summaries line up
/// down the list instead of stepping in and out with the branching.
export function graphWidth(rows: readonly GraphRow[], maxLanes = MAX_LANES): number {
  let w = 0;
  for (const r of rows) w = Math.max(w, Math.min(rowWidth(r), maxLanes));
  if (rows.some((r) => r.overflow)) w += 1;
  return w;
}
