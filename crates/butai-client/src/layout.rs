//! The tmux-style layout engine: a binary split tree per window.
//!
//! Everything here is pure — no I/O, no async — so it can be tested
//! exhaustively. Rect math is integer cell arithmetic; a 1-cell separator is
//! reserved between split siblings (the renderer draws borders there).

use butai_protocol::{Dir, PaneId, SplitDir};
use serde::{Deserialize, Serialize};

/// Minimum pane extent on either axis. Ratios are clamped so no pane ever
/// shrinks below this.
pub const MIN_PANE: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self { x, y, width, height }
    }

    pub fn right(&self) -> u16 {
        self.x + self.width
    }

    pub fn bottom(&self) -> u16 {
        self.y + self.height
    }

    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum LayoutNode {
    Leaf(PaneId),
    Split { dir: SplitDir, ratio: f32, first: Box<LayoutNode>, second: Box<LayoutNode> },
}

impl LayoutNode {
    pub fn leaf(id: PaneId) -> Self {
        LayoutNode::Leaf(id)
    }

    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect_panes(&mut out);
        out
    }

    fn collect_panes(&self, out: &mut Vec<PaneId>) {
        match self {
            LayoutNode::Leaf(id) => out.push(*id),
            LayoutNode::Split { first, second, .. } => {
                first.collect_panes(out);
                second.collect_panes(out);
            }
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            LayoutNode::Leaf(p) => *p == id,
            LayoutNode::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    pub fn first_pane(&self) -> PaneId {
        match self {
            LayoutNode::Leaf(id) => *id,
            LayoutNode::Split { first, .. } => first.first_pane(),
        }
    }

    /// Replace the leaf `target` with a split of `target` and `new`. The new
    /// pane becomes `second` (to the right / below). Returns false if
    /// `target` is not in the tree.
    pub fn split(&mut self, target: PaneId, dir: SplitDir, new: PaneId) -> bool {
        match self {
            LayoutNode::Leaf(id) if *id == target => {
                *self = LayoutNode::Split {
                    dir,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Leaf(target)),
                    second: Box::new(LayoutNode::Leaf(new)),
                };
                true
            }
            LayoutNode::Leaf(_) => false,
            LayoutNode::Split { first, second, .. } => {
                first.split(target, dir, new) || second.split(target, dir, new)
            }
        }
    }

    /// Remove the leaf `target`; its sibling subtree promotes into the
    /// parent's place. Returns false when `target` is the root leaf or not
    /// present (callers handle "last pane in window" themselves).
    pub fn remove(&mut self, target: PaneId) -> bool {
        if let LayoutNode::Split { first, second, .. } = self {
            if matches!(first.as_ref(), LayoutNode::Leaf(id) if *id == target) {
                *self = (**second).clone();
                return true;
            }
            if matches!(second.as_ref(), LayoutNode::Leaf(id) if *id == target) {
                *self = (**first).clone();
                return true;
            }
            return first.remove(target) || second.remove(target);
        }
        false
    }

    /// Compute the screen rect of every pane. Split siblings are separated by
    /// a 1-cell gap on the split axis for the border.
    pub fn rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        self.rects_into(area, &mut out);
        out
    }

    fn rects_into(&self, area: Rect, out: &mut Vec<(PaneId, Rect)>) {
        match self {
            LayoutNode::Leaf(id) => out.push((*id, area)),
            LayoutNode::Split { dir, ratio, first, second } => {
                let (a, b) = split_area(area, *dir, *ratio);
                first.rects_into(a, out);
                second.rects_into(b, out);
            }
        }
    }

    /// Adjust the ratio of the nearest ancestor split of `dir`'s orientation
    /// that contains `target`, by `delta_cells` within `area`. Growth is in
    /// the direction of `dir` (tmux semantics: resize-pane -L grows leftward
    /// edge). Returns true if some split was adjusted.
    pub fn resize(&mut self, target: PaneId, dir: Dir, delta_cells: i16, area: Rect) -> bool {
        let want = match dir {
            Dir::Left | Dir::Right => SplitDir::Horizontal,
            Dir::Up | Dir::Down => SplitDir::Vertical,
        };
        self.resize_inner(target, want, dir, delta_cells, area)
    }

    fn resize_inner(
        &mut self,
        target: PaneId,
        want: SplitDir,
        dir: Dir,
        delta_cells: i16,
        area: Rect,
    ) -> bool {
        let LayoutNode::Split { dir: sdir, ratio, first, second } = self else {
            return false;
        };
        let (a, b) = split_area(area, *sdir, *ratio);
        // Prefer the deepest matching split: recurse first.
        if first.contains(target) {
            if first.resize_inner(target, want, dir, delta_cells, a) {
                return true;
            }
        } else if second.contains(target) {
            if second.resize_inner(target, want, dir, delta_cells, b) {
                return true;
            }
        } else {
            return false;
        }
        if *sdir != want {
            return false;
        }
        let extent = match sdir {
            SplitDir::Horizontal => area.width,
            SplitDir::Vertical => area.height,
        };
        if extent <= 2 * MIN_PANE + 1 {
            return false;
        }
        // Growing the first child moves the border toward second, and vice
        // versa. "Grow" means: pane containing `target` gets bigger when the
        // user resizes toward the opposite edge.
        let target_in_first = first.contains(target);
        let grow_first = match dir {
            Dir::Right | Dir::Down => target_in_first,
            Dir::Left | Dir::Up => !target_in_first,
        };
        let signed = if grow_first { delta_cells } else { -delta_cells };
        let new_ratio = *ratio + signed as f32 / extent as f32;
        *ratio = clamp_ratio(new_ratio, extent);
        true
    }

    /// The pane geometrically adjacent to `from` in direction `dir`,
    /// preferring the neighbor with the largest shared edge.
    pub fn neighbor(&self, from: PaneId, dir: Dir, area: Rect) -> Option<PaneId> {
        let rects = self.rects(area);
        let (_, fr) = rects.iter().find(|(id, _)| *id == from)?;
        let fr = *fr;
        rects
            .iter()
            .filter(|(id, _)| *id != from)
            .filter(|(_, r)| match dir {
                Dir::Left => r.right() <= fr.x && v_overlap(r, &fr) > 0,
                Dir::Right => r.x >= fr.right() && v_overlap(r, &fr) > 0,
                Dir::Up => r.bottom() <= fr.y && h_overlap(r, &fr) > 0,
                Dir::Down => r.y >= fr.bottom() && h_overlap(r, &fr) > 0,
            })
            .min_by_key(|(_, r)| {
                let gap = match dir {
                    Dir::Left => fr.x - r.right(),
                    Dir::Right => r.x - fr.right(),
                    Dir::Up => fr.y - r.bottom(),
                    Dir::Down => r.y - fr.bottom(),
                } as u32;
                let overlap = match dir {
                    Dir::Left | Dir::Right => v_overlap(r, &fr),
                    Dir::Up | Dir::Down => h_overlap(r, &fr),
                } as u32;
                // Closest first; among equally close, largest overlap.
                (gap, u32::MAX - overlap)
            })
            .map(|(id, _)| *id)
    }
}

