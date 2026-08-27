//! What is under the pointer.
//!
//! A pure function from a column and a row to the thing at it, so a click can
//! be tested without a daemon, a terminal or a mouse. The daemon's version
//! ([`butai-server`'s `mouse_down`], ~440 lines) could be none of those things:
//! it resolved the click and carried it out in one pass over `&mut self`, so
//! the only way to ask "what is at (40, 12)?" was to run a whole workbench and
//! see what changed.
//!
//! It resolves against the same [`crate::chrome::Chrome`] the drawing uses,
//! which is the point of that type living where both can reach it: a hit box
//! that computes its own geometry is a hit box that drifts off its button.
//! Where a region is not a plain rectangle — the tab chips, the footer buttons
//! — the span comes from the same function the renderer draws with.

use crate::chrome::Chrome;

use butai_protocol::api::{ChangesDto, WorkspaceDetail};

use crate::chrome::{self, Focus, Page, Tab, View};

/// The thing under the pointer.
///
/// Deliberately says *what was clicked*, not what should happen: the loop
/// decides that, and it already knows how — every arm maps onto something a key
/// can do too. A click that means "spawn an agent" would put the workbench's
/// vocabulary in two places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A tab chip, by index into the flattened tab list.
    Tab(usize),
    /// The `[x]` on the active chip: close this workspace.
    CloseTab,
    /// A page reached by pressing something that names it outright — the BOOTH
    /// chip, and the footer's SETTINGS.
    ///
    /// The six *spaces* no longer arrive here. They used to be six buttons on
    /// the bar and six rows down the view rail; they are rows of the menu
    /// [`Spaces`](Self::Spaces) opens now, and a row of an open modal is
    /// resolved by [`chrome::overlay_hit`] rather than by this function.
    Space(Page),
    /// The spaces button on the tab bar: open the menu of them.
    ///
    /// One target for the control, not one per space, because that is what the
    /// press *does* — the choice happens in the modal it opens, which is the
    /// same route the git menu and the machine picker take.
    Spaces,
    NewWorkspace,
    /// The machines button on the tab bar.
    ///
    /// One target because there is now one control. `NewHost` and `Hosts` were
    /// two — the offer and the roll call — sitting next to each other and
    /// resolving to the same flow, which is two names for one press.
    Machines,
    /// A footer button, by its label — the same string the footer draws.
    Footer(&'static str),
    /// A row in one of the rails. Selecting is the first click; the second on
    /// the same row is what stages it.
    Rail(Focus, usize),
    /// A verb under one of the three lists — the key it stands for, so a click
    /// and the key it advertises go the same way.
    ///
    /// The `[+ agent]` and `[+ term]` buttons on the box borders resolve here
    /// too, to the spawn verb each of them is a second spelling of. That is the
    /// whole point of naming the *key* rather than the action: a button and the
    /// footer word beside it cannot come to mean different things.
    AgentsVerb(char),
    ProcsVerb(char),
    ChangesVerb(char),
    /// A SYSTEM gauge, by what it is rather than which row it landed on.
    ///
    /// The index it used to carry meant "0 and 1 are CPU and RAM, 2 and up are
    /// GPUs", which stopped being true the moment the section grew a network
    /// row and started opening the GPU monitor for it.
    System(chrome::Gauge),
    /// Inside the staged pane, in *pane-local* coordinates.
    Stage(u16, u16),
    /// Somewhere with nothing on it.
    Nothing,
}

