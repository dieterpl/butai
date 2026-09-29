//! Dragging a selection over the screen, and copying what it covers.
//!
//! Client work by nature, and now client work in fact. The daemon did this
//! against the frame it had composed for one particular client — which is the
//! clearest case in the whole refactor of the server holding something that was
//! never its own: a text selection is a person pointing at pixels, per client,
//! and two people looking at one workspace select different things.
//!
//! It reads the *composed screen*, not a pane, so a drag copies whatever is
//! under it: a rail's rows, a diff, a file. [`region`] confines it to the column
//! the drag began in, so straying into the tree sidebar still yields a clean
//! rectangle rather than two panes interleaved line by line.
//!
//! The copy goes out as OSC 52, which the terminal forwards to the system
//! clipboard — over ssh too, and with no display server, which is why this does
//! not go through arboard the way [`crate::clipboard`] does.

use crate::layout::Rect as LRect;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::chrome::{Page, View};

/// A drag in progress, or a finished one waiting to be copied.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Drag {
    /// Where the press landed. Set on mouse-down, cleared on copy.
    pub anchor: Option<(u16, u16)>,
    /// Anchor and current position, once the pointer has actually moved.
    pub span: Option<((u16, u16), (u16, u16))>,
    /// The region the press started in; the selection stays inside it.
    pub clip: Option<LRect>,
}

/// What the page's own state adds to a geometry that is otherwise the screen's.
///
/// Two numbers the rectangles depend on and the screen cannot supply, both from
/// the page struct the caller holds: how many cells of line numbers the open
/// file is drawing, and how many columns the Finder trail has been walked to.
/// They travel together because every site that needs one needs the other.
#[derive(Debug, Clone, Copy, Default)]
pub struct PageMetrics {
    /// Cells of line numbers down the left of the text, from
    /// [`crate::chrome::editor_gutter_w`] — zero on every page that has none.
    pub gutter: u16,
    /// Columns in the Files trail, from [`crate::chrome::Files::depth`]. One
    /// everywhere else, since the browser is the only thing sized by it.
    pub depth: usize,
}

impl Drag {
    /// Begin a drag at `(x, y)`, confined to whatever region it landed in.
    ///
    /// `m` is what the caller knows and the geometry does not — see
    /// [`PageMetrics`]. Passed in rather than looked up because only the caller
    /// holds the page structs those numbers come off.
    pub fn press(&mut self, view: &View, cols: u16, rows: u16, x: u16, y: u16, m: PageMetrics) {
        self.anchor = Some((x, y));
        self.span = None;
        self.clip = region(view, cols, rows, x, y, m);
    }

    /// Extend to `(x, y)`. Does nothing before a press, so a drag that began
    /// off-screen cannot select anything.
    pub fn to(&mut self, x: u16, y: u16) {
        if let Some(anchor) = self.anchor {
            self.span = Some((anchor, (x, y)));
        }
    }

    /// Finish, returning the text covered — `None` when nothing was selected,
    /// which is every ordinary click.
    pub fn finish(&mut self, screen: &Buffer) -> Option<String> {
        self.anchor = None;
        let (a, b) = self.span.take()?;
        let text = extract(screen, a, b, self.clip);
        (!text.trim().is_empty()).then_some(text)
    }

    /// Drop it without copying — what changing tab or opening a modal means.
    pub fn clear(&mut self) {
        *self = Drag::default();
    }
}

/// Row-major span `[x0, x1]` covered by a linear selection on row `y`.
///
/// Linear, not rectangular: a selection that runs off the end of one line
/// continues at the start of the next, which is what a terminal selection does
/// and what makes copied prose reflow correctly.
pub fn span(area: Rect, start: (u16, u16), end: (u16, u16), y: u16) -> (u16, u16) {
    let last = area.right().saturating_sub(1);
    if start.1 == end.1 {
        (start.0.min(end.0), start.0.max(end.0))
    } else if y == start.1 {
        (start.0, last)
    } else if y == end.1 {
        (area.x, end.0)
    } else {
        (area.x, last)
    }
}

