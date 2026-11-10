//! The Keys topic of HELP, drawn as a keyboard.
//!
//! A table of keys answers "what does `x` do" and nothing else: you have to
//! already know which key to look up. Drawn on the keyboard you are holding,
//! the same bindings answer the other question too — "what is on the keys
//! near my hand, and which ones are free" — and a layer you switch to shows a
//! whole modifier at once.
//!
//! **Nothing here names a key.** Every keycap is read from a table the client
//! dispatches from: the prefix layer from the live [`Keymap`] (so `[keys]`
//! overrides show), the Alt layer from what the Alt dispatcher actually does
//! with each key (see `alt_layer` in the workbench), BOOTH from
//! [`super::BOOTH_KEYS`], and the rails, the diff and the GIT page from their
//! [`Verb`] tables. What *is* written here is how an action is said on a
//! keycap five columns wide, once per action rather than once per key — so a
//! key that moves takes its label with it, and a renamed table cannot leave
//! the keyboard describing the old one.

use std::collections::BTreeMap;

use butai_protocol::{Command, KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::style::Color;

use super::{ellipsize, put_str, Focus, LRect, Page, Pen, Theme, BOOTH_KEYS};
use crate::keymap::{Action, Keymap, ViewVerb};
use crate::verbs::{self, DiffMode, Verb, VerbId};

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

/// Which side of the board a modifier is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    L,
    R,
}

/// A physical key. A shifted character is its unshifted key plus shift — `J`
/// is [`Cap::Char`]`('j')` held with shift — because that is the key you press.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cap {
    Char(char),
    Esc,
    Tab,
    Caps,
    Enter,
    Bksp,
    Shift(Side),
    Ctrl(Side),
    Alt(Side),
    Super(Side),
    Space,
    F(u8),
    Ins,
    Del,
    Home,
    End,
    PgUp,
    PgDn,
    Up,
    Down,
    Left,
    Right,
}

/// US layout: each unshifted symbol, and the character shift turns it into.
const PLAIN: &str = "`1234567890-=[]\\;',./";
const SHIFTED: &str = "~!@#$%^&*()_+{}|:\"<>?";

/// The key a character is typed on, and whether shift is held for it.
pub fn cap_of_char(c: char) -> Option<(Cap, bool)> {
    match c {
        ' ' => Some((Cap::Space, false)),
        '\n' => Some((Cap::Enter, false)),
        '\t' => Some((Cap::Tab, false)),
        'a'..='z' | '0'..='9' => Some((Cap::Char(c), false)),
        'A'..='Z' => Some((Cap::Char(c.to_ascii_lowercase()), true)),
        _ if PLAIN.contains(c) => Some((Cap::Char(c), false)),
        _ => {
            let i = SHIFTED.chars().position(|s| s == c)?;
            Some((Cap::Char(PLAIN.chars().nth(i)?), true))
        }
    }
}

/// The key a protocol key code is pressed on.
pub fn cap_of(code: &KeyCode) -> Option<(Cap, bool)> {
    Some(match code {
        KeyCode::Char(c) => return cap_of_char(*c),
        KeyCode::Enter => (Cap::Enter, false),
        KeyCode::Esc => (Cap::Esc, false),
        KeyCode::Backspace => (Cap::Bksp, false),
        KeyCode::Tab => (Cap::Tab, false),
        KeyCode::BackTab => (Cap::Tab, true),
        KeyCode::Left => (Cap::Left, false),
        KeyCode::Right => (Cap::Right, false),
        KeyCode::Up => (Cap::Up, false),
        KeyCode::Down => (Cap::Down, false),
        KeyCode::Home => (Cap::Home, false),
        KeyCode::End => (Cap::End, false),
        KeyCode::PageUp => (Cap::PgUp, false),
        KeyCode::PageDown => (Cap::PgDn, false),
        KeyCode::Delete => (Cap::Del, false),
        KeyCode::Insert => (Cap::Ins, false),
        KeyCode::F(n) => (Cap::F(*n), false),
    })
}

/// A key as the reference writes it: `j`, `pgdn`, `enter`.
fn cap_name(cap: Cap) -> String {
    match cap {
        Cap::Char(c) => c.to_string(),
        Cap::Esc => "esc".into(),
        Cap::Tab => "tab".into(),
        Cap::Caps => "caps".into(),
        Cap::Enter => "enter".into(),
        Cap::Bksp => "bksp".into(),
        Cap::Shift(_) => "shift".into(),
        Cap::Ctrl(_) => "ctrl".into(),
        Cap::Alt(_) => "alt".into(),
        Cap::Super(_) => "super".into(),
        Cap::Space => "space".into(),
        Cap::F(n) => format!("F{n}"),
        Cap::Ins => "ins".into(),
        Cap::Del => "del".into(),
        Cap::Home => "home".into(),
        Cap::End => "end".into(),
        Cap::PgUp => "pgup".into(),
        Cap::PgDn => "pgdn".into(),
        Cap::Up => "up".into(),
        Cap::Down => "down".into(),
        Cap::Left => "left".into(),
        Cap::Right => "right".into(),
    }
}

