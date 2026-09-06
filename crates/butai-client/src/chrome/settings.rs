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
    /// Every machine this client knows of: the ones in the tab bar and the
    /// `[[remote]]` blocks that are not.
    ///
    /// Assembled by the loop from `daemons`, `hosts`, the live forwards and the
    /// config file — see [`Machine`]. Handed to the page rather than gathered by
    /// it for the reason the theme list is: none of those things is the page's,
    /// and a page holding its own copy of a daemon list would be drawing a tab
    /// bar the rest of the client had moved on from.
    pub machines: Vec<Machine>,
    /// How many keys are bound, and how many of those came from `[keys]`.
    pub bindings: (usize, usize),
    /// Whether butai looks for a newer release: `[update] check`.
    pub update_check: bool,
    /// Which releases it looks at: `[update] channel`.
    pub update_channel: crate::update::Channel,
    /// A newer release, once a check has found one. `None` covers both "no
    /// check has answered yet" and "this is the latest", which the ABOUT row
    /// draws the same way — there is nothing to offer either way.
    pub update_available: Option<String>,
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
            machines: Vec::new(),
            bindings: (0, 0),
            update_check: true,
            update_channel: crate::update::Channel::default(),
            update_available: None,
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
    /// Enter *does* something, and the something does not come back as a value
    /// on this row. The word is the verb the footer offers — "update",
    /// "connect", "forget".
    ///
    /// **Why this is not a toggle.** Every other row on this page answers a
    /// question the file also answers, and its value is the answer; pressing it
    /// changes what the row reads. Connecting a machine, or asking a daemon to
    /// replace its own binary, has no such value: the row would have to read
    /// `off` for "not connected" and flip to `on` several seconds later when an
    /// ssh landed, or not flip at all when it did not. A toggle that lies about
    /// whether it took is worse than a row that plainly says what it will do.
    ///
    /// It is deliberately still [`Row::editable`], because that predicate
    /// drives both the dim ink and which verbs the footer offers, and an action
    /// row is exactly a row the cursor can do something on.
    Action(&'static str),
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
    UpdateCheck,
    UpdateChannel,
    /// Ask the daemon on this row's machine to replace its own binary.
    MachineUpdate,
    /// Dial a `[[remote]]` that is not in the tab bar.
    MachineConnect,
    /// Drop the link to a machine that is, and forget its block.
    MachineDisconnect,
    /// Remove a `[[remote]]` block without there being a link to drop.
    MachineForget,
    /// Bring another machine in — the machines picker, from here.
    MachineAdd,
    /// Read-only. Several rows share it because none of them acts.
    Fact,
}

/// One setting.
pub struct Row {
    pub id: RowId,
    /// Owned rather than `&'static str` because the MACHINES group names
    /// machines, and a hostname is not known at compile time. Ellipsized to
    /// [`LABEL_W`] when it is drawn, like every other label.
    pub label: String,
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
    /// Which machine this row is about, by the badge its tabs carry — empty for
    /// the local daemon, which has none, and `None` on every row that is not
    /// one of a machine's.
    ///
    /// The badge rather than an index into [`Settings::machines`], for the
    /// reason [`RowId`] exists at all: an index is a position in a list that is
    /// rebuilt every frame, and a machine leaving the tab bar between the frame
    /// you pressed Enter on and the press arriving would silently move the
    /// action onto its neighbour.
    pub machine: Option<String>,
    /// Drawn two columns in, so a machine's rows read as a block under its
    /// name rather than as five more settings.
    pub indent: bool,
}

impl Row {
    fn info(
        label: impl Into<String>,
        key: &'static str,
        value: String,
        desc: &'static str,
    ) -> Self {
        Self {
            id: RowId::Fact,
            label: label.into(),
            key,
            desc,
            value,
            kind: Kind::Info,
            machine: None,
            indent: false,
        }
    }

    /// Whether the cursor can do anything here. Drives both the dim ink and
    /// which verbs the footer offers, so the two cannot disagree.
    pub fn editable(&self) -> bool {
        self.kind != Kind::Info
    }