/// The rectangle a selection wraps within: the drag's region clamped to the
/// screen, or the whole screen when the press landed nowhere in particular.
fn area_of(screen: &Buffer, clip: Option<LRect>) -> Rect {
    match clip {
        Some(c) => Rect::new(c.x, c.y, c.width, c.height).intersection(screen.area),
        None => screen.area,
    }
}

/// Clamp an endpoint into a region's cell range.
fn clamp(p: (u16, u16), area: Rect) -> (u16, u16) {
    (
        p.0.clamp(area.x, area.right().saturating_sub(1)),
        p.1.clamp(area.y, area.bottom().saturating_sub(1)),
    )
}

/// The selected text, with trailing blanks trimmed per line — what every
/// terminal does, and what stops a copied block carrying a ragged right edge of
/// spaces into whatever it is pasted into.
pub fn extract(screen: &Buffer, a: (u16, u16), b: (u16, u16), clip: Option<LRect>) -> String {
    let area = area_of(screen, clip);
    if area.width == 0 || area.height == 0 {
        return String::new();
    }
    let (a, b) = (clamp(a, area), clamp(b, area));
    let (start, end) = if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) };
    let mut lines: Vec<String> = Vec::new();
    for y in start.1..=end.1 {
        let (x0, x1) = span(area, start, end, y);
        let mut line = String::new();
        for x in x0..=x1 {
            let s = screen[(x, y)].symbol();
            line.push_str(if s.is_empty() { " " } else { s });
        }
        while line.ends_with(' ') {
            line.pop();
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// Reverse-video the active selection over the composed screen.
pub fn highlight(screen: &mut Buffer, a: (u16, u16), b: (u16, u16), clip: Option<LRect>) {
    let area = area_of(screen, clip);
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (a, b) = (clamp(a, area), clamp(b, area));
    let (start, end) = if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) };
    for y in start.1..=end.1 {
        let (x0, x1) = span(area, start, end, y);
        for x in x0..=x1 {
            if let Some(cell) = screen.cell_mut((x, y)) {
                cell.modifier |= ratatui::style::Modifier::REVERSED;
            }
        }
    }
}

/// Which content region a press at `(x, y)` belongs to.
///
/// `None` for the tab bar and the footer: neither holds anything worth copying,
/// and a drag that started on one would otherwise be free to run over the whole
/// screen. Everything else answers with the box interior it landed in, so a
/// selection stays in one column.
pub fn region(view: &View, cols: u16, rows: u16, x: u16, y: u16, m: PageMetrics) -> Option<LRect> {
    if y == 0 || y + 1 >= rows {
        return None;
    }
    let geom = crate::chrome::page_geom(cols, rows, view);
    // A full-screen page owns everything between the bars — but it still has
    // columns inside it, and "one column" is the whole point of a clip.
    if view.page != Page::Agents {
        return Some(page_region(view, cols, &geom, x, y, m));
    }
    for r in [geom.agents_rows, geom.procs_rows, geom.system_rows] {
        if r.contains(x, y) {
            return Some(r);
        }
    }
    if geom.changes_rows.contains(x, y) {
        return Some(geom.changes_rows);
    }
    if geom.stage_inner.contains(x, y) {
        return Some(to_lrect(geom.stage_inner));
    }
    None
}

