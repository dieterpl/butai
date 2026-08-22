//! Chrome geometry: where the tab bar, rails, stage and footer sit.
//!
//! Pure integer cell math over a terminal size — no I/O, no state, nothing that
//! knows whether it is running in a daemon or a client. That is why it lives
//! here rather than beside a renderer: while the workbench moves from
//! server-side composition to client-side drawing, **both** sides have to agree
//! about these rectangles exactly, or a user switching between them sees the
//! screen shift. Sharing the arithmetic is the only way to guarantee that; a
//! copy on each side would be two things that drift.

use crate::config::RailGeom;
use crate::layout::Rect as LRect;

pub const LEFT_W: u16 = 28;
pub const RIGHT_W: u16 = 38;
pub const ZEN_W: u16 = 4;
/// Clamp bounds for configured rail widths.
pub const RAIL_MIN_W: u16 = 12;
pub const RAIL_MAX_W: u16 = 60;
/// Minimum stage width to keep between the rails. When the terminal is too
/// narrow to honour it, `Chrome::compute` drops both rails to 0 rather than
/// squeeze the stage; interactive resizing caps rail growth to respect it.
pub const MIN_STAGE_W: u16 = 20;
/// Rows one gauge occupies: a label-and-value line, then its trace across the
/// full width of the rail.
///
/// The trace gets a line of its own because a sparkline squeezed between a
/// label and a right-aligned value had ten cells to work with on a default
/// rail, which is four samples. A full-width braille row is two samples per
/// cell — around fifty — which is the difference between a texture and a trend.
pub const GAUGE_H: u16 = 2;
/// Rows the network gauge occupies: a label-and-value line, then one trace for
/// each direction.
///
/// A row more than every other gauge, and the reason is that the others measure
/// a level while this one measures two flows. Mirroring both around a midline
/// fits in [`GAUGE_H`] but leaves two dot rows per direction, which is one bit:
/// a 59 kB/s download under a 572 kB/s upload lands at 10% of the shared scale,
/// rounds to nothing and is then floored back to the same single dot an idle
/// link draws. The extra row buys four levels each and a colour per direction,
/// which is the difference between "something is happening" and reading it.
pub const NET_GAUGE_H: u16 = 3;
/// Rows a disk gauge occupies: one.
///
/// Every other gauge is a head row and a graphic row, because every other gauge
/// is a *series*. A disk is a level with no history — `DiskDto` carries none,
/// and a filesystem does not visibly move across the two and a half minutes the
/// window holds — so the second row would be a flat line, and a workstation with
/// three disks would spend six rows drawing it three times. The reading is the
/// head row's own: the mount on the left, `899/916G` on the right, in the colour
/// its fullness earns.
pub const DISK_GAUGE_H: u16 = 1;

/// Height the SYSTEM section wants to hold `rows` of gauge: a separator row,
/// then the gauges themselves.
pub const fn system_h_for_gauge_rows(rows: u16) -> u16 {
    1 + rows
}

/// Shorthand for `gauges` ordinary two-row gauges — the layout arithmetic that
/// predates the network gauge having a height of its own.
pub const fn system_h_for(gauges: usize) -> u16 {
    system_h_for_gauge_rows(gauges as u16 * GAUGE_H)
}

/// The height SYSTEM is *assumed* to take when sizing AGENTS.
///
/// Cpu, ram and one network interface is what an ordinary machine without a GPU
/// shows. AGENTS is sized against this rather than against what SYSTEM actually
/// took, so the rows a second GPU — or a disk — costs come out of PROCESSES and
/// the agent list you are reading does not move under you.
///
/// The disks are deliberately *not* in it, though every machine has one. This is
/// a floor as much as a baseline: raising it shrinks AGENTS' share, and on a
/// 24-row terminal one more row here is enough to put PROCESSES above AGENTS,
/// which is the one thing the split exists to prevent.
pub const SYSTEM_H_BASE: u16 = system_h_for_gauge_rows(2 * GAUGE_H + NET_GAUGE_H);

/// Rows a section keeps once the user starts moving boundaries around: enough
/// for a row and its verb hint, so no section can be squeezed out of existence.
pub const SECTION_MIN_H: u16 = 3;
/// Interior height below which SYSTEM yields the rail entirely.
pub const SYSTEM_FLOOR_H: u16 = 12;
/// A backstop, not the usual limit: cpu, ram, network, four GPUs and a capped
/// set of disks. What normally bounds SYSTEM is leaving [`SECTION_MIN_H`] for
/// each list, which is a property of the terminal rather than a number chosen
/// here.
pub const SYSTEM_MAX_H: u16 = system_h_for_gauge_rows(
    6 * GAUGE_H + NET_GAUGE_H + crate::chrome::DISK_GAUGE_MAX as u16 * DISK_GAUGE_H,
);

/// The GIT page's two stacked lists at their floors — borders included, so
/// each is "a box with at least two rows in it". Below their sum REFS yields
/// entirely and the page is history alone, the same way every side column here
/// gives up rather than shrinking into illegibility.
pub const GIT_REFS_MIN_H: u16 = 6;
pub const GIT_HIST_MIN_H: u16 = 6;

/// Lanes the commit graph will draw before it collapses the rest into `…`.
///
/// Six is where a busy repository stops being informative: past that the
/// columns cost more summary than the shape is worth, and the reader is
/// counting lines instead of reading commits.
pub const GIT_MAX_LANES: usize = 6;
/// Below this the lane column is dropped and HISTORY draws as a plain list.
/// The graph is a garnish on a list, never the thing the list is made of.
pub const GIT_GRAPH_MIN_W: u16 = 30;

/// The width an agent's own TUI is written for — Claude Code, codex and aider
/// all reflow against it.
///
/// Nothing in the layout is allowed to take the work stage below it. It used to
/// be the number the view rail was measured against: the rail existed only at
/// widths where the stage still cleared this, which is why it appeared at 160
/// columns and vanished at 153. The rail is gone and the spaces live on the tab
/// bar, so no horizontal chrome is bought at the stage's expense any more and
/// this is documentation of the floor rather than a threshold anything crosses.
pub const WORK_STAGE_MIN_W: u16 = 80;