fn clamp_ratio(ratio: f32, extent: u16) -> f32 {
    let min = (MIN_PANE as f32 + 0.5) / extent as f32;
    ratio.clamp(min.min(0.5), (1.0 - min).max(0.5))
}

fn v_overlap(a: &Rect, b: &Rect) -> u16 {
    overlap(a.y, a.bottom(), b.y, b.bottom())
}

fn h_overlap(a: &Rect, b: &Rect) -> u16 {
    overlap(a.x, a.right(), b.x, b.right())
}

fn overlap(a0: u16, a1: u16, b0: u16, b1: u16) -> u16 {
    let lo = a0.max(b0);
    let hi = a1.min(b1);
    hi.saturating_sub(lo)
}

/// Divide `area` into two child areas with a 1-cell separator between them.
/// The first child gets `round(inner * ratio)` cells (clamped to MIN_PANE);
/// the second gets the remainder. Degenerate areas collapse gracefully.
pub fn split_area(area: Rect, dir: SplitDir, ratio: f32) -> (Rect, Rect) {
    match dir {
        SplitDir::Horizontal => {
            let inner = area.width.saturating_sub(1);
            let first_w = share(inner, ratio);
            let second_w = inner - first_w;
            (
                Rect::new(area.x, area.y, first_w, area.height),
                Rect::new(
                    area.x + first_w + u16::from(area.width > first_w),
                    area.y,
                    second_w,
                    area.height,
                ),
            )
        }
        SplitDir::Vertical => {
            let inner = area.height.saturating_sub(1);
            let first_h = share(inner, ratio);
            let second_h = inner - first_h;
            (
                Rect::new(area.x, area.y, area.width, first_h),
                Rect::new(
                    area.x,
                    area.y + first_h + u16::from(area.height > first_h),
                    area.width,
                    second_h,
                ),
            )
        }
    }
}