/// The column of a full-screen page a press landed in.
///
/// **This was the whole band, on every page that owns it.** The module has
/// always said a selection stays in the column it began in, and on the agents
/// page it did; the pages that widen the stage — Files, Docs, Docker, BOOTH —
/// got one rectangle covering all of their columns at once. Since a selection
/// is linear rather than rectangular, a drag down the open file wrapped at the
/// band's right edge and resumed at the band's left, so every line after the
/// first came back with a filename from the tree in front of it. The same for
/// the Docker logs, whose lines arrived wearing a container name.
///
/// Falls back to the band rather than to `None`: a press on a box border is
/// still on this page, and `None` would free the drag to run over the tab bar.
///
/// The gutters come off the front of the text columns for the same reason the
/// other columns come off the band: line numbers and diff markers are chrome
/// drawn *inside* the text, and a copy that took them pasted code that no longer
/// compiles. The clip starting after the gutter also means a drag begun on the
/// numbers is clamped onto the first character of the line, which is what
/// aiming at the left margin means anywhere else.
fn page_region(
    view: &View,
    cols: u16,
    geom: &crate::chrome::Chrome,
    x: u16,
    y: u16,
    m: PageMetrics,
) -> LRect {
    use crate::chrome as c;
    let gutter = m.gutter;
    // Each column is (where a press counts as being in it, what it clips to).
    // The two differ exactly where a gutter is: pressing *on* the line numbers
    // is still pressing on the file, and must clip to the file rather than fall
    // through to the whole band.
    let columns: [(LRect, LRect); 4] = match view.page {
        Page::Files | Page::Docs => {
            let body = c::files_body_inner(geom, m.depth);
            let text = (body, trim_left(body, gutter));
            // A drag in the browser stays in the column it began in, or a copy
            // of one directory's names would come back with a neighbour's
            // interleaved a row at a time.
            // Nothing rather than the body when the press is not over a column:
            // the columns are searched in order, so a fallback that overlapped
            // the text would answer for it and hand back an unclipped body —
            // taking the line numbers with the copy.
            let column =
                c::files_col_at(geom, m.depth, x).map(|(_, r)| r).unwrap_or(LRect::new(0, 0, 0, 0));
            [(column, column), text, text, text]
        }
        Page::Docker => {
            let logs = c::docker_logs_inner(geom.stage_box);
            let list = c::docker_row_area(geom);
            [(list, list), (logs, logs), (logs, logs), (logs, logs)]
        }
        // BOOTH is three columns and a tray, and the tray is a list of its own —
        // the same agent appears in both, so a drag must not run between them.
        Page::Booth => {
            let h = c::booth_columns(c::booth_area(cols, geom));
            [
                (h.tray_rows, h.tray_rows),
                (h.fleet_rows, h.fleet_rows),
                (h.stage_inner, h.stage_inner),
                (h.compute_rows, h.compute_rows),
            ]
        }
        // Three real columns: the two lists never share a drag, and the body
        // is a diff, so it clips like one.
        Page::Git => {
            let g = c::git_columns(geom.stage_box);
            let body = trim_left(inner_of(g.body_box), gutter);
            [
                (g.refs_rows, g.refs_rows),
                (g.hist_rows, g.hist_rows),
                (inner_of(g.body_box), body),
                (inner_of(g.body_box), body),
            ]
        }
        // The diff is one column, less its marker and line-number columns. The
        // width is the caller's to compute — it depends on the patch being read,
        // not only on the page, which is why it arrives as `gutter` rather than
        // as the constant this used to subtract.
        Page::Diff => return trim_left(to_lrect(geom.stage_inner), gutter),
        // SETTINGS is a form, not a document: its rows are values you change,
        // and the one string on it worth copying — a path — is short enough to
        // read. A drag clips to the band so a stray one cannot run off into a
        // pane that is not under it.
        Page::Settings => return to_lrect(geom.stage_inner),
        // HELP is two columns and the right one is prose, so a drag down it is
        // the one thing on this page anybody copies — a key table, usually,
        // into a note or an issue. Clipped to the reading column for the reason
        // the file page's body is: a selection that wrapped at the band's edge
        // would come back with a topic name welded to every line but the first.
        Page::Help => {
            let h = c::help::columns(to_lrect(geom.stage_box));
            [(h.list, h.list), (h.body, h.body), (h.body, h.body), (h.body, h.body)]
        }
        // USAGE is one column of read-only rows. A selection drag clips to the
        // band, which is what makes copying an account name or a token total
        // out of it behave — the numbers are the reason to reach for the mouse.
        Page::Usage => return to_lrect(geom.stage_inner),
        Page::Agents => return to_lrect(geom.stage_inner),
    };
    columns
        .into_iter()
        .find(|(hit, _)| hit.contains(x, y))
        .map(|(_, clip)| clip)
        .unwrap_or(to_lrect(geom.stage_inner))
}