/// The rail geometry a fresh workspace starts with: default widths and
/// automatic section heights.
/// The rail geometry a `[ui]` section asks for, clamped to what will fit.
///
/// The daemon used to do this — it owned the layout and mirrored the result
/// onto every workspace. The client owns the layout now, so it reads the
/// section, and `Config::save_ui` writes back to the same keys when Alt-l
/// resizing ends. Without this the save was one-way: the file was written and
/// never read, so a resized rail came back at the default on the next start.
///
/// Widths clamp rather than fall back, so a nonsense `left_rail = 900` gives a
/// wide-but-usable rail instead of silently ignoring what was asked for.
/// Heights stay `Option`: unset means "size yourself to the terminal", which is
/// not the same as any particular number.
pub fn geom_from_config(ui: &crate::config::UiConfig) -> RailGeom {
    let d = default_geom();
    let rail = |w: Option<u16>, default: u16| {
        w.map(|w| w.clamp(RAIL_MIN_W, RAIL_MAX_W)).unwrap_or(default)
    };
    RailGeom {
        left_w: rail(ui.left_rail, d.left_w),
        right_w: rail(ui.right_rail, d.right_w),
        procs_h: ui.procs_height.map(|h| h.max(SECTION_MIN_H)),
        system_h: ui.system_height.map(|h| h.min(SYSTEM_MAX_H)),
    }
}

pub fn default_geom() -> RailGeom {
    RailGeom { left_w: LEFT_W, right_w: RIGHT_W, procs_h: None, system_h: None }
}

/// Resolved heights of the left rail's three sections, top to bottom.
///
/// An unset height is *automatic*: it tracks the terminal the way it always
/// has. Once layout mode sets one it is honoured literally, clamped only so the
/// neighbouring sections keep [`SECTION_MIN_H`].
fn left_split(geom: RailGeom, inner_h: u16, zen: bool, want_system_h: u16) -> (u16, u16, u16) {
    // Zen rails are status strips, and below SYSTEM_FLOOR_H rows the gauges
    // would crowd out both lists — in either case SYSTEM yields the whole rail.
    let system_h = if zen || inner_h < SYSTEM_FLOOR_H {
        0
    } else {
        geom.system_h
            .unwrap_or(want_system_h)
            .min(SYSTEM_MAX_H)
            .min(inner_h.saturating_sub(2 * SECTION_MIN_H))
    };
    let list_h = inner_h.saturating_sub(system_h);
    // AGENTS takes its share of the rail as if SYSTEM were its baseline height,
    // and PROCESSES gets what is left. So plugging in a second GPU costs
    // PROCESSES two rows and leaves the agent list where it was — that list
    // grows with the work, while processes is usually the shell and a server or
    // two. Once PROCESSES is down to SECTION_MIN_H it stops giving and AGENTS
    // starts paying instead.
    let agents_share = (inner_h.saturating_sub(SYSTEM_H_BASE) * 3 / 5).max(SECTION_MIN_H);
    let floor = SECTION_MIN_H.min(list_h);
    let cap = list_h.saturating_sub(SECTION_MIN_H).max(floor);
    let procs_h =
        geom.procs_h.unwrap_or_else(|| list_h.saturating_sub(agents_share)).clamp(floor, cap);
    (list_h - procs_h, procs_h, system_h)
}

/// Section heights as layout mode sees them, ignoring zen. Resizing seeds an
/// unset (automatic) height from these, so the first keypress continues from
/// what is on screen instead of jumping to some constant.
pub struct Sections {
    pub agents_h: u16,
    pub procs_h: u16,
    pub system_h: u16,
    pub changes_h: u16,
}

pub fn sections(geom: RailGeom, rows: u16, want_system_h: u16) -> Sections {
    // Both rails span the same box, so one interior height serves for both.
    let inner_h = rows.saturating_sub(2).saturating_sub(2);
    let (agents_h, procs_h, system_h) = left_split(geom, inner_h, false, want_system_h);
    // The right rail is CHANGES and nothing else, so its one section is the
    // whole interior.
    Sections { agents_h, procs_h, system_h, changes_h: inner_h }
}

/// All screen regions, computed per frame. Every rect is in absolute cells.
pub struct Chrome {
    pub tabbar: LRect,
    /// Left rail interior rows (inside the box, below the top border).
    pub agents_rows: LRect,
    pub agents_hint: Option<LRect>,
    pub procs_sep: u16,
    pub procs_rows: LRect,
    pub procs_hint: Option<LRect>,
    pub system_sep: u16,
    pub system_rows: LRect,
    pub left_box: LRect,
    /// Stage interior (inside its box).
    pub stage_inner: LRect,
    pub stage_box: LRect,
    /// Right rail interior; zero-width when absent. Spans the whole rail, so
    /// wheel routing and drag-selection clipping keep working unchanged; the
    /// git list itself only gets `changes_rows`.
    pub right_inner: LRect,
    /// The changes list. The whole of `right_inner`: the rail holds one list.
    pub changes_rows: LRect,
    pub right_box: LRect,
    pub footer: LRect,
    pub zen: bool,
}

impl Chrome {
    /// `want_system_h` is the height the SYSTEM section asks for on the machine
    /// on screen — see [`crate::chrome::system_h_wanted`]. Rows rather than a
    /// gauge count, because the gauges are no longer all the same height: the
    /// network one is [`NET_GAUGE_H`]. It is a parameter rather than a constant
    /// because the section grows with the hardware, and it reaches hit testing
    /// through the same [`crate::chrome::View`] the drawing uses, so the two
    /// cannot disagree about where a gauge is.
    pub fn compute(cols: u16, rows: u16, zen: bool, geom: RailGeom, want_system_h: u16) -> Self {
        // Rows: 0 tabbar, 1..rows-2 boxes, rows-1 footer.
        let box_top = 1;
        let box_h = rows.saturating_sub(2);
        // The band is the whole width. There used to be a view rail carved off
        // the left edge here, and every rect below was downstream of it; the
        // spaces are a tab-bar menu now, so the only things taking columns are
        // the two rails that describe the workspace.
        let avail = cols;

        let (left_w, right_w) = if zen { (ZEN_W, ZEN_W) } else { (geom.left_w, geom.right_w) };
        let (left_w, right_w) =
            if avail < left_w + right_w + MIN_STAGE_W { (0, 0) } else { (left_w, right_w) };
        let stage_w = avail.saturating_sub(left_w + right_w);

        let left_box = LRect::new(0, box_top, left_w, box_h);
        let stage_box = LRect::new(left_w, box_top, stage_w, box_h);
        let right_box = LRect::new(left_w + stage_w, box_top, right_w, box_h);

        // Left rail interior: sections. Interior spans y in (top+1 .. bottom-1).
        let inner_top = box_top + 1;
        let inner_h = box_h.saturating_sub(2);
        let inner_w = left_w.saturating_sub(2);
        let inner_x = left_box.x + 1;
        let (agents_h, procs_h, system_h) = left_split(geom, inner_h, zen, want_system_h);

        let hint = |y: u16, h: u16| -> (LRect, Option<LRect>) {
            // Last row of a section is the verb-hint line when it fits.
            if h >= 3 && !zen {
                (
                    LRect::new(inner_x, y, inner_w, h - 1),
                    Some(LRect::new(inner_x, y + h - 1, inner_w, 1)),
                )
            } else {
                (LRect::new(inner_x, y, inner_w, h), None)
            }
        };
        let (agents_rows, agents_hint) = hint(inner_top, agents_h.saturating_sub(0));
        // Processes section starts with a separator row.
        let procs_sep = inner_top + agents_h;
        let (procs_rows, procs_hint) = hint(procs_sep + 1, procs_h.saturating_sub(1));
        let system_sep = procs_sep + procs_h;
        let system_rows = LRect::new(inner_x, system_sep + 1, inner_w, system_h.saturating_sub(1));

        // Right rail interior. One list fills it: CHANGES.
        let right_inner = LRect::new(
            right_box.x + 1,
            right_box.y + 1,
            right_box.width.saturating_sub(2),
            right_box.height.saturating_sub(2),
        );
        let changes_rows = right_inner;

        Self {
            tabbar: LRect::new(0, 0, cols, 1),
            agents_rows,
            agents_hint,
            procs_sep,
            procs_rows,
            procs_hint,
            system_sep,
            system_rows,
            left_box,
            stage_inner: LRect::new(
                stage_box.x + 1,
                stage_box.y + 1,
                stage_box.width.saturating_sub(2),
                stage_box.height.saturating_sub(2),
            ),
            stage_box,
            right_inner,
            changes_rows,
            right_box,
            footer: LRect::new(0, rows.saturating_sub(1), cols, 1),
            zen,
        }
    }
}

