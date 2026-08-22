//! The SETTINGS page: this client's own configuration, as a page you enter,
//! change and leave.
//!
//! **Why a page and not the modal it would obviously have been.** A modal is
//! right for one question whose whole answer fits on screen — which is what
//! every other overlay here is. Settings is six groups of them, and one of the
//! six cannot be answered in a box at all: the only way to judge a palette is
//! to see what it does to a screen. So moving the cursor onto a theme applies
//! it to the whole workbench, live, and leaving without choosing puts the old
//! one back. That is the feature a modal cannot have, because the modal is
//! covering the thing you are trying to look at.
//!
//! **Every row names the key it writes.** The label is for the reader and the
//! faint text beside it is the actual TOML — `[theme] name`. A settings page
//! that invents its own vocabulary for a file people already own leaves them
//! with two things to learn and no way to map a row onto the line they would
//! edit by hand.
//!
//! **There is no Save button.** A change applies and is written when you make
//! it, because that is what this client already does everywhere else: dragging
//! a rail calls [`crate::config::Config::save_ui`], and pinning an agent calls
//! `save_default_agent`. A Save button here would make this the one
//! surface in the product where something you can see has not happened yet.
//! The writes stay surgical — `toml_edit` rewrites one key and leaves every
//! comment, ordering and unrelated table alone — so a hand-written config
//! survives being edited by this page.
//!
//! **Nothing on this page is the daemon's.** A palette and a keymap belong to
//! whatever is drawing, and the daemon draws no chrome — so there is no config
//! route to call and none is invented here. The one daemon-owned fact the page
//! shows, the list of configured agent types, arrives on `GET /v1/agents` and
//! is drawn as a fact rather than a setting.

use super::{ellipsize, put_str, Geom, LRect, Page, Pen, Theme, View};
use ratatui::buffer::Buffer;

/// The page's own state: where the cursor is, what it has loaded, and where to
/// go back to.
///
/// Its own struct rather than fields on [`View`] for the reason [`super::Git`]
/// and [`super::Docker`] have theirs: it is about one page and is dropped when
/// that page is left.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Which group of settings the cursor is in.
    pub group: usize,
    /// Which row within it.
    pub row: usize,
    /// A choice row expanded in place, and which option is highlighted.
    ///
    /// In place rather than in a modal: a page that opens a modal to answer one
    /// of its own rows is a page that did not need to be a page.
    pub open: Option<usize>,
    /// Agent types the active daemon has, from `GET /v1/agents`.
    ///
    /// Loaded on arrival, like the GIT page's reads: nothing here changes
    /// unless the daemon's own config does.
    pub agents: Vec<String>,
    /// Palettes that resolve right now — the built-ins, then `~/.butai/themes`.
    pub themes: Vec<String>,
    /// The palette named in `config.toml`. Held so that previewing a theme by
    /// moving the cursor can be undone by leaving without choosing.
    pub saved_theme: String,
    /// Whether a `butai` over ssh may pull its machine into the tab bar.
    pub auto_attach: bool,
    /// `[[remote]]` blocks, as they read.
    pub remotes: Vec<String>,
    /// How many keys are bound, and how many of those came from `[keys]`.
    pub bindings: (usize, usize),
    pub loaded: bool,
    /// The page this one was entered from, so `esc` puts it back rather than
    /// dropping you somewhere you never were.
    pub ret: Page,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            group: 0,
            row: 0,
            open: None,
            agents: Vec::new(),
            themes: Vec::new(),
            saved_theme: crate::config::DEFAULT_THEME.into(),
            auto_attach: true,
            remotes: Vec::new(),
            bindings: (0, 0),
            loaded: false,
            ret: Page::Agents,
        }
    }
}

/// Which measurement a size row moves.
///
/// Rail widths go through `resize_rail`, which is what LAYOUT mode's drag
/// calls; band heights go through `set_band`, which shares that drag's floor.
/// Either way the clamping lives in one place, so a rail cannot be typed into
/// a state it could not be dragged into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dim {
    LeftRail,
    RightRail,
    Band(super::Band),
}

/// What a row does when it is pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// One of a list. Enter opens it in place; moving inside it previews.
    Choice(Vec<String>),
    Toggle(bool),
    Size(Dim),
    /// Read-only: either it is the daemon's, or it is a fact about this client
    /// rather than a choice it offers.
    Info,
}

/// Which setting a row is.
///
/// The handler matches on this rather than on "row 0 of the APPEARANCE group",
/// so inserting a setting above another cannot silently reassign what Enter
/// does to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowId {
    Theme,
    DefaultAgent,
    AutoAttach,
    LeftRail,
    RightRail,
    ProcsRows,
    SystemRows,
    Links,
    /// Read-only. Several rows share it because none of them acts.
    Fact,
}