/// Take `n` cells off a rectangle's left edge, never past its right one.
fn trim_left(r: LRect, n: u16) -> LRect {
    let n = n.min(r.width);
    LRect::new(r.x + n, r.y, r.width - n, r.height)
}

fn to_lrect(r: LRect) -> LRect {
    r
}

/// The inside of a box, borders excluded — what a drag may actually cover.
fn inner_of(r: LRect) -> LRect {
    LRect::new(r.x + 1, r.y + 1, r.width.saturating_sub(2), r.height.saturating_sub(2))
}

#[cfg(test)]
mod tests {

    /// A one-column Files trail with no gutter — what most of these tests want.
    fn one() -> PageMetrics {
        PageMetrics { gutter: 0, depth: 1 }
    }

    /// The same, with a gutter of `n`.
    fn gut(n: u16) -> PageMetrics {
        PageMetrics { gutter: n, depth: 1 }
    }
    use super::*;

    fn screen(lines: &[&str]) -> Buffer {
        let w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
        let mut b = Buffer::empty(Rect::new(0, 0, w, lines.len() as u16));
        for (y, line) in lines.iter().enumerate() {
            for (x, ch) in line.chars().enumerate() {
                if let Some(c) = b.cell_mut((x as u16, y as u16)) {
                    c.set_char(ch);
                }
            }
        }
        b
    }

    /// A selection inside one row is the run between the two columns, either
    /// way round — dragging right-to-left is the same selection.
    #[test]
    fn a_selection_on_one_row_is_the_run_between_its_ends() {
        let b = screen(&["hello world"]);
        assert_eq!(extract(&b, (0, 0), (4, 0), None), "hello");
        assert_eq!(extract(&b, (4, 0), (0, 0), None), "hello", "dragging backwards");
        assert_eq!(extract(&b, (6, 0), (10, 0), None), "world");
    }

    /// Across rows it is linear, not rectangular: the first row runs to its
    /// end, the last starts at its beginning. A rectangular selection would
    /// chop prose into a column.
    #[test]
    fn a_selection_across_rows_wraps_like_text() {
        let b = screen(&["one two", "three  ", "four   "]);
        assert_eq!(extract(&b, (4, 0), (3, 2), None), "two\nthree\nfour");
    }

    /// Trailing blanks go. A terminal row is padded to its full width, so
    /// without this every copied line carries the padding with it.
    #[test]
    fn trailing_blanks_are_trimmed_off_each_line() {
        let b = screen(&["short      ", "also short "]);
        assert_eq!(extract(&b, (0, 0), (10, 1), None), "short\nalso short");
    }

    /// The clip is what keeps a drag in one column.
    ///
    /// Straying into the next pane is the ordinary case, not an edge one — the
    /// rails are narrow — and without this a copy interleaves two panes line by
    /// line, which is worse than useless.
    #[test]
    fn a_drag_that_strays_stays_in_the_column_it_started_in() {
        let b = screen(&["LEFT │ RIGHT", "left │ right"]);
        let clip = LRect::new(0, 0, 4, 2);
        // Dragging well past the divider still copies only the left column.
        assert_eq!(extract(&b, (0, 0), (11, 1), Some(clip)), "LEFT\nleft");
        // And without a clip it takes both, which is what "no region" means.
        assert_eq!(extract(&b, (0, 0), (11, 1), None), "LEFT │ RIGHT\nleft │ right");
    }