    /// Columns the label is indented by, which the key column has to give back
    /// so it stays in the same place on every row of the group.
    fn pad(&self) -> u16 {
        if self.indent {
            INDENT
        } else {
            0
        }
    }
}

/// Columns a machine's own rows are set in from its name.
const INDENT: u16 = 2;

/// Whether this client is talking to a machine, and why not when it is not.
///
/// Four states rather than a bool because they want four different things done
/// about them, and the section exists to make that obvious: a machine that is
/// away is one the loop is already re-dialling and you need do nothing about; a
/// machine that is offline has a `[[remote]]` block and no link, and the row
/// under it offers to dial it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// In the tab bar with its event stream up.
    Here,
    /// In the tab bar, stream down. Its rails are a photograph, and the loop is
    /// rebuilding the forward on its own clock.
    Away,
    /// An ssh of ours is mid key exchange.
    Dialling,
    /// Configured and not connected. `why` is the last dial failure, which is
    /// the whole answer to "why is that machine not here" — see
    /// [`Machine::note`].
    Offline,
}

/// What build a daemon is running, as far as this client can tell.
///
/// **Where the version comes from.** A daemon names its build in the framed
/// handshake (`ServerMsg::Hello.server_version`) and nowhere else — there is no
/// REST route that reports it, and `POST /v1/update` reports one only by
/// *performing* an update. So this is read by opening a control connection and
/// reading the Hello, exactly as `kill-server` and `reload-config` already do,
/// and it is read on arriving at the page rather than on a timer: a daemon's
/// version changes when it restarts, which is an event this client is told
/// about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Build {
    /// Not asked yet, or the machine did not answer.
    #[default]
    Unknown,
    On(String),
    /// It accepted an update and is going down to come back on `to`. Kept
    /// distinct from [`Build::On`] so the page cannot go on claiming a version
    /// that the machine has already been told to stop running.
    Updating {
        from: String,
        to: Option<String>,
    },
}

/// One machine, as the MACHINES section shows it.
///
/// Every field is somebody else's: the link and the counts are the loop's view
/// of its `Daemon`, the destination is its `[[remote]]` block, and the build
/// came off a handshake. Gathered into one record so the page can draw a
/// machine without knowing about any of the three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    /// The badge its tabs carry, and what a disconnect names it by. Empty for
    /// the daemon on this machine, which has no badge — see [`Machine::name`].
    pub badge: String,
    pub link: Link,
    pub build: Build,
    /// How it is reached: `ssh gpu-box`, `socket /tmp/fwd.sock`, or the local
    /// daemon's own socket path.
    pub how: String,
    /// Agents running across all of its workspaces, and workspaces open on it.
    pub agents: usize,
    pub spaces: usize,
    /// Whether a `[[remote]]` block names it, so it comes back tomorrow.
    pub configured: bool,
    /// Whether *this* client opened the ssh underneath it. False for a
    /// `[[remote]] socket` block — somebody else's forward, with no child of
    /// ours to kill — which is what decides whether a disconnect row is offered
    /// at all, the same question `disconnect_daemon` asks before it removes
    /// anything.
    pub ours: bool,
    /// The daemon on this machine. It is never dialled, never dropped, and
    /// updates by being replaced rather than by being asked.
    pub local: bool,
    /// The ssh destination a connect row would dial. `None` for a block that
    /// names a `socket` instead, which this client cannot bring up itself.
    pub target: Option<String>,
    /// What went wrong last time, when something did.
    pub note: Option<String>,
}

impl Machine {
    /// What to call it in a row. The local daemon has no badge and an unnamed
    /// row reads as a bug, so it gets the same word the BOOTH compute column
    /// gives it.
    pub fn name(&self) -> &str {
        if self.badge.is_empty() {
            "local"
        } else {
            &self.badge
        }
    }