/// One setting.
pub struct Row {
    pub id: RowId,
    pub label: &'static str,
    /// The TOML key this writes, drawn faint so the page and the file are never
    /// two vocabularies for one setting. Empty where there is no key — a fact,
    /// not a setting.
    pub key: &'static str,
    /// One sentence, under every row rather than only under the cursor: a
    /// description that appears only where the cursor is makes the list jump as
    /// you walk it.
    pub desc: &'static str,
    pub value: String,
    pub kind: Kind,
}

impl Row {
    fn info(label: &'static str, key: &'static str, value: String, desc: &'static str) -> Self {
        Self { id: RowId::Fact, label, key, desc, value, kind: Kind::Info }
    }

    /// Whether the cursor can do anything here. Drives both the dim ink and
    /// which verbs the footer offers, so the two cannot disagree.
    pub fn editable(&self) -> bool {
        self.kind != Kind::Info
    }
}

/// One group of settings — a row in the list down the left.
pub struct Group {
    pub id: GroupId,
    pub label: &'static str,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupId {
    Appearance,
    Agents,
    Workbench,
    Machines,
    Keys,
    About,
}

/// A change the page made that the key handler cannot finish on its own —
/// because it writes a file, repaints in a new palette, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// The cursor moved and nothing was written. Still worth reporting: walking
    /// an open theme list previews as it goes, so the palette on screen is a
    /// function of where the cursor is and has to be recomputed after any move.
    Moved,
    /// Keep the highlighted palette: apply it and write `[theme] name`.
    Theme(String),
    /// Pin (or, with `None`, clear) the agent `a` spawns without asking.
    DefaultAgent(Option<String>),
    AutoAttach(bool),
    /// Mark URLs up as hyperlinks for the terminal, or stop.
    Links(bool),
    /// `view.geom` has already moved; persist it to `[ui]`.
    Geom,
}

/// The default-agent row's "no pin" option.
///
/// A named constant because choosing it writes `None`, and matching a literal
/// in two files is how that comes to mean an agent actually called this.
pub const ASK_EVERY_TIME: &str = "ask every time";