#[cfg(test)]
mod tests {

    /// `[ui]` is a round trip: `Config::save_ui` writes it when Alt-l resizing
    /// ends and this reads it back at start. It was one-way for a while — the
    /// daemon did the reading, stopped when it stopped owning the layout, and
    /// the client kept saving into a file nothing loaded, so every resize came
    /// back at the default on the next run.
    #[test]
    fn a_saved_rail_geometry_comes_back() {
        use crate::config::UiConfig;
        let d = default_geom();

        // Unset is the default, and an unset height still means "size yourself".
        let g = geom_from_config(&UiConfig::default());
        assert_eq!((g.left_w, g.right_w), (d.left_w, d.right_w));
        assert_eq!(g.procs_h, None);

        let asked = UiConfig {
            left_rail: Some(34),
            right_rail: Some(20),
            procs_height: Some(9),
            ..Default::default()
        };
        let g = geom_from_config(&asked);
        assert_eq!((g.left_w, g.right_w, g.procs_h), (34, 20, Some(9)), "what was asked for");

        // Nonsense clamps to something usable rather than being ignored.
        let silly = UiConfig { left_rail: Some(900), right_rail: Some(1), ..Default::default() };
        let g = geom_from_config(&silly);
        assert_eq!((g.left_w, g.right_w), (RAIL_MAX_W, RAIL_MIN_W));
    }
    use super::*;

    /// An ordinary machine's gauge count: cpu, ram and one network interface.
    /// Tests that care about the section growing say their own number.
    const GAUGES: usize = 3;

    #[test]
    fn regions_tile_horizontally() {
        let c = Chrome::compute(120, 40, false, default_geom(), system_h_for(GAUGES));
        assert_eq!(c.left_box.width, LEFT_W);
        assert_eq!(c.right_box.width, RIGHT_W);
        assert_eq!(c.left_box.width + c.stage_box.width + c.right_box.width, 120);
        assert_eq!(c.stage_box.x, LEFT_W);
        assert!(c.stage_inner.width > 40);
        assert_eq!(c.footer.y, 39);
    }

    #[test]
    fn zen_shrinks_rails() {
        let c = Chrome::compute(120, 40, true, default_geom(), system_h_for(GAUGES));
        assert_eq!(c.left_box.width, ZEN_W);
        assert_eq!(c.right_box.width, ZEN_W);
        assert!(c.stage_inner.width > 100);
    }

    #[test]
    fn tiny_terminals_drop_rails() {
        let c = Chrome::compute(40, 10, false, default_geom(), system_h_for(GAUGES));
        assert_eq!(c.left_box.width, 0);
        assert_eq!(c.stage_box.width, 40);
    }

    #[test]
    fn configured_rail_widths_apply() {
        let c = Chrome::compute(
            120,
            40,
            false,
            RailGeom { left_w: 20, right_w: 44, ..default_geom() },
            system_h_for(GAUGES),
        );
        assert_eq!(c.left_box.width, 20);
        assert_eq!(c.right_box.width, 44);
        assert_eq!(c.stage_box.x, 20);
    }

    /// The right rail is the changes list and nothing else.
    ///
    /// It used to be split with an ALL AGENTS panel underneath, and the split
    /// was the fiddly part — an off-by-one put the changes verb hints under the
    /// panel and misrouted their clicks. With the panel gone the list simply
    /// *is* the interior, at every size and in zen.
    #[test]
    fn the_changes_list_owns_the_whole_right_rail() {
        for (cols, rows, zen) in [(120u16, 40u16, false), (120, 12, false), (120, 40, true)] {
            let c = Chrome::compute(cols, rows, zen, default_geom(), system_h_for(GAUGES));
            assert_eq!(c.changes_rows, c.right_inner, "{cols}x{rows} zen={zen}");
        }
    }

    /// The left rail leans on agents: processes gets a visibly smaller share,
    /// and the two still tile the rail above SYSTEM with no gap.
    #[test]
    fn processes_take_less_than_agents() {
        for rows in [24u16, 40, 60] {
            let c = Chrome::compute(120, rows, false, default_geom(), system_h_for(GAUGES));
            assert!(
                c.procs_rows.height < c.agents_rows.height,
                "processes did not shrink at {rows} rows"
            );
            assert_eq!(c.procs_sep, c.agents_rows.bottom() + 1, "gap at {rows} rows");
            assert_eq!(c.procs_rows.y, c.procs_sep + 1);
        }
    }

    /// The section is as tall as the machine needs: two rows a gauge, plus its
    /// separator. A rail that sized SYSTEM from a constant would clip the last
    /// GPU's trace off the bottom of the rail on a four-GPU box.
    #[test]
    fn the_system_section_grows_with_the_hardware() {
        for gauges in 2..=6usize {
            let c = Chrome::compute(120, 50, false, default_geom(), system_h_for(gauges));
            // The literal, not `GAUGE_H`: asserting against the constant the
            // code divides by makes the test move with it, and this is exactly
            // the pair of rows — a label and its trace — being pinned down.
            assert_eq!(
                c.system_rows.height,
                gauges as u16 * 2,
                "{gauges} gauges need two rows each"
            );
            assert_eq!(c.system_sep, c.procs_rows.bottom() + 1, "gap above SYSTEM at {gauges}");
            assert_eq!(c.system_rows.y, c.system_sep + 1);
            assert_eq!(
                c.system_rows.bottom(),
                c.left_box.bottom() - 1,
                "SYSTEM should still sit on the rail's bottom border at {gauges}"
            );
        }
    }