    /// Whether the daemon is answering right now — the one question every
    /// action row's availability turns on.
    fn reachable(&self) -> bool {
        self.link == Link::Here
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
    /// Look for a newer release, or stop looking.
    UpdateCheck(bool),
    /// Follow the stable releases, or the dev track's prereleases too.
    UpdateChannel(crate::update::Channel),
    /// `view.geom` has already moved; persist it to `[ui]`.
    Geom,
}

/// The default-agent row's "no pin" option.
///
/// A named constant because choosing it writes `None`, and matching a literal
/// in two files is how that comes to mean an agent actually called this.
pub const ASK_EVERY_TIME: &str = "ask every time";

/// One machine's block: what it is doing, what it is running, where it is, and
/// the two or three things you can do about it.
///
/// **The rows an unreachable machine does not get.** Update and disconnect are
/// requests to a daemon that has to answer them, and a row that can only ever
/// report a refusal is a row that reads as broken — the same rule the machines
/// picker follows when it declines to offer a disconnect for a forward it does
/// not own. So an offline machine gets `connect` and `forget` instead, which
/// are the two things that *are* possible without it answering.
///
/// `latest` is the newest release this client's own update check found, which
/// is what a daemon's version is compared against. It is this client's answer
/// rather than that machine's — a daemon follows the channel configured where
/// it runs — so the version row's sentence says so.
fn machine_rows(m: &Machine, latest: Option<&str>) -> Vec<Row> {
    let action =
        |id: RowId, label: &'static str, verb: &'static str, key, value: String, desc| Row {
            id,
            label: label.into(),
            key,
            desc,
            value,
            kind: Kind::Action(verb),
            machine: Some(m.badge.clone()),
            indent: true,
        };
    let fact = |label: &'static str, value: String, desc| Row {
        machine: Some(m.badge.clone()),
        indent: true,
        ..Row::info(label, "", value, desc)
    };

    let mut rows = vec![Row {
        machine: Some(m.badge.clone()),
        // The block's own head, so the name is not repeated down five rows.
        // It writes nothing itself; the key names the block underneath it,
        // which is the line you would delete by hand to undo the whole thing.
        ..Row::info(
            m.name(),
            if m.configured { "[[remote]]" } else { "" },
            status_line(m),
            "Whether this client is talking to it, and what it is carrying.",
        )
    }];

    rows.push(fact(
        "version",
        version_line(m, latest),
        "Read at its handshake. Newer means newer than this client last saw.",
    ));
    rows.push(fact(
        "where",
        m.how.clone(),
        "The ssh destination, or the socket already forwarded here.",
    ));

    if m.reachable() {
        rows.push(action(
            RowId::MachineUpdate,
            "update",
            "update",
            "",
            if m.local {
                "download and restart this butai".into()
            } else {
                "downloads there, then restarts that daemon".into()
            },
            // Says what happens to the machine rather than to the row, because
            // that is the part you cannot take back: panes are killed and
            // restored, and every client on it is detached in the meantime.
            "It restarts the daemon; workspaces are saved and come back.",
        ));
    }

    match (m.local, m.reachable(), m.ours, m.configured, &m.target) {
        // Nothing to drop and nothing to dial: this client *is* this machine.
        (true, ..) => {}
        (_, true, true, ..) => rows.push(action(
            RowId::MachineDisconnect,
            "disconnect",
            "disconnect",
            "[[remote]]",
            "drops the ssh and forgets it".into(),
            "The far daemon keeps running; only this client's link to it goes.",
        )),
        // Here on a forward this client did not open. There is no ssh of ours
        // to kill, so the row says why instead of offering.
        (_, true, false, ..) => rows.push(fact(
            "link",
            "on a forward of its own".into(),
            "Somebody else's `ssh -L`. Close that to disconnect it.",
        )),
        (_, false, _, true, Some(target)) => {
            rows.push(action(
                RowId::MachineConnect,
                "connect",
                "connect",
                "[[remote]]",
                format!("dial {target} now"),
                "One ssh, then its projects join the tab bar. `alt-h` does this too.",
            ));
            rows.push(action(
                RowId::MachineForget,
                "forget",
                "forget",
                "[[remote]]",
                "removes its block".into(),
                "Stops it being dialled every morning. Nothing on it is touched.",
            ));
        }
        // A `[[remote]] socket` that is not answering: the forward is somebody
        // else's to bring up, but the block is still ours to remove.
        (_, false, _, true, None) => rows.push(action(
            RowId::MachineForget,
            "forget",
            "forget",
            "[[remote]]",
            "removes its block".into(),
            "Stops it being dialled every morning. Nothing on it is touched.",
        )),
        (_, false, _, false, _) => {}
    }
    rows
}