/// Every group and row, built fresh from the live state.
///
/// Built rather than cached so a value can never be stale: the geometry also
/// moves from LAYOUT mode, the pin also moves from the agent picker's `d`, and
/// a page holding its own copy of either would show a number the rest of the
/// client had moved on from.
pub fn groups(s: &Settings, view: &View) -> Vec<Group> {
    let g = view.geom;
    let band = |v: Option<u16>| v.map(|n| format!("{n} rows")).unwrap_or_else(|| "auto".into());

    vec![
        Group {
            id: GroupId::Appearance,
            label: "APPEARANCE",
            rows: vec![
                Row {
                    id: RowId::Theme,
                    label: "theme",
                    key: "[theme] name",
                    desc: "The palette every part of the chrome draws from.",
                    value: s.saved_theme.clone(),
                    kind: Kind::Choice(s.themes.clone()),
                },
                Row::info(
                    "themes directory",
                    "",
                    crate::theme::themes_dir().display().to_string(),
                    "Any <name>.toml here joins the list above, and may extend a built-in.",
                ),
            ],
        },
        Group {
            id: GroupId::Agents,
            label: "AGENTS",
            rows: vec![
                Row {
                    id: RowId::DefaultAgent,
                    label: "default agent",
                    key: "[general] default_agent",
                    desc: "What `a` and [+] spawn with nothing in between. `A` still picks.",
                    value: view.pinned_agent.clone().unwrap_or_else(|| ASK_EVERY_TIME.into()),
                    kind: Kind::Choice(
                        std::iter::once(ASK_EVERY_TIME.to_string())
                            .chain(s.agents.iter().cloned())
                            .collect(),
                    ),
                },
                Row::info(
                    "available agents",
                    "[[agents]]",
                    if s.agents.is_empty() {
                        "none configured".into()
                    } else {
                        s.agents.join(", ")
                    },
                    "The daemon's own, not this client's. Its config file defines them.",
                ),
            ],
        },
        Group {
            id: GroupId::Workbench,
            label: "WORKBENCH",
            rows: vec![
                Row {
                    id: RowId::LeftRail,
                    label: "left rail",
                    key: "[ui] left_rail",
                    desc: "Agents, processes and the gauges. alt-l drags the same number.",
                    value: format!("{} cells", g.left_w),
                    kind: Kind::Size(Dim::LeftRail),
                },
                Row {
                    id: RowId::RightRail,
                    label: "right rail",
                    key: "[ui] right_rail",
                    desc: "The CHANGES rail.",
                    value: format!("{} cells", g.right_w),
                    kind: Kind::Size(Dim::RightRail),
                },
                Row {
                    id: RowId::ProcsRows,
                    label: "processes rows",
                    key: "[ui] procs_height",
                    desc: "Rows for PROCESSES; AGENTS takes whatever is left over.",
                    value: band(g.procs_h),
                    kind: Kind::Size(Dim::Band(super::Band::Procs)),
                },
                Row {
                    id: RowId::SystemRows,
                    label: "system rows",
                    key: "[ui] system_height",
                    desc: "Rows for the gauges under that rail — cpu, ram, gpu, net, disks.",
                    value: band(g.system_h),
                    kind: Kind::Size(Dim::Band(super::Band::System)),
                },
                Row {
                    id: RowId::Links,
                    label: "clickable links",
                    key: "[ui] links",
                    desc: "Mark URLs up for your terminal, so the pointer can follow one.",
                    value: on_off(view.links).into(),
                    kind: Kind::Toggle(view.links),
                },
            ],
        },
        Group {
            id: GroupId::Machines,
            label: "MACHINES",
            rows: std::iter::once(Row {
                id: RowId::AutoAttach,
                label: "auto-attach",
                key: "[general] remote_auto_attach",
                desc: "Let `butai` over ssh in a pane pull its machine into this bar.",
                value: on_off(s.auto_attach).into(),
                kind: Kind::Toggle(s.auto_attach),
            })
            .chain(s.remotes.iter().map(|r| {
                Row::info(
                    "remote",
                    "[[remote]]",
                    r.clone(),
                    "Dialled at start, so the machine is in the bar every morning.",
                )
            }))
            .collect(),
        },
        Group {
            id: GroupId::Keys,
            label: "KEYS",
            rows: vec![
                Row::info(
                    "prefix",
                    "[general] prefix",
                    view.prefix.clone(),
                    "The key that opens a prefix binding, tmux-style.",
                ),
                Row::info(
                    "bindings",
                    "[keys]",
                    format!("{} bound, {} from your config", s.bindings.0, s.bindings.1),
                    "The same table `?` lists. Edit them in the file below.",
                ),
            ],
        },
        Group {
            id: GroupId::About,
            label: "ABOUT",
            rows: vec![
                Row::info(
                    "version",
                    "",
                    format!("butai {}", env!("CARGO_PKG_VERSION")),
                    "Client and daemon agree a protocol version at the handshake.",
                ),
                Row::info(
                    "config",
                    "",
                    crate::config::Config::path().display().to_string(),
                    "This file. Every row above writes one key in it, and nothing else.",
                ),
                Row::info(
                    "socket",
                    "",
                    butai_protocol::paths::socket_path().display().to_string(),
                    "BUTAI_SOCKET overrides it. HTTP and the framed protocol share it.",
                ),
            ],
        },
    ]
}

fn on_off(v: bool) -> &'static str {
    if v {
        "on"
    } else {
        "off"
    }
}

/// Columns for the group list. The longest label is `APPEARANCE`, at 10.
const GROUP_W: u16 = 22;
/// Columns the label takes before the key it writes.
const LABEL_W: u16 = 20;
/// Columns the TOML key takes. `[general] remote_auto_attach` is the longest at
/// 28, and a key clipped to an ellipsis is one you cannot search the file for.
const KEY_W: u16 = 29;
/// The widest a row is drawn, however much body there is.
///
/// A settings row is a label, the key it writes and its value, and those three
/// have to read as one line. Measured against the body's own width they came
/// apart on a wide terminal: the gutter, the two fixed columns and a value
/// column wide enough for `~/.butai/config.toml` add up to this, and whatever
/// body is left over is margin.
const BODY_MAX_W: u16 = 3 + LABEL_W + KEY_W + 55 + 3;
/// Rows the group list puts between entries.
pub const GROUP_STRIDE: u16 = 2;
/// Rows a setting takes: the row, its sentence, and a blank.
pub const ROW_STRIDE: u16 = 3;

/// Where the page's two columns sit: the groups, and the settings themselves.
///
/// There was a third — a palette column down the right — and the swatches it
/// held are now drawn under the theme row that they are about. A column costs
/// its width on every group, including the five that have nothing to do with
/// colour, and it had to be dropped below 132 columns, which made the page two
/// different pages depending on the terminal.
pub struct Columns {
    pub groups: LRect,
    pub body: LRect,
}

