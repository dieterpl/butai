//! Key-string parsing ("C-b", "M-x", "f5") and the prefix-key binding table.
//!
//! Keybinding values use the same command mini-language as the command
//! palette, so `[keys]` in config and palette entries share one vocabulary.

use std::collections::HashMap;

use butai_protocol::{Command, Dir, KeyCode, KeyEvent, KeyMods, PaneKind, SplitDir};

use crate::chrome::{Focus, Page};

/// What a resolved keybinding does. `Detach` is client-connection-scoped and
/// therefore not a [`Command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Command(Command),
    Detach,
    /// Open the `:` command prompt (interactive clients only).
    CommandPrompt,
    /// Change what this client is looking at.
    View(ViewVerb),
}

/// A verb that moves this client's view — a space, a rail, a tab.
///
/// Deliberately not a [`Command`]. The daemon draws no screen, and refuses that
/// whole family with "menus, zoom and the agent panel are each client's own
/// view"; a view verb sent over the wire would move every viewer at once, which
/// is the behaviour the rails were built to end.
///
/// It exists as an `Action` so that the prefix table, `[keys]` and the `:`
/// prompt can *name* what the Alt layer does. Before this, they could not: the
/// shipped table still spoke of splits, window selection and layouts — a
/// free-pane vocabulary the workbench dropped — so 23 of its 33 default
/// bindings resolved to `NotHere` and did nothing but flash a message. The Alt
/// layer had the real verbs and no names for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewVerb {
    /// Show one space, or return to `work` if it is already showing — the
    /// toggle the Alt keys have, so the key that took you there brings you back.
    Space(Page),
    /// Walk the spaces in the order the menu lists them.
    SpaceNext,
    SpacePrev,
    /// Open the tab bar's spaces menu — every space at once, with its badge.
    Spaces,
    /// Put the cursor on a rail.
    Focus(Focus),
    /// A workspace tab by number, counting from 1 as the bar labels them.
    Tab(usize),
    /// Walk the tab bar, which spans every connected daemon.
    TabNext,
    TabPrev,
    /// Open a workspace, starting from the one you are in.
    NewWorkspace,
    /// Close this workspace, agents and all. Asks first.
    CloseWorkspace,
    /// The machines: add one, whose projects join the tab bar, or drop one
    /// that is already there.
    Host,
    /// Rail-resizing mode.
    Layout,
    /// A new shell in this workspace.
    NewTerminal,
    /// The machine's own monitor on the stage — `htop`, or a GPU monitor.
    ///
    /// What clicking a SYSTEM gauge does, and for a long time the *only* thing
    /// that did it: the gauges are drawn between the PROCESSES rail and the
    /// footer, they are not a list the cursor can reach, and so the one action
    /// they offer was reachable by pointer alone.
    Monitor {
        gpu: bool,
    },
    /// The search overlay.
    Search,
    /// The links on screen, as a list to choose from — the keyboard's way to
    /// follow one, and the only way at all in a terminal that does not speak
    /// OSC 8.
    Links,
    /// The branch picker.
    Branch,
    /// The reference.
    Help,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("unknown key {0:?}")]
    UnknownKey(String),
    #[error("unknown command {0:?}")]
    UnknownCommand(String),
    #[error("bad argument in {0:?}")]
    BadArgument(String),
}