/// What a key is called with shift held: `J`, `?`, `S-tab`.
fn shifted_name(cap: Cap) -> String {
    match cap {
        Cap::Char(c) if c.is_ascii_alphabetic() => c.to_ascii_uppercase().to_string(),
        Cap::Char(c) => PLAIN
            .chars()
            .position(|p| p == c)
            .and_then(|i| SHIFTED.chars().nth(i))
            .map_or_else(|| format!("S-{c}"), |s| s.to_string()),
        other => format!("S-{}", cap_name(other)),
    }
}

/// A key as it is printed on the cap.
fn cap_glyph(cap: Cap) -> String {
    match cap {
        Cap::Up => "↑".into(),
        Cap::Down => "↓".into(),
        Cap::Left => "←".into(),
        Cap::Right => "→".into(),
        other => cap_name(other),
    }
}

// ---------------------------------------------------------------------------
// What a key does
// ---------------------------------------------------------------------------

/// What kind of thing a binding does, which is the colour of its keycap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cat {
    Move,
    Agent,
    Git,
    Space,
    Tool,
    Danger,
}

impl Cat {
    pub const ALL: [Cat; 6] = [Cat::Move, Cat::Agent, Cat::Git, Cat::Space, Cat::Tool, Cat::Danger];

    pub fn color(self, theme: &Theme) -> Color {
        match self {
            Cat::Move => theme.info,
            Cat::Agent => theme.ok,
            Cat::Git => theme.attention,
            Cat::Space => theme.accent,
            Cat::Tool => theme.muted,
            Cat::Danger => theme.danger,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Cat::Move => "views & moving",
            Cat::Agent => "agents & panes",
            Cat::Git => "git",
            Cat::Space => "workspaces & machines",
            Cat::Tool => "tools",
            Cat::Danger => "closes or discards",
        }
    }
}

/// What the Alt dispatcher was seen to do with a key, for the parts of it that
/// are not a [`ViewVerb`]. Filled in by the workbench, which owns the
/// dispatcher; described here, which owns the keycaps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Does {
    View(ViewVerb),
    Detach,
    Zen,
    Page(Page),
    Settings,
    PasteImage,
    PickAgent,
    /// Bound, but to something this module has no words for. A test keeps
    /// every shipped binding out of here.
    Unknown,
}

/// One key's binding on one layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bind {
    /// What the keycap says. Short, because the cap is.
    pub label: String,
    /// What the list under the keyboard says, which has room for more.
    pub long: String,
    pub cat: Cat,
}

impl Bind {
    fn new(label: impl Into<String>, cat: Cat) -> Self {
        let label = label.into();
        Self { long: label.clone(), label, cat }
    }
}

fn page_bind(p: Page) -> Bind {
    match p {
        Page::Booth => Bind::new("BOOTH", Cat::Space),
        Page::Agents => Bind::new("work", Cat::Move),
        other => Bind::new(other.label(), Cat::Move),
    }
}

/// How a view verb is said on a keycap.
pub fn view_bind(v: ViewVerb) -> Bind {
    match v {
        ViewVerb::Space(p) => page_bind(p),
        ViewVerb::SpaceNext => Bind::new("next view", Cat::Move),
        ViewVerb::SpacePrev => Bind::new("prev view", Cat::Move),
        ViewVerb::Spaces => Bind::new("views", Cat::Move),
        ViewVerb::Focus(Focus::Agents) => Bind::new("agents", Cat::Agent),
        ViewVerb::Focus(Focus::Processes) => Bind::new("procs", Cat::Agent),
        ViewVerb::Focus(Focus::Changes) => Bind::new("changes", Cat::Git),
        ViewVerb::Focus(Focus::AllAgents) => Bind::new("fleet", Cat::Space),
        ViewVerb::Focus(Focus::Stage) => Bind::new("stage", Cat::Agent),
        ViewVerb::Focus(_) => Bind::new("focus", Cat::Move),
        ViewVerb::Tab(n) => Bind::new(format!("tab {n}"), Cat::Space),
        ViewVerb::TabNext => Bind::new("next tab", Cat::Space),
        ViewVerb::TabPrev => Bind::new("prev tab", Cat::Space),
        ViewVerb::NewWorkspace => Bind::new("open ws", Cat::Space),
        ViewVerb::CloseWorkspace => Bind::new("close ws", Cat::Danger),
        ViewVerb::Host => Bind::new("machines", Cat::Space),
        ViewVerb::Layout => Bind::new("layout", Cat::Tool),
        ViewVerb::NewTerminal => Bind::new("shell", Cat::Agent),
        ViewVerb::Monitor { gpu: false } => Bind::new("htop", Cat::Tool),
        ViewVerb::Monitor { gpu: true } => Bind::new("gpu mon", Cat::Tool),
        ViewVerb::Search => Bind::new("find", Cat::Tool),
        ViewVerb::Links => Bind::new("links", Cat::Tool),
        ViewVerb::Branch => Bind::new("branches", Cat::Git),
        ViewVerb::Update => Bind::new("update", Cat::Tool),
        ViewVerb::Help => Bind::new("help", Cat::Tool),
        ViewVerb::Prompts => Bind::new("prompts", Cat::Tool),
    }
}