pub fn columns(outer: LRect) -> Columns {
    Columns {
        groups: LRect::new(outer.x, outer.y, GROUP_W, outer.height),
        body: LRect::new(
            outer.x + GROUP_W,
            outer.y,
            outer.width.saturating_sub(GROUP_W),
            outer.height,
        ),
    }
}

/// Which group row `y` is over, if any.
pub fn group_at(list: LRect, count: usize, y: u16) -> Option<usize> {
    let first = list.y + 2;
    if y < first {
        return None;
    }
    let offset = y - first;
    offset
        .is_multiple_of(GROUP_STRIDE)
        .then_some((offset / GROUP_STRIDE) as usize)
        .filter(|i| *i < count)
}

/// Which setting `y` is over, accounting for an expanded choice pushing
/// everything below it down.
pub fn row_at(area: LRect, grp: &Group, state: &Settings, y: u16) -> Option<usize> {
    let mut cursor = area.y + 2;
    let row_ix = clamp_row(state.row, grp);
    for (i, row) in grp.rows.iter().enumerate() {
        if y == cursor {
            return Some(i);
        }
        cursor += ROW_STRIDE;
        if i == row_ix {
            if let (Some(_), Kind::Choice(options)) = (state.open, &row.kind) {
                cursor += options.len() as u16 + 1;
            }
        }
    }
    None
}

/// Which option of the expanded row `y` is over.
pub fn option_at(area: LRect, grp: &Group, state: &Settings, y: u16) -> Option<usize> {
    state.open?;
    let row_ix = clamp_row(state.row, grp);
    let Kind::Choice(options) = &grp.rows.get(row_ix)?.kind else { return None };
    let first = area.y + 2 + row_ix as u16 * ROW_STRIDE + ROW_STRIDE;
    (y >= first && y < first + options.len() as u16).then(|| (y - first) as usize)
}

fn clamp_row(row: usize, grp: &Group) -> usize {
    row.min(grp.rows.len().saturating_sub(1))
}

/// The verbs under the page: the keys that work on the row the cursor is on, so
/// a row that cannot be changed does not advertise Enter.
pub fn verbs(grp: &Group, state: &Settings) -> Vec<(&'static str, &'static str)> {
    let mut v: Vec<(&str, &str)> = vec![("j/k", "move")];
    match grp.rows.get(clamp_row(state.row, grp)).map(|r| &r.kind) {
        Some(Kind::Choice(_)) if state.open.is_some() => {
            v.push(("enter", "choose"));
            v.push(("esc", "keep the old one"));
            return v;
        }
        Some(Kind::Choice(_)) => v.push(("enter", "change")),
        Some(Kind::Toggle(_)) => v.push(("space", "toggle")),
        Some(Kind::Size(_)) => {
            v.push(("-/+", "adjust"));
            v.push(("0", "auto"));
        }
        _ => {}
    }
    v.push(("tab", "group"));
    v.push(("esc", "close"));
    v
}

pub fn draw(buf: &mut Buffer, geom: &Geom, s: Option<&Settings>, view: &View, theme: &Theme) {
    let Some(state) = s else { return };
    let cols = columns(geom.stage_box);
    let grps = groups(state, view);
    let group = state.group.min(grps.len().saturating_sub(1));

    draw_groups(buf, cols.groups, &grps, group, theme);
    draw_body(buf, cols.body, &grps[group], state, theme);
}

fn draw_groups(buf: &mut Buffer, area: LRect, grps: &[Group], group: usize, theme: &Theme) {
    let bound = area.x + area.width;
    put_str(
        buf,
        area.x + 1,
        area.y,
        "SETTINGS",
        bound,
        Pen { fg: theme.accent, bg: theme.ground, bold: true },
    );

    for (i, grp) in grps.iter().enumerate() {
        let y = area.y + 2 + i as u16 * GROUP_STRIDE;
        if y >= area.y + area.height {
            break;
        }
        let on = i == group;
        let bg = theme.row_bg(on);
        for x in area.x..bound {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_bg(bg);
            }
        }
        if on {
            put_str(buf, area.x + 1, y, ">", bound, Pen::new(theme.accent, bg));
        }
        let fg = if on { theme.accent } else { theme.muted };
        put_str(buf, area.x + 3, y, grp.label, bound, Pen { fg, bg, bold: on });
        let n = grp.rows.len().to_string();
        put_str(
            buf,
            bound.saturating_sub(n.len() as u16 + 1),
            y,
            &n,
            bound,
            Pen::new(theme.faint, bg),
        );
    }

    // The two lines that keep the page honest: it is a view of a file, the file
    // is still there, and nothing on it is waiting for a Save button.
    let bottom = area.y + area.height;
    if bottom > area.y + 3 {
        // The file's *name*, not its path: this column is 22 wide, and
        // `/home/somebody/.butai/config.toml` ellipsizes to a prefix that names
        // nothing. ABOUT has the body's width and carries the whole path, which
        // is where you go when you actually need to type it.
        let name = crate::config::Config::path()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config.toml".into());
        put_str(
            buf,
            area.x + 1,
            bottom - 2,
            &ellipsize(&name, area.width.saturating_sub(2) as usize),
            bound,
            Pen::new(theme.faint, theme.ground),
        );
        put_str(
            buf,
            area.x + 1,
            bottom - 1,
            "saved on change",
            bound,
            Pen::new(theme.faint, theme.ground),
        );
    }
}