    /// The rows a new GPU costs come out of PROCESSES, not AGENTS.
    ///
    /// That is the whole reason AGENTS is sized against a fixed baseline: the
    /// agent list is what you are reading while the work happens, and a machine
    /// that spins up a second GPU mid-session must not shuffle it under you.
    #[test]
    fn a_new_gauge_is_paid_for_by_processes() {
        let at = |gauges| {
            let c = Chrome::compute(120, 50, false, default_geom(), system_h_for(gauges));
            (c.agents_rows.height, c.procs_rows.height)
        };
        let (agents3, procs3) = at(3);
        let (agents4, procs4) = at(4);
        assert_eq!(agents3, agents4, "the agent list moved when a gauge was added");
        assert_eq!(procs4 + GAUGE_H, procs3, "PROCESSES should have paid the two rows");
    }

    /// Once PROCESSES is down to its floor it stops paying and AGENTS starts,
    /// rather than the sections overrunning the rail.
    #[test]
    fn a_crowded_rail_takes_the_rows_from_agents_instead() {
        // Sixteen interior rows against seven gauges: SYSTEM alone wants
        // fifteen, so there is nothing like enough to go round.
        let c = Chrome::compute(120, 20, false, default_geom(), 7);
        assert!(c.procs_rows.height >= 1, "processes vanished");
        assert!(c.agents_rows.height >= 1, "agents vanished");
        assert_eq!(c.system_rows.bottom(), c.left_box.bottom() - 1, "the rail overflowed");
    }

    /// Set heights are honoured literally, and the three sections still tile the
    /// rail interior exactly — a gap here would leave a dead row between two
    /// lists, an overlap would misroute clicks.
    #[test]
    fn set_section_heights_tile_the_left_rail() {
        for (procs, system) in [(4u16, 3u16), (12, 6), (20, 10)] {
            let geom = RailGeom { procs_h: Some(procs), system_h: Some(system), ..default_geom() };
            let c = Chrome::compute(120, 50, false, geom, system_h_for(GAUGES));
            assert_eq!(c.procs_rows.height + 2, procs, "procs {procs}/{system}");
            assert_eq!(c.system_rows.height + 1, system, "system {procs}/{system}");
            assert_eq!(c.procs_sep, c.agents_rows.bottom() + 1, "gap at {procs}/{system}");
            assert_eq!(c.procs_rows.y, c.procs_sep + 1);
            assert_eq!(c.system_sep, c.procs_rows.bottom() + 1);
            assert_eq!(c.system_rows.y, c.system_sep + 1);
            assert_eq!(c.system_rows.bottom(), c.left_box.bottom() - 1);
        }
    }

    /// However hard the user leans on the boundary, every section keeps a row
    /// and its hint line — a zero-height list has nowhere to put the cursor.
    #[test]
    fn set_section_heights_leave_every_section_usable() {
        let geom = RailGeom { procs_h: Some(400), system_h: Some(400), ..default_geom() };
        for rows in [12u16, 20, 40, 80] {
            let c = Chrome::compute(120, rows, false, geom, system_h_for(GAUGES));
            assert!(c.agents_rows.height >= 1, "agents vanished at {rows} rows");
            assert!(c.procs_rows.height >= 1, "processes vanished at {rows} rows");
            assert!(c.system_rows.height < SYSTEM_MAX_H, "system overshot its cap at {rows} rows");
            // Nothing spills past the rail's bottom border.
            assert!(
                c.system_sep + c.system_rows.height < c.left_box.bottom(),
                "sections overflowed the rail at {rows} rows"
            );
        }
    }

    /// Zen is a status strip: set heights do not resurrect the sections.
    #[test]
    fn zen_ignores_set_section_heights() {
        let geom = RailGeom { procs_h: Some(10), system_h: Some(8), ..default_geom() };
        let c = Chrome::compute(120, 40, true, geom, system_h_for(GAUGES));
        assert_eq!(c.system_rows.height, 0);
        assert_eq!(c.changes_rows, c.right_inner);
    }

    /// `sections` is what layout mode seeds from, so it must agree with what is
    /// drawn — otherwise the first keypress jumps.
    #[test]
    fn sections_match_the_drawn_chrome() {
        for rows in [14u16, 24, 40, 80] {
            let s = sections(default_geom(), rows, system_h_for(GAUGES));
            let c = Chrome::compute(120, rows, false, default_geom(), system_h_for(GAUGES));
            // Each drawn rect is its section less the rows its separator and
            // verb hint take; a section that is absent draws no separator.
            let sep = |h: u16| u16::from(h > 0);
            assert_eq!(c.agents_rows.height + 1, s.agents_h, "agents at {rows}");
            assert_eq!(c.procs_rows.height + 2, s.procs_h, "procs at {rows}");
            assert_eq!(c.system_rows.height + sep(s.system_h), s.system_h, "system at {rows}");
            assert_eq!(c.changes_rows.height, s.changes_h, "changes at {rows}");
        }
    }
}

// ---------------------------------------------------------------------------
// Row model
// ---------------------------------------------------------------------------
//
// What a rail row *says* — its sprite, its status token, how a long title
// scrolls — as opposed to where the row sits or what colour it ends up. Pure
// functions over the same facts an `AgentDto` carries, so the daemon's renderer
// and a client drawing its own rails produce identical text from identical
// state. That is the whole point of it living here: two implementations of
// "what does a working agent's row say" would drift within a week, and the
// difference would only show up as a screen that changes when you switch
// clients.
//
// Colour is named, not resolved: these return a [`Role`], and each renderer maps
// it through whatever palette it holds.

use butai_protocol::api::AgentState;

/// A semantic colour name, resolved against a [`crate::theme::Palette`] by the
/// caller.
///
/// Only the roles a rail row actually uses. Returning a role rather than a
/// colour is what lets this be shared: the daemon paints into a ratatui buffer
/// and a client may not use ratatui at all, but both agree that a waiting agent
/// is `Danger`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Ink,
    Faint,
    Ok,
    Info,
    Attention,
    Danger,
    Accent,
}

impl Role {
    /// Resolve against a palette.
    pub fn color(self, p: &crate::theme::Palette) -> crate::theme::ThemeColor {
        match self {
            Role::Ink => p.ink,
            Role::Faint => p.faint,
            Role::Ok => p.ok,
            Role::Info => p.info,
            Role::Attention => p.attention,
            Role::Danger => p.danger,
            Role::Accent => p.accent,
        }
    }
}