fn share(inner: u16, ratio: f32) -> u16 {
    if inner == 0 {
        return 0;
    }
    let first = (inner as f32 * ratio).round() as u16;
    if inner <= 2 * MIN_PANE {
        // Too small to honor minimums; just clamp into range.
        return first.min(inner);
    }
    first.clamp(MIN_PANE, inner - MIN_PANE)
}

/// One window (tab): a pane tree plus focus and zoom state.
#[derive(Debug, Clone)]
pub struct Window {
    pub name: String,
    pub root: LayoutNode,
    pub focused: PaneId,
    pub zoomed: Option<PaneId>,
}

impl Window {
    pub fn new(name: impl Into<String>, pane: PaneId) -> Self {
        Self { name: name.into(), root: LayoutNode::leaf(pane), focused: pane, zoomed: None }
    }

    /// Rects of the panes that should be rendered: the whole area for a
    /// zoomed pane, the tree layout otherwise.
    pub fn visible_rects(&self, area: Rect) -> Vec<(PaneId, Rect)> {
        if let Some(z) = self.zoomed {
            if self.root.contains(z) {
                return vec![(z, area)];
            }
        }
        self.root.rects(area)
    }

    pub fn split(&mut self, dir: SplitDir, new: PaneId) -> bool {
        self.zoomed = None;
        let ok = self.root.split(self.focused, dir, new);
        if ok {
            self.focused = new;
        }
        ok
    }

    /// Remove a pane. Returns `RemoveOutcome::WindowEmpty` when it was the
    /// last pane.
    pub fn remove(&mut self, id: PaneId) -> RemoveOutcome {
        if self.zoomed == Some(id) {
            self.zoomed = None;
        }
        if matches!(&self.root, LayoutNode::Leaf(p) if *p == id) {
            return RemoveOutcome::WindowEmpty;
        }
        if self.root.remove(id) {
            if self.focused == id {
                self.focused = self.root.first_pane();
            }
            RemoveOutcome::Removed
        } else {
            RemoveOutcome::NotFound
        }
    }

    pub fn focus_dir(&mut self, dir: Dir, area: Rect) -> bool {
        if self.zoomed.is_some() {
            return false;
        }
        if let Some(next) = self.root.neighbor(self.focused, dir, area) {
            self.focused = next;
            true
        } else {
            false
        }
    }