fn command_bind(c: &Command) -> Bind {
    match c {
        Command::ClosePane => Bind::new("kill pane", Cat::Danger),
        Command::ZoomToggle => Bind::new("zen", Cat::Tool),
        Command::ScrollPage(n) if *n < 0 => Bind::new("scroll up", Cat::Agent),
        Command::ScrollPage(_) => Bind::new("scroll dn", Cat::Agent),
        Command::GitMenu => Bind::new("git menu", Cat::Git),
        Command::PasteImage => Bind::new("paste img", Cat::Tool),
        Command::ListAgents => Bind::new("agent", Cat::Agent),
        Command::SpawnAgent(name) => Bind::new(name.clone(), Cat::Agent),
        Command::NewProcess { name, .. } => Bind::new(name.clone(), Cat::Agent),
        Command::ReloadConfig => Bind::new("reload", Cat::Tool),
        Command::KillServer | Command::KillServerClear => Bind::new("kill srv", Cat::Danger),
        Command::ToggleAllAgents => Bind::new("fleet", Cat::Space),
        Command::SetTheme(_) | Command::ListThemes => Bind::new("theme", Cat::Tool),
        _ => Bind::new("command", Cat::Tool),
    }
}

/// How a prefix-table action is said on a keycap.
pub fn action_bind(a: &Action) -> Bind {
    match a {
        Action::View(v) => view_bind(*v),
        Action::Command(c) => command_bind(c),
        Action::Detach => Bind::new("detach", Cat::Danger),
        Action::CommandPrompt => Bind::new(":command", Cat::Tool),
    }
}

/// How something the Alt dispatcher does is said on a keycap.
pub fn does_bind(d: &Does) -> Bind {
    match d {
        Does::View(v) => view_bind(*v),
        Does::Detach => Bind::new("detach", Cat::Danger),
        Does::Zen => Bind::new("zen", Cat::Tool),
        Does::Page(p) => page_bind(*p),
        Does::Settings => Bind::new("settings", Cat::Tool),
        Does::PasteImage => Bind::new("paste img", Cat::Tool),
        Does::PickAgent => Bind::new("pick agent", Cat::Agent),
        Does::Unknown => Bind::new("?", Cat::Tool),
    }
}

/// A surface verb's colour. Its label is the table's own: every [`Verb`] label
/// is already written for a footer slot, which is the same budget a keycap has.
fn verb_cat(v: &Verb, surface: Cat) -> Cat {
    if v.danger {
        return Cat::Danger;
    }
    match v.id {
        VerbId::Down | VerbId::Up | VerbId::First | VerbId::Last => Cat::Move,
        VerbId::Help | VerbId::Menu | VerbId::Refresh => Cat::Tool,
        VerbId::GoToChanges | VerbId::GitMenu => Cat::Git,
        _ => surface,
    }
}

// ---------------------------------------------------------------------------
// Layers
// ---------------------------------------------------------------------------

/// One way of reaching keys: a modifier held, the prefix pressed first, or the
/// cursor on a surface.
#[derive(Debug, Clone)]
pub struct Layer {
    /// The layer's tab.
    pub name: String,
    /// How you get keys to it, written before the list: `alt-` or `C-b `.
    pub lead: String,
    /// The line under the tabs.
    pub hint: String,
    /// Keys drawn as held down: the modifier, or the prefix.
    pub held: Vec<Cap>,
    pub keys: BTreeMap<(Cap, bool), Bind>,
}

impl Layer {
    fn new(name: &str, lead: &str, hint: &str, held: Vec<Cap>) -> Self {
        Self {
            name: name.into(),
            lead: lead.into(),
            hint: hint.into(),
            held,
            keys: BTreeMap::new(),
        }
    }

    /// The first binding for a key wins, so a table listed in its own priority
    /// order keeps it.
    fn bind(&mut self, key: (Cap, bool), b: Bind) {
        self.keys.entry(key).or_insert(b);
    }

    fn verbs(mut self, verbs: impl IntoIterator<Item = Verb>, surface: Cat) -> Self {
        for v in verbs {
            if let Some(key) = cap_of_char(v.key) {
                self.bind(key, Bind::new(v.label, verb_cat(&v, surface)));
            }
        }
        self
    }

    /// The name a binding is listed under: `alt-o`, `C-b X`, `pgdn`.
    pub fn key_text(&self, (cap, shift): (Cap, bool)) -> String {
        let k = if shift { shifted_name(cap) } else { cap_name(cap) };
        format!("{}{k}", self.lead)
    }
}

/// Every layer the keyboard can show.
#[derive(Debug, Clone)]
pub struct Keys {
    pub layers: Vec<Layer>,
}

