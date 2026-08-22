//! Commit lanes: turning a page of history into something with edges.
//!
//! Pure — no I/O, no async, no repository — so it can be tested exhaustively,
//! the same standing [`crate::layout`] has and for the same reason. The input
//! is one page of `git log`, newest first, and the output is one row per
//! commit: which column its node sits in, which columns carry a line past it,
//! and which join or leave there.
//!
//! **One row per commit, and that is the whole design constraint.** `git log
//! --graph` emits connector rows between commits, which is prettier and makes
//! the list stop being a list: the HISTORY cursor indexes commits, so a row
//! that is not a commit is a row the cursor has to skip, and every "diff what
//! I am on" becomes off-by-the-number-of-connectors-above-it. The rail's own
//! `change_rows` doc-comment records that bug happening once already. So joins
//! and forks are drawn *on* the commit's row, beside its node.
//!
//! The topological order this depends on comes from the daemon: `git/log`
//! walks `--topo-order` precisely so a child is never listed after its parent,
//! which is what lets lanes be assigned in one pass down the page.

/// One drawn row of the graph.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GraphRow {
    /// The column this commit's node sits in.
    pub lane: usize,
    /// Columns carrying a line straight past this row, node excluded.
    pub through: Vec<usize>,
    /// Columns that end here because they were waiting for this commit — the
    /// branches being merged in.
    pub merged: Vec<usize>,
    /// Columns that begin here, one per parent after the first: a merge seen
    /// from below is where its side branches start.
    pub forked: Vec<usize>,
    /// More lanes were open than [`graph_rows`] was allowed to draw.
    pub overflow: bool,
    /// This commit has more than one parent.
    pub merge: bool,
}

impl GraphRow {
    /// How many columns this row actually needs.
    pub fn width(&self) -> usize {
        let mut w = self.lane;
        for set in [&self.through, &self.merged, &self.forked] {
            for i in set {
                w = w.max(*i);
            }
        }
        w + 1
    }
}

/// Assign every commit a lane, in one pass down the page.
///
/// `max_lanes` bounds the drawing, not the walk: a repository with thirty live
/// branches still gets correct rows, but anything past the limit is reported as
/// [`GraphRow::overflow`] rather than allowed to squeeze the summary off the
/// screen.
///
/// `parents` may be empty on every commit — an older daemon does not send them
/// — and that is not an error: every row comes back in lane 0 with no edges,
/// which draws as the plain list of dots the page had before the graph existed.
pub fn graph_rows<'a, I>(commits: I, max_lanes: usize) -> Vec<GraphRow>
where
    I: IntoIterator<Item = (&'a str, &'a [String])>,
{
    // Each open lane is waiting for one commit id. That is the whole state.
    let mut lanes: Vec<Option<&'a str>> = Vec::new();
    let mut out = Vec::new();

    for (id, parents) in commits {
        // Every lane expecting this commit converges on it. The leftmost is
        // where the node goes, so a long-running branch keeps its column
        // instead of drifting right every time something merges into it.
        let waiting: Vec<usize> =
            lanes.iter().enumerate().filter(|(_, l)| **l == Some(id)).map(|(i, _)| i).collect();
        let lane = match waiting.first() {
            Some(&i) => i,
            None => free_lane(&mut lanes),
        };

        // Lines passing this row untouched: open, not the node, not merging.
        let through: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(i, l)| l.is_some() && *i != lane && !waiting.contains(i))
            .map(|(i, _)| i)
            .collect();

        let merged: Vec<usize> = waiting.iter().skip(1).copied().collect();
        for &i in &merged {
            lanes[i] = None;
        }

        // The first parent inherits the node's lane — that is what makes a
        // branch a straight line down the page rather than a staircase.
        lanes[lane] = parents.first().map(String::as_str);

        let mut forked = Vec::new();
        for p in parents.iter().skip(1) {
            // If something is already waiting for this parent, join it rather
            // than opening a second lane for the same commit: two lanes that
            // converge immediately are a fork nobody drew on purpose.
            let at = match lanes.iter().position(|l| *l == Some(p.as_str())) {
                Some(i) => i,
                None => {
                    let i = free_lane(&mut lanes);
                    lanes[i] = Some(p.as_str());
                    i
                }
            };
            forked.push(at);
        }

        // A lane that ended is only free once nothing to its right needs the
        // width; trimming here keeps the graph from growing a permanent margin
        // after one long-dead branch.
        while lanes.last() == Some(&None) {
            lanes.pop();
        }

        let mut row =
            GraphRow { lane, through, merged, forked, overflow: false, merge: parents.len() > 1 };
        row.overflow = row.width() > max_lanes;
        out.push(row);
    }
    out
}

