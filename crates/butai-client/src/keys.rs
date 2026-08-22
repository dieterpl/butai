//! What a bound key means to *this* client.
//!
//! The prefix table is user config (`[keys]` in `config.toml`), so the client
//! reads it rather than inventing a second vocabulary — the keymap, the `:`
//! prompt and the command palette have always shared one, and that stays true
//! now the resolution happens here instead of in the daemon.
//!
//! The daemon used to own both halves: it held `prefix_armed` per connected
//! client and turned the second keystroke into an action. Only the second half
//! was ever daemon work, and only for the commands that are actually the
//! daemon's. This module is the sorting step — [`bind`] says which kind a
//! resolved action is, and the loop acts on that.
//!
//! [`Bound::NotHere`] is the one interesting answer: a binding that means
//! nothing in a workbench of fixed rails and one stage. The daemon refuses the
//! same set for the same reason, so it is not a client gap — and there is no
//! third kind. Everything else this table can name, the client can now do,
//! which is what had to be true before the composed path could be deleted.

use crate::keymap::{Action, ViewVerb};
use butai_protocol::{Command, PaneKind};

/// The client's reading of one resolved binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bound {
    /// Leave. Connection-scoped, which is why it was never a `Command`.
    Detach,
    /// Open the `:` prompt.
    Prompt,
    /// Done here, with no daemon involved.
    Local(Local),
    /// One call the loop makes on the daemon's behalf.
    Ask(Ask),
    /// Bound to something that has no meaning in this workbench.
    NotHere(&'static str),
}

/// Bindings the client answers by itself.
///
/// Every one of these is interface state, which is the whole reason it moved:
/// two clients on one workspace each get their own answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Local {
    /// Collapse both rails.
    Zen,
    /// The git menu.
    GitMenu,
    /// The agent picker (`agent` with no name).
    PickAgent,
    /// Put a file on the Files page.
    OpenFile(String),
    /// Switch palette, or list the palettes. Client-side since phase 3: the
    /// daemon no longer draws the chrome, so it has no say in its colours.
    Theme(Option<String>),
    /// Scroll the staged pane's scrollback, in pages. The pane is the daemon's,
    /// but the connection streaming it is already open, so this is a message on
    /// it rather than a route of its own.
    Scroll(i16),
    /// Pin the agent the AGENTS `+` button spawns without asking; `None`
    /// unpins.
    ///
    /// The client's own config, because the pin is about what a *button* does
    /// and the button is the client's. It also has to be: a daemon on another
    /// machine keeps its `config.toml` over there, so pinning through it would
    /// write the wrong file on the wrong host.
    PinAgent(Option<String>),
    /// Put the clipboard's image in the workspace's scratch directory and paste
    /// its path where typing would have gone.
    ///
    /// Local in the sense that matters — only this machine can read this
    /// machine's clipboard, which is the whole reason the command exists.
    PasteImage,
    /// Move this client's view — a space, a rail, a tab. The most local thing
    /// there is: it is the reason two clients on one workspace can look at
    /// different parts of it.
    View(ViewVerb),
}

/// Bindings that are a call on the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    SpawnAgent(String),
    NewProcess {
        name: String,
        command: String,
    },
    /// Kill whatever is on the stage.
    ClosePane,
    /// Stop the daemon. `clear` also forgets the persisted session.
    KillServer {
        clear: bool,
    },
    ReloadConfig,
}