/// Braille spinner frames for the "working" indicator.
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

// -- ALL AGENTS sprites -----------------------------------------------------
//
// A working agent gets a little figure typing; the head glyph says how long it
// has been alive, so a long-running agent reads differently at a glance from one
// you just started. Deliberately plain ASCII, one column per char: a renderer
// advances a column per `char` while the wire format advances by display width,
// so a double-width glyph (any emoji) would shear the rail. See `docs/design.md`
// on single-cell status glyphs.

/// Sprite width in cells. Every frame in every table below must be this long.
pub const SPRITE_W: usize = 3;

/// Age thresholds in seconds, and the head glyph each one wears. Ascending.
pub const AGE_HEADS: [(u64, char); 4] =
    [(5 * 60, 'o'), (20 * 60, '0'), (60 * 60, 'O'), (u64::MAX, '@')];

/// Hand positions cycled while an agent is working: fingers on a keyboard.
/// Deliberately the smallest motion that still reads as movement — this panel is
/// meant to be left open, so a working agent should be noticeable at a glance
/// without pulling the eye off the stage. No frame is mirrored, which is what
/// keeps a rail of them from beating in time like a metronome.
pub const SPRITE_ARMS: [(char, char); 4] = [('.', '\''), (',', '.'), ('\'', ','), ('.', '.')];

/// The still frame for an agent that is alive but not working.
pub const SPRITE_RESTING: (char, char) = ('-', '-');
/// Blocked on you: the figure throws its hands up.
pub const SPRITE_WAITING: (char, char) = ('?', '?');
/// A finished turn: hands off the keyboard. Its own const rather than
/// `SPRITE_ARMS[0]` because a typing cycle has no frame that reads as
/// celebration — every one of them is just mid-keystroke.
pub const SPRITE_DONE: (char, char) = ('\\', '/');
/// The process is gone.
pub const SPRITE_EXITED: &str = "x_x";

/// The 3-cell sprite for an agent, its role, and whether it is moving (which is
/// what keeps a fast clock repainting).
///
/// `age_secs` is the agent's whole life, `fast_tick` the animation phase.
pub fn agent_sprite(
    state: AgentState,
    exited: Option<u32>,
    age_secs: u64,
    fast_tick: u64,
) -> (String, Role, bool) {
    if let Some(code) = exited {
        let role = if code == 0 { Role::Faint } else { Role::Danger };
        return (SPRITE_EXITED.to_string(), role, false);
    }
    let head =
        AGE_HEADS.iter().find(|(limit, _)| age_secs < *limit).map(|(_, h)| *h).unwrap_or('@');
    let frame = |(l, r): (char, char)| format!("{l}{head}{r}");
    match state {
        AgentState::Exited => (SPRITE_EXITED.to_string(), Role::Faint, false),
        AgentState::Waiting => (frame(SPRITE_WAITING), Role::Danger, false),
        AgentState::Working => {
            let arms = SPRITE_ARMS[(fast_tick as usize) % SPRITE_ARMS.len()];
            (frame(arms), Role::Attention, true)
        }
        // A finished turn is the one still pose worth celebrating.
        AgentState::Finished => (frame(SPRITE_DONE), Role::Info, false),
        AgentState::Idle => (frame(SPRITE_RESTING), Role::Faint, false),
    }
}

