//! The agent tree, as the CLI sees it: who belongs under whom.
//!
//! An agent spawned from inside a pane records that pane as its parent, and a
//! fork records the agent it copied (`AgentDto::parent`, `AgentDto::forked`).
//! A **helper** is a child that is not a fork: the parent started it to do part
//! of its work, so it is the parent's to wait on and to close. A fork is a
//! person's copy of a conversation and belongs to nobody — the daemon leaves it
//! running when its parent is killed.
//!
//! Everything here is pure, over `(pane, parent, forked)` links, so the shapes
//! the commands print and act on can be pinned without a daemon.

use std::collections::HashSet;

use butai_protocol::api::AgentDto;
use butai_protocol::PaneId;

/// One agent's place in the tree: `(pane, parent, forked)`.
pub type Link = (PaneId, Option<PaneId>, bool);

/// The links of a workspace's agents, in the daemon's order.
pub fn links<'a>(agents: impl IntoIterator<Item = &'a AgentDto>) -> Vec<Link> {
    agents.into_iter().map(|a| (a.pane, a.parent, a.forked)).collect()
}

/// The order to list agents in so each child sits right under its parent, as
/// indices into `links`, each with the branch glyphs to put in front of its
/// label: `│ ` for every ancestor with more siblings below, blanks where it has
/// none, then `├ ` or `└ `. A root gets no glyphs.
///
/// The same shape as BOOTH's fleet. Siblings keep the order they came in, and
/// an agent whose parent is not in the list is a root — so a filtered list
/// (just one agent's helpers, say) draws as a tree of its own.
pub fn tree_order(links: &[Link]) -> Vec<(usize, String)> {
    let present: HashSet<PaneId> = links.iter().map(|l| l.0).collect();
    let is_root = |l: &Link| l.1.is_none_or(|p| p == l.0 || !present.contains(&p));
    let mut out = Vec::with_capacity(links.len());
    let mut seen = HashSet::new();
    for (i, l) in links.iter().enumerate() {
        if is_root(l) {
            walk(links, i, String::new(), String::new(), &mut seen, &mut out);
        }
    }
    // A parent loop has no root to start from. It is a bug somewhere else, but
    // a listing that silently drops agents would hide it: list them flat.
    for i in 0..links.len() {
        if !seen.contains(&i) {
            walk(links, i, String::new(), String::new(), &mut seen, &mut out);
        }
    }
    out
}

fn walk(
    links: &[Link],
    i: usize,
    glyphs: String,
    indent: String,
    seen: &mut HashSet<usize>,
    out: &mut Vec<(usize, String)>,
) {
    if !seen.insert(i) {
        return;
    }
    out.push((i, glyphs));
    let pane = links[i].0;
    let kids: Vec<usize> = (0..links.len())
        .filter(|&k| k != i && links[k].1 == Some(pane) && !seen.contains(&k))
        .collect();
    for (n, &k) in kids.iter().enumerate() {
        let last = n + 1 == kids.len();
        let glyphs = format!("{indent}{}", if last { "└ " } else { "├ " });
        let indent = format!("{indent}{}", if last { "  " } else { "│ " });
        walk(links, k, glyphs, indent, seen, out);
    }
}

/// The helpers of `of`: its children that are not forks, in list order. With
/// `deep`, their helpers too, to any depth, parents before their children —
/// but never through a fork, whose subtree is not `of`'s.
pub fn helpers(links: &[Link], of: PaneId, deep: bool) -> Vec<PaneId> {
    let mut out = Vec::new();
    let mut seen = HashSet::from([of]);
    collect_helpers(links, of, deep, &mut seen, &mut out);
    out
}

fn collect_helpers(
    links: &[Link],
    of: PaneId,
    deep: bool,
    seen: &mut HashSet<PaneId>,
    out: &mut Vec<PaneId>,
) {
    for &(pane, parent, forked) in links {
        if parent == Some(of) && !forked && seen.insert(pane) {
            out.push(pane);
            if deep {
                collect_helpers(links, pane, deep, seen, out);
            }
        }
    }
}