/// Parse a key string: optional `C-` / `M-` / `S-` modifier prefixes followed
/// by a single character or a named key (`enter`, `space`, `up`, `f5`, ...).
pub fn parse_key(s: &str) -> Result<KeyEvent, ParseError> {
    let mut mods = KeyMods::default();
    let mut rest = s;
    loop {
        if let Some(r) = rest.strip_prefix("C-") {
            mods.ctrl = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("M-") {
            mods.alt = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("S-") {
            mods.shift = true;
            rest = r;
        } else {
            break;
        }
    }
    let mut chars = rest.chars();
    let code = match (chars.next(), chars.next()) {
        (Some(c), None) => KeyCode::Char(c),
        _ => match rest.to_ascii_lowercase().as_str() {
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "space" => KeyCode::Char(' '),
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "backspace" | "bspace" => KeyCode::Backspace,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            "delete" | "del" => KeyCode::Delete,
            "insert" => KeyCode::Insert,
            f if f.starts_with('f') => {
                let n: u8 = f[1..].parse().map_err(|_| ParseError::UnknownKey(s.to_string()))?;
                KeyCode::F(n)
            }
            _ => return Err(ParseError::UnknownKey(s.to_string())),
        },
    };
    Ok(KeyEvent { code, mods })
}

/// Parse a command string from the keybinding/palette mini-language.
pub fn parse_action(s: &str) -> Result<Action, ParseError> {
    let mut words = s.split_whitespace();
    let head = words.next().ok_or_else(|| ParseError::UnknownCommand(s.to_string()))?;
    let arg = words.next();
    let dir_arg = |arg: Option<&str>| -> Result<Dir, ParseError> {
        match arg {
            Some("left") => Ok(Dir::Left),
            Some("right") => Ok(Dir::Right),
            Some("up") => Ok(Dir::Up),
            Some("down") => Ok(Dir::Down),
            _ => Err(ParseError::BadArgument(s.to_string())),
        }
    };
    let cmd = match head {
        "split" => {
            let dir = match arg {
                Some("horizontal" | "h") => SplitDir::Horizontal,
                Some("vertical" | "v") => SplitDir::Vertical,
                _ => return Err(ParseError::BadArgument(s.to_string())),
            };
            Command::SplitPane { dir, kind: PaneKind::Terminal { command: None } }
        }
        "open-pane" => {
            let kind = match arg {
                Some("filetree") => PaneKind::FileTree,
                Some("git") => PaneKind::Git,
                Some("diff") => PaneKind::Diff,
                Some("editor") => PaneKind::Editor { path: None },
                Some("terminal") => PaneKind::Terminal { command: None },
                _ => return Err(ParseError::BadArgument(s.to_string())),
            };
            Command::SplitPane { dir: SplitDir::Horizontal, kind }
        }
        // Sections first, directions second. The workbench has rails to move
        // between and no free panes to move *past*, so the section names are
        // what this word means now — but `focus left` stays parseable, because a
        // config that still says it deserves the "not in this workbench"
        // explanation rather than a parse error about an unknown command.
        "focus" => match arg {
            Some("agents") => return Ok(Action::View(ViewVerb::Focus(Focus::Agents))),
            Some("processes" | "procs") => {
                return Ok(Action::View(ViewVerb::Focus(Focus::Processes)))
            }
            Some("changes") => return Ok(Action::View(ViewVerb::Focus(Focus::Changes))),
            // `fleet` is what the list is called on the BOOTH space, which is
            // the only place it is drawn now that the panel under CHANGES has
            // gone. `all-agents` is what the panel was called and still
            // resolves, so a `[keys]` line written against it keeps working.
            Some("fleet" | "all-agents") => {
                return Ok(Action::View(ViewVerb::Focus(Focus::AllAgents)))
            }
            Some("stage") => return Ok(Action::View(ViewVerb::Focus(Focus::Stage))),
            _ => Command::FocusDir(dir_arg(arg)?),
        },
        // The spaces, by the name on their row.
        "space" => {
            let verb = match arg {
                Some("work") => ViewVerb::Space(Page::Agents),
                Some("files") => ViewVerb::Space(Page::Files),
                Some("docker") => ViewVerb::Space(Page::Docker),
                Some("docs") => ViewVerb::Space(Page::Docs),
                // The repository over time. It was the one space the menu
                // lists that this language could not name — `alt-r` reached it
                // and nothing else did, so it could be neither rebound nor
                // reached from `:`.
                Some("git") => ViewVerb::Space(Page::Git),
                Some("usage") => ViewVerb::Space(Page::Usage),
                // `home` is the name this page had until it was named for what
                // you do on it. Kept as an alias rather than removed: it is in
                // users' keymaps, and a config that stops parsing is a worse
                // greeting than an old word.
                Some("booth") | Some("home") => ViewVerb::Space(Page::Booth),
                Some("next") => ViewVerb::SpaceNext,
                Some("prev") => ViewVerb::SpacePrev,
                // The menu of them, which is what the tab bar's own control
                // opens. Named here so the button is not the only way in.
                Some("menu") => ViewVerb::Spaces,
                _ => return Err(ParseError::BadArgument(s.to_string())),
            };
            return Ok(Action::View(verb));
        }
        // Tabs are workspaces. Numbered from 1, as the bar labels them.
        "workspace" | "ws" => {
            let verb = match arg {
                Some("next") => ViewVerb::TabNext,
                Some("prev") => ViewVerb::TabPrev,
                Some("new") => ViewVerb::NewWorkspace,
                Some("close") => ViewVerb::CloseWorkspace,
                Some(n) => match n.parse::<usize>() {
                    Ok(n) if n >= 1 => ViewVerb::Tab(n),
                    _ => return Err(ParseError::BadArgument(s.to_string())),
                },
                None => return Err(ParseError::BadArgument(s.to_string())),
            };
            return Ok(Action::View(verb));
        }
        "host" => return Ok(Action::View(ViewVerb::Host)),
        "terminal" | "term" => return Ok(Action::View(ViewVerb::NewTerminal)),
        // Bare is the CPU/RAM monitor, which every machine has; `gpu` is the
        // other gauge, and it is an argument rather than a second word because
        // the two are one idea pointed at different hardware.
        "monitor" => {
            let verb = match arg {
                None => ViewVerb::Monitor { gpu: false },
                Some("gpu") => ViewVerb::Monitor { gpu: true },
                Some(_) => return Err(ParseError::BadArgument(s.to_string())),
            };
            return Ok(Action::View(verb));
        }
        "find" | "search" => return Ok(Action::View(ViewVerb::Search)),
        "links" | "urls" => return Ok(Action::View(ViewVerb::Links)),
        "branch" => return Ok(Action::View(ViewVerb::Branch)),
        "help" => return Ok(Action::View(ViewVerb::Help)),
        "resize" => {
            let dir = dir_arg(arg)?;
            let cells: i16 = match words.next() {
                Some(n) => n.parse().map_err(|_| ParseError::BadArgument(s.to_string()))?,
                None => 2,
            };
            Command::ResizePane { dir, cells }
        }
        "close-pane" | "kill-pane" => Command::ClosePane,
        "zoom" => Command::ZoomToggle,
        "scroll-up" => Command::ScrollPage(-1),
        "scroll-down" => Command::ScrollPage(1),
        "new-window" => Command::NewWindow,
        "next-window" => Command::NextWindow,
        "prev-window" => Command::PrevWindow,
        "select-window" => {
            let n: usize = arg
                .and_then(|a| a.parse().ok())
                .ok_or_else(|| ParseError::BadArgument(s.to_string()))?;
            Command::SelectWindow(n)
        }
        "rename-window" => {
            let name = arg.ok_or_else(|| ParseError::BadArgument(s.to_string()))?;
            let rest: String = std::iter::once(name).chain(words).collect::<Vec<_>>().join(" ");
            Command::RenameWindow(rest)
        }
        // Bare `layout` is the rail-resizing mode the `[layout]` button opens.
        // Named, it is the old preset-applying command, which the workbench has
        // nothing for — kept parseable so an existing binding explains itself.
        "layout" => match arg {
            None => return Ok(Action::View(ViewVerb::Layout)),
            Some(name) => Command::ApplyLayout(name.to_string()),
        },
        "agent" => match arg {
            Some(name) => Command::SpawnAgent(name.to_string()),
            None => Command::ListAgents,
        },
        // Bare = unpin, unlike `theme`'s bare-lists shape: the pin is a single
        // value the `+` button already displays, so there is nothing to list,
        // and "how do I undo this" is the question a pin actually raises.
        "agent-default" => Command::SetDefaultAgent(arg.map(|a| a.to_string())),
        "process" => {
            let name = arg.ok_or_else(|| ParseError::BadArgument(s.to_string()))?;
            let command: String = words.collect::<Vec<_>>().join(" ");
            if command.is_empty() {
                return Err(ParseError::BadArgument(s.to_string()));
            }
            Command::NewProcess { name: name.to_string(), command }
        }
        "theme" => match arg {
            Some(name) => Command::SetTheme(name.to_string()),
            None => Command::ListThemes,
        },
        "list-sessions" => Command::ListSessions,
        // Bare keeps the session; `clear` is the one that throws it away. Spelled
        // as an argument rather than a second command name so the destructive
        // form cannot be reached by tab-completing or mistyping the safe one.
        "kill-server" => match arg {
            None => Command::KillServer,
            Some("clear") => Command::KillServerClear,
            Some(_) => return Err(ParseError::BadArgument(s.to_string())),
        },
        "reload-config" => Command::ReloadConfig,
        "all-agents" => Command::ToggleAllAgents,
        "git-menu" => Command::GitMenu,
        "paste-image" => Command::PasteImage,
        "detach" => return Ok(Action::Detach),
        "prompt" => return Ok(Action::CommandPrompt),
        _ => return Err(ParseError::UnknownCommand(s.to_string())),
    };
    Ok(Action::Command(cmd))
}

/// The prefix-key binding table used by the server's input router.
#[derive(Debug, Clone)]
pub struct Keymap {
    pub prefix: KeyEvent,
    bindings: HashMap<KeyEvent, Action>,
}

/// Drop a Shift that the character already carries.
///
/// A terminal reports `I` as `Char('I')` *plus* Shift, while `parse_key("I")`
/// produces `Char('I')` and nothing else — so a binding on any capital letter
/// could never match. The shipped table has one (`I` = `layout ide`), which is
/// how this was found: the client now says "S-Q is not bound" where the daemon
/// logged it where nobody looks.
///
/// Shift is redundant with the character for every `Char`, and only for `Char`:
/// on `Tab`, `F5` or an arrow it is the whole difference between two keys.
pub fn normalize(key: &KeyEvent) -> KeyEvent {
    let mut key = *key;
    if matches!(key.code, KeyCode::Char(_)) {
        key.mods.shift = false;
    }
    key
}

impl Keymap {
    /// How many keys the table binds.
    ///
    /// Reported on the SETTINGS page beside the count from `[keys]`, so "31
    /// bound, 4 from your config" tells the shipped table apart from what the
    /// user added to it — which is the question you have when a key does
    /// something you did not expect.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// Never true in practice — the shipped table is not empty — but `len`
    /// without it is a clippy error, and a keymap that somehow had no bindings
    /// is worth being able to ask about.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    pub fn resolve(&self, key: &KeyEvent) -> Option<&Action> {
        self.bindings.get(&normalize(key))
    }

    /// Build from the default table with `[keys]` overrides applied on top.
    /// Invalid entries are returned as warnings, not errors.
    pub fn from_config(prefix: &str, overrides: &HashMap<String, String>) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let prefix = parse_key(prefix).unwrap_or_else(|e| {
            warnings.push(format!("bad prefix ({e}); falling back to C-b"));
            KeyEvent::ctrl('b')
        });
        let prefix = normalize(&prefix);
        let mut bindings = default_bindings();
        for (k, v) in overrides {
            match (parse_key(k), parse_action(v)) {
                (Ok(key), Ok(action)) => {
                    bindings.insert(normalize(&key), action);
                }
                (Err(e), _) | (_, Err(e)) => warnings.push(format!("[keys] {k:?}: {e}")),
            }
        }
        (Self { prefix, bindings }, warnings)
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Self { prefix: KeyEvent::ctrl('b'), bindings: default_bindings() }
    }
}