/// Compact `m:ss` (or `Ns` under a minute) for the working timer.
pub fn fmt_elapsed_secs(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// The mark on a status token whose turn you have not read yet.
///
/// A single cell, appended rather than substituted, so the word it follows keeps
/// the width and spelling it always had — `done` and `done•` line up in a column
/// of right-aligned tokens, and a client that renders the two identically is
/// merely losing the distinction, not misreading the row.
pub const UNREAD_MARK: char = '•';

/// Right-aligned status token for an agent row: a plain word (`WAIT`/`done`/
/// `idle`/`exit`) or, while working, an animated spinner + elapsed time.
///
/// `unread` is [`AgentDto::unread`](butai_protocol::api::AgentDto::unread): the
/// row reached a your-move state and has not been looked at. It earns the
/// [`UNREAD_MARK`] and keeps its full-strength colour, while the same state once
/// read drops to [`Role::Faint`] — which is what makes a rail of finished agents
/// legible instead of a wall of identical `done`s. Only `Finished` and `Exited`
/// can be unread; `WAIT` is urgent regardless of how often you have read it, and
/// deliberately does not fade.
///
/// Returns `(text, text_role, name_role, animating)`; `animating` keeps the
/// frame repainting so the spinner and timer stay live. `working_secs` is how
/// long the current turn has run, `None` when it has not started one.
pub fn agent_status(
    state: AgentState,
    exited: Option<u32>,
    working_secs: Option<u64>,
    tick: u64,
    unread: bool,
) -> (String, Role, Role, bool) {
    let mark = |s: String| if unread { format!("{s}{UNREAD_MARK}") } else { s };
    if let Some(code) = exited {
        // Exited rows keep their gray/red dim; the title already says "[exited]".
        // A non-zero code stays `Danger` once read: you having seen a crash does
        // not make it less of one, and only the mark goes away.
        let label = if code == 0 { "exit".to_string() } else { format!("exit {code}") };
        let role = if code == 0 { Role::Faint } else { Role::Danger };
        return (mark(label), role, Role::Faint, false);
    }
    match state {
        AgentState::Exited => (mark("exit".to_string()), Role::Faint, Role::Faint, false),
        AgentState::Waiting => ("WAIT".to_string(), Role::Danger, Role::Ink, false),
        AgentState::Working => {
            let spin = SPINNER[(tick as usize) % SPINNER.len()];
            let text = match working_secs {
                Some(s) => format!("{spin} {}", fmt_elapsed_secs(s)),
                None => spin.to_string(),
            };
            (text, Role::Attention, Role::Ink, true)
        }
        // The one row the whole read/unread distinction is for: a turn that
        // landed while you were away is news, the one you read an hour ago is
        // furniture, and before this they were the same word in the same colour.
        AgentState::Finished => {
            let role = if unread { Role::Info } else { Role::Faint };
            (mark("done".to_string()), role, Role::Ink, false)
        }
        AgentState::Idle => ("idle".to_string(), Role::Faint, Role::Ink, false),
    }
}

/// Split a leading status glyph off an agent's title: `("◐", "Fix the parser")`.
///
/// An agent's title is its pane's OSC title, and an interactive agent rewrites
/// that title every frame — Claude Code prefixes it with an animated `◐`/`◑`
/// while it works and a `✳` while it waits, and other agents do the same with a
/// glyph of their own. The glyph is a *state*, not part of the name, so it
/// belongs in the pinned column beside the row's marker rather than inside the
/// text a narrow rail scrolls: a name travelling past a fixed point is
/// readable, and a name towing a spinner through the same cells is what this
/// splits apart. The pinned half is what [`draw_row`]'s `pin` was built for.
///
/// The rule is deliberately about *shape*, not about a table of glyphs: one
/// leading char that is neither alphanumeric nor whitespace, one space, and a
/// name after it. A table would be a list of the spinners three agents happened
/// to use in 2026, wrong the first time any of them changed theirs, and the
/// cost of guessing wide is only that a leading `>` or `·` stops moving too.
///
/// Returns `("", title)` when there is nothing to split — a bare `claude`, a
/// glyph with no name behind it, or a title that simply starts with a letter.
///
/// [`draw_row`]: super::draw_row
pub fn split_status_glyph(title: &str) -> (&str, &str) {
    let mut chars = title.char_indices();
    let Some((_, first)) = chars.next() else { return ("", title) };
    if first.is_alphanumeric() || first.is_whitespace() {
        return ("", title);
    }
    let Some((sp, ' ')) = chars.next() else { return ("", title) };
    let rest = title[sp + 1..].trim_start();
    // A glyph on its own is the whole title, not a prefix of one: pinning it
    // would leave the row blank next to a mark nothing explains.
    if rest.is_empty() {
        ("", title)
    } else {
        (&title[..sp], rest)
    }
}

/// Clamp `s` to `width` characters, marking the cut with an ellipsis. Unlike
/// [`marquee`] this is for text that never animates (box titles).
pub fn ellipsize(s: &str, width: usize) -> String {
    if width == 0 || s.chars().count() <= width {
        return s.to_string();
    }
    s.chars().take(width.saturating_sub(1)).chain(['\u{2026}']).collect()
}

/// Horizontally scroll `s` so a string wider than `width` reveals its whole
/// length over time. Returns the visible slice and whether it is scrolling.
/// Fits untouched when it already fits; pauses briefly at the start of a loop.
pub fn marquee(s: &str, width: usize, phase: u64) -> (String, bool) {
    let chars: Vec<char> = s.chars().collect();
    if width == 0 || chars.len() <= width {
        return (chars.into_iter().take(width).collect(), false);
    }
    const GAP: usize = 3;
    const HOLD: u64 = 3; // ticks paused at the start before scrolling
    let period = chars.len() + GAP;
    let step = (phase % (period as u64 + HOLD)).saturating_sub(HOLD) as usize;
    let mut looped = chars.clone();
    looped.extend(std::iter::repeat_n(' ', GAP));
    looped.extend_from_slice(&chars);
    let end = (step + width).min(looped.len());
    (looped[step..end].iter().collect(), true)
}

/// Colour ramp for a load percentage.
pub fn load_role(pct: f32) -> Role {
    if pct >= 85.0 {
        Role::Danger
    } else if pct >= 50.0 {
        Role::Attention
    } else {
        Role::Ok
    }
}

/// A four-cell block sparkline over the tail of `hist` (percentages).
pub fn sparkline(hist: &[f32]) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let take = 4usize;
    let mut out = String::new();
    let start = hist.len().saturating_sub(take);
    for v in &hist[start..] {
        let idx = ((v / 100.0) * 7.0).round().clamp(0.0, 7.0) as usize;
        out.push(BARS[idx]);
    }
    while out.chars().count() < take {
        out.insert(0, '▁');
    }
    out
}

/// The last `n` samples, oldest first, left-padded with the oldest one when the
/// series is shorter than the space it has to fill.
///
/// Padding with the oldest value rather than zero keeps a short history from
/// drawing a cliff on the left that never happened — a client that attached ten
/// seconds ago should see a flat lead-in, not a wall.
fn tail(hist: &[f32], n: usize) -> Vec<f32> {
    let start = hist.len().saturating_sub(n);
    let pad = n.saturating_sub(hist.len());
    let first = hist.first().copied().unwrap_or(0.0);
    std::iter::repeat_n(first, pad).chain(hist[start..].iter().copied()).collect()
}

// Braille dot numbering is column-major and not in reading order: the left
// column is dots 1,2,3,7 top to bottom and the right is 4,5,6,8, which as bit
// positions in U+2800's low byte are these.
const B_LEFT: [u8; 4] = [0, 1, 2, 6];
const B_RIGHT: [u8; 4] = [3, 4, 5, 7];

/// Dots to light for a percentage, over four rows. Never zero: a gauge at rest
/// draws a baseline rather than a blank, or an idle machine reads as no data.
fn dot_level(pct: f32) -> u8 {
    (((pct / 100.0) * 4.0).round() as i32).clamp(1, 4) as u8
}

fn fill_up(bits: &mut u8, map: [u8; 4], level: u8) {
    for k in 0..level.min(4) {
        *bits |= 1 << map[3 - k as usize];
    }
}

fn braille(bits: u8) -> char {
    char::from_u32(0x2800 + u32::from(bits)).unwrap_or(' ')
}

/// A braille trace over `cells` cells, filled from the baseline up.
///
/// Two samples per cell and four rows per sample, so the same cells carry
/// roughly eight times what block glyphs do. Takes percentages: a series with
/// no natural ceiling — throughput — must be scaled by the caller, which is
/// also the only place that knows what to scale it against.
pub fn braille_trace(hist: &[f32], cells: usize) -> String {
    if cells == 0 {
        return String::new();
    }
    let s = tail(hist, cells * 2);
    (0..cells)
        .map(|i| {
            let mut bits = 0u8;
            fill_up(&mut bits, B_LEFT, dot_level(s[i * 2]));
            fill_up(&mut bits, B_RIGHT, dot_level(s[i * 2 + 1]));
            braille(bits)
        })
        .collect()
}

/// A braille trace for a series that can be genuinely *absent*, drawn from an
/// empty baseline rather than [`braille_trace`]'s one-dot floor.
///
/// The difference is what the zero means. A CPU is always doing some amount of
/// nothing, so a flat baseline is the honest picture of an idle machine and a
/// blank row would read as a dead feed. Throughput has a real zero: a link with
/// no traffic on it is silent, not idle-at-some-level, and floor-clamping that
/// to one dot is what drew a permanent two-line axis under 230 B/s of ssh
/// keepalives and made a quiet minute indistinguishable from a saturated link.
///
/// So: exactly zero draws nothing, and anything the caller passes as non-zero
/// gets at least one dot. Which rates count as silence is the caller's to
/// decide — it is the only side that knows the units.
pub fn braille_traffic(hist: &[f32], cells: usize) -> String {
    if cells == 0 {
        return String::new();
    }
    let s = tail(hist, cells * 2);
    let level = |pct: f32| {
        if pct <= 0.0 {
            0
        } else {
            (((pct / 100.0) * 4.0).round() as i32).clamp(1, 4) as u8
        }
    };
    (0..cells)
        .map(|i| {
            let mut bits = 0u8;
            fill_up(&mut bits, B_LEFT, level(s[i * 2]));
            fill_up(&mut bits, B_RIGHT, level(s[i * 2 + 1]));
            braille(bits)
        })
        .collect()
}