    /// An empty region selects nothing rather than panicking. `Rect`
    /// intersection can produce one when a rail has been narrowed to nothing.
    #[test]
    fn an_empty_region_selects_nothing() {
        let b = screen(&["text"]);
        let outside = LRect::new(50, 50, 4, 4);
        assert_eq!(extract(&b, (0, 0), (3, 0), Some(outside)), "");
        let mut b2 = b.clone();
        highlight(&mut b2, (0, 0), (3, 0), Some(outside));
        assert_eq!(b2, b, "nothing should have been marked");
    }

    /// The highlight covers exactly what the copy would take.
    #[test]
    fn the_highlight_marks_what_the_copy_takes() {
        use ratatui::style::Modifier;
        let mut b = screen(&["one two", "three  "]);
        highlight(&mut b, (4, 0), (2, 1), None);
        let marked: Vec<(u16, u16)> = (0..2u16)
            .flat_map(|y| (0..7u16).map(move |x| (x, y)))
            .filter(|(x, y)| b[(*x, *y)].modifier.contains(Modifier::REVERSED))
            .collect();
        // Row 0 from column 4 to the end, row 1 up to column 2.
        assert_eq!(marked, vec![(4, 0), (5, 0), (6, 0), (0, 1), (1, 1), (2, 1)]);
    }

    /// A press with no drag copies nothing — which is every ordinary click, so
    /// getting this wrong would put the whole screen on the clipboard whenever
    /// anyone selected a rail row.
    #[test]
    fn a_click_without_a_drag_copies_nothing() {
        let b = screen(&["hello"]);
        let mut d = Drag::default();
        d.press(&View::default(), 80, 24, 2, 3, PageMetrics::default());
        assert_eq!(d.finish(&b), None);
        // And a drag over blank space copies nothing either.
        let mut d = Drag::default();
        d.press(&View::default(), 80, 24, 0, 0, PageMetrics::default());
        d.to(4, 0);
        assert_eq!(d.finish(&screen(&["     "])), None);
    }

    /// The bars hold nothing worth copying, and a drag from one must not be
    /// free to run over the entire screen.
    #[test]
    fn the_tab_bar_and_the_footer_start_no_selection() {
        let view = View::default();
        assert_eq!(region(&view, 120, 40, 10, 0, PageMetrics::default()), None, "tab bar");
        assert_eq!(region(&view, 120, 40, 10, 39, PageMetrics::default()), None, "footer");
        assert!(
            region(&view, 120, 40, 10, 5, PageMetrics::default()).is_some(),
            "a rail row should be selectable"
        );
    }

    /// The pages that own the whole band are still two columns, and a copy out
    /// of one must not carry the other.
    ///
    /// They were one rectangle covering both. A selection is linear, so a drag
    /// down the open file wrapped at the band's right edge and came back at its
    /// left — every line but the first arrived with a filename from the tree in
    /// front of it. The Docker logs did the same with container names.
    #[test]
    fn each_column_of_a_full_width_page_is_its_own_selection() {
        const COLS: u16 = 160;
        const ROWS: u16 = 45;
        for page in [Page::Files, Page::Docs, Page::Docker] {
            let view = View { page, ..Default::default() };
            let geom = crate::chrome::page_geom(COLS, ROWS, &view);
            let (list, body) = if page == Page::Docker {
                (
                    crate::chrome::docker_row_area(&geom),
                    crate::chrome::docker_logs_inner(geom.stage_box),
                )
            } else {
                (
                    crate::chrome::files_columns(&geom, 1).remove(0),
                    crate::chrome::files_body_inner(&geom, 1),
                )
            };
            let in_list =
                region(&view, COLS, ROWS, list.x + 1, list.y + 1, one()).expect("{page:?} list");
            let in_body =
                region(&view, COLS, ROWS, body.x + 1, body.y + 1, one()).expect("{page:?} body");
            assert_ne!(in_list, in_body, "{page:?}: both columns gave one region");
            assert!(
                in_body.x >= list.right(),
                "{page:?}: a drag in the file would wrap back into the tree ({in_body:?})"
            );
            assert!(
                in_list.right() <= body.x,
                "{page:?}: a drag in the tree would run into the file ({in_list:?})"
            );
        }
    }