/// The leftmost unused column, widening the graph only when it must.
fn free_lane(lanes: &mut Vec<Option<&str>>) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(i) => i,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}

/// The glyphs for one row, left to right, clamped to `max_lanes` columns.
///
/// Box-drawing and `●`, both of which the interface already uses — `draw_box`
/// everywhere and the Docker page's status dots. **No arrows or pointing
/// glyphs**: those are East-Asian-ambiguous width and render two cells wide in
/// some terminals, which does not look wrong, it shifts every cell after them
/// on the row. `SPACES_MARK`'s comment records the same rule for the chevron on
/// the tab bar's spaces button, which is `v` and not `▾` for exactly this.
pub fn glyphs(row: &GraphRow, max_lanes: usize) -> String {
    let width = row.width().min(max_lanes);
    // How far the connectors reach, so the run of `─` joining them to the node
    // is drawn and nothing beyond it is.
    let reach = row
        .merged
        .iter()
        .chain(row.forked.iter())
        .copied()
        .fold(row.lane, usize::max)
        .min(max_lanes.saturating_sub(1));
    let near = row.merged.iter().chain(row.forked.iter()).copied().fold(row.lane, usize::min);

    let mut s = String::with_capacity(width + 1);
    for i in 0..width {
        s.push(if i == row.lane {
            if row.merge {
                '◆'
            } else {
                '●'
            }
        } else if row.merged.contains(&i) {
            '╯'
        } else if row.forked.contains(&i) {
            '╮'
        } else if row.through.contains(&i) {
            // A line passing through wins over the horizontal run crossing it:
            // losing a branch's continuity is worse than a join that looks
            // like it steps around one.
            '│'
        } else if i > near && i < reach {
            '─'
        } else {
            ' '
        });
    }
    if row.overflow {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the `(id, parents)` pairs `graph_rows` takes from a compact
    /// spelling: `"c b a"` means commit `c` with parents `b` and `a`.
    fn walk(spec: &[&str]) -> Vec<(String, Vec<String>)> {
        spec.iter()
            .map(|line| {
                let mut it = line.split_whitespace();
                let id = it.next().unwrap().to_string();
                (id, it.map(str::to_string).collect())
            })
            .collect()
    }

    fn rows(spec: &[&str], max_lanes: usize) -> Vec<GraphRow> {
        let owned = walk(spec);
        graph_rows(owned.iter().map(|(id, ps)| (id.as_str(), ps.as_slice())), max_lanes)
    }

    fn drawn(spec: &[&str], max_lanes: usize) -> Vec<String> {
        rows(spec, max_lanes).iter().map(|r| glyphs(r, max_lanes)).collect()
    }

    /// A history with no branching is one column, whatever else is true.
    #[test]
    fn a_straight_line_stays_in_one_lane() {
        let rows = rows(&["c b", "b a", "a"], 6);
        assert!(rows.iter().all(|r| r.lane == 0), "{rows:?}");
        assert!(rows.iter().all(|r| r.through.is_empty() && r.forked.is_empty()));
        assert_eq!(drawn(&["c b", "b a", "a"], 6), vec!["●", "●", "●"]);
    }

    /// An older daemon sends no parents at all. That must degrade to the plain
    /// list the page drew before the graph existed, not to a panic or to a
    /// staircase of unrelated lanes.
    #[test]
    fn a_page_with_no_parents_is_a_plain_list() {
        assert_eq!(drawn(&["c", "b", "a"], 6), vec!["●", "●", "●"]);
    }

    /// The shape the whole module exists for: a merge opens a lane to its
    /// right, the side branch runs down it, and the lane closes where the two
    /// histories meet again.
    #[test]
    fn a_merge_opens_a_lane_and_the_join_closes_it() {
        // m ──┬── main (n) ── base
        //     └── side (s) ── base
        let spec = &["m n s", "n base", "s base", "base"];
        let rows = rows(spec, 6);

        assert!(rows[0].merge, "the merge is not marked");
        assert_eq!(rows[0].lane, 0);
        assert_eq!(rows[0].forked, vec![1], "the second parent did not open a lane");

        assert_eq!(rows[1].lane, 0, "the first parent did not inherit the lane");
        assert_eq!(rows[1].through, vec![1], "the side branch's lane is not drawn past it");

        assert_eq!(rows[2].lane, 1, "the side commit is not in the lane opened for it");

        // Both lanes were waiting for `base`; it takes the leftmost and the
        // other ends there.
        assert_eq!(rows[3].lane, 0);
        assert_eq!(rows[3].merged, vec![1], "the side lane never closed");

        assert_eq!(drawn(spec, 6), vec!["◆╮", "●│", "│●", "●╯"]);
    }

    /// Two independent tips, neither reachable from the other, get a column
    /// each — this is what `?all=1` produces and the reason it exists.
    #[test]
    fn unrelated_tips_get_their_own_lanes() {
        let rows = rows(&["a", "b"], 6);
        assert_eq!(rows[0].lane, 0);
        assert_eq!(rows[1].lane, 0, "a finished lane is reused, not left as a gap");
    }

    /// A merge whose second parent is already awaited joins that lane instead
    /// of opening a duplicate — two lanes for one commit would converge on the
    /// very next row, which reads as a fork that was never there.
    #[test]
    fn a_second_parent_already_awaited_joins_its_lane() {
        // Both `m` and `n` have `s` as a parent.
        let rows = rows(&["m n s", "n s", "s"], 6);
        assert_eq!(rows[0].forked, vec![1]);
        // `n` keeps lane 0 and its only parent is `s`, which lane 1 already
        // awaits — so by the time `s` arrives, two lanes want it.
        assert_eq!(rows[2].lane, 0);
        assert_eq!(rows[2].merged, vec![1], "the duplicate lane was not collapsed");
    }

    /// Past the limit the row says so rather than letting the graph eat the
    /// summary. Silent truncation would read as "this repository has six
    /// branches", which is a different and wrong statement.
    #[test]
    fn too_many_lanes_are_reported_not_hidden() {
        // One commit with five parents opens four lanes beside its own.
        let rows = rows(&["m a b c d e", "a", "b", "c", "d", "e"], 3);
        assert!(rows[0].overflow, "a five-lane row did not report overflow");
        let g = glyphs(&rows[0], 3);
        assert!(g.ends_with('…'), "overflow is not marked: {g}");
        assert_eq!(g.chars().count(), 4, "overflow row is wider than its cap: {g}");
    }

    /// Width is what the drawing asks the row for, so it has to count every
    /// column the row uses — not just the node's.
    #[test]
    fn width_counts_every_column_in_use() {
        let row = GraphRow {
            lane: 0,
            through: vec![2],
            merged: vec![],
            forked: vec![4],
            ..GraphRow::default()
        };
        assert_eq!(row.width(), 5);
    }

    /// A line passing through is never overwritten by the horizontal run of a
    /// join reaching across it: a branch that visibly stops mid-page is a
    /// branch the reader thinks ended.
    #[test]
    fn a_passing_line_survives_a_join_reaching_over_it() {
        let row = GraphRow {
            lane: 0,
            through: vec![1],
            merged: vec![2],
            forked: vec![],
            overflow: false,
            merge: false,
        };
        assert_eq!(glyphs(&row, 6), "●│╯");
    }
}