#[cfg(test)]
mod row_tests {
    use super::*;

    /// An ordinary machine's gauge count: cpu, ram and one network interface.
    const GAUGES: usize = 3;

    #[test]
    fn a_working_agent_animates_and_a_resting_one_does_not() {
        let (_, _, moving) = agent_sprite(AgentState::Working, None, 10, 0);
        assert!(moving);
        let (_, _, moving) = agent_sprite(AgentState::Idle, None, 10, 0);
        assert!(!moving);
    }

    #[test]
    fn every_sprite_frame_is_exactly_three_cells() {
        // A wider frame shears every row below it in the rail, because the wire
        // format advances by display width and the renderer by char.
        for state in
            [AgentState::Waiting, AgentState::Working, AgentState::Finished, AgentState::Idle]
        {
            for tick in 0..SPRITE_ARMS.len() as u64 {
                for age in [0, 6 * 60, 21 * 60, 2 * 60 * 60] {
                    let (s, _, _) = agent_sprite(state, None, age, tick);
                    assert_eq!(s.chars().count(), SPRITE_W, "{state:?} age {age}: {s:?}");
                }
            }
        }
        let (s, _, _) = agent_sprite(AgentState::Idle, Some(1), 0, 0);
        assert_eq!(s.chars().count(), SPRITE_W);
    }

    #[test]
    fn the_head_glyph_ages() {
        let head = |secs| agent_sprite(AgentState::Idle, None, secs, 0).0.chars().nth(1).unwrap();
        assert_eq!(head(1), 'o');
        assert_eq!(head(10 * 60), '0');
        assert_eq!(head(30 * 60), 'O');
        assert_eq!(head(5 * 60 * 60), '@');
    }

    #[test]
    fn a_nonzero_exit_is_danger_and_names_the_code() {
        let (text, role, _, _) = agent_status(AgentState::Exited, Some(3), None, 0, false);
        assert_eq!(text, "exit 3");
        assert_eq!(role, Role::Danger);
        let (text, role, _, _) = agent_status(AgentState::Exited, Some(0), None, 0, false);
        assert_eq!(text, "exit");
        assert_eq!(role, Role::Faint);
    }

    #[test]
    fn a_working_row_carries_a_spinner_and_a_clock() {
        let (text, _, _, animating) = agent_status(AgentState::Working, None, Some(75), 0, false);
        assert!(animating);
        assert!(text.contains("1:15"), "{text}");
        assert!(SPINNER.iter().any(|s| text.starts_with(s)), "{text}");
    }

    #[test]
    fn an_unread_finished_turn_is_marked_and_a_read_one_fades() {
        let (text, role, _, _) = agent_status(AgentState::Finished, None, None, 0, true);
        assert_eq!(text, "done•", "an unread turn carries the mark");
        assert_eq!(role, Role::Info, "and keeps its full-strength colour");

        let (text, role, _, _) = agent_status(AgentState::Finished, None, None, 0, false);
        assert_eq!(text, "done", "reading it takes the mark away");
        assert_eq!(role, Role::Faint, "and drops it to the background");
    }

    #[test]
    fn an_unread_exit_is_marked_but_keeps_its_alarm() {
        // The mark is about whether you have looked; the colour is about what
        // happened. Reading a crash must not recolour it as routine.
        let (text, role, _, _) = agent_status(AgentState::Exited, Some(3), None, 0, true);
        assert_eq!(text, "exit 3•");
        assert_eq!(role, Role::Danger);
        let (_, role, _, _) = agent_status(AgentState::Exited, Some(3), None, 0, false);
        assert_eq!(role, Role::Danger, "a read crash is still a crash");
    }

    #[test]
    fn waiting_never_fades_and_never_takes_the_mark() {
        // An unanswered question is urgent however many times you have read it,
        // so `unread` must not reach it in either direction.
        for unread in [true, false] {
            let (text, role, _, _) = agent_status(AgentState::Waiting, None, None, 0, unread);
            assert_eq!(text, "WAIT", "unread={unread}");
            assert_eq!(role, Role::Danger, "unread={unread}");
        }
    }

    #[test]
    fn elapsed_switches_from_seconds_to_minutes_at_a_minute() {
        assert_eq!(fmt_elapsed_secs(0), "0s");
        assert_eq!(fmt_elapsed_secs(9), "9s");
        assert_eq!(fmt_elapsed_secs(59), "59s");
        assert_eq!(fmt_elapsed_secs(72), "1:12");
        assert_eq!(fmt_elapsed_secs(60), "1:00");
        assert_eq!(fmt_elapsed_secs(605), "10:05");
    }

    #[test]
    fn ellipsize_never_exceeds_the_width() {
        assert_eq!(ellipsize("abcdef", 4), "abc\u{2026}");
        assert_eq!(ellipsize("ab", 4), "ab");
        assert_eq!(ellipsize("abcdef", 0), "abcdef");
    }

    #[test]
    fn sparkline_maps_range() {
        assert_eq!(sparkline(&[0.0, 50.0, 100.0]), "▁▁▅█");
        assert_eq!(sparkline(&[]), "▁▁▁▁");
    }

    /// The spinners three real agents write into their OSC title, and the
    /// ordinary titles that must survive untouched.
    #[test]
    fn a_titles_leading_spinner_comes_off_the_name() {
        for glyph in ["◐", "◑", "✳", "✻", "·", "*", ">"] {
            let title = format!("{glyph} Fix the parser");
            assert_eq!(
                split_status_glyph(&title),
                (glyph, "Fix the parser"),
                "{title:?} should split"
            );
        }
        for whole in ["claude", "3 of 4 done", "  padded", "◐", "◐ ", "", "◐Fix"] {
            assert_eq!(split_status_glyph(whole), ("", whole), "{whole:?} is all name");
        }
    }

    /// The glyph is the *only* thing that comes off. A name that scrolled two
    /// characters short of where it used to would be this fix charging rent.
    #[test]
    fn splitting_a_title_loses_nothing_but_the_gap() {
        let title = "◑ Check Claude API usage session limits";
        let (glyph, name) = split_status_glyph(title);
        assert_eq!(format!("{glyph} {name}"), title);
    }