    pub fn toggle_zoom(&mut self) {
        self.zoomed = match self.zoomed {
            Some(_) => None,
            None if matches!(self.root, LayoutNode::Split { .. }) => Some(self.focused),
            None => None,
        };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveOutcome {
    Removed,
    WindowEmpty,
    NotFound,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(n: u64) -> PaneId {
        PaneId(n)
    }

    fn area() -> Rect {
        Rect::new(0, 0, 120, 40)
    }

    /// Panes must tile the area exactly: no overlap, and every cell belongs
    /// to exactly one pane or one separator line.
    fn assert_tiling(node: &LayoutNode, area: Rect) {
        let rects = node.rects(area);
        let mut covered = vec![vec![0u8; area.width as usize]; area.height as usize];
        for (_, r) in &rects {
            for y in r.y..r.bottom() {
                for x in r.x..r.right() {
                    covered[(y - area.y) as usize][(x - area.x) as usize] += 1;
                }
            }
        }
        for row in &covered {
            for &c in row {
                assert!(c <= 1, "pane rects overlap");
            }
        }
        let pane_cells: u32 = rects.iter().map(|(_, r)| r.width as u32 * r.height as u32).sum();
        let seps = rects.len() as u32 - 1;
        assert!(pane_cells + seps <= area.width as u32 * area.height as u32, "panes exceed area");
    }

    #[test]
    fn single_pane_fills_area() {
        let node = LayoutNode::leaf(p(1));
        assert_eq!(node.rects(area()), vec![(p(1), area())]);
    }

    #[test]
    fn split_and_remove_roundtrip() {
        let mut node = LayoutNode::leaf(p(1));
        assert!(node.split(p(1), SplitDir::Horizontal, p(2)));
        assert!(node.split(p(2), SplitDir::Vertical, p(3)));
        assert_tiling(&node, area());
        assert_eq!(node.panes(), vec![p(1), p(2), p(3)]);
        assert!(node.remove(p(3)));
        assert!(node.remove(p(2)));
        assert_eq!(node, LayoutNode::leaf(p(1)));
    }

    #[test]
    fn horizontal_split_rects() {
        let mut node = LayoutNode::leaf(p(1));
        node.split(p(1), SplitDir::Horizontal, p(2));
        let rects = node.rects(Rect::new(0, 0, 81, 24));
        // 80 inner cells, ratio 0.5 -> 40 | border | 40
        assert_eq!(rects[0].1, Rect::new(0, 0, 40, 24));
        assert_eq!(rects[1].1, Rect::new(41, 0, 40, 24));
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        let mut node = LayoutNode::leaf(p(1));
        node.split(p(1), SplitDir::Horizontal, p(2));
        node.split(p(2), SplitDir::Vertical, p(3));
        for w in 0..6 {
            for h in 0..6 {
                let _ = node.rects(Rect::new(0, 0, w, h));
            }
        }
    }

    #[test]
    fn neighbor_navigation() {
        let mut node = LayoutNode::leaf(p(1));
        node.split(p(1), SplitDir::Horizontal, p(2)); // 1 | 2
        node.split(p(2), SplitDir::Vertical, p(3)); // 1 | (2 / 3)
        let a = area();
        assert_eq!(node.neighbor(p(1), Dir::Right, a), Some(p(2)));
        assert_eq!(node.neighbor(p(2), Dir::Left, a), Some(p(1)));
        assert_eq!(node.neighbor(p(2), Dir::Down, a), Some(p(3)));
        assert_eq!(node.neighbor(p(3), Dir::Up, a), Some(p(2)));
        assert_eq!(node.neighbor(p(1), Dir::Left, a), None);
        assert_eq!(node.neighbor(p(3), Dir::Left, a), Some(p(1)));
    }

    #[test]
    fn resize_moves_border() {
        let mut node = LayoutNode::leaf(p(1));
        node.split(p(1), SplitDir::Horizontal, p(2));
        let a = Rect::new(0, 0, 101, 30);
        let before = node.rects(a)[0].1.width;
        assert!(node.resize(p(1), Dir::Right, 10, a));
        let after = node.rects(a)[0].1.width;
        assert_eq!(after, before + 10);
        // Resizing on the wrong axis of the only split fails.
        assert!(!node.resize(p(1), Dir::Down, 5, a));
    }

    #[test]
    fn resize_respects_min_pane() {
        let mut node = LayoutNode::leaf(p(1));
        node.split(p(1), SplitDir::Horizontal, p(2));
        let a = Rect::new(0, 0, 41, 20);
        assert!(node.resize(p(1), Dir::Right, 1000, a));
        let rects = node.rects(a);
        assert!(rects[1].1.width >= MIN_PANE);
        assert_tiling(&node, a);
    }

    #[test]
    fn window_zoom_and_focus() {
        let mut w = Window::new("main", p(1));
        w.split(SplitDir::Horizontal, p(2));
        assert_eq!(w.focused, p(2));
        assert!(w.focus_dir(Dir::Left, area()));
        assert_eq!(w.focused, p(1));
        w.toggle_zoom();
        assert_eq!(w.zoomed, Some(p(1)));
        assert_eq!(w.visible_rects(area()), vec![(p(1), area())]);
        w.toggle_zoom();
        assert_eq!(w.zoomed, None);
    }

    #[test]
    fn window_remove_last_pane_reports_empty() {
        let mut w = Window::new("main", p(1));
        assert_eq!(w.remove(p(1)), RemoveOutcome::WindowEmpty);
        w.split(SplitDir::Vertical, p(2));
        assert_eq!(w.remove(p(2)), RemoveOutcome::Removed);
        assert_eq!(w.focused, p(1));
    }
}