impl Keys {
    /// Build every layer from the tables the client dispatches from.
    ///
    /// `alt` is the Alt dispatcher's answer for each key it takes, which only
    /// the workbench can ask (see `alt_layer` there). Empty, the layer is left
    /// out rather than drawn bare.
    pub fn build(keymap: &Keymap, alt: Vec<((Cap, bool), Does)>) -> Self {
        let mut layers = Vec::new();

        let mut a = Layer::new(
            "alt",
            "alt-",
            "hold alt with the key · works everywhere, even with the keyboard on a pane",
            vec![Cap::Alt(Side::L), Cap::Alt(Side::R)],
        );
        for (key, d) in alt {
            a.bind(key, does_bind(&d));
        }
        layers.push(a);

        let prefix = crate::keys::key_label(&keymap.prefix);
        let mut held = Vec::new();
        if keymap.prefix.mods.ctrl {
            held.push(Cap::Ctrl(Side::L));
        }
        if keymap.prefix.mods.alt {
            held.push(Cap::Alt(Side::L));
        }
        held.extend(cap_of(&keymap.prefix.code).map(|(c, _)| c));
        let mut p = Layer::new(
            "prefix",
            &format!("{prefix} "),
            &format!(
                "press {prefix}, let go, then the key · set in [general] prefix, bound in [keys]"
            ),
            held,
        );
        for (ev, action) in keymap.bindings() {
            if let Some(key) = plain_key(ev) {
                p.bind(key, action_bind(action));
            }
        }
        layers.push(p);

        layers.push(card_layer());
        layers.push(booth_layer());

        let agents = verbs::agents_verbs(true).iter().chain(verbs::agents_verbs(false)).copied();
        layers.push(
            Layer::new(
                "agents",
                "",
                "the cursor on the AGENTS rail · on the stage, keys go to the pane",
                vec![],
            )
            .verbs(agents, Cat::Agent),
        );
        layers.push(
            Layer::new("processes", "", "the cursor on the PROCESSES rail", vec![])
                .verbs(verbs::procs_verbs().iter().copied(), Cat::Agent),
        );
        layers.push(
            Layer::new(
                "changes",
                "",
                "the cursor on the CHANGES rail · what a key does depends on the row",
                vec![],
            )
            .verbs(verbs::changes_help_verbs(), Cat::Git),
        );
        let diff = verbs::diff_verbs(DiffMode::Read, false, true)
            .into_iter()
            .chain(verbs::diff_verbs(DiffMode::Read, true, true))
            .chain(verbs::diff_verbs(DiffMode::Lines, false, true));
        layers.push(
            Layer::new(
                "diff",
                "",
                "a diff open · v picks lines, and then j k space enter work on them",
                vec![],
            )
            .verbs(diff, Cat::Git),
        );
        layers.push(
            Layer::new("git", "", "the GIT page · keys act on the row under the cursor", vec![])
                .verbs(verbs::git_help_verbs(), Cat::Git),
        );

        layers.retain(|l| !l.keys.is_empty());
        Self { layers }
    }
}

/// A prefix-table key as a key on the board, when it is one: a binding that
/// itself needs ctrl or alt is a chord no single cap can show.
fn plain_key(ev: &KeyEvent) -> Option<(Cap, bool)> {
    if ev.mods.ctrl || ev.mods.alt {
        return None;
    }
    let (cap, shift) = cap_of(&ev.code)?;
    Some((cap, shift || ev.mods.shift))
}

/// The workbench with the keyboard on a rail, from the `?` card's table.
///
/// A row pairs its keys with its meanings in order — `a A` with `new agent ·
/// choose which` — so each key takes its own half. A sequence (`gg`, `gt`), a
/// chord (`C-\ C-n`) and an Alt key cannot be one cap on this board: the Alt
/// ones are the Alt layer's, and the rest are in the card itself.
fn card_layer() -> Layer {
    let mut l = Layer::new(
        "workbench",
        "",
        "the keyboard on a rail, vim style · i types into the stage, C-\\ C-n comes back",
        vec![],
    );
    for (head, rows) in crate::keycard::KEY_CARD {
        let group = match head {
            "MOVE" => Cat::Move,
            "ROWS" => Cat::Agent,
            "WORKSPACES" => Cat::Space,
            _ => Cat::Tool,
        };
        for (keys, what) in rows {
            let keys: Vec<&str> = keys.split_whitespace().collect();
            let parts: Vec<&str> = what.split('·').map(str::trim).collect();
            for (i, k) in keys.iter().enumerate() {
                let Some(key) = named_key(k) else { continue };
                let long = if parts.len() == keys.len() { parts[i] } else { what };
                let long = long.split(" (").next().unwrap_or(long);
                let short =
                    ["the ", "a ", "an "].iter().find_map(|a| long.strip_prefix(a)).unwrap_or(long);
                let cat = if long.starts_with("close") { Cat::Danger } else { group };
                l.bind(key, Bind { label: short.to_string(), long: long.to_string(), cat });
            }
        }
    }
    l
}

/// A key as the cards write it — `j`, `G`, `pgdn`, `enter` — when it is one
/// key and not a sequence, a chord or an Alt key.
fn named_key(k: &str) -> Option<(Cap, bool)> {
    match k {
        "pgdn" => Some((Cap::PgDn, false)),
        "pgup" => Some((Cap::PgUp, false)),
        "home" => Some((Cap::Home, false)),
        "end" => Some((Cap::End, false)),
        "enter" => Some((Cap::Enter, false)),
        "tab" => Some((Cap::Tab, false)),
        "esc" => Some((Cap::Esc, false)),
        "space" => Some((Cap::Space, false)),
        _ if k.chars().count() == 1 => k.chars().next().and_then(cap_of_char),
        _ => None,
    }
}