    /// The line numbers are not part of the file.
    ///
    /// They are chrome drawn inside the text column, so a selection clipped to
    /// the column took them: a copied function pasted as code with a column of
    /// numbers welded to the front of every line but the first. The diff's
    /// marker column is the same thing one cell wide.
    #[test]
    fn a_copy_out_of_a_file_leaves_the_line_numbers_behind() {
        const COLS: u16 = 160;
        const ROWS: u16 = 45;
        const GUTTER: u16 = 5;
        for page in [Page::Files, Page::Docs] {
            let view = View { page, ..Default::default() };
            let geom = crate::chrome::page_geom(COLS, ROWS, &view);
            let body = crate::chrome::files_body_inner(&geom, 1);
            let clip = region(&view, COLS, ROWS, body.x + GUTTER + 1, body.y + 1, gut(GUTTER))
                .expect("the file column");
            assert_eq!(clip.x, body.x + GUTTER, "{page:?}: the numbers are still in the clip");
            assert_eq!(clip.right(), body.right(), "{page:?}: the text lost its right edge");
            // Pressing *on* the numbers is still pressing on the file: it must
            // clip to the text, not fall through to the whole band.
            let on_gutter = region(&view, COLS, ROWS, body.x, body.y + 1, gut(GUTTER))
                .expect("the file column");
            assert_eq!(on_gutter, clip, "{page:?}: a press on the numbers escaped the column");
        }
        // The diff goes the same way, and its gutter is told to it for the same
        // reason the file's is: the marker column is one cell, but the two
        // line-number columns beside it are as wide as the patch needs and are
        // dropped altogether on a narrow body.
        let diff_gutter = crate::chrome::DiffView::new(
            crate::chrome::DiffKind::Unstaged { path: None },
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-a\n+b\n",
        )
        .gutter_w(COLS);
        assert!(diff_gutter > crate::chrome::DIFF_GUTTER_W, "a wide body draws line numbers");
        for page in [Page::Diff, Page::Git] {
            let view = View { page, ..Default::default() };
            let geom = crate::chrome::page_geom(COLS, ROWS, &view);
            let body = match page {
                Page::Git => inner_of(crate::chrome::git_columns(geom.stage_box).body_box),
                _ => to_lrect(geom.stage_inner),
            };
            let clip =
                region(&view, COLS, ROWS, body.x + diff_gutter + 1, body.y + 1, gut(diff_gutter))
                    .expect("the diff body");
            assert_eq!(clip.x, body.x + diff_gutter, "{page:?}: the gutter is still in the clip");
        }
    }

    /// BOOTH is three columns and a tray, and the same agent is in two of them —
    /// so a drag over the fleet must not pick up its own copy from the tray.
    #[test]
    fn booths_columns_do_not_run_into_each_other() {
        const COLS: u16 = 160;
        const ROWS: u16 = 45;
        let view = View { page: Page::Booth, ..Default::default() };
        let geom = crate::chrome::page_geom(COLS, ROWS, &view);
        let h = crate::chrome::booth_columns(crate::chrome::booth_area(COLS, &geom));
        let at =
            |r: LRect| region(&view, COLS, ROWS, r.x + 1, r.y + 1, one()).expect("a BOOTH column");
        let (tray, fleet, stage, compute) =
            (at(h.tray_rows), at(h.fleet_rows), at(h.stage_inner), at(h.compute_rows));
        assert_ne!(tray, fleet, "the tray and the list are one region");
        assert_ne!(fleet, stage, "the fleet and the pane are one region");
        assert_ne!(stage, compute, "the pane and the gauges are one region");
        assert!(fleet.right() <= stage.x && stage.right() <= compute.x, "the columns overlap");
    }
}