    #[test]
    fn a_fitting_string_does_not_marquee() {
        let (text, scrolling) = marquee("short", 10, 7);
        assert_eq!(text, "short");
        assert!(!scrolling);
    }

    #[test]
    fn marquee_holds_at_the_start_then_scrolls() {
        let (first, scrolling) = marquee("a-long-title", 4, 0);
        assert!(scrolling);
        assert_eq!(first, "a-lo", "the loop should hold at the start");
        assert_eq!(marquee("a-long-title", 4, 3).0, "a-lo");
        assert_ne!(marquee("a-long-title", 4, 6).0, "a-lo", "it should have moved by now");
        // Whatever the phase, it stays exactly `width` cells wide.
        for phase in 0..40 {
            assert_eq!(marquee("a-long-title", 4, phase).0.chars().count(), 4, "phase {phase}");
        }
    }

    /// The trace fills exactly the cells it is given, whatever the history is —
    /// a short one pads and a long one takes its tail. A trace that came back
    /// the wrong width would push the value column off the end of the rail.
    #[test]
    fn a_braille_trace_is_exactly_as_wide_as_asked() {
        for cells in [0usize, 1, 4, 26, 52] {
            for hist in [vec![], vec![50.0], (0..200).map(|i| i as f32 % 100.0).collect()] {
                assert_eq!(
                    braille_trace(&hist, cells).chars().count(),
                    cells,
                    "{cells} cells from {} samples",
                    hist.len()
                );
                assert_eq!(
                    braille_traffic(&hist, cells).chars().count(),
                    cells,
                    "traffic: {cells} cells from {} samples",
                    hist.len()
                );
            }
        }
    }

    /// Every glyph is a braille cell. A value outside 0..=100 must still land in
    /// the block — `clamp` is what holds this, and without it an autoscaled
    /// series that overshoots its peak would index past the dot rows.
    #[test]
    fn a_trace_stays_inside_the_braille_block() {
        let wild = [-50.0, 0.0, 37.0, 100.0, 140.0, f32::MAX, f32::MIN];
        for ch in braille_trace(&wild, 4).chars().chain(braille_traffic(&wild, 4).chars()) {
            assert!(('\u{2800}'..='\u{28FF}').contains(&ch), "{ch:?} is not braille");
        }
    }

    /// Full is the whole column, empty is still one dot: a gauge at rest has to
    /// draw a baseline, or an idle machine reads as a dead feed.
    #[test]
    fn a_trace_floors_at_a_baseline_and_tops_out_full() {
        assert_eq!(braille_trace(&[100.0; 8], 4), "⣿⣿⣿⣿");
        assert_eq!(braille_trace(&[0.0; 8], 4), "⣀⣀⣀⣀");
    }

    /// Silence draws as silence. This is the whole point of the second trace
    /// function: a link with nothing on it must not paint the same line a busy
    /// one does, which is what the old mirrored trace did at every rate under
    /// a quarter of its own scale.
    #[test]
    fn traffic_draws_nothing_when_there_is_none() {
        assert_eq!(braille_traffic(&[0.0; 8], 4), "\u{2800}\u{2800}\u{2800}\u{2800}");
        assert_eq!(braille_traffic(&[100.0; 8], 4), "⣿⣿⣿⣿");
        // The smallest non-zero rate the caller admits still shows: below the
        // dead band it arrives as an exact 0.0 and above it as something, and
        // nothing in between rounds away to a blank row.
        assert_eq!(braille_traffic(&[0.001; 8], 4), "⣀⣀⣀⣀");
    }

    /// The two trace functions disagree about zero on purpose, and that
    /// disagreement is the fix. A CPU at rest is a baseline; a silent link is
    /// nothing.
    #[test]
    fn a_level_floors_at_a_baseline_where_a_flow_floors_at_nothing() {
        assert_eq!(braille_trace(&[0.0; 8], 4), "⣀⣀⣀⣀");
        assert_ne!(braille_traffic(&[0.0; 8], 4), braille_trace(&[0.0; 8], 4));
    }

    #[test]
    fn a_sparkline_is_always_four_cells() {
        assert_eq!(sparkline(&[]).chars().count(), 4);
        assert_eq!(sparkline(&[10.0]).chars().count(), 4);
        assert_eq!(sparkline(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]).chars().count(), 4);
        assert_eq!(sparkline(&[100.0, 100.0, 100.0, 100.0]), "████");
        assert_eq!(sparkline(&[0.0, 0.0, 0.0, 0.0]), "▁▁▁▁");
    }

    #[test]
    fn the_load_ramp_climbs() {
        assert_eq!(load_role(10.0), Role::Ok);
        assert_eq!(load_role(60.0), Role::Attention);
        assert_eq!(load_role(90.0), Role::Danger);
    }

    /// The band is the whole terminal, and the AGENTS rail starts at column 0.
    ///
    /// It used to start at 14 on a wide screen, because the view rail owned the
    /// left edge. Asserted against the literal edge rather than against a
    /// width, since the point of the change is that no constant sits here any
    /// more.
    #[test]
    fn the_rails_start_at_the_left_edge() {
        let c = Chrome::compute(200, 40, false, default_geom(), system_h_for(GAUGES));
        assert_eq!(c.left_box.x, 0, "AGENTS owns the left edge");
        assert_eq!(c.agents_rows.x, c.left_box.x + 1, "and its rows are inside its own box");
        assert_eq!(c.system_rows.x, c.left_box.x + 1);
        assert_eq!(
            c.right_box.x + c.right_box.width,
            200,
            "the right rail still ends at the screen edge"
        );
        assert_eq!(
            c.left_box.width + c.stage_box.width + c.right_box.width,
            200,
            "the three boxes tile the band with nothing left over"
        );
    }

    /// The stage keeps the columns the view rail used to take.
    ///
    /// 160 was the width the labelled rail appeared at, and it bought those 14
    /// columns from the one page that could least afford them. The rail is a tab
    /// bar menu now, so at that width the work stage is 14 wider than it was and
    /// clears [`WORK_STAGE_MIN_W`] with room to spare rather than exactly.
    #[test]
    fn the_stage_keeps_the_columns_the_view_rail_used_to_take() {
        let g = default_geom();
        let c = Chrome::compute(160, 40, false, g, system_h_for(GAUGES));
        assert_eq!(c.stage_box.width, 160 - g.left_w - g.right_w);
        assert_eq!(
            c.stage_box.width,
            WORK_STAGE_MIN_W + 14,
            "the rail's 14 columns are back on the stage"
        );
    }
}