/// BOOTH's fleet, from the key card's own table.
///
/// The card writes several keys to a row (`j k`) and sends the Alt keys it
/// mentions to their own layer. A row's description is read up to its first
/// `:` or `·` — `a page down · pgup, a page up` is the `pgdn` row's own words —
/// and a leading article comes off, which is all a keycap has room for.
fn booth_layer() -> Layer {
    let mut l = Layer::new(
        "booth",
        "",
        "the cursor on BOOTH's FLEET · tab hands keys to the preview",
        vec![],
    );
    for (keys, desc) in BOOTH_KEYS {
        let long = desc.split([':', '·']).next().unwrap_or(desc).trim();
        let short = ["the ", "a ", "an "].iter().find_map(|a| long.strip_prefix(a)).unwrap_or(long);
        let cat = if long.starts_with("end") { Cat::Danger } else { Cat::Space };
        for k in keys.split_whitespace() {
            if let Some(key) = named_key(k) {
                l.bind(key, Bind { label: short.to_string(), long: desc.to_string(), cat });
            }
        }
    }
    l
}

// ---------------------------------------------------------------------------
// The board
// ---------------------------------------------------------------------------

/// One row of keys: each key and its width in quarter units. `None` is a gap.
type Row = &'static [(Option<Cap>, u16)];

const fn k(c: char) -> (Option<Cap>, u16) {
    (Some(Cap::Char(c)), 4)
}

/// The main block, 15 units (60 quarters) wide, an ANSI layout.
const MAIN: [Row; 6] = [
    &[
        (Some(Cap::Esc), 4),
        (None, 4),
        (Some(Cap::F(1)), 4),
        (Some(Cap::F(2)), 4),
        (Some(Cap::F(3)), 4),
        (Some(Cap::F(4)), 4),
        (None, 2),
        (Some(Cap::F(5)), 4),
        (Some(Cap::F(6)), 4),
        (Some(Cap::F(7)), 4),
        (Some(Cap::F(8)), 4),
        (None, 2),
        (Some(Cap::F(9)), 4),
        (Some(Cap::F(10)), 4),
        (Some(Cap::F(11)), 4),
        (Some(Cap::F(12)), 4),
    ],
    &[
        k('`'),
        k('1'),
        k('2'),
        k('3'),
        k('4'),
        k('5'),
        k('6'),
        k('7'),
        k('8'),
        k('9'),
        k('0'),
        k('-'),
        k('='),
        (Some(Cap::Bksp), 8),
    ],
    &[
        (Some(Cap::Tab), 6),
        k('q'),
        k('w'),
        k('e'),
        k('r'),
        k('t'),
        k('y'),
        k('u'),
        k('i'),
        k('o'),
        k('p'),
        k('['),
        k(']'),
        (Some(Cap::Char('\\')), 6),
    ],
    &[
        (Some(Cap::Caps), 7),
        k('a'),
        k('s'),
        k('d'),
        k('f'),
        k('g'),
        k('h'),
        k('j'),
        k('k'),
        k('l'),
        k(';'),
        k('\''),
        (Some(Cap::Enter), 9),
    ],
    &[
        (Some(Cap::Shift(Side::L)), 9),
        k('z'),
        k('x'),
        k('c'),
        k('v'),
        k('b'),
        k('n'),
        k('m'),
        k(','),
        k('.'),
        k('/'),
        (Some(Cap::Shift(Side::R)), 11),
    ],
    &[
        (Some(Cap::Ctrl(Side::L)), 6),
        (Some(Cap::Super(Side::L)), 5),
        (Some(Cap::Alt(Side::L)), 5),
        (Some(Cap::Space), 26),
        (Some(Cap::Alt(Side::R)), 5),
        (Some(Cap::Super(Side::R)), 5),
        (Some(Cap::Ctrl(Side::R)), 8),
    ],
];

/// The navigation block to the right of it, 3 units wide.
const NAV: [Row; 6] = [
    &[],
    &[(Some(Cap::Ins), 4), (Some(Cap::Home), 4), (Some(Cap::PgUp), 4)],
    &[(Some(Cap::Del), 4), (Some(Cap::End), 4), (Some(Cap::PgDn), 4)],
    &[],
    &[(None, 4), (Some(Cap::Up), 4)],
    &[(Some(Cap::Left), 4), (Some(Cap::Down), 4), (Some(Cap::Right), 4)],
];

const MAIN_Q: u16 = 60;
/// The main block, a half-unit gap, and the navigation block.
const WITH_NAV_Q: u16 = MAIN_Q + 2 + 12;
/// Rows each key takes: its glyph, then two of legend.
const KEY_H: u16 = 3;
/// Below this many columns to a key unit, the legend is too short to read and
/// the keyboard is left out; the list under it still has every binding.
const MIN_UNIT: u16 = 5;
/// Above this the keys only get emptier.
const MAX_UNIT: u16 = 11;
/// Width of one entry in the list under the board.
const LIST_COL: u16 = 26;

/// How many quarters wide the board is drawn in `width` columns, or `None`
/// when it does not fit at all.
fn board_quarters(width: u16) -> Option<u16> {
    [WITH_NAV_Q, MAIN_Q].into_iter().find(|q| width * 4 / q >= MIN_UNIT)
}

fn board_width(width: u16, quarters: u16) -> u16 {
    width.min(quarters * MAX_UNIT / 4)
}