fn draw_body(buf: &mut Buffer, area: LRect, grp: &Group, state: &Settings, theme: &Theme) {
    // Rows stop at [`BODY_MAX_W`] rather than at the body's own edge. Values
    // are set hard right, so on a wide terminal a row read `auto-attach
    // [general] remote_auto_attach` and then a hundred columns of nothing
    // before `on` — a label and its value too far apart to take in together.
    let bound = area.x + area.width.min(BODY_MAX_W);
    // The verbs take the last row, so the settings stop one short of it.
    let floor = area.y + area.height.saturating_sub(2);
    put_str(
        buf,
        area.x + 1,
        area.y,
        grp.label,
        bound,
        Pen { fg: theme.accent, bg: theme.ground, bold: true },
    );

    let row_ix = clamp_row(state.row, grp);
    let mut y = area.y + 2;
    for (i, row) in grp.rows.iter().enumerate() {
        if y + 1 >= floor {
            break;
        }
        let on = i == row_ix;
        let bg = theme.row_bg(on);
        for x in area.x..bound {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_bg(bg);
            }
        }
        if on {
            put_str(buf, area.x + 1, y, ">", bound, Pen::new(theme.accent, bg));
        }
        let fg = if !row.editable() {
            theme.faint
        } else if on {
            theme.accent
        } else {
            theme.ink
        };
        put_str(
            buf,
            area.x + 3,
            y,
            &ellipsize(row.label, LABEL_W as usize),
            bound,
            Pen { fg, bg, bold: on },
        );
        if !row.key.is_empty() {
            put_str(
                buf,
                area.x + 3 + LABEL_W,
                y,
                &ellipsize(row.key, KEY_W as usize),
                bound,
                Pen::new(theme.faint, bg),
            );
        }

        // The value, hard right. A chevron says the row opens a list; a toggle
        // reads as the word it is, in ok when it is on.
        let tail = if matches!(row.kind, Kind::Choice(_)) { "  >" } else { "" };
        let room = (bound - area.x)
            .saturating_sub(3 + LABEL_W + KEY_W + tail.len() as u16 + 2)
            .max(MIN_VALUE_W);
        let value = ellipsize(&row.value, room as usize);
        let vw = value.chars().count() as u16;
        let vx = bound.saturating_sub(vw + tail.len() as u16 + 2);
        let vfg = match &row.kind {
            Kind::Toggle(true) => theme.ok,
            Kind::Info => theme.muted,
            _ => theme.ink,
        };
        put_str(buf, vx, y, &value, bound, Pen::new(vfg, bg));
        put_str(buf, vx + vw, y, tail, bound, Pen::new(theme.accent, bg));
        y += 1;

        put_str(
            buf,
            area.x + 3,
            y,
            &ellipsize(row.desc, bound.saturating_sub(area.x + 5) as usize),
            bound,
            Pen::new(theme.faint, theme.ground),
        );
        y += 1;

        if on {
            if let (Some(opt), Kind::Choice(options)) = (state.open, &row.kind) {
                for (oi, option) in options.iter().enumerate() {
                    if y >= floor {
                        break;
                    }
                    let sel = oi == opt;
                    let obg = if sel { theme.selection } else { theme.ground };
                    for x in area.x + 4..bound.saturating_sub(2) {
                        if let Some(cell) = buf.cell_mut((x, y)) {
                            cell.set_symbol(" ");
                            cell.set_bg(obg);
                        }
                    }
                    if sel {
                        put_str(buf, area.x + 6, y, ">", bound, Pen::new(theme.accent, obg));
                    }
                    let current = *option == row.value;
                    let ofg = if sel {
                        theme.accent
                    } else if current {
                        theme.ink
                    } else {
                        theme.muted
                    };
                    put_str(buf, area.x + 8, y, option, bound, Pen { fg: ofg, bg: obg, bold: sel });
                    if current {
                        put_str(
                            buf,
                            area.x + 9 + option.chars().count() as u16,
                            y,
                            "(current)",
                            bound,
                            Pen::new(theme.faint, obg),
                        );
                    }
                    y += 1;
                }
            }
        }
        y += 1;
    }

    // Under the rows it is about, and only there. The other five groups get the
    // width back.
    if grp.id == GroupId::Appearance {
        draw_palette(buf, area, y, floor, theme);
    }

    // The keys that work on the row the cursor is on, drawn where every other
    // list here draws its verbs: directly under it.
    let vy = area.y + area.height.saturating_sub(1);
    let mut x = area.x + 3;
    for (key, label) in verbs(grp, state) {
        let w = key.len() as u16 + label.len() as u16 + 3;
        if x + w >= bound {
            break;
        }
        put_str(buf, x, vy, key, bound, Pen { fg: theme.accent, bg: theme.ground, bold: true });
        x += key.len() as u16 + 1;
        put_str(buf, x, vy, label, bound, Pen::new(theme.faint, theme.ground));
        x += label.len() as u16 + 3;
    }
}