/// A machine's head row: what it is doing, and what it is carrying while it
/// does it.
fn status_line(m: &Machine) -> String {
    let load = format!(
        "{} agent{}, {} workspace{}",
        m.agents,
        if m.agents == 1 { "" } else { "s" },
        m.spaces,
        if m.spaces == 1 { "" } else { "s" }
    );
    match &m.link {
        Link::Here => format!("connected — {load}"),
        // The counts stay, and say what they are: a machine that went away is
        // still running everything it was running, and the numbers are the last
        // ones it sent rather than nothing at all.
        Link::Away => match &m.note {
            Some(why) => format!("away — {why}"),
            None => format!("away — last seen with {load}"),
        },
        Link::Dialling => "connecting…".into(),
        Link::Offline => match &m.note {
            Some(why) => format!("not connected — {why}"),
            None => "not connected".into(),
        },
    }
}

/// The version row's value: the build, and whether a newer one is known.
fn version_line(m: &Machine, latest: Option<&str>) -> String {
    match &m.build {
        Build::On(v) => match latest {
            Some(l) if l != v => format!("{v} — {l} available"),
            _ => v.clone(),
        },
        Build::Updating { from, to: Some(to) } => format!("{from} → {to}, restarting"),
        Build::Updating { from, to: None } => format!("{from} — updating"),
        // Not a failure worth dressing up: a machine that is not answering has
        // no handshake to read a version off, and saying so is the honest row.
        Build::Unknown => "unknown".into(),
    }
}

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
                    label: "theme".into(),
                    key: "[theme] name",
                    desc: "The palette every part of the chrome draws from.",
                    value: s.saved_theme.clone(),
                    kind: Kind::Choice(s.themes.clone()),
                    machine: None,
                    indent: false,
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
                    label: "default agent".into(),
                    key: "[general] default_agent",
                    desc: "What `a` and [+] spawn with nothing in between. `A` still picks.",
                    value: view.pinned_agent.clone().unwrap_or_else(|| ASK_EVERY_TIME.into()),
                    kind: Kind::Choice(
                        std::iter::once(ASK_EVERY_TIME.to_string())
                            .chain(s.agents.iter().cloned())
                            .collect(),
                    ),
                    machine: None,
                    indent: false,
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
                    label: "left rail".into(),
                    key: "[ui] left_rail",
                    desc: "Agents, processes and the gauges. alt-l drags the same number.",
                    value: format!("{} cells", g.left_w),
                    kind: Kind::Size(Dim::LeftRail),
                    machine: None,
                    indent: false,
                },
                Row {
                    id: RowId::RightRail,
                    label: "right rail".into(),
                    key: "[ui] right_rail",
                    desc: "The CHANGES rail.",
                    value: format!("{} cells", g.right_w),
                    kind: Kind::Size(Dim::RightRail),
                    machine: None,
                    indent: false,
                },
                Row {
                    id: RowId::ProcsRows,
                    label: "processes rows".into(),
                    key: "[ui] procs_height",
                    desc: "Rows for PROCESSES; AGENTS takes whatever is left over.",
                    value: band(g.procs_h),
                    kind: Kind::Size(Dim::Band(super::Band::Procs)),
                    machine: None,
                    indent: false,
                },
                Row {
                    id: RowId::SystemRows,
                    label: "system rows".into(),
                    key: "[ui] system_height",
                    desc: "Rows for the gauges under that rail — cpu, ram, gpu, net, disks.",
                    value: band(g.system_h),
                    kind: Kind::Size(Dim::Band(super::Band::System)),
                    machine: None,
                    indent: false,
                },
                Row {
                    id: RowId::Links,
                    label: "clickable links".into(),
                    key: "[ui] links",
                    desc: "Mark URLs up for your terminal, so the pointer can follow one.",
                    value: on_off(view.links).into(),
                    kind: Kind::Toggle(view.links),
                    machine: None,
                    indent: false,
                },
            ],
        },
        Group {
            id: GroupId::Machines,
            label: "MACHINES",
            rows: std::iter::once(Row {
                id: RowId::AutoAttach,
                label: "auto-attach".into(),
                key: "[general] remote_auto_attach",
                desc: "Let `butai` over ssh in a pane pull its machine into this bar.",
                value: on_off(s.auto_attach).into(),
                kind: Kind::Toggle(s.auto_attach),
                machine: None,
                indent: false,
            })
            .chain(s.machines.iter().flat_map(|m| machine_rows(m, s.update_available.as_deref())))
            .chain(std::iter::once(Row {
                id: RowId::MachineAdd,
                label: "add a machine".into(),
                key: "[[remote]]",
                // Named as the thing it opens rather than as a second way to
                // do it: the picker is `alt-h` everywhere else in the client,
                // and a settings page that grew its own destination prompt
                // would be a second implementation of connecting.
                desc: "The machines picker — an ssh alias, or a destination typed in.",
                value: "alt-h".into(),
                kind: Kind::Action("add"),
                machine: None,
                indent: false,
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
                    match &s.update_available {
                        Some(v) => format!("butai {} — {v} available", env!("CARGO_PKG_VERSION")),
                        None => format!("butai {}", env!("CARGO_PKG_VERSION")),
                    },
                    "Client and daemon agree a protocol version at the handshake.",
                ),
                Row {
                    id: RowId::UpdateCheck,
                    label: "check for updates".into(),
                    key: "[update] check",
                    // Where the *acting* lives, since this row only decides
                    // whether to look. A page that both looked and installed
                    // would be a page that restarts the daemon under you.
                    desc: "Ask GitHub about new releases at start. `:update` asks now.",
                    value: on_off(s.update_check).into(),
                    kind: Kind::Toggle(s.update_check),
                    machine: None,
                    indent: false,
                },
                Row {
                    id: RowId::UpdateChannel,
                    label: "release channel".into(),
                    key: "[update] channel",
                    // Says what changes rather than what a track is: the two
                    // words are only meaningful next to the sentence that
                    // tells you a dev build arrives every few days.
                    desc: "`dev` takes the prereleases cut from develop, not just stable ones.",
                    value: s.update_channel.as_str().into(),
                    kind: Kind::Choice(
                        crate::update::Channel::ALL
                            .iter()
                            .map(|c| c.as_str().to_string())
                            .collect(),
                    ),
                    machine: None,
                    indent: false,
                },
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
                    "BUTAI_HOME, then BUTAI_SOCKET, override it. HTTP and framing share it.",
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
        // The row's own word, not "act": Enter on a machine's `forget` row and
        // Enter on its `update` row do very different things, and the footer is
        // the last thing read before one of them happens.
        Some(Kind::Action(verb)) => v.push(("enter", verb)),
        _ => {}
    }
    // Only where there is something to re-ask. Every other group is a view of a
    // file this page just wrote, so a refresh key on one would be a key that
    // does nothing — the version and the link state are the only things here
    // that another machine can change under you.
    if grp.id == GroupId::Machines {
        v.push(("r", "re-read"));
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
        // The label moves in for a machine's own rows; the key column does not,
        // so it stays in the same place down the whole group.
        put_str(
            buf,
            area.x + 3 + row.pad(),
            y,
            &ellipsize(&row.label, (LABEL_W - row.pad()) as usize),
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
            // Not a value at all — a sentence about what pressing it will do —
            // so it is inked as the offer it is rather than as a reading.
            Kind::Action(_) => theme.accent,
            _ => theme.ink,
        };
        put_str(buf, vx, y, &value, bound, Pen::new(vfg, bg));
        put_str(buf, vx + vw, y, tail, bound, Pen::new(theme.accent, bg));
        y += 1;

        put_str(
            buf,
            area.x + 3 + row.pad(),
            y,
            &ellipsize(row.desc, bound.saturating_sub(area.x + 5 + row.pad()) as usize),
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
            machines: vec![local(), gpu_box(), asleep()],
            ..Default::default()
        }
    }

    /// This machine: connected, never dialled, nothing to forget.
    fn local() -> Machine {
        Machine {
            badge: String::new(),
            link: Link::Here,
            build: Build::On("1.3.0".into()),
            how: "/run/user/1000/butai/butai.sock".into(),
            agents: 2,
            spaces: 1,
            configured: false,
            ours: false,
            local: true,
            target: None,
            note: None,
        }
    }

    /// A machine in the bar on an ssh of ours, with a block remembering it.
    fn gpu_box() -> Machine {
        Machine {
            badge: "gpu-box".into(),
            link: Link::Here,
            build: Build::On("1.2.0".into()),
            how: "ssh gpu-box".into(),
            agents: 3,
            spaces: 2,
            configured: true,
            ours: true,
            local: false,
            target: Some("gpu-box".into()),
            note: None,
        }
    }

    /// Configured, not here, and the reason it is not here.
    fn asleep() -> Machine {
        Machine {
            badge: "pi-farm".into(),
            link: Link::Offline,
            build: Build::Unknown,
            how: "ssh pi@farm".into(),
            agents: 0,
            spaces: 0,
            configured: true,
            ours: false,
            local: false,
            target: Some("pi@farm".into()),
            note: Some("ssh: connect to host farm port 22: No route to host".into()),
        }
    }

    /// The MACHINES rows for one machine, by id.
    fn ids(m: &Machine, latest: Option<&str>) -> Vec<RowId> {
        machine_rows(m, latest).iter().map(|r| r.id).collect()
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

    /// A machine's block answers the four questions a machine raises, and the
    /// rows that *act* are only offered where they could work.
    ///
    /// The section was one read-only line per `[[remote]]` carrying the host
    /// string and nothing else, so none of "is it connected", "what is it
    /// running", "why did it not come up" and "can I have it back" had an
    /// answer anywhere on the page.
    #[test]
    fn a_machine_block_says_what_it_is_doing_and_offers_what_it_can() {
        let here = machine_rows(&gpu_box(), None);
        assert_eq!(here[0].label, "gpu-box");
        assert_eq!(here[0].key, "[[remote]]", "a remembered machine names its block");
        assert!(here[0].value.starts_with("connected"), "{}", here[0].value);
        assert!(here[0].value.contains("3 agents"), "{}", here[0].value);
        assert!(here[0].value.contains("2 workspaces"), "{}", here[0].value);
        assert_eq!(
            ids(&gpu_box(), None),
            vec![
                RowId::Fact,
                RowId::Fact,
                RowId::Fact,
                RowId::MachineUpdate,
                RowId::MachineDisconnect
            ]
        );

        // Asleep: nothing can be asked of a daemon that is not answering, so
        // the two rows that ask are not drawn — and the two that do not need it
        // are. A row that could only ever report a refusal reads as broken.
        let gone = machine_rows(&asleep(), None);
        assert_eq!(
            ids(&asleep(), None),
            vec![
                RowId::Fact,
                RowId::Fact,
                RowId::Fact,
                RowId::MachineConnect,
                RowId::MachineForget
            ]
        );
        assert!(
            gone[0].value.contains("No route to host"),
            "the dial failure is the whole answer to why it is not here: {}",
            gone[0].value
        );
        assert!(
            gone[3].value.contains("pi@farm"),
            "the row names what it will dial: {}",
            gone[3].value
        );

        // This machine is neither dialled nor dropped, and has no block.
        assert_eq!(local().name(), "local");
        assert_eq!(
            ids(&local(), None),
            vec![RowId::Fact, RowId::Fact, RowId::Fact, RowId::MachineUpdate]
        );
        assert_eq!(machine_rows(&local(), None)[0].key, "", "nothing in the file names it");

        // Somebody else's forward: connected, and not ours to drop — the same
        // distinction the machines picker draws before it offers a disconnect.
        let borrowed = Machine { ours: false, ..gpu_box() };
        assert_eq!(
            ids(&borrowed, None),
            vec![RowId::Fact, RowId::Fact, RowId::Fact, RowId::MachineUpdate, RowId::Fact],
            "the link row explains itself rather than offering"
        );
    }

    /// The version row says which build, and whether a newer one is known.
    #[test]
    fn the_version_row_reads_the_handshake_and_the_release_it_knows_of() {
        let m = gpu_box();
        assert_eq!(version_line(&m, None), "1.2.0", "nothing to compare it against");
        assert_eq!(version_line(&m, Some("1.2.0")), "1.2.0", "it is the newest there is");
        assert_eq!(version_line(&m, Some("1.3.0")), "1.2.0 — 1.3.0 available");

        // A machine that has never answered a handshake has no version, and
        // saying so beats inventing one.
        assert_eq!(version_line(&asleep(), Some("1.3.0")), "unknown");

        // And one that has just been told to update must not go on advertising
        // the build it is on its way off.
        let updating = Machine {
            build: Build::Updating { from: "1.2.0".into(), to: Some("1.3.0".into()) },
            ..gpu_box()
        };
        assert_eq!(version_line(&updating, Some("1.3.0")), "1.2.0 → 1.3.0, restarting");
    }

    /// An action row offers its own verb, and a fact still offers none.
    ///
    /// The footer is the last thing read before Enter happens, so "act" would
    /// be the wrong word on every one of these: forgetting a machine and asking
    /// its daemon to replace itself are not the same press.
    #[test]
    fn an_action_row_advertises_what_enter_will_do() {
        let grps = groups(&state(), &View::default());
        let machines = grps.iter().find(|g| g.id == GroupId::Machines).expect("MACHINES");
        let at = |label: &str| {
            machines.rows.iter().position(|r| r.label == label).unwrap_or_else(|| panic!("{label}"))
        };

        let on_update = Settings { group: 3, row: at("update"), ..state() };
        let v = verbs(machines, &on_update);
        assert!(v.contains(&("enter", "update")), "{v:?}");

        let on_forget = Settings { group: 3, row: at("forget"), ..state() };
        assert!(verbs(machines, &on_forget).contains(&("enter", "forget")));

        // The head row is a fact and stays one.
        let on_head = Settings { group: 3, row: at("gpu-box"), ..state() };
        let v = verbs(machines, &on_head);
        assert!(!v.iter().any(|(k, _)| *k == "enter"), "a fact must not offer enter: {v:?}");

        // Only this group can be re-read, because only this group has facts
        // another machine can change while you are looking at them.
        assert!(v.contains(&("r", "re-read")), "{v:?}");
        let appearance = &grps[0];
        assert!(
            !verbs(appearance, &state()).iter().any(|(k, _)| *k == "r"),
            "a key that does nothing here would read as broken"
        );
    }

    /// Every machine is one block, and the way to add another is the last row.
    #[test]
    fn the_section_lists_every_machine_and_ends_with_the_way_to_add_one() {
        let grps = groups(&state(), &View::default());
        let machines = grps.iter().find(|g| g.id == GroupId::Machines).expect("MACHINES");

        assert_eq!(machines.rows[0].id, RowId::AutoAttach, "the setting comes first");
        let heads: Vec<&str> = machines
            .rows
            .iter()
            .filter(|r| !r.indent && r.id == RowId::Fact)
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(heads, vec!["local", "gpu-box", "pi-farm"]);
        assert_eq!(machines.rows.last().map(|r| r.id), Some(RowId::MachineAdd));

        // Every row of a machine's block knows which machine it is about, by
        // the badge rather than by where it sits.
        for row in machines.rows.iter().filter(|r| r.indent) {
            assert!(row.machine.is_some(), "{} is in a block and names no machine", row.label);
        }
        assert!(machines.rows[0].machine.is_none(), "auto-attach is not about one machine");
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