/// Resolve a press at `(x, y)`.
///
/// Order matters and follows the drawing's own stacking: the tab bar and the
/// footer own their rows outright, the panel sits inside the right rail, and the
/// stage is what is left. Overlays are *not* here — a modal is a question the
/// interface is asking, and the loop refuses the pointer while one is open for
/// the same reason it refuses the keyboard.
///
/// `daemons` is how many machines this client holds open, counting the local
/// one — the same count [`chrome::Scene`] paints the bar with, and not
/// derivable from `tabs`, because a connected machine with nothing open on it
/// contributes no chip and still has a badge to click.
///
/// `ws` is the open workspace, and it is here because the three rails scroll:
/// which entry a row is depends on how far its list has scrolled, and that
/// comes from the cursor and the list's own length. It used to be the
/// `ChangesDto` alone, back when only that rail's verb footer needed workspace
/// data — the rails were drawn from the top and a screen row *was* the entry.
// Eight, and every one of them is a screen the answer depends on. The
// alternative is a struct per caller, which is what `Scene` is for the drawing
// — worth it there, where the list keeps growing with each page; here it has
// grown by one in the life of the module.
#[allow(clippy::too_many_arguments)]
pub fn at(
    cols: u16,
    rows: u16,
    view: &View,
    tabs: &[Tab<'_>],
    daemons: usize,
    ws: Option<&WorkspaceDetail>,
    x: u16,
    y: u16,
) -> Target {
    let geom = chrome::page_geom(cols, rows, view);

    if y == geom.tabbar.y {
        // BOOTH's chip is leftmost and cannot overlap anything, but it is tested
        // first because the drawing paints it first.
        let (hx, hend) = chrome::tabbar_booth_span(&geom.tabbar);
        if x >= hx && x < hend {
            return Target::Space(Page::Booth);
        }
        // The spaces button next: it is right of the chips and cannot overlap
        // them, but testing it here keeps the order the drawing paints in. Its
        // span is the ink, so the blank reserved beside it answers as nothing —
        // a press has to land on the button you can see.
        if let Some((start, end)) = chrome::spaces_button_span(&geom.tabbar, view, daemons) {
            if x >= start && x < end {
                return Target::Spaces;
            }
        }
        // The `[x]` before the chip it sits on, or the chip would swallow it.
        if let Some((start, end)) = chrome::tab_close_span(&geom.tabbar, tabs, view, daemons) {
            if x >= start && x < end {
                return Target::CloseTab;
            }
        }
        let strip = chrome::tab_strip(&geom.tabbar, tabs, view, daemons);
        for (i, span) in strip.spans.iter().enumerate() {
            if let Some((start, end)) = *span {
                if x >= start && x < end {
                    return Target::Tab(i);
                }
            }
        }
        // Either arrow is the workspace it reaches — the chip that is not on the
        // strip, named by the button that brings it back. Nothing new to handle:
        // a press here is the same press as a press on a chip.
        for ((start, end), i) in [strip.prev, strip.next].into_iter().flatten() {
            if x >= start && x < end {
                return Target::Tab(i);
            }
        }
        for (label, start, end) in chrome::tabbar_buttons(&geom.tabbar, daemons) {
            if x >= start && x < end {
                return if label == chrome::TAB_NEW_LABEL {
                    Target::NewWorkspace
                } else {
                    Target::Machines
                };
            }
        }
        return Target::Nothing;
    }

    if y == geom.footer.y {
        for (label, start, end) in chrome::footer_button_spans(cols) {
            if x >= start && x < end {
                return Target::Footer(label);
            }
        }
        return Target::Nothing;
    }

    // BOOTH owns the whole band and draws none of the workspace rails, so it must
    // be resolved before them. Without this the rail rectangles are still in the
    // geometry — `Chrome` is page-agnostic by design — and a press in the fleet
    // column came back as `Rail(Agents, n)`, silently selecting a row in a list
    // that is not on screen.
    if view.page == Page::Booth {
        let c = chrome::booth_columns(chrome::booth_area(cols, &geom));
        if c.stage_inner.contains(x, y) {
            return Target::Stage(x - c.stage_inner.x, y - c.stage_inner.y);
        }
        // A fleet row needs the cross-daemon list, which only [`on_fleet`]
        // takes; everything else in BOOTH's band is nothing, and crucially not
        // the rails underneath.
        return Target::Nothing;
    }

    // Zen collapses both rails, so everything but the bars is stage.
    if !view.zen {
        // The buttons on the box borders, before the rows they sit above. Each
        // resolves to the verb it is a second spelling of.
        if y == geom.left_box.y {
            let label = chrome::agents_add_label(view.pinned_agent.as_deref(), geom.left_box);
            let (start, end) = chrome::agents_add_span_for(&geom, &label);
            if x >= start && x < end {
                return Target::AgentsVerb(crate::verbs::SPAWN_AGENT_KEY);
            }
        }
        if y == geom.procs_sep {
            let (start, end) = chrome::procs_add_span(&geom);
            if x >= start && x < end {
                return Target::ProcsVerb(crate::verbs::NEW_SHELL_KEY);
            }
        }
        if let Some(h) = geom.agents_hint.filter(|h| h.contains(x, y)) {
            let pinned = view.pinned_agent.is_some();
            return match chrome::rail_verb_at(crate::verbs::agents_verbs(pinned), h.width, x - h.x)
            {
                Some(key) => Target::AgentsVerb(key),
                None => Target::Nothing,
            };
        }
        if let Some(h) = geom.procs_hint.filter(|h| h.contains(x, y)) {
            return match chrome::rail_verb_at(crate::verbs::procs_verbs(), h.width, x - h.x) {
                Some(key) => Target::ProcsVerb(key),
                None => Target::Nothing,
            };
        }
        // Each rail resolves against the same scroll its drawing used, from the
        // same cursor and the same length — see [`chrome::rail_first`]. A screen
        // row is only the entry while the list fits.
        if geom.agents_rows.contains(x, y) {
            let len = ws.map(|w| w.agents.len()).unwrap_or(0);
            let first = chrome::rail_first(view.agent_sel, len, geom.agents_rows.height);
            return Target::Rail(Focus::Agents, first + (y - geom.agents_rows.y) as usize);
        }
        if geom.procs_rows.contains(x, y) {
            let len = ws.map(|w| w.processes.len()).unwrap_or(0);
            let first = chrome::rail_first(view.proc_sel, len, geom.procs_rows.height);
            return Target::Rail(Focus::Processes, first + (y - geom.procs_rows.y) as usize);
        }
        if geom.system_rows.contains(x, y) {
            // A gauge owns every row it draws — a label row and one trace row,
            // or two traces for the network gauge — so the offset is walked
            // through the gauges rather than divided by a constant. Dividing
            // was right while they were all the same height and silently
            // opened the wrong monitor as soon as one of them was not. The list
            // comes off the same `view` the drawing sized the section with,
            // which is what keeps the two from disagreeing. Falling out of the
            // loop is the padding a configured height leaves below the last
            // gauge.
            let mut off = y - geom.system_rows.y;
            for &g in &view.gauges {
                let h = chrome::gauge_height(g);
                if off < h {
                    return Target::System(g);
                }
                off -= h;
            }
            return Target::Nothing;
        }
        if geom.changes_rows.contains(x, y) {
            return changes_target(&geom, view, ws.and_then(|w| w.changes.as_ref()), x, y);
        }
    }

    if geom.stage_inner.contains(x, y) {
        return Target::Stage(x - geom.stage_inner.x, y - geom.stage_inner.y);
    }
    Target::Nothing
}

/// Which agent of BOOTH's fleet is under the pointer.
///
/// Separate from [`at`] because it is the one region whose contents cross
/// daemons: resolving it needs the assembled fleet and the machines it groups
/// by, and no other caller has a reason to build those. The loop, which holds
/// both already, asks this first while BOOTH is up and falls back to [`at`].
///
/// **The NEEDS YOU tray answers too**, and answers as the agent it is a copy of.
/// The tray is the shortest route to the row that is actually asking for you,
/// and it was the one list on the page the pointer could not reach: a press on
/// it fell through to nothing while the identical row six lines down worked.
/// Since a tray row *is* the fleet row, it resolves to the same index and means
/// the same thing.
// Eight, and the three lists are the point: this region's contents cross
// daemons, so resolving it needs the fleet, the projects and the machines that
// group them. The alternative is a struct built per press by the one caller
// that has them all anyway.
#[allow(clippy::too_many_arguments)]
pub fn on_fleet(
    cols: u16,
    rows: u16,
    view: &View,
    fleet: &[chrome::AllAgentRow<'_>],
    spaces: &[chrome::SpaceRow<'_>],
    machines: &[chrome::MachineRow<'_>],
    x: u16,
    y: u16,
) -> Option<FleetHit> {
    if view.page != Page::Booth {
        return None;
    }
    let geom = chrome::page_geom(cols, rows, view);
    let c = chrome::booth_columns(chrome::booth_area(cols, &geom));
    // The tray sits above the list and the two rectangles are disjoint, so this
    // is a first look rather than a precedence: only one of them can contain the
    // point. It carries no `[open]` — four rows are too few to spend six columns
    // on a button, and the copy's original is right there in the list with one.
    //
    // A tray copy resolves to its original's *row*, which is what the cursor
    // counts. An original folded away inside its project has no row to move to,
    // and the press does nothing rather than moving the cursor somewhere else —
    // the tray is still the shortest route to it, through unfolding.
    let booth = chrome::booth_rows(spaces, machines, &view.folds);
    if let Some(agent) = chrome::booth_tray_row_at(&c, fleet, x, y) {
        return booth
            .iter()
            .position(|r| matches!(r, chrome::BoothRow::Agent { sel, .. } if *sel == agent))
            .map(FleetHit::Row);
    }
    let row = chrome::booth_fleet_row_at(&c, &booth, view.booth_sel, x, y)?;
    // Buttons before the row they sit on, or the row would swallow them — the
    // same order the tab bar resolves its `[x]` in.
    match booth.get(row)? {
        chrome::BoothRow::Agent { .. } => {
            if let Some((start, end)) = chrome::fleet_open_span(c.fleet_rows) {
                if x >= start && x < end {
                    return Some(FleetHit::Open(row));
                }
            }
            Some(FleetHit::Row(row))
        }
        chrome::BoothRow::Space { space, folded, .. } => {
            let l = chrome::space_layout(c.fleet_rows, space, *folded);
            if let Some(((start, end), _)) = l.add {
                if x >= start && x < end {
                    return Some(FleetHit::New(row));
                }
            }
            // The name, and only the name. A project row has nothing to preview
            // and going there is its one meaning, so the name is a link — but
            // the rest of the row is the fold's target, and the two must not be
            // one press with two outcomes depending on where in a word it lands.
            let (nx, ne) = l.name;
            let drawn = (space.name.chars().count() as u16).min(ne.saturating_sub(nx));
            if x >= nx && x < nx + drawn {
                return Some(FleetHit::Go(row));
            }
            Some(FleetHit::Fold(row))
        }
        chrome::BoothRow::Machine { .. } => Some(FleetHit::Fold(row)),
    }
}

/// Which machine of BOOTH's COMPUTE column is under the pointer.
///
/// Beside [`on_fleet`] and for its reason: the column lists every connected
/// daemon, which only the loop can assemble.
pub fn on_compute(
    cols: u16,
    rows: u16,
    view: &View,
    machines: &[chrome::MachineRow<'_>],
    x: u16,
    y: u16,
) -> Option<usize> {
    if view.page != Page::Booth {
        return None;
    }
    let geom = chrome::page_geom(cols, rows, view);
    let c = chrome::booth_columns(chrome::booth_area(cols, &geom));
    chrome::booth_compute_machine_at(&c, machines, view, x, y)
}

/// What a press on BOOTH's fleet list landed on. Every variant carries a *row*
/// index — the currency [`View::booth_sel`] is counted in.
///
/// **A press on an agent row is not the same act as going to it, which is why
/// BOOTH's list does not take the rails' two-step.** A second click on a rail
/// row stages a pane of the workspace you are already in; a fleet row can be
/// another workspace on another machine, so going to it moves the tab bar out
/// from under you. Reported as a bug and it is one: a click meant "let me look
/// at this", and looking at it threw the whole workbench onto somebody else's
/// project.
///
/// So an *agent* row only ever moves the cursor — BOOTH's middle column follows
/// it, which is the entire point of the page — and `[open]` is the one thing on
/// it that travels. That rule is about agent rows and it has not moved: what
/// [`Go`](FleetHit::Go) adds is a project's *name*, on a row with nothing to
/// preview, where going there is the only thing pressing it could mean. Nothing
/// here takes you somewhere by accident; every route out is a field you aimed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FleetHit {
    /// An agent row: put the cursor on it, and the preview with it. Never more.
    Row(usize),
    /// The `[open]` button: go to that agent now, wherever it lives.
    Open(usize),
    /// A project's name: go to that workspace, on its machine.
    Go(usize),
    /// A project's `[+]`: start its preferred agent, without moving the page.
    New(usize),
    /// A machine or project row, off its name and off its button: fold it.
    Fold(usize),
}

/// Rows inside the CHANGES rail: the list, then the verb row(s) pinned to the
/// bottom.
///
/// The split comes from [`chrome::changes_split`], which the drawing also uses,
/// so a click on a verb cannot land on the file above it.
fn changes_target(
    geom: &Chrome,
    view: &View,
    changes: Option<&ChangesDto>,
    x: u16,
    y: u16,
) -> Target {
    let rows = geom.changes_rows;
    let (list_h, footer_h) = chrome::changes_split(geom);
    if y < rows.y + list_h {
        // Headings are rows of this list too, so the length that decides the
        // scroll is the built list's — the same one the drawing walks.
        let len = changes.map(|c| chrome::change_rows(c).len()).unwrap_or(0);
        let first = chrome::rail_first(view.changes_sel, len, list_h);
        return Target::Rail(Focus::Changes, first + (y - rows.y) as usize);
    }
    let Some(c) = changes else { return Target::Nothing };
    let verbs = chrome::changes_verbs(c, view.changes_sel);
    let row = (y - rows.y - list_h) as usize;
    let col = x.saturating_sub(rows.x) as usize;
    match crate::verbs::hit(&verbs, rows.width as usize, footer_h as usize, row, col) {
        Some(key) => Target::ChangesVerb(key),
        None => Target::Nothing,
    }
}

/// What a click on a full-screen page landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageTarget {
    /// A row of the page's list, by index into it — the scroll is already
    /// applied, so this is the entry, not the screen row.
    Row(usize),
    /// The right-hand column: the open file, or the logs.
    Body,
    /// The `[find]` button on the tree box's border.
    Find,
    Nothing,
}

/// Resolve a click on the Files or Docker page.
///
/// Kept apart from [`at`] because these resolve against a scroll offset, which
/// is page state rather than geometry. `sel` is where that page's cursor is,
/// which is all the scroll depends on — passed in rather than reached for, so
/// this stays a function of the screen and one number instead of two page
/// structs.
pub fn on_page(cols: u16, rows: u16, view: &View, sel: usize, x: u16, y: u16) -> PageTarget {
    if !view.page.is_tree() && view.page != Page::Docker {
        return PageTarget::Nothing;
    }
    let geom = chrome::page_geom(cols, rows, view);
    let list = if view.page.is_tree() {
        chrome::files_row_area(&geom)
    } else {
        chrome::docker_row_area(&geom)
    };
    // `[find]` sits on the tree box's top border, above the rows.
    if view.page.is_tree() {
        let tree_box = chrome::files_tree_box(&geom);
        let (start, end) = chrome::files_find_span(&tree_box);
        if y == tree_box.y && x >= start && x < end {
            return PageTarget::Find;
        }
    }
    if list.contains(x, y) {
        let first = chrome::first_visible(sel, list.height);
        return PageTarget::Row(first + (y - list.y) as usize);
    }
    // Everything right of the list, inside the page's box, is the body.
    if geom.stage_box.contains(x, y) && x >= list.right() {
        return PageTarget::Body;
    }
    PageTarget::Nothing
}

/// What a click on the GIT page landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitTarget {
    /// A row of the REFS list, by index into it — scroll already applied.
    RefRow(usize),
    /// A verb under REFS, by its key.
    RefVerb(char),
    HistRow(usize),
    HistVerb(char),
    /// The diff on the right.
    Body,
    Nothing,
}

/// Resolve a click on the GIT page.
///
/// Kept apart from [`on_page`] because this page has two lists and two verb
/// footers rather than one of each, and because the footers must be tested
/// *before* the rows they sit under — `git_split` is what draws the boundary
/// and this reads it, so a click on a verb cannot land on the row above it.
#[allow(clippy::too_many_arguments)]
pub fn on_git_page(
    cols: u16,
    rows: u16,
    view: &View,
    git: &chrome::Git,
    changes: Option<&butai_protocol::api::ChangesDto>,
    here: Option<butai_protocol::SessionId>,
    x: u16,
    y: u16,
) -> GitTarget {
    if view.page != Page::Git {
        return GitTarget::Nothing;
    }
    let geom = chrome::page_geom(cols, rows, view);
    let c = chrome::git_columns(geom.stage_box);

    let hit_list = |area: crate::layout::Rect,
                    verbs: &[crate::verbs::Verb],
                    sel: usize,
                    len: usize|
     -> Option<(Option<usize>, Option<char>)> {
        if !area.contains(x, y) {
            return None;
        }
        let (list_h, footer_h) = chrome::git_split(area, verbs);
        if y >= area.y + list_h {
            let row = (y - (area.y + list_h)) as usize;
            let key = crate::verbs::hit(
                verbs,
                area.width as usize,
                footer_h as usize,
                row,
                (x - area.x) as usize,
            );
            return Some((None, key));
        }
        // The same derivation the drawing uses, from the same cursor: a stored
        // offset here is how a click comes to select a different row than the
        // one under the pointer.
        let first = chrome::first_visible(sel.min(len.saturating_sub(1)), list_h);
        Some((Some(first + (y - area.y) as usize), None))
    };

    let ref_list = chrome::ref_rows(git, changes, here);
    let ref_verbs = crate::verbs::git_footer(chrome::ref_row_kind(&ref_list, git.refs_sel));
    if let Some((row, key)) = hit_list(c.refs_rows, &ref_verbs, git.refs_sel, ref_list.len()) {
        return match (row, key) {
            (Some(r), _) if r < ref_list.len() => GitTarget::RefRow(r),
            (_, Some(k)) => GitTarget::RefVerb(k),
            _ => GitTarget::Nothing,
        };
    }

    let hist_kind =
        if git.log.is_empty() { crate::verbs::GitRow::None } else { crate::verbs::GitRow::Commit };
    let hist_verbs = crate::verbs::git_footer(hist_kind);
    if let Some((row, key)) = hit_list(c.hist_rows, &hist_verbs, git.hist_sel, git.log.len()) {
        return match (row, key) {
            (Some(r), _) if r < git.log.len() => GitTarget::HistRow(r),
            (_, Some(k)) => GitTarget::HistVerb(k),
            _ => GitTarget::Nothing,
        };
    }

    if c.body_box.contains(x, y) {
        return GitTarget::Body;
    }
    GitTarget::Nothing
}

#[cfg(test)]
mod tests {
    use super::*;
    use butai_protocol::api::{RepoState, WorkspaceSummary};
    use butai_protocol::SessionId;

    fn summary(name: &str) -> WorkspaceSummary {
        WorkspaceSummary {
            id: SessionId(1),
            name: name.into(),
            cwd: "/tmp".into(),
            agents: 0,
            waiting: 0,
            working: 0,
            finished: 0,
            questions: 0,
            exited: 0,
            unread: 0,
            processes: 0,
            changes: 0,
            conflicts: 0,
            repo_state: RepoState::Clean,
            attached_clients: 1,
            autostart: Vec::new(),
        }
    }

    const COLS: u16 = 120;
    const ROWS: u16 = 40;
    /// A wide terminal, where every tab-bar control has room.
    const WIDE: u16 = 200;

    /// The spaces button resolves to the menu across its whole width, and the
    /// band below it belongs to whatever is drawn there.
    ///
    /// The rail used to own the left edge on every page; a press at column 0 is
    /// the AGENTS rail's now, which is the regression this pins.
    #[test]
    fn a_press_on_the_spaces_button_opens_the_menu() {
        let a = summary("alpha");
        let tabs = [Tab { summary: &a, host: None, live: true }];
        let view = View::default();
        let geom =
            Chrome::compute(WIDE, ROWS, false, view.geom, chrome::system_h_wanted(&view.gauges));
        let (start, end) = chrome::spaces_button_span(&geom.tabbar, &view, 1)
            .expect("{WIDE} cols should afford it");
        for x in start..end {
            assert_eq!(
                at(WIDE, ROWS, &view, &tabs, 1, None, x, geom.tabbar.y),
                Target::Spaces,
                "column {x} of the spaces button"
            );
        }
        // One column either side is something else, so the button cannot be
        // swallowing a neighbour.
        for x in [start - 1, end] {
            assert_ne!(at(WIDE, ROWS, &view, &tabs, 1, None, x, geom.tabbar.y), Target::Spaces);
        }
        // And the band's left edge belongs to the workspace again: the AGENTS
        // box is at column 0, its rows one in, and nothing down there resolves
        // to a space any more. That column used to be the view rail for the
        // whole height of the screen.
        for y in geom.agents_rows.y..geom.agents_rows.y + geom.agents_rows.height {
            assert!(
                !matches!(at(WIDE, ROWS, &view, &tabs, 1, None, 0, y), Target::Space(_)),
                "column 0 of row {y} still answers as a space"
            );
        }
        assert!(matches!(
            at(WIDE, ROWS, &view, &tabs, 1, None, geom.agents_rows.x, geom.agents_rows.y),
            Target::Rail(Focus::Agents, _)
        ));
    }

    /// The BOOTH chip is clickable across its whole width, at every terminal
    /// size, and it never collides with the first workspace chip — it is the
    /// only pointer route back to BOOTH, which the spaces menu does not carry.
    #[test]
    fn the_booth_chip_is_clickable_and_clear_of_the_first_workspace() {
        let a = summary("alpha");
        let tabs = [Tab { summary: &a, host: None, live: true }];
        for cols in [COLS, WIDE] {
            for page in [Page::Agents, Page::Booth, Page::Files] {
                let view = View { page, ..Default::default() };
                let geom = Chrome::compute(
                    cols,
                    ROWS,
                    false,
                    view.geom,
                    chrome::system_h_wanted(&view.gauges),
                );
                let (hx, hend) = chrome::tabbar_booth_span(&geom.tabbar);
                for x in hx..hend {
                    assert_eq!(
                        at(cols, ROWS, &view, &tabs, 1, None, x, geom.tabbar.y),
                        Target::Space(Page::Booth),
                        "({x}, 0) at {cols} cols on `{}`",
                        page.label()
                    );
                }
                let chips = chrome::tab_strip(&geom.tabbar, &tabs, &view, 1);
                assert!(
                    chips.spans[0].expect("one workspace always has a chip").0 >= hend,
                    "the first workspace chip overlaps BOOTH's"
                );
            }
        }
    }

    /// BOOTH's fleet rows resolve to the agent under the pointer, headers
    /// resolve to nothing, and — the bug this exists for — a press in the fleet
    /// column never comes back as a row of the AGENTS rail, which BOOTH does not
    /// draw but the geometry still describes.
    #[test]
    fn a_press_on_the_fleet_names_the_agent_under_it_and_never_a_hidden_rail() {
        use butai_protocol::api::{AgentDto, AgentState};

        let agent = |title: &str, pane: u64| AgentDto {
            pane: butai_protocol::PaneId(pane),
            title: title.into(),
            state: AgentState::Idle,
            exited: None,
            question: false,
            started_ms: 0,
            working_since_ms: None,
            unread: false,
        };
        let (a, b) = (agent("claude", 1), agent("codex", 2));
        let fleet = vec![
            chrome::AllAgentRow {
                workspace: "one",
                workspace_id: butai_protocol::SessionId(1),
                agent: &a,
                host: None,
                daemon: 0,
            },
            chrome::AllAgentRow {
                workspace: "two",
                workspace_id: butai_protocol::SessionId(2),
                agent: &b,
                host: None,
                daemon: 0,
            },
        ];
        let sys = butai_protocol::api::SysDto::default();
        let (machines, spaces) = booth_scaffold(&sys, &fleet, &[("one", 1, 1), ("two", 2, 1)]);
        let view = View { page: Page::Booth, ..Default::default() };
        let tabs: [Tab<'_>; 0] = [];
        let geom = chrome::page_geom(WIDE, ROWS, &view);
        let c = chrome::booth_columns(chrome::booth_area(WIDE, &geom));

        let mut seen = Vec::new();
        for y in c.fleet_rows.y..c.fleet_rows.y + c.fleet_rows.height {
            // The one thing that must never happen: a rail BOOTH does not draw.
            let t = at(WIDE, ROWS, &view, &tabs, 1, None, c.fleet_rows.x, y);
            assert!(
                !matches!(t, Target::Rail(..) | Target::System(_)),
                "row {y} of the fleet resolved to a hidden rail: {t:?}"
            );
            let hit = on_fleet(WIDE, ROWS, &view, &fleet, &spaces, &machines, c.fleet_rows.x, y);
            if let Some(hit) = hit {
                seen.push((y, hit));
            }
        }
        // Left edge of the list: the machine and the projects are their own
        // fold targets there, and only an agent row is a plain `Row`. Which
        // rows those are is the row model's business — what this pins down is
        // that every drawn row answers, and answers as what it is.
        assert_eq!(
            seen.iter().map(|(_, h)| *h).collect::<Vec<_>>(),
            vec![
                FleetHit::Fold(0),
                FleetHit::Fold(1),
                FleetHit::Row(2),
                FleetHit::Fold(3),
                FleetHit::Row(4),
            ],
            "every row should answer, as the thing it is"
        );

        // `[open]` is a button on the row, not the row: pressing it goes there
        // and pressing beside it only moves the cursor. Both name the same row,
        // which is what makes the button a second verb on one row rather than a
        // second row.
        //
        // Anchored to the rows the sweep above actually found agents on — the
        // list interleaves headers, so "the first two rows" is not the same
        // thing as "the first two agents".
        let (start, end) = chrome::fleet_open_span(c.fleet_rows).expect("wide enough for [open]");
        for (y, hit) in &seen {
            let FleetHit::Row(i) = hit else { continue };
            for x in start..end {
                assert_eq!(
                    on_fleet(WIDE, ROWS, &view, &fleet, &spaces, &machines, x, *y),
                    Some(FleetHit::Open(*i)),
                    "column {x} of row {y} should be the jump button"
                );
            }
            assert_eq!(
                on_fleet(WIDE, ROWS, &view, &fleet, &spaces, &machines, start - 1, *y),
                Some(FleetHit::Row(*i)),
                "the column left of the button is still the row"
            );
        }
    }

    /// One machine and one project, which is the least the fleet needs to draw
    /// anything at all now that its rows come from those lists rather than from
    /// the agents.
    fn booth_scaffold<'a>(
        sys: &'a butai_protocol::api::SysDto,
        fleet: &'a [chrome::AllAgentRow<'a>],
        names: &'a [(&'a str, u64, usize)],
    ) -> (Vec<chrome::MachineRow<'a>>, Vec<chrome::SpaceRow<'a>>) {
        let machines =
            vec![chrome::MachineRow { label: "local", sys, agents: fleet.len(), live: true }];
        let mut spaces = Vec::new();
        let mut first = 0;
        for (name, id, n) in names {
            spaces.push(chrome::SpaceRow {
                name,
                id: butai_protocol::SessionId(*id),
                daemon: 0,
                agents: &fleet[first..first + n],
                first,
                preferred: Some("claude"),
                tab: spaces.len(),
            });
            first += n;
        }
        (machines, spaces)
    }

    /// A press in the NEEDS YOU tray names the agent the row is a copy of, and
    /// the tray's empty rows name nothing.
    ///
    /// The tray is a fixed four rows whether or not anything is in it, so
    /// "resolve the row at this y" has to answer `None` below the last copy —
    /// clamping to the nearest would make three quarters of a quiet page a
    /// button that selects whatever is at the top of the tray.
    #[test]
    fn a_press_in_the_needs_you_tray_names_the_agent_it_is_a_copy_of() {
        use butai_protocol::api::{AgentDto, AgentState};

        let agent = |title: &str, pane: u64, state: AgentState| AgentDto {
            pane: butai_protocol::PaneId(pane),
            title: title.into(),
            state,
            exited: None,
            question: false,
            started_ms: 0,
            working_since_ms: None,
            unread: false,
        };
        let calm = agent("claude", 1, AgentState::Idle);
        let asking = agent("codex", 2, AgentState::Waiting);
        let busy = agent("gemini", 3, AgentState::Working);
        let row = |ws, a| chrome::AllAgentRow {
            workspace: ws,
            workspace_id: butai_protocol::SessionId(1),
            agent: a,
            host: None,
            daemon: 0,
        };
        let fleet = vec![row("one", &calm), row("one", &asking), row("one", &busy)];
        let sys = butai_protocol::api::SysDto::default();
        let (machines, spaces) = booth_scaffold(&sys, &fleet, &[("one", 1, 3)]);
        let view = View { page: Page::Booth, ..Default::default() };
        let tabs: [Tab<'_>; 0] = [];
        let geom = chrome::page_geom(WIDE, ROWS, &view);
        let c = chrome::booth_columns(chrome::booth_area(WIDE, &geom));
        assert!(c.tray_rows.height > 1, "this terminal is tall enough for a tray");

        let x = c.tray_rows.x;
        // Row 3 of the list, not agent 1: a tray copy resolves to its
        // original's *row*, which is the currency the cursor is counted in —
        // machine, project, then the three agents.
        assert_eq!(
            on_fleet(WIDE, ROWS, &view, &fleet, &spaces, &machines, x, c.tray_rows.y),
            Some(FleetHit::Row(3)),
            "the only waiting agent is the only copy in the tray, and it is on row 3"
        );

        // Fold its project and the copy still answers — but there is no row to
        // move a cursor to, so it names nothing rather than naming a row that
        // belongs to something else.
        let mut folded = View { page: Page::Booth, ..Default::default() };
        folded.folds.toggle_space("local", butai_protocol::SessionId(1));
        assert_eq!(
            on_fleet(WIDE, ROWS, &folded, &fleet, &spaces, &machines, x, c.tray_rows.y),
            None,
            "a copy of a folded-away agent must not select some other row"
        );
        for y in c.tray_rows.y + 1..c.tray_rows.y + c.tray_rows.height {
            assert_eq!(
                on_fleet(WIDE, ROWS, &view, &fleet, &spaces, &machines, x, y),
                None,
                "row {y} of the tray is empty and must name nothing"
            );
            // And it must not fall through to a rail BOOTH does not draw, which
            // is the trap the fleet list below has its own assertion for.
            let t = at(WIDE, ROWS, &view, &tabs, 1, None, x, y);
            assert!(matches!(t, Target::Nothing), "row {y} of the tray resolved to {t:?}");
        }
    }

    /// Every chip is clickable across its whole width, and the gaps between the
    /// last chip and the buttons hit nothing rather than the nearest thing.
    #[test]
    fn a_click_on_a_tab_chip_names_that_tab() {
        let (a, b) = (summary("alpha"), summary("beta"));
        let tabs = [
            Tab { summary: &a, host: None, live: true },
            Tab { summary: &b, host: None, live: true },
        ];
        let view = View::default();
        let geom =
            Chrome::compute(COLS, ROWS, false, view.geom, chrome::system_h_wanted(&view.gauges));
        let spans: Vec<(u16, u16)> = chrome::tab_strip(&geom.tabbar, &tabs, &view, 1)
            .spans
            .into_iter()
            .map(|s| s.expect("both chips fit"))
            .collect();
        assert_eq!(spans.len(), 2);
        // The active chip carries `[x]`, which is its own target — every other
        // column of it is still the chip.
        let close = chrome::tab_close_span(&geom.tabbar, &tabs, &view, 1)
            .expect("the active chip should offer its close button");
        for (i, (start, end)) in spans.iter().enumerate() {
            for x in *start..*end {
                if (close.0..close.1).contains(&x) {
                    continue;
                }
                assert_eq!(
                    at(COLS, ROWS, &view, &tabs, 1, None, x, geom.tabbar.y),
                    Target::Tab(i),
                    "column {x} should be tab {i}"
                );
            }
        }
        // Between the chips and the buttons: nothing.
        let after = spans[1].1;
        assert_eq!(
            at(COLS, ROWS, &view, &tabs, 1, None, after + 1, geom.tabbar.y),
            Target::Nothing
        );
    }

    /// The strip's arrows answer the pointer across their labels, and each one
    /// names a workspace that is *not* on the strip — which is the whole reason
    /// to press it. They resolve to [`Target::Tab`] rather than a scroll of
    /// their own: the strip follows the workspace you are in, so reaching one is
    /// how you scroll to it, and the loop needs no new verb to handle them.
    #[test]
    fn the_strip_arrows_reach_the_workspaces_it_cannot_show() {
        let names: Vec<_> = (0..12).map(|i| summary(&format!("project-{i}"))).collect();
        let tabs: Vec<Tab<'_>> =
            names.iter().map(|s| Tab { summary: s, host: None, live: true }).collect();
        // From the middle, so both ends of the strip have something off them.
        let view = View { tab: 6, ..Default::default() };
        let geom =
            Chrome::compute(COLS, ROWS, false, view.geom, chrome::system_h_wanted(&view.gauges));
        let strip = chrome::tab_strip(&geom.tabbar, &tabs, &view, 1);
        let prev = strip.prev.expect("scrolled to the seventh chip, something is behind it");
        let next = strip.next.expect("twelve chips do not fit");
        for ((start, end), i) in [prev, next] {
            assert!(strip.spans[i].is_none(), "the arrow points at a chip already on screen");
            for x in start..end {
                assert_eq!(
                    at(COLS, ROWS, &view, &tabs, 1, None, x, geom.tabbar.y),
                    Target::Tab(i),
                    "column {x} of the arrow should be workspace {i}"
                );
            }
        }
        // And they are the nearest ones either way, not an arbitrary jump.
        let on: Vec<usize> =
            strip.spans.iter().enumerate().filter(|(_, s)| s.is_some()).map(|(i, _)| i).collect();
        assert_eq!(prev.1 + 1, on[0], "`[<]` skipped a workspace");
        assert_eq!(next.1, on[on.len() - 1] + 1, "`[>]` skipped a workspace");
    }

    /// The `[x]` is clickable exactly where it is painted, and only on the
    /// active chip — the daemon's version kept the label and the hit box apart
    /// and warned in a comment that the two drift, which is what this pins.
    #[test]
    fn the_close_button_is_on_the_active_chip_and_nowhere_else() {
        let (a, b) = (summary("alpha"), summary("beta"));
        let tabs = [
            Tab { summary: &a, host: None, live: true },
            Tab { summary: &b, host: None, live: true },
        ];
        for active in 0..2usize {
            let view = View { tab: active, ..Default::default() };
            let geom = Chrome::compute(
                COLS,
                ROWS,
                false,
                view.geom,
                chrome::system_h_wanted(&view.gauges),
            );
            let (start, end) = chrome::tab_close_span(&geom.tabbar, &tabs, &view, 1).unwrap();
            for x in start..end {
                assert_eq!(
                    at(COLS, ROWS, &view, &tabs, 1, None, x, geom.tabbar.y),
                    Target::CloseTab,
                    "column {x} of the tab-{active} close button"
                );
            }
            // The span is the mark itself, drawn inside that chip.
            let strip = chrome::tab_strip(&geom.tabbar, &tabs, &view, 1);
            let (chip_start, chip_end) = strip.spans[active].expect("the active chip is on screen");
            assert!(
                chip_start < start && end <= chip_end,
                "{start}..{end} vs {chip_start}..{chip_end}"
            );
            assert_eq!((end - start) as usize, chrome::TAB_CLOSE_MARK.len());
            // And the inactive chip has none: one press, one workspace.
            let other = 1 - active;
            let (o_start, o_end) = strip.spans[other].expect("both chips fit");
            for x in o_start..o_end {
                assert_eq!(
                    at(COLS, ROWS, &view, &tabs, 1, None, x, geom.tabbar.y),
                    Target::Tab(other)
                );
            }
        }
    }

    /// The spaces button never overlaps the chips, at the narrow width too.
    #[test]
    fn the_spaces_button_is_clear_of_the_chips() {
        let a = summary("alpha");
        let tabs = [Tab { summary: &a, host: None, live: true }];
        let view = View::default();
        let geom =
            Chrome::compute(COLS, ROWS, false, view.geom, chrome::system_h_wanted(&view.gauges));
        let (first, end) =
            chrome::spaces_button_span(&geom.tabbar, &view, 1).expect("it fits at 120");
        for x in first..end {
            assert_eq!(
                at(COLS, ROWS, &view, &tabs, 1, None, x, geom.tabbar.y),
                Target::Spaces,
                "column {x} of the spaces button"
            );
        }
        let chips = chrome::tab_strip(&geom.tabbar, &tabs, &view, 1);
        assert!(chips.spans.iter().flatten().all(|(_, end)| *end < first), "{chips:?} vs {first}");
        // And the machines button starts clear of it.
        let (mx, _) = chrome::machines_span(&geom.tabbar, 1);
        assert!(mx > end, "the spaces button runs into the machines one: {end} vs {mx}");
    }

    #[test]
    fn the_tab_bar_buttons_are_where_they_are_drawn() {
        let a = summary("alpha");
        let tabs = [Tab { summary: &a, host: None, live: true }];
        let view = View::default();
        let geom =
            Chrome::compute(COLS, ROWS, false, view.geom, chrome::system_h_wanted(&view.gauges));
        let y = geom.tabbar.y;
        for (label, start, end) in chrome::tabbar_buttons(&geom.tabbar, 1) {
            let want = if label == chrome::TAB_NEW_LABEL {
                Target::NewWorkspace
            } else {
                Target::Machines
            };
            assert_eq!(at(COLS, ROWS, &view, &tabs, 1, None, start, y), want, "{label} start");
            assert_eq!(at(COLS, ROWS, &view, &tabs, 1, None, end - 1, y), want, "{label} end");
        }
    }

    /// One machines button, answering across its whole label at every count.
    ///
    /// It was two controls — `[+ host]` and a separate `N hosts` count — and the
    /// count only existed past one machine, so the columns it sat in answered
    /// nothing on the common single-machine client. Now the same columns are a
    /// button at every count; only the word in them changes.
    #[test]
    fn the_machines_button_opens_the_picker_at_every_count() {
        let a = summary("alpha");
        let tabs = [Tab { summary: &a, host: None, live: true }];
        let view = View::default();
        for cols in [COLS, WIDE] {
            for daemons in [1usize, 3] {
                let geom = Chrome::compute(
                    cols,
                    ROWS,
                    false,
                    view.geom,
                    chrome::system_h_wanted(&view.gauges),
                );
                let y = geom.tabbar.y;
                let (start, end) = chrome::machines_span(&geom.tabbar, daemons);
                for x in start..end {
                    assert_eq!(
                        at(cols, ROWS, &view, &tabs, daemons, None, x, y),
                        Target::Machines,
                        "column {x} at {cols} columns with {daemons} machines"
                    );
                }
            }
        }
    }

    #[test]
    fn the_footer_buttons_are_where_they_are_drawn() {
        let view = View::default();
        let geom =
            Chrome::compute(COLS, ROWS, false, view.geom, chrome::system_h_wanted(&view.gauges));
        for (label, start, end) in chrome::footer_button_spans(COLS) {
            for x in start..end {
                assert_eq!(
                    at(COLS, ROWS, &view, &[], 1, None, x, geom.footer.y),
                    Target::Footer(label),
                    "column {x} of {label}"
                );
            }
        }
    }

    /// The rails resolve to a row index, and the index is relative to the rail
    /// rather than to the screen.
    #[test]
    fn a_rail_click_is_a_row_of_that_rail() {
        let view = View::default();
        let geom =
            Chrome::compute(COLS, ROWS, false, view.geom, chrome::system_h_wanted(&view.gauges));
        let x = geom.agents_rows.x + 2;
        assert_eq!(
            at(COLS, ROWS, &view, &[], 1, None, x, geom.agents_rows.y),
            Target::Rail(Focus::Agents, 0)
        );
        assert_eq!(
            at(COLS, ROWS, &view, &[], 1, None, x, geom.agents_rows.y + 3),
            Target::Rail(Focus::Agents, 3)
        );
        let px = geom.procs_rows.x + 2;
        assert_eq!(
            at(COLS, ROWS, &view, &[], 1, None, px, geom.procs_rows.y + 1),
            Target::Rail(Focus::Processes, 1)
        );
        // A gauge owns both of its rows — the one naming it and the trace under
        // it — so a press on either resolves to the same gauge. Getting this
        // wrong is invisible until you click a trace and open the monitor for
        // whatever is drawn below it.
        let view = View {
            gauges: vec![chrome::Gauge::Cpu, chrome::Gauge::Ram, chrome::Gauge::Gpu(0)],
            ..view
        };
        let geom =
            Chrome::compute(COLS, ROWS, false, view.geom, chrome::system_h_wanted(&view.gauges));
        let sys = geom.system_rows.y;
        for (dy, want) in [
            (0, chrome::Gauge::Cpu),
            (1, chrome::Gauge::Cpu),
            (2, chrome::Gauge::Ram),
            (3, chrome::Gauge::Ram),
            (4, chrome::Gauge::Gpu(0)),
            (5, chrome::Gauge::Gpu(0)),
        ] {
            assert_eq!(
                at(COLS, ROWS, &view, &[], 1, None, px, sys + dy),
                Target::System(want),
                "row {dy} of the SYSTEM section"
            );
        }
    }

    /// A one-row gauge among two- and three-row ones.
    ///
    /// `Disk` is the first gauge whose height is neither of the other two, and
    /// it sits *below* them — so anything that resolves a row by arithmetic
    /// instead of by walking the list lands on the wrong disk, or on a disk
    /// while pointing at the network trace above it. The walk is already there;
    /// this is what keeps it.
    #[test]
    fn a_press_on_a_disk_row_names_that_disk() {
        use chrome::Gauge;
        let view = View {
            gauges: vec![Gauge::Cpu, Gauge::Ram, Gauge::Net(0), Gauge::Disk(0), Gauge::Disk(1)],
            ..View::default()
        };
        let geom =
            Chrome::compute(COLS, ROWS, false, view.geom, chrome::system_h_wanted(&view.gauges));
        let px = geom.procs_rows.x + 2;
        let sys = geom.system_rows.y;
        for (dy, want) in [
            (0, Target::System(Gauge::Cpu)),
            (1, Target::System(Gauge::Cpu)),
            (2, Target::System(Gauge::Ram)),
            (3, Target::System(Gauge::Ram)),
            (4, Target::System(Gauge::Net(0))),
            (6, Target::System(Gauge::Net(0))),
            (7, Target::System(Gauge::Disk(0))),
            (8, Target::System(Gauge::Disk(1))),
        ] {
            assert_eq!(
                at(COLS, ROWS, &view, &[], 1, None, px, sys + dy),
                want,
                "row {dy} of the SYSTEM section"
            );
        }
    }

    /// A click on a rail that has scrolled names the entry, not the screen row.
    ///
    /// The rails scroll now that a list longer than its section is reachable,
    /// and this is the other half of that: the scroll comes from the same
    /// [`chrome::rail_first`] the drawing used, so the row under the pointer and
    /// the row that gets selected are the same one. Resolving against the raw
    /// `y` offset — which is what this did while the lists were drawn from the
    /// top — selects the agent `first` places above the one clicked.
    #[test]
    fn a_click_on_a_scrolled_rail_names_the_entry_under_it() {
        let geom = Chrome::compute(
            COLS,
            ROWS,
            false,
            View::default().geom,
            chrome::system_h_wanted(&View::default().gauges),
        );
        let h = geom.agents_rows.height as usize;
        // Twice the section, with the cursor at the end: the list is scrolled by
        // as much as it can be.
        let ws = workspace(2 * h, 2 * h, None);
        let view = View { agent_sel: 2 * h - 1, proc_sel: 2 * h - 1, ..Default::default() };

        let x = geom.agents_rows.x + 2;
        assert_eq!(
            at(COLS, ROWS, &view, &[], 1, Some(&ws), x, geom.agents_rows.y),
            Target::Rail(Focus::Agents, h),
            "the top row of a list scrolled to its end"
        );
        assert_eq!(
            at(COLS, ROWS, &view, &[], 1, Some(&ws), x, geom.agents_rows.bottom() - 1),
            Target::Rail(Focus::Agents, 2 * h - 1),
            "the bottom row is the cursor's own"
        );

        // The processes list scrolls on its own cursor and its own height.
        let ph = geom.procs_rows.height as usize;
        assert_eq!(
            at(COLS, ROWS, &view, &[], 1, Some(&ws), geom.procs_rows.x + 2, geom.procs_rows.y),
            Target::Rail(Focus::Processes, 2 * h - ph),
        );

        // Unscrolled, a screen row is still the entry — the case the old code
        // got right, and the one every short list is in.
        let top = View::default();
        assert_eq!(
            at(COLS, ROWS, &top, &[], 1, Some(&ws), x, geom.agents_rows.y + 3),
            Target::Rail(Focus::Agents, 3)
        );
    }

    /// The changes list runs to the bottom of the right rail.
    ///
    /// It did not while the ALL AGENTS panel shared the rail, and the hazard
    /// then was a stale split — a click below the list resolving to the list
    /// because the geometry had been computed without the panel. With one list
    /// in the rail the question is simply whether its last row is its own.
    #[test]
    fn the_changes_list_runs_to_the_bottom_of_the_rail() {
        let view = View::default();
        let geom =
            Chrome::compute(COLS, ROWS, false, view.geom, chrome::system_h_wanted(&view.gauges));
        let x = geom.changes_rows.x + 2;
        assert_eq!(geom.changes_rows.bottom(), geom.right_inner.bottom());
        // The list stops above the verb rows, which are pinned to the bottom.
        let (list_h, footer_h) = chrome::changes_split(&geom);
        assert!(footer_h > 0, "the rail should give its verbs a row");
        let last_list = geom.changes_rows.y + list_h - 1;
        assert!(matches!(
            at(COLS, ROWS, &view, &[], 1, None, x, last_list),
            Target::Rail(Focus::Changes, _)
        ));
    }

    /// A workspace with `agents` agents, `procs` processes and whatever changes
    /// were asked for — the three lists `at` scrolls, each long enough to say so.
    fn workspace(agents: usize, procs: usize, changes: Option<ChangesDto>) -> WorkspaceDetail {
        use butai_protocol::api::{AgentDto, AgentState, ProcessDto};
        use butai_protocol::PaneId;
        WorkspaceDetail {
            id: SessionId(1),
            name: "proj".into(),
            cwd: "/tmp/proj".into(),
            agents: (0..agents)
                .map(|i| AgentDto {
                    pane: PaneId(i as u64),
                    title: format!("agent-{i}"),
                    state: AgentState::Idle,
                    exited: None,
                    question: false,
                    started_ms: 0,
                    working_since_ms: None,
                    unread: false,
                })
                .collect(),
            processes: (0..procs)
                .map(|i| ProcessDto {
                    pane: PaneId(100 + i as u64),
                    name: format!("proc-{i}"),
                    command: String::new(),
                    status: "ok".into(),
                    exited: None,
                })
                .collect(),
            changes,
            stage: None,
            autostart: Vec::new(),
        }
    }

    fn changes(unstaged: &str) -> ChangesDto {
        use butai_protocol::api::{FileChange, RepoState};
        ChangesDto {
            branch: "main".into(),
            staged: vec![],
            unstaged: vec![FileChange {
                path: unstaged.into(),
                code: "M".into(),
                added: 1,
                deleted: 0,
            }],
            recent_commits: vec![],
            conflicted: vec![],
            upstream: Some("origin/main".into()),
            ahead: 0,
            behind: 0,
            state: RepoState::Clean,
            detached: false,
        }
    }

    /// Every verb the rail draws is clickable, at the columns the table laid it
    /// out in — and the click carries the same key the button advertises.
    ///
    /// The table is what makes that safe: `verbs::layout` places them and
    /// `verbs::hit` reads them back, so this cannot drift the way a hand-written
    /// span table did.
    #[test]
    fn every_verb_the_rail_draws_can_be_clicked() {
        // The cursor on an unstaged file, which is the row with the most verbs.
        let view = View { changes_sel: 1, ..Default::default() };
        let c = changes("src/main.rs");
        let ws = workspace(0, 0, Some(c.clone()));
        let geom =
            Chrome::compute(COLS, ROWS, false, view.geom, chrome::system_h_wanted(&view.gauges));
        let (list_h, footer_h) = chrome::changes_split(&geom);
        let verbs = chrome::changes_verbs(&c, view.changes_sel);
        let spans =
            crate::verbs::layout(&verbs, geom.changes_rows.width as usize, footer_h as usize);
        assert!(!spans.is_empty(), "an unstaged file should offer verbs");
        for span in &spans {
            let y = geom.changes_rows.y + list_h + span.row as u16;
            for col in span.start..span.end {
                let x = geom.changes_rows.x + col as u16;
                assert_eq!(
                    at(COLS, ROWS, &view, &[], 1, Some(&ws), x, y),
                    Target::ChangesVerb(span.key),
                    "column {col} of row {} should be `{}`",
                    span.row,
                    span.key
                );
            }
        }
        // And a rail with no changes has no verbs to hit rather than a panic.
        assert_eq!(
            at(COLS, ROWS, &view, &[], 1, None, geom.changes_rows.x, geom.changes_rows.y + list_h),
            Target::Nothing
        );
    }

    /// Every verb the left rail draws is clickable at the columns it was drawn
    /// in, and each carries the key that word advertises.
    ///
    /// The hint lines used to be one hit box apiece: any column of
    /// `a:new  A:pick  x:kill` meant "spawn an agent", so clicking the word
    /// `x:kill` did the opposite of what it said. Anchored to
    /// `verbs::layout` — the same packing the drawing uses — rather than to
    /// `at` itself, which would pass with the words anywhere at all.
    ///
    /// Run with and without a pinned agent, because the AGENTS table is not the
    /// same table in both: pinned, `a` and `A` are different verbs and both are
    /// drawn; unpinned they are the same one, so `A` is bound but not drawn. The
    /// hit-test reads `view.pinned_agent` to decide, exactly as the drawing
    /// does, and this is what says the two agree.
    #[test]
    fn every_verb_the_left_rail_draws_can_be_clicked() {
        use crate::verbs::{layout, RAIL_FOOTER_ROWS};
        for pin in [None, Some("claude".to_string())] {
            let view = View { pinned_agent: pin.clone(), ..Default::default() };
            let geom = Chrome::compute(
                COLS,
                ROWS,
                false,
                view.geom,
                chrome::system_h_wanted(&view.gauges),
            );
            let sections = [
                (
                    geom.agents_hint.expect("AGENTS has a verb row"),
                    crate::verbs::agents_verbs(pin.is_some()),
                    true,
                ),
                (
                    geom.procs_hint.expect("PROCESSES has a verb row"),
                    crate::verbs::procs_verbs(),
                    false,
                ),
            ];
            for (hint, verbs, agents) in sections {
                let spans = layout(verbs, hint.width as usize, RAIL_FOOTER_ROWS);
                // Against the verbs that ask for a slot, not against every verb
                // in the table: a `quiet` one is deliberately not drawn, and
                // counting it here would demand a button for something that has
                // none by design.
                let drawn = verbs.iter().filter(|v| v.footer).count();
                assert_eq!(spans.len(), drawn, "a verb was dropped: {spans:?}");
                for span in &spans {
                    for col in span.start..span.end {
                        let x = hint.x + col as u16;
                        let want = if agents {
                            Target::AgentsVerb(span.key)
                        } else {
                            Target::ProcsVerb(span.key)
                        };
                        assert_eq!(
                            at(COLS, ROWS, &view, &[], 1, None, x, hint.y),
                            want,
                            "column {col} of the `{}` verb row, pinned={:?}",
                            span.key,
                            pin
                        );
                    }
                }
            }
        }
        let view = View::default();
        let geom =
            Chrome::compute(COLS, ROWS, false, view.geom, chrome::system_h_wanted(&view.gauges));

        // The two `[+ ...]` buttons on the box borders are the spawn verbs
        // under another name, so they resolve to the same keys and go the same
        // way — a button that meant something the footer does not say is how
        // the two drifted apart in the first place.
        let label = chrome::agents_add_label(view.pinned_agent.as_deref(), geom.left_box);
        let (start, end) = chrome::agents_add_span_for(&geom, &label);
        for x in start..end {
            assert_eq!(
                at(COLS, ROWS, &view, &[], 1, None, x, geom.left_box.y),
                Target::AgentsVerb(crate::verbs::SPAWN_AGENT_KEY)
            );
        }
        let (start, end) = chrome::procs_add_span(&geom);
        for x in start..end {
            assert_eq!(
                at(COLS, ROWS, &view, &[], 1, None, x, geom.procs_sep),
                Target::ProcsVerb(crate::verbs::NEW_SHELL_KEY)
            );
        }
    }

    /// A page's rows are entries, not screen rows.
    ///
    /// The scroll is what makes those different, and it is why these do not
    /// live in `at`: a directory outgrows its column, and a click twenty rows
    /// down a scrolled list means the twentieth *visible* entry, not the
    /// twentieth entry.
    #[test]
    fn a_page_row_is_an_entry_not_a_screen_row() {
        let view = View { page: Page::Files, ..Default::default() };
        let geom =
            Chrome::compute(COLS, ROWS, false, view.geom, chrome::system_h_wanted(&view.gauges));
        let list = chrome::files_row_area(&geom);
        let (x, top) = (list.x + 1, list.y);

        // Unscrolled, the first visible row is entry 0.
        assert_eq!(on_page(COLS, ROWS, &view, 0, x, top), PageTarget::Row(0));
        assert_eq!(on_page(COLS, ROWS, &view, 0, x, top + 3), PageTarget::Row(3));

        // With the cursor past the bottom, the list has scrolled and the same
        // screen row is a later entry.
        let scrolled = list.height as usize + 5;
        let first = chrome::first_visible(scrolled, list.height);
        assert!(first > 0, "the list should have scrolled, or this proves nothing");
        assert_eq!(on_page(COLS, ROWS, &view, scrolled, x, top), PageTarget::Row(first));

        // Right of the list is the open file, and the rails are not on this
        // page at all.
        let body_x = list.right() + 2;
        assert_eq!(on_page(COLS, ROWS, &view, 0, body_x, top), PageTarget::Body);
        // On the agents page nothing here is a page row.
        let work = View::default();
        assert_eq!(on_page(COLS, ROWS, &work, 0, x, top), PageTarget::Nothing);
    }

    /// A click on the stage arrives in the pane's own coordinates, because that
    /// is the only frame of reference the program on the other end has.
    #[test]
    fn a_stage_click_is_translated_into_the_panes_own_coordinates() {
        let view = View::default();
        let geom =
            Chrome::compute(COLS, ROWS, false, view.geom, chrome::system_h_wanted(&view.gauges));
        let (x, y) = (geom.stage_inner.x, geom.stage_inner.y);
        assert_eq!(at(COLS, ROWS, &view, &[], 1, None, x, y), Target::Stage(0, 0));
        assert_eq!(at(COLS, ROWS, &view, &[], 1, None, x + 5, y + 2), Target::Stage(5, 2));
        // The border is not the pane.
        assert_ne!(at(COLS, ROWS, &view, &[], 1, None, x - 1, y), Target::Stage(0, 0));
    }

    /// Zen hides the rails, so their columns become stage. A click that still
    /// selected an invisible rail row would be the worst kind of bug: nothing
    /// on screen would explain it.
    #[test]
    fn zen_gives_the_rails_columns_back_to_the_stage() {
        let view = View { zen: true, ..Default::default() };
        let geom =
            Chrome::compute(COLS, ROWS, true, view.geom, chrome::system_h_wanted(&view.gauges));
        for y in geom.stage_inner.y..geom.stage_inner.bottom() {
            let t = at(COLS, ROWS, &view, &[], 1, None, geom.stage_inner.x, y);
            assert!(matches!(t, Target::Stage(0, _)), "row {y} resolved to {t:?}");
        }
    }
}