/// The shipped prefix table.
///
/// It used to be tmux's, and stayed tmux's for a while after the workbench
/// stopped having anything for it to name: splits, directional focus, resizes,
/// windows and layout presets: 23 of its 33 entries resolved to
/// [`crate::keys::Bound::NotHere`] and did nothing but flash a message. This
/// table is the same interface as the Alt layer, so a key that works there
/// works here, and the two cannot drift into disagreeing about what the
/// workbench can do.
///
/// The letters follow the Alt layer wherever it has one, because a user who
/// learns `alt-o` should not have to learn a second letter for files.
pub(crate) fn default_bindings() -> HashMap<KeyEvent, Action> {
    let table: &[(&str, &str)] = &[
        // Spaces, on the Alt layer's own letters.
        ("o", "space files"),
        ("m", "space docs"),
        ("c", "space docker"),
        // On the Alt layer's letter, which is `r` for the same reason there:
        // `g` is the CHANGES rail, and the two git surfaces must not share one.
        ("r", "space git"),
        ("u", "space usage"),
        ("w", "space work"),
        (",", "space prev"),
        (".", "space next"),
        ("e", "space files"),
        // The menu of them, on the Alt layer's own key. Space rather than a
        // letter for the reason given there: every letter that names this
        // control is already spoken for.
        ("space", "space menu"),
        // Workspaces — the tab bar, which spans every connected daemon.
        ("n", "workspace new"),
        ("[", "workspace prev"),
        ("]", "workspace next"),
        ("1", "workspace 1"),
        ("2", "workspace 2"),
        ("3", "workspace 3"),
        ("4", "workspace 4"),
        ("5", "workspace 5"),
        ("6", "workspace 6"),
        ("7", "workspace 7"),
        ("8", "workspace 8"),
        ("9", "workspace 9"),
        ("X", "workspace close"),
        ("H", "host"),
        // The rails.
        ("A", "focus agents"),
        ("P", "focus processes"),
        ("G", "focus changes"),
        ("W", "focus fleet"),
        ("s", "focus stage"),
        // What fills them.
        ("a", "agent"),
        ("t", "terminal"),
        ("x", "close-pane"),
        ("g", "git-menu"),
        ("b", "branch"),
        // The SYSTEM gauges, which are the one part of the left rail the cursor
        // cannot reach — so before these two the only way to open the monitor
        // they stand for was to click one. `S` is the section's own initial;
        // `Y` has no mnemonic at all, because every letter in `gpu` is already a
        // rail, a space or a verb. It is bound anyway: the gauge is clickable,
        // and a click with no key is the thing this table exists to prevent.
        ("S", "monitor"),
        ("Y", "monitor gpu"),
        // The rest of the workbench.
        ("z", "zoom"),
        ("l", "layout"),
        ("/", "find"),
        // `f` for follow, the word every keyboard-driven browser uses for this.
        //
        // **Here and bare, but deliberately not on the Alt layer**, which is
        // where every other letter in this table also lives. `alt-f` is
        // readline's forward-word, and butai leaves it — with `alt-b` and
        // `alt-y` — to the pane on purpose, so a shell inside butai edits its
        // line the way it does everywhere else. `g` (the git menu) and `b`
        // (branches) are bare-and-prefix for the same kind of reason, so this
        // is the shape the table already has for a verb the Alt layer skips.
        ("f", "links"),
        ("v", "paste-image"),
        ("?", "help"),
        ("d", "detach"),
        (":", "prompt"),
        ("pageup", "scroll-up"),
        ("pagedown", "scroll-down"),
    ];
    table
        .iter()
        .map(|(k, v)| {
            (normalize(&parse_key(k).expect("default key")), parse_action(v).expect("default cmd"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    /// The prefix a `config.toml` names is the prefix the keymap binds. Asserted
    /// in `butai-core`'s config tests until the keymap moved here.
    #[test]
    fn the_config_prefix_becomes_the_keymap_prefix() {
        let cfg: crate::config::Config = toml::from_str("[general]\nprefix = \"C-a\"\n").unwrap();
        let (km, warnings) = Keymap::from_config(&cfg.general.prefix, &cfg.keys);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(km.prefix, butai_protocol::KeyEvent::ctrl('a'));
    }

    use super::*;

    #[test]
    fn parses_keys() {
        assert_eq!(parse_key("C-b").unwrap(), KeyEvent::ctrl('b'));
        assert_eq!(parse_key("%").unwrap(), KeyEvent::char('%'));
        assert_eq!(
            parse_key("M-enter").unwrap(),
            KeyEvent { code: KeyCode::Enter, mods: KeyMods { alt: true, ..Default::default() } }
        );
        assert_eq!(parse_key("f5").unwrap().code, KeyCode::F(5));
        assert!(parse_key("C-").is_err());
        assert!(parse_key("bogus").is_err());
    }

    /// A capital letter binds.
    ///
    /// The terminal reports `A` as `Char('A')` *plus* Shift, and `parse_key`
    /// produces `Char('A')` alone — so without normalizing, the shipped
    /// capitals (the rail keys) could never fire, and neither could any user
    /// binding on one. Both spellings of the config side have to reach the same
    /// entry.
    #[test]
    fn a_capital_letter_resolves_however_the_terminal_spells_it() {
        let keymap = Keymap::default();
        let from_terminal = KeyEvent {
            code: KeyCode::Char('A'),
            mods: KeyMods { shift: true, ..Default::default() },
        };
        assert_eq!(
            keymap.resolve(&from_terminal),
            Some(&Action::View(ViewVerb::Focus(Focus::Agents))),
            "a shifted capital did not reach its binding"
        );
        assert_eq!(keymap.resolve(&parse_key("A").unwrap()), keymap.resolve(&from_terminal));

        // A user's `S-x` and `X` are one binding, not two.
        let overrides = HashMap::from([("S-X".to_string(), "detach".to_string())]);
        let (keymap, warnings) = Keymap::from_config("C-b", &overrides);
        assert!(warnings.is_empty(), "{warnings:?}");
        let typed = KeyEvent {
            code: KeyCode::Char('X'),
            mods: KeyMods { shift: true, ..Default::default() },
        };
        assert_eq!(keymap.resolve(&typed), Some(&Action::Detach));

        // Shift is still the whole difference on a key whose code does not
        // carry it.
        let tab = KeyEvent { code: KeyCode::Tab, mods: KeyMods::default() };
        let shift_tab =
            KeyEvent { code: KeyCode::Tab, mods: KeyMods { shift: true, ..Default::default() } };
        assert_ne!(normalize(&tab), normalize(&shift_tab));
    }

    #[test]
    fn parses_actions() {
        assert_eq!(
            parse_action("split horizontal").unwrap(),
            Action::Command(Command::SplitPane {
                dir: SplitDir::Horizontal,
                kind: PaneKind::Terminal { command: None }
            })
        );
        assert_eq!(parse_action("detach").unwrap(), Action::Detach);
        assert_eq!(
            parse_action("resize left 5").unwrap(),
            Action::Command(Command::ResizePane { dir: Dir::Left, cells: 5 })
        );
        assert_eq!(
            parse_action("layout ide").unwrap(),
            Action::Command(Command::ApplyLayout("ide".into()))
        );
        assert!(parse_action("frobnicate").is_err());
    }

    /// The destructive form of `kill-server` has to be asked for by name. A
    /// mistyped argument is an error rather than a fallback to either meaning:
    /// silently keeping the session would be surprising, and silently clearing
    /// it would be unrecoverable.
    #[test]
    fn kill_server_clears_only_when_asked() {
        assert_eq!(parse_action("kill-server").unwrap(), Action::Command(Command::KillServer));
        assert_eq!(
            parse_action("kill-server clear").unwrap(),
            Action::Command(Command::KillServerClear)
        );
        assert!(parse_action("kill-server please").is_err());
    }

    /// `theme` mirrors `agent`: bare it lists, with an argument it selects.
    /// Validating the name is the server's job — the parser has no filesystem.
    #[test]
    fn theme_takes_an_optional_name() {
        assert_eq!(
            parse_action("theme tokyonight").unwrap(),
            Action::Command(Command::SetTheme("tokyonight".into()))
        );
        assert_eq!(parse_action("theme").unwrap(), Action::Command(Command::ListThemes));
        assert_eq!(
            parse_action("theme not-a-real-theme").unwrap(),
            Action::Command(Command::SetTheme("not-a-real-theme".into()))
        );
    }

    /// The SYSTEM gauges have a name and two default keys.
    ///
    /// They are drawn below the PROCESSES rail, they are not a list the cursor
    /// can walk, and the monitor they open was reachable by clicking one and by
    /// nothing else — the last click in the workbench with no keyboard
    /// spelling. `gpu` is an argument rather than a second command name for the
    /// reason `kill-server clear` is: it is the same idea, pointed at other
    /// hardware.
    #[test]
    fn the_system_monitor_is_nameable_and_bound() {
        assert_eq!(
            parse_action("monitor").unwrap(),
            Action::View(ViewVerb::Monitor { gpu: false })
        );
        assert_eq!(
            parse_action("monitor gpu").unwrap(),
            Action::View(ViewVerb::Monitor { gpu: true })
        );
        assert!(parse_action("monitor cpu").is_err(), "an unknown gauge is not the CPU one");
        let km = Keymap::default();
        assert_eq!(
            km.resolve(&parse_key("S").unwrap()),
            Some(&Action::View(ViewVerb::Monitor { gpu: false }))
        );
        assert_eq!(
            km.resolve(&parse_key("Y").unwrap()),
            Some(&Action::View(ViewVerb::Monitor { gpu: true }))
        );
    }

    #[test]
    fn default_keymap_resolves() {
        let km = Keymap::default();
        assert_eq!(km.prefix, KeyEvent::ctrl('b'));
        assert_eq!(km.resolve(&KeyEvent::char('d')), Some(&Action::Detach));
        assert!(km.resolve(&KeyEvent::char('o')).is_some());
        // `%` was `split horizontal`, and a split is the one thing this
        // workbench certainly does not do.
        assert!(km.resolve(&KeyEvent::char('%')).is_none());
    }

    /// The shipped table names only things the workbench can carry out.
    ///
    /// This is the regression that mattered: for a long time 23 of its 33
    /// entries were splits, directional focus, resizes, windows and layout
    /// presets — a free-pane vocabulary the rails replaced — so pressing them
    /// after the prefix flashed "not in this workbench" and did nothing. A new
    /// binding that resolves to one of those families fails here.
    #[test]
    fn no_default_binding_names_the_free_pane_model() {
        for (key, action) in default_bindings() {
            let Action::Command(cmd) = &action else { continue };
            assert!(
                !matches!(
                    cmd,
                    Command::SplitPane { .. }
                        | Command::FocusDir(_)
                        | Command::FocusPane(_)
                        | Command::ResizePane { .. }
                        | Command::ApplyLayout(_)
                        | Command::NewWindow
                        | Command::NextWindow
                        | Command::PrevWindow
                        | Command::SelectWindow(_)
                        | Command::RenameWindow(_)
                ),
                "{key:?} is bound to {cmd:?}, which the workbench has nothing for"
            );
        }
    }

    #[test]
    fn config_overrides_and_warns() {
        let mut over = HashMap::new();
        over.insert("s".to_string(), "split vertical".to_string());
        over.insert("bad key".to_string(), "zoom".to_string());
        let (km, warnings) = Keymap::from_config("C-a", &over);
        assert_eq!(km.prefix, KeyEvent::ctrl('a'));
        assert!(km.resolve(&KeyEvent::char('s')).is_some());
        assert_eq!(warnings.len(), 1);
    }
}