/// The keys of one layer, as list entries ordered by colour and then by key.
fn entries(layer: &Layer) -> Vec<((Cap, bool), &Bind)> {
    let mut v: Vec<_> = layer.keys.iter().map(|(k, b)| (*k, b)).collect();
    v.sort_by_key(|(k, b)| (b.cat, *k));
    v
}

/// The rows the list takes in `width` columns.
fn list_rows(layer: &Layer, width: u16) -> u16 {
    let cols = (width / LIST_COL).max(1);
    (layer.keys.len() as u16).div_ceil(cols)
}

/// How many rows the whole block takes: the tabs, the hint, the board, the
/// colour legend and the list, and a blank row before the prose after it.
pub fn height(keys: &Keys, layer: usize, width: u16) -> u16 {
    let Some(l) = keys.layers.get(layer) else { return 0 };
    let board = if board_quarters(width).is_some() { 6 * KEY_H + 1 } else { 0 };
    // tabs, hint, blank · board, blank · legend, blank · list · blank
    3 + board + 2 + list_rows(l, width) + 1
}

/// Where each layer's tab sits on the first row, as `[start, end)` columns
/// from the block's left edge. Drawn and hit-tested from this one list.
pub fn tab_spans(keys: &Keys) -> Vec<(u16, u16)> {
    let mut x = 0;
    keys.layers
        .iter()
        .map(|l| {
            let w = tab_text(l).chars().count() as u16;
            let span = (x, x + w);
            x += w + 1;
            span
        })
        .collect()
}

fn tab_text(l: &Layer) -> String {
    format!(" {} {} ", l.name, l.keys.len())
}

/// Draw the block for `layer` with its top-left corner at `area`'s.
pub fn draw(buf: &mut Buffer, area: LRect, keys: &Keys, layer: usize, theme: &Theme) {
    let Some(l) = keys.layers.get(layer) else { return };
    let bound = area.x + area.width;
    let ground = theme.ground;
    let mut y = area.y;

    for ((start, _), (i, lay)) in tab_spans(keys).into_iter().zip(keys.layers.iter().enumerate()) {
        let on = i == layer;
        let pen = if on {
            Pen { fg: ground, bg: theme.accent, bold: true }
        } else {
            Pen::new(theme.muted, theme.surface)
        };
        put_str(buf, area.x + start, y, &tab_text(lay), bound, pen);
    }
    y += 1;
    put_str(buf, area.x, y, &l.hint, bound, Pen::new(theme.faint, ground));
    y += 2;

    if let Some(q) = board_quarters(area.width) {
        let w = board_width(area.width, q);
        draw_board(buf, area.x, y, w, q, l, theme);
        y += 6 * KEY_H + 1;
    }

    // The colours this layer uses, and what they mean.
    let mut x = area.x;
    for cat in Cat::ALL.into_iter().filter(|c| l.keys.values().any(|b| b.cat == *c)) {
        put_str(buf, x, y, "▎", bound, Pen::new(cat.color(theme), ground));
        put_str(buf, x + 1, y, cat.name(), bound, Pen::new(theme.muted, ground));
        x += cat.name().chars().count() as u16 + 3;
    }
    y += 2;

    let cols = (area.width / LIST_COL).max(1);
    let rows = list_rows(l, area.width);
    for (n, (key, b)) in entries(l).into_iter().enumerate() {
        let (col, row) = (n as u16 / rows.max(1), n as u16 % rows.max(1));
        if col >= cols {
            break;
        }
        let ex = area.x + col * LIST_COL;
        let ey = y + row;
        put_str(buf, ex, ey, "▎", bound, Pen::new(b.cat.color(theme), ground));
        let name = l.key_text(key);
        put_str(buf, ex + 1, ey, &name, bound, Pen::new(theme.accent, ground));
        let lx = ex + 1 + (name.chars().count() as u16).max(7) + 1;
        let room = (ex + LIST_COL).saturating_sub(lx + 1) as usize;
        put_str(buf, lx, ey, &ellipsize(&b.long, room), bound, Pen::new(theme.ink, ground));
    }
}

fn draw_board(
    buf: &mut Buffer,
    x0: u16,
    y0: u16,
    width: u16,
    quarters: u16,
    l: &Layer,
    theme: &Theme,
) {
    let at = |q: u16| x0 + q * width / quarters;
    for (r, row) in MAIN.iter().enumerate() {
        let y = y0 + r as u16 * KEY_H;
        let mut q = 0;
        for (cap, w) in row.iter() {
            if let Some(cap) = cap {
                draw_key(buf, at(q), y, at(q + w) - at(q), *cap, l, theme);
            }
            q += w;
        }
    }
    if quarters == WITH_NAV_Q {
        for (r, row) in NAV.iter().enumerate() {
            let y = y0 + r as u16 * KEY_H;
            let mut q = MAIN_Q + 2;
            for (cap, w) in row.iter() {
                if let Some(cap) = cap {
                    draw_key(buf, at(q), y, at(q + w) - at(q), *cap, l, theme);
                }
                q += w;
            }
        }
    }
}