/// Sort a resolved binding into what the client does about it.
pub fn bind(action: Action) -> Bound {
    let cmd = match action {
        Action::Detach => return Bound::Detach,
        Action::CommandPrompt => return Bound::Prompt,
        Action::View(verb) => return Bound::Local(Local::View(verb)),
        Action::Command(cmd) => cmd,
    };
    match cmd {
        // -- the client's own --
        Command::ZoomToggle => Bound::Local(Local::Zen),
        Command::GitMenu => Bound::Local(Local::GitMenu),
        Command::ListAgents => Bound::Local(Local::PickAgent),
        Command::SetTheme(name) => Bound::Local(Local::Theme(Some(name))),
        Command::ListThemes => Bound::Local(Local::Theme(None)),
        Command::OpenFile(path) => {
            Bound::Local(Local::OpenFile(path.to_string_lossy().into_owned()))
        }
        // `open-pane filetree` is how the daemon's config spells "show me the
        // files", and the Files page is where that went.
        Command::SplitPane { kind: PaneKind::FileTree, .. } => {
            Bound::Local(Local::OpenFile(String::new()))
        }
        Command::ScrollPage(pages) => Bound::Local(Local::Scroll(pages)),
        Command::SetDefaultAgent(name) => Bound::Local(Local::PinAgent(name)),
        Command::PasteImage => Bound::Local(Local::PasteImage),

        // -- the daemon's, over routes that exist --
        Command::SpawnAgent(name) => Bound::Ask(Ask::SpawnAgent(name)),
        Command::NewProcess { name, command } => Bound::Ask(Ask::NewProcess { name, command }),
        Command::ClosePane => Bound::Ask(Ask::ClosePane),
        Command::KillServer => Bound::Ask(Ask::KillServer { clear: false }),
        Command::KillServerClear => Bound::Ask(Ask::KillServer { clear: true }),
        Command::ReloadConfig => Bound::Ask(Ask::ReloadConfig),

        // -- no meaning in a workbench that stages one pane --
        //
        // The daemon refuses this same set with one message ("the workbench has
        // fixed rails, not free panes"), so none of it is a client gap: the
        // free-pane model these belong to went when the rails arrived.
        Command::SplitPane { .. } => Bound::NotHere("the stage holds one pane"),
        Command::FocusDir(_) | Command::FocusPane(_) => Bound::NotHere("nothing to focus past"),
        Command::ResizePane { .. } => Bound::NotHere("the stage is sized by the rails"),
        Command::ApplyLayout(_) => Bound::NotHere("the rails are the layout"),
        Command::NewWindow
        | Command::NextWindow
        | Command::PrevWindow
        | Command::SelectWindow(_)
        | Command::RenameWindow(_) => Bound::NotHere("tabs are workspaces, not windows"),
        // These are the CLI's, and a key that opened a list nothing reads would
        // be a key that appears to do nothing.
        Command::ListSessions | Command::NewSession { .. } | Command::KillSession(_) => {
            Bound::NotHere("use the tab bar, or the CLI")
        }
        // The panel this opened is gone. Every agent on every machine is what
        // the BOOTH space *is*, and a second list of them under the changes rail
        // was one question answered twice. It still parses, so a `[keys]` line
        // naming it is not a config error — it says where the list went.
        Command::ToggleAllAgents => Bound::NotHere("the BOOTH space lists every agent"),
        // Never bound: it is the *answer* to a paste, not a thing to bind, and
        // the client builds it from its own clipboard.
        Command::PutFile { .. } => Bound::NotHere("nothing binds a file transfer"),
    }
}

// ---------------------------------------------------------------------------
// macOS: Option is a compose key, not a modifier
// ---------------------------------------------------------------------------