/// The narrowest a value may be squeezed before it stops being worth drawing at
/// all — enough for `auto` and an ellipsis.
const MIN_VALUE_W: u16 = 6;

/// Columns one swatch and its role name take. `rule_focus` is the longest at 10.
const SWATCH_W: u16 = 17;

/// Every role the chrome spends a colour on, in the order they are shown.
///
/// Named here rather than inline so the count is one number: the grid wraps to
/// the width it is given, which is the whole reason this can sit under a row
/// instead of down a column of its own.
fn roles(theme: &Theme) -> [(&'static str, ratatui::style::Color); 15] {
    [
        ("ground", theme.ground),
        ("surface", theme.surface),
        ("selection", theme.selection),
        ("ink", theme.ink),
        ("muted", theme.muted),
        ("faint", theme.faint),
        ("rule", theme.rule),
        ("rule_focus", theme.rule_focus),
        ("accent", theme.accent),
        ("info", theme.info),
        ("ok", theme.ok),
        ("attention", theme.attention),
        ("danger", theme.danger),
        ("status_bg", theme.status_bg),
        ("status_fg", theme.status_fg),
    ]
}

/// The palette, in the roles the chrome actually spends them on, under the row
/// that chooses it.
///
/// A swatch grid rather than a miniature workbench: the live apply already
/// shows the theme in context — it is painting the page you are reading — so
/// what this adds is the roles you *cannot* currently see, which is exactly
/// what a grid is for. Walking the open theme list repaints these with it, so
/// the grid answers the question the list is asking.
///
/// Wrapped to the body's width rather than stacked one per line: fifteen rows
/// would push the sentence under the last setting off the bottom on a short
/// terminal, and the point of moving here was to stop the palette costing
/// anything the rest of the page needs.
fn draw_palette(buf: &mut Buffer, area: LRect, top: u16, floor: u16, theme: &Theme) {
    let bound = area.x + area.width;
    let per_row = (area.width.saturating_sub(4) / SWATCH_W).max(1) as usize;
    let mut y = top;
    if y >= floor {
        return;
    }
    put_str(buf, area.x + 3, y, "palette", bound, Pen::new(theme.faint, theme.ground));
    y += 1;

    for chunk in roles(theme).chunks(per_row) {
        if y >= floor {
            return;
        }
        let mut x = area.x + 3;
        for (name, color) in chunk {
            // Painted as a filled run rather than a glyph: a block character is
            // East-Asian-ambiguous width in some terminals and would shift the
            // row.
            for cx in x..x + 4 {
                if let Some(cell) = buf.cell_mut((cx, y)) {
                    cell.set_symbol(" ");
                    cell.set_bg(*color);
                }
            }
            put_str(buf, x + 5, y, name, bound, Pen::new(theme.faint, theme.ground));
            x += SWATCH_W;
        }
        y += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Settings {
        Settings {
            themes: vec!["blueprint-dark".into(), "blueprint-light".into(), "terminal".into()],
            agents: vec!["claude".into(), "codex".into()],
            ..Default::default()
        }
    }

    /// Every group has rows, and every sentence fits the body at the narrowest
    /// terminal the workbench claims to work at. A description that can only
    /// ever render as an ellipsis documents nothing.
    #[test]
    fn every_row_has_a_sentence_that_fits() {
        let grps = groups(&state(), &View::default());
        assert!(!grps.is_empty());
        // 100 columns: narrow enough to be a real terminal somebody works in,
        // and the width at which the palette column has already given up — so
        // the body is as wide as it will ever be for its share of the screen.
        // `WORK_STAGE_MIN_W` is the *stage's* floor, not a whole terminal's,
        // which is what made the first version of this budget wrong.
        let body = 100 - GROUP_W;
        for grp in &grps {
            assert!(!grp.rows.is_empty(), "{} has no rows", grp.label);
            for row in &grp.rows {
                assert!(
                    row.desc.chars().count() <= (body - 5) as usize,
                    "{}/{}: sentence is {} cols, the body gives {}",
                    grp.label,
                    row.label,
                    row.desc.chars().count(),
                    body - 5
                );
                assert!(row.desc.ends_with('.'), "{}/{}: not a sentence", grp.label, row.label);
            }
        }
    }

    /// The label and key columns must hold what goes in them. A key clipped to
    /// `[general] remote_auto_att…` is one you cannot find in the file, which
    /// is the entire job that column has.
    #[test]
    fn the_key_column_holds_the_longest_key() {
        for grp in groups(&state(), &View::default()) {
            for row in &grp.rows {
                assert!(
                    row.key.chars().count() <= KEY_W as usize,
                    "{}: key {:?} is {} cols, the column gives {KEY_W}",
                    grp.label,
                    row.key,
                    row.key.chars().count()
                );
                assert!(
                    row.label.chars().count() <= LABEL_W as usize,
                    "{}: label {:?} is {} cols",
                    grp.label,
                    row.label,
                    row.label.chars().count()
                );
            }
        }
    }

    /// The body has the whole width that is not the group list, at every size.
    ///
    /// It did not while the palette had a column: the page was one shape above
    /// 132 columns and another below it, and the sentences under the settings
    /// were the thing paying for the difference.
    #[test]
    fn the_body_takes_every_column_the_group_list_does_not() {
        for width in [100, 132, 150, 220] {
            let c = columns(LRect::new(0, 0, width, 40));
            assert_eq!(c.groups.width, GROUP_W);
            assert_eq!(
                c.groups.width + c.body.width,
                width,
                "nothing between the two columns at {width}"
            );
            assert_eq!(c.body.x, c.groups.x + GROUP_W, "the body starts where the groups end");
        }
    }

    /// A row does not spread out to fill whatever body it is given.
    ///
    /// The value is set hard right, so an uncapped row on a 200-column terminal
    /// puts `on` a hundred columns from the key it belongs to. Now that the
    /// body takes every column the group list does not, this is the shape the
    /// page is always in on a wide screen. Checked as the gap between where the
    /// key ends and where the value starts, which is the thing that was
    /// actually wrong — asserting on `BODY_MAX_W` itself would just restate the
    /// constant.
    #[test]
    fn a_row_does_not_stretch_across_a_wide_terminal() {
        let theme = Theme::default();
        let st = state();
        let grps = groups(&st, &View::default());
        let machines = grps.iter().find(|g| g.id == GroupId::Machines).expect("MACHINES");

        for width in [110u16, 150, 200, 400] {
            let mut buf = Buffer::empty(ratatui::layout::Rect::new(0, 0, width, 20));
            draw_body(&mut buf, LRect::new(0, 0, width, 20), machines, &st, &theme);
            let row: String =
                (0..width).map(|x| buf.cell((x, 2)).unwrap().symbol().to_owned()).collect();
            let key_end = row.find("remote_auto_attach").expect("the key is drawn") + 18;
            let value_at = row.rfind("on").expect("the value is drawn");
            assert!(
                value_at - key_end < 60,
                "at {width} columns the value sits {} cells past its key: {row:?}",
                value_at - key_end
            );
        }
    }

    /// The swatches are drawn under the APPEARANCE rows and nowhere else.
    ///
    /// Checked by painting: the grid has no row model of its own — it wraps to
    /// whatever width the body has — so the only honest question is whether the
    /// colours reached the buffer under the right group.
    #[test]
    fn the_palette_is_painted_under_the_theme_row_only() {
        let theme = Theme::default();
        let grps = groups(&state(), &View::default());
        let area = LRect::new(0, 0, 120, 40);

        let painted = |grp: &Group| {
            let mut buf = Buffer::empty(ratatui::layout::Rect::new(0, 0, area.width, area.height));
            draw_body(&mut buf, area, grp, &state(), &theme);
            let mut text = String::new();
            for y in 0..area.height {
                for x in 0..area.width {
                    text.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
                }
                text.push('\n');
            }
            text
        };

        let appearance = grps.iter().find(|g| g.id == GroupId::Appearance).expect("APPEARANCE");
        let body = painted(appearance);
        assert!(body.contains("rule_focus"), "a role name is missing:\n{body}");
        // By a role name rather than by the heading: "palette" is also a word
        // in the theme row's own sentence, and the first match would be that.
        assert!(
            body.find("rule_focus") > body.find("themes directory"),
            "the palette belongs under the rows it is about:\n{body}"
        );

        for grp in grps.iter().filter(|g| g.id != GroupId::Appearance) {
            let body = painted(grp);
            assert!(
                !body.contains("rule_focus"),
                "{} is not about colour and must not draw the palette:\n{body}",
                grp.label
            );
        }
    }

    /// A click on the group list lands on the group it points at, and on
    /// nothing in the gaps between them.
    #[test]
    fn a_click_lands_on_the_group_it_points_at() {
        let list = LRect::new(0, 1, GROUP_W, 30);
        assert_eq!(group_at(list, 6, 3), Some(0));
        assert_eq!(group_at(list, 6, 4), None, "the gap belongs to nothing");
        assert_eq!(group_at(list, 6, 5), Some(1));
        assert_eq!(group_at(list, 6, 1), None, "the heading is not a group");
        assert_eq!(group_at(list, 6, 3 + 6 * GROUP_STRIDE), None, "past the end");
    }

    /// An expanded choice pushes the rows below it down, and a click has to
    /// follow it — otherwise opening the theme list makes every row under it
    /// answer for its neighbour.
    #[test]
    fn a_click_follows_an_expanded_row() {
        let area = LRect::new(0, 1, 60, 30);
        let mut st = state();
        let grps = groups(&st, &View::default());
        let grp = &grps[0];

        assert_eq!(row_at(area, grp, &st, 3), Some(0));
        assert_eq!(row_at(area, grp, &st, 6), Some(1), "closed, the next row is 3 down");
        assert_eq!(option_at(area, grp, &st, 6), None, "and nothing is expanded");

        st.open = Some(0);
        assert_eq!(row_at(area, grp, &st, 3), Some(0));
        assert_eq!(
            row_at(area, grp, &st, 6),
            None,
            "that row is the first theme now, not the next setting"
        );
        assert_eq!(option_at(area, grp, &st, 6), Some(0));
        assert_eq!(option_at(area, grp, &st, 8), Some(2), "three themes are listed");
        assert_eq!(option_at(area, grp, &st, 9), None, "and the fourth row is not one");
        // Three options plus the blank under them push the next setting down.
        assert_eq!(row_at(area, grp, &st, 6 + 4), Some(1));
    }

    /// The footer offers Enter only where Enter does something.
    #[test]
    fn the_verbs_are_the_keys_that_work_on_this_row() {
        let st = state();
        let grps = groups(&st, &View::default());
        let appearance = &grps[0];
        let v = verbs(appearance, &st);
        assert!(v.contains(&("enter", "change")), "the theme row opens: {v:?}");

        // Its second row is the themes directory, which is a fact.
        let on_info = Settings { row: 1, ..state() };
        let v = verbs(appearance, &on_info);
        assert!(!v.iter().any(|(k, _)| *k == "enter"), "a fact must not offer enter: {v:?}");

        // While the list is open, esc means "keep the old one" rather than
        // "close the page" — leaving a preview must not also leave settings.
        let open = Settings { open: Some(1), ..state() };
        let v = verbs(appearance, &open);
        assert_eq!(v.iter().filter(|(k, _)| *k == "esc").count(), 1, "{v:?}");
        assert!(v.contains(&("esc", "keep the old one")), "{v:?}");
    }

    /// The default-agent row always offers a way back to being asked, and it is
    /// first — unpinning is the question a pin actually raises.
    #[test]
    fn the_default_agent_row_offers_no_pin_at_all() {
        let grps = groups(&state(), &View::default());
        let agents = grps.iter().find(|g| g.id == GroupId::Agents).expect("AGENTS group");
        let Kind::Choice(options) = &agents.rows[0].kind else { panic!("not a choice") };
        assert_eq!(options[0], ASK_EVERY_TIME);
        assert!(options.contains(&"claude".to_string()));

        // With nothing pinned, that is what the row reads.
        assert_eq!(agents.rows[0].value, ASK_EVERY_TIME);
        let pinned = View { pinned_agent: Some("codex".into()), ..Default::default() };
        let grps = groups(&state(), &pinned);
        let agents = grps.iter().find(|g| g.id == GroupId::Agents).unwrap();
        assert_eq!(agents.rows[0].value, "codex");
    }
}