/// One keycap, `span` columns wide including the column of gap after it.
///
/// Its top row is the glyph on a lighter ground than the legend under it, so
/// stacked keys read as separate caps without a blank row between them; down
/// its left edge, the colour of what it does.
fn draw_key(buf: &mut Buffer, x: u16, y: u16, span: u16, cap: Cap, l: &Layer, theme: &Theme) {
    let w = span.saturating_sub(1);
    if w < 2 {
        return;
    }
    let base = l.keys.get(&(cap, false));
    let shifted = l.keys.get(&(cap, true));
    let held = l.held.contains(&cap);
    let bound = x + w;
    let (top, body) = if held {
        (theme.accent, theme.accent)
    } else if base.is_some() || shifted.is_some() {
        (theme.selection, theme.surface)
    } else {
        (theme.surface, theme.surface)
    };
    for dy in 0..KEY_H {
        let bg = if dy == 0 { top } else { body };
        put_str(buf, x, y + dy, &" ".repeat(w as usize), bound, Pen::new(theme.ink, bg));
    }
    let stripe = base.or(shifted).map(|b| b.cat.color(theme));
    if let Some(c) = stripe.filter(|_| !held) {
        for dy in 0..KEY_H {
            let bg = if dy == 0 { top } else { body };
            put_str(buf, x, y + dy, "▎", bound, Pen::new(c, bg));
        }
    }

    let room = (w - 1) as usize;
    let glyph_fg = if held {
        theme.ground
    } else if base.is_some() || shifted.is_some() {
        theme.ink
    } else {
        theme.faint
    };
    put_str(
        buf,
        x + 1,
        y,
        &ellipsize(&cap_glyph(cap), room),
        bound,
        Pen { fg: glyph_fg, bg: top, bold: true },
    );
    if let Cap::Char(c) = cap {
        if !c.is_ascii_alphanumeric() || c.is_ascii_digit() {
            let s = shifted_name(cap);
            if s.chars().count() == 1 && room >= 3 {
                put_str(
                    buf,
                    x + w - 1,
                    y,
                    &s,
                    bound,
                    Pen::new(if held { theme.ground } else { theme.faint }, top),
                );
            }
        }
    }

    let ink = if held { theme.ground } else { theme.ink };
    match (shifted, base) {
        (Some(s), b) => {
            let line = format!("{} {}", shifted_name(cap), s.label);
            let fg = if held { theme.ground } else { s.cat.color(theme) };
            put_str(buf, x + 1, y + 1, &ellipsize(&line, room), bound, Pen::new(fg, body));
            if let Some(b) = b {
                put_str(buf, x + 1, y + 2, &ellipsize(&b.label, room), bound, Pen::new(ink, body));
            }
        }
        (None, Some(b)) => {
            for (i, line) in two_lines(&b.label, room).iter().enumerate() {
                put_str(buf, x + 1, y + 1 + i as u16, line, bound, Pen::new(ink, body));
            }
        }
        (None, None) => {}
    }
}