/// What a macOS Option-composed character was before the keyboard composed it.
///
/// On a Mac, Option is a dead/compose modifier by default: pressing Option-o
/// types `ø` and the terminal reports exactly that, with no Alt anywhere. So
/// the entire Alt layer is unreachable out of the box for most Mac users, and
/// nothing on screen explains it — the keys simply do nothing.
///
/// The characters are the US layout's, which is the one this problem is about:
/// a layout that puts Alt on Option does not need this, and a layout that
/// composes something else was never going to be covered by a fixed table.
///
/// **Only the keys the Alt layer binds are here.** Option-b is `∫` and nothing
/// binds Alt-b, so `∫` stays a character you can type into a pane. That is the
/// whole reason this is a table and not a rule: the smaller it is, the less it
/// takes away.
///
/// Four Alt bindings cannot be recovered this way. Option-e and Option-n are
/// *dead* keys — they emit nothing at all until the next keystroke — and
/// Option-Esc and Option-Enter are not composed, so they arrive
/// indistinguishable from a bare Esc or Enter. All four are on the prefix
/// layer, which is why that layer covering the same set matters.
pub fn option_char(c: char) -> Option<char> {
    Some(match c {
        'å' => 'a',
        'ç' => 'c',
        '∂' => 'd',
        '©' => 'g',
        '˙' => 'h',
        '¬' => 'l',
        'µ' => 'm',
        'ø' => 'o',
        'π' => 'p',
        '®' => 'r',
        'ß' => 's',
        '†' => 't',
        '√' => 'v',
        '∑' => 'w',
        '≈' => 'x',
        'Ω' => 'z',
        '≤' => ',',
        '≥' => '.',
        // Option-Shift-, and Option-Shift-. — the pair that walks the tab bar.
        '¯' => '<',
        '˘' => '>',
        '÷' => '/',
        'º' => '0',
        '¡' => '1',
        '™' => '2',
        '£' => '3',
        '¢' => '4',
        '∞' => '5',
        '§' => '6',
        '¶' => '7',
        '•' => '8',
        'ª' => '9',
        _ => return None,
    })
}

/// The character a Mac's Option key composes for `plain`, if this table has
/// one. The reverse of [`option_char`], for asking "is this binding reachable
/// on a Mac at all?".
pub fn option_char_for(plain: char) -> Option<char> {
    ('\u{20}'..='\u{2fff}').find(|&c| option_char(c) == Some(plain))
}

/// How a key reads on screen — `^B`, `M-x`, `F5`, `enter`.
///
/// Terminal notation rather than the config's own (`C-b`), because this is
/// shown next to the workspace name in the footer, where `^B` is what a tmux
/// user's eye is already looking for. The prefix is configurable, so a
/// hard-coded marker would be a lie the moment anyone changes it.
pub fn key_label(key: &butai_protocol::KeyEvent) -> String {
    use butai_protocol::KeyCode;
    let name = match key.code {
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) if key.mods.ctrl => c.to_ascii_uppercase().to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::F(n) => format!("F{n}"),
        other => format!("{other:?}").to_lowercase(),
    };
    let mut out = String::new();
    if key.mods.ctrl {
        out.push('^');
    }
    if key.mods.alt {
        out.push_str("M-");
    }
    if key.mods.shift {
        out.push_str("S-");
    }
    out.push_str(&name);
    out
}