/// What the daemon closes when `target` is killed, in the order it closes
/// them: every helper below it, deepest first, then `target` last. With
/// `keep_helpers`, `target` alone. Mirrors the daemon's own plan, so the CLI
/// can say what went without a second round-trip.
pub fn kill_order(links: &[Link], target: PaneId, keep_helpers: bool) -> Vec<PaneId> {
    if keep_helpers {
        return vec![target];
    }
    let depth_of = |mut pane: PaneId| {
        let mut d = 0usize;
        while pane != target && d <= links.len() {
            match links.iter().find(|l| l.0 == pane).and_then(|l| l.1) {
                Some(p) => {
                    pane = p;
                    d += 1;
                }
                None => break,
            }
        }
        d
    };
    let below = helpers(links, target, true);
    let max = below.iter().map(|p| depth_of(*p)).max().unwrap_or(0);
    let mut out: Vec<PaneId> = Vec::with_capacity(below.len() + 1);
    // The daemon walks breadth first and closes level by level; within a level
    // its order is the rail's, which is `links` order.
    for depth in (1..=max).rev() {
        out.extend(
            links.iter().map(|l| l.0).filter(|p| below.contains(p) && depth_of(*p) == depth),
        );
    }
    out.push(target);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(pane: u64, parent: Option<u64>, forked: bool) -> Link {
        (PaneId(pane), parent.map(PaneId), forked)
    }

    fn ids(panes: &[u64]) -> Vec<PaneId> {
        panes.iter().copied().map(PaneId).collect()
    }

    /// A lead (1) with helpers 2 and 3, a helper of a helper (4, under 2), a
    /// fork (5) of the lead with a helper of its own (6), and an unrelated
    /// agent (7). Listed in spawn order, which is how the daemon lists them.
    fn family() -> Vec<Link> {
        vec![
            link(1, None, false),
            link(2, Some(1), false),
            link(3, Some(1), false),
            link(4, Some(2), false),
            link(5, Some(1), true),
            link(6, Some(5), false),
            link(7, None, false),
        ]
    }

    fn drawn(links: &[Link]) -> Vec<String> {
        tree_order(links).into_iter().map(|(i, g)| format!("{g}{}", links[i].0)).collect()
    }

    #[test]
    fn children_sit_under_their_parent_with_branch_glyphs() {
        assert_eq!(drawn(&family()), ["1", "├ 2", "│ └ 4", "├ 3", "└ 5", "  └ 6", "7"]);
    }

    /// A child listed before its parent still draws under it, and an agent
    /// whose parent is not in the list is a root.
    #[test]
    fn order_comes_from_the_links_and_missing_parents_make_roots() {
        let links = [link(3, Some(1), false), link(1, None, false), link(9, Some(42), false)];
        assert_eq!(drawn(&links), ["1", "└ 3", "9"]);
    }

    /// A loop is somebody's bug; it must neither hang the listing nor drop rows.
    #[test]
    fn a_parent_loop_lists_every_agent_once() {
        let links = [link(1, Some(2), false), link(2, Some(1), false), link(3, Some(3), false)];
        let rows = drawn(&links);
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(rows.contains(&"3".to_string()), "a self-parent is a root: {rows:?}");
    }

    #[test]
    fn helpers_are_the_children_that_are_not_forks() {
        let links = family();
        assert_eq!(helpers(&links, PaneId(1), false), ids(&[2, 3]));
        assert_eq!(helpers(&links, PaneId(1), true), ids(&[2, 4, 3]), "never through the fork");
        assert_eq!(helpers(&links, PaneId(5), false), ids(&[6]), "a fork has helpers of its own");
        assert!(helpers(&links, PaneId(7), true).is_empty());
        assert!(helpers(&links, PaneId(99), true).is_empty(), "an unknown pane has none");
    }

    /// The same order the daemon closes in: deepest first, the target last,
    /// the fork and its helper left alone.
    #[test]
    fn a_kill_closes_helpers_deepest_first_then_the_target() {
        let links = family();
        assert_eq!(kill_order(&links, PaneId(1), false), ids(&[4, 2, 3, 1]));
        assert_eq!(kill_order(&links, PaneId(1), true), ids(&[1]));
        assert_eq!(kill_order(&links, PaneId(2), false), ids(&[4, 2]));
        assert_eq!(kill_order(&links, PaneId(7), false), ids(&[7]), "a leaf is only itself");
        assert_eq!(kill_order(&links, PaneId(99), false), ids(&[99]), "so is a process");
    }
}