/// A label broken on spaces into at most two lines of `width`, the second
/// cut short if it has to be.
fn two_lines(label: &str, width: usize) -> Vec<String> {
    if label.chars().count() <= width {
        return vec![label.to_string()];
    }
    let mut first = String::new();
    let mut words = label.split(' ').peekable();
    while let Some(w) = words.peek() {
        let need = first.chars().count() + usize::from(!first.is_empty()) + w.chars().count();
        if need > width {
            break;
        }
        if !first.is_empty() {
            first.push(' ');
        }
        first.push_str(w);
        words.next();
    }
    if first.is_empty() {
        return vec![ellipsize(label, width)];
    }
    let rest: Vec<&str> = words.collect();
    vec![first, ellipsize(&rest.join(" "), width)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn keys() -> Keys {
        Keys::build(&Keymap::default(), Vec::new())
    }

    fn layer<'a>(k: &'a Keys, name: &str) -> &'a Layer {
        k.layers.iter().find(|l| l.name == name).expect(name)
    }

    /// Every row of the main block is the same 15 units wide, or the right
    /// edge is ragged and the navigation block lands on top of a key.
    #[test]
    fn every_row_is_fifteen_units() {
        for (i, row) in MAIN.iter().enumerate() {
            let q: u16 = row.iter().map(|(_, w)| w).sum();
            assert!(q <= MAIN_Q, "row {i} is {q} quarters");
            if i > 0 {
                assert_eq!(q, MAIN_Q, "row {i}");
            }
        }
    }

    /// The prefix layer is the keymap: every shipped binding is on a key, in
    /// words this module has, and nothing else is.
    #[test]
    fn the_prefix_layer_is_the_keymap() {
        let km = Keymap::default();
        let k = Keys::build(&km, Vec::new());
        let p = layer(&k, "prefix");
        let shown = km.bindings().filter(|(ev, _)| plain_key(ev).is_some()).count();
        assert_eq!(p.keys.len(), shown, "one cap per binding");
        for (ev, action) in km.bindings() {
            assert_ne!(action_bind(action).label, "command", "{ev:?} → {action:?} has no words");
        }
        assert_eq!(p.keys[&(Cap::Char('x'), true)].label, "close ws", "X is shift-x");
        assert_eq!(p.keys[&(Cap::Char('/'), true)].label, "help", "? is shift-/");
        assert!(p.held.contains(&Cap::Ctrl(Side::L)) && p.held.contains(&Cap::Char('b')));
    }

    /// A `[keys]` override is on the board, because the board reads the keymap
    /// the client is using rather than the shipped table.
    #[test]
    fn a_key_you_bound_yourself_is_on_the_board() {
        let ov = std::collections::HashMap::from([(
            "F5".to_string(),
            "process build cargo build".to_string(),
        )]);
        let (km, warn) = Keymap::from_config("C-a", &ov);
        assert!(warn.is_empty(), "{warn:?}");
        let k = Keys::build(&km, Vec::new());
        let p = layer(&k, "prefix");
        assert_eq!(p.keys[&(Cap::F(5), false)].label, "build");
        assert_eq!(p.lead, "^A ", "named the way the rest of the client names it");
        assert!(p.held.contains(&Cap::Char('a')));
    }

    /// The surface layers are their verb tables, key for key.
    #[test]
    fn the_surface_layers_are_their_verb_tables() {
        let k = keys();
        let c = layer(&k, "changes");
        for v in verbs::changes_help_verbs() {
            let key = cap_of_char(v.key).expect("a key on the board");
            assert!(c.keys.contains_key(&key), "{} is missing", v.key);
        }
        assert_eq!(c.keys[&(Cap::Char('x'), false)].cat, Cat::Danger, "discard is marked");
        let a = layer(&k, "agents");
        assert_eq!(a.keys[&(Cap::Char('f'), true)].label, "fork");
        assert!(a.keys.contains_key(&(Cap::Space, false)), "space folds");
    }

    /// The `?` card's rows land on their keys, each with its own half of the
    /// meaning, and its sequences and chords stay off the board.
    #[test]
    fn the_workbench_layer_reads_the_question_mark_card() {
        let k = keys();
        let w = layer(&k, "workbench");
        assert_eq!(w.keys[&(Cap::Char('a'), false)].label, "new agent");
        assert_eq!(w.keys[&(Cap::Char('a'), true)].label, "choose which");
        assert_eq!(w.keys[&(Cap::Char('g'), true)].label, "bottom", "G, without gg");
        assert_eq!(w.keys[&(Cap::Char('q'), false)].label, "detach");
        assert_eq!(w.keys[&(Cap::Char('x'), false)].cat, Cat::Danger);
        assert!(!w.keys.contains_key(&(Cap::Char('1'), false)), "alt-1..9 is the Alt layer's");
    }

    /// BOOTH's card rows each land on their keys, with the Alt ones left to
    /// the Alt layer.
    #[test]
    fn booth_reads_the_key_card() {
        let k = keys();
        let b = layer(&k, "booth");
        assert!(b.keys.contains_key(&(Cap::Char('j'), false)));
        assert!(b.keys.contains_key(&(Cap::Char('k'), false)), "`j k` is two keys");
        assert!(b.keys.contains_key(&(Cap::Char('z'), true)), "Z");
        assert!(b.keys.contains_key(&(Cap::PgDn, false)));
        assert!(!b.keys.contains_key(&(Cap::Char('w'), false)), "alt-w is not plain w");
        assert_eq!(b.keys[&(Cap::Char('x'), false)].cat, Cat::Danger);
    }

    #[test]
    fn shifted_characters_are_their_keys_with_shift() {
        assert_eq!(cap_of_char('J'), Some((Cap::Char('j'), true)));
        assert_eq!(cap_of_char('?'), Some((Cap::Char('/'), true)));
        assert_eq!(cap_of_char('<'), Some((Cap::Char(','), true)));
        assert_eq!(cap_of_char('['), Some((Cap::Char('['), false)));
        assert_eq!(cap_of_char(' '), Some((Cap::Space, false)));
        assert_eq!(shifted_name(Cap::Char('.')), ">");
    }

    fn rows(keys: &Keys, layer: usize, w: u16) -> Vec<String> {
        let h = height(keys, layer, w);
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        draw(&mut buf, LRect::new(0, 0, w, h), keys, layer, &Theme::default());
        (0..h)
            .map(|y| {
                (0..w)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
            })
            .collect()
    }

    /// Drawn wide, the board is there and each cap says what it does; drawn
    /// too narrow for one, the list still carries every binding.
    #[test]
    fn the_board_draws_and_degrades_to_the_list() {
        let k = keys();
        let p = k.layers.iter().position(|l| l.name == "prefix").unwrap();
        let wide = rows(&k, p, 110).join("\n");
        assert!(wide.contains("files"), "{wide}");
        assert!(wide.contains("pgup"), "the navigation block fits at 110");
        let narrow = rows(&k, p, 50);
        assert_eq!(narrow.len() as u16, height(&k, p, 50));
        assert!(!narrow.join("\n").contains("bksp"), "no board at 50 columns");
        assert!(narrow.join("\n").contains("^B o"), "but the list is there");
    }

    #[test]
    fn a_label_wraps_on_words_into_two_lines() {
        assert_eq!(two_lines("agent picker", 6), vec!["agent", "picker"]);
        assert_eq!(two_lines("files", 6), vec!["files"]);
        assert_eq!(two_lines("open a folder as a workspace", 6).len(), 2);
    }
}