impl Bound {
    /// What the footer says when a key resolves to something unavailable.
    pub fn why(&self) -> Option<String> {
        match self {
            Bound::NotHere(what) => Some(format!("not in this workbench: {what}")),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::parse_action;

    fn bound(s: &str) -> Bound {
        bind(parse_action(s).expect("parses"))
    }

    /// The prefix table's own defaults, sorted.
    ///
    /// The point of the test is the *shape* of the split: what the client
    /// answers itself, and what it asks the daemon for.
    #[test]
    fn the_keys_people_press_are_all_answerable() {
        assert_eq!(bound("detach"), Bound::Detach);
        assert_eq!(bound("prompt"), Bound::Prompt);
        assert_eq!(bound("all-agents"), Bound::NotHere("the BOOTH space lists every agent"));
        assert_eq!(bound("git-menu"), Bound::Local(Local::GitMenu));
        assert_eq!(bound("zoom"), Bound::Local(Local::Zen));
        assert_eq!(bound("agent"), Bound::Local(Local::PickAgent));
        assert_eq!(bound("agent claude"), Bound::Ask(Ask::SpawnAgent("claude".into())));
        assert_eq!(bound("open-pane filetree"), Bound::Local(Local::OpenFile(String::new())));
        assert_eq!(bound("close-pane"), Bound::Ask(Ask::ClosePane));
        assert_eq!(
            bound("process build cargo build"),
            Bound::Ask(Ask::NewProcess { name: "build".into(), command: "cargo build".into() })
        );
        assert_eq!(bound("kill-server"), Bound::Ask(Ask::KillServer { clear: false }));
        assert_eq!(bound("kill-server clear"), Bound::Ask(Ask::KillServer { clear: true }));
        // The four that used to report themselves unavailable. Each was the
        // gate on deleting the composed path, so each is pinned here by name.
        assert_eq!(bound("scroll-up"), Bound::Local(Local::Scroll(-1)));
        assert_eq!(bound("scroll-down"), Bound::Local(Local::Scroll(1)));
        assert_eq!(
            bound("agent-default claude"),
            Bound::Local(Local::PinAgent(Some("claude".into())))
        );
        assert_eq!(bound("agent-default"), Bound::Local(Local::PinAgent(None)));
        assert_eq!(bound("paste-image"), Bound::Local(Local::PasteImage));
    }

    /// A split has nowhere to go in a workbench with one stage, and says so
    /// rather than doing nothing.
    #[test]
    fn a_binding_with_no_meaning_here_explains_itself() {
        let split = bound("split horizontal");
        assert!(matches!(split, Bound::NotHere(_)), "{split:?}");
        assert!(split.why().unwrap().contains("one pane"), "{:?}", split.why());
        assert!(matches!(bound("focus left"), Bound::NotHere(_)));
        assert!(matches!(bound("new-window"), Bound::NotHere(_)));
        // And an answerable one says nothing at all.
        assert_eq!(bound("detach").why(), None);
    }

    /// Nothing the table can name is out of reach.
    ///
    /// This is the assertion that had to become true before `compose_workbench`
    /// could go: every binding is either something the client does or something
    /// the *daemon* refuses too. Adding a `Command` with no client answer breaks
    /// this rather than shipping a key that reports itself unavailable.
    #[test]
    fn no_binding_is_merely_missing() {
        for s in [
            "scroll-up",
            "scroll-down",
            "agent-default x",
            "agent-default",
            "paste-image",
            "detach",
            "prompt",
            "all-agents",
            "git-menu",
            "zoom",
            "agent",
            "agent claude",
            "open-pane filetree",
            "close-pane",
            "process p echo hi",
            "kill-server",
            "reload-config",
            "theme dark",
            "theme",
            "list-sessions",
        ] {
            let b = bound(s);
            assert!(
                !matches!(b, Bound::NotHere(_)) || REFUSED_BY_THE_DAEMON_TOO.contains(&s),
                "{s} resolves to {b:?}, which nothing carries out"
            );
        }
    }

    /// The bindings the daemon answers with "the workbench has fixed rails, not
    /// free panes". The client saying the same thing is parity, not a gap.
    ///
    /// `all-agents` joined them when the panel it opened was removed: the BOOTH
    /// space is the list of every agent on every machine, and the daemon
    /// refuses `toggle_all_agents` for the same reason it refuses the rest —
    /// what is folded is each client's own.
    const REFUSED_BY_THE_DAEMON_TOO: &[&str] = &["list-sessions", "all-agents"];

    /// Every key in the shipped table does something.
    ///
    /// Not "resolves to an `Action`" — it always did that. The 23 dead entries
    /// resolved fine and then sorted to [`Bound::NotHere`], which is a key that
    /// flashes a message and changes nothing. Walking the real table rather
    /// than a hand-written sample is the point: the old sample listed `%` and
    /// `e`, so it agreed the table was healthy while two thirds of it was not.
    #[test]
    fn every_default_binding_does_something() {
        for (key, action) in crate::keymap::default_bindings() {
            let bound = bind(action);
            assert!(
                !matches!(bound, Bound::NotHere(_)),
                "{key:?} is bound to something the workbench cannot do: {bound:?}"
            );
        }
    }
}
