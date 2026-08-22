//! The public butai client<->server API contract.
//!
//! Every client — the built-in TUI, a script, an Electron or Swift GUI —
//! speaks exactly these types over a Unix domain socket, framed as
//! length-prefixed JSON (see [`framing`]). MessagePack may be negotiated at
//! handshake as a wire optimization; JSON is always accepted.
//!
//! The wire format is documented with examples in `docs/protocol.md`. Breaking
//! changes bump [`PROTOCOL_VERSION`].

pub mod api;
pub mod b64;
pub mod framing;
pub mod hunk;
pub mod names;
pub mod paths;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        #[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
        // `number`, not ts-rs's default `bigint` for a `u64`. These ids cross
        // the wire as JSON numbers, so `JSON.parse` hands the browser a
        // `number` whatever the binding claims — and a binding that says
        // `bigint` would make every client write conversions around a value
        // that never was one. An id is a counter; it is nowhere near 2^53.
        //
        // (ts-rs also prints "failed to parse serde attribute: transparent"
        // here. It is cosmetic: a one-field tuple struct already emits its
        // inner type, which is what `transparent` asks for.)
        pub struct $name(#[cfg_attr(feature = "ts", ts(type = "number"))] pub u64);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_type!(PaneId);
id_type!(WindowId);
id_type!(SessionId);

// ---------------------------------------------------------------------------
// Styled cells (what clients render — no VT parser needed client-side)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct Mods {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dim: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub underline: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reverse: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub crossed_out: bool,
}

/// One screen cell: a grapheme plus its style.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct Cell {
    /// Grapheme (usually one char; may be multi-codepoint or empty for the
    /// trailing half of a wide character).
    pub ch: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub fg: Color,
    #[serde(default, skip_serializing_if = "is_default")]
    pub bg: Color,
    #[serde(default, skip_serializing_if = "is_default")]
    pub mods: Mods,
}

fn is_default<T: Default + PartialEq>(v: &T) -> bool {
    *v == T::default()
}

/// A horizontal run of changed cells starting at (x, y), in screen
/// coordinates of the client's viewport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct CellRun {
    pub x: u16,
    pub y: u16,
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

/// A damage update for the client's whole viewport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct FrameUpdate {
    /// When true the client should clear before applying (attach, resize).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub full: bool,
    pub cells: Vec<CellRun>,
    /// Absolute (x, y) of the visible cursor, or `None` when hidden.
    pub cursor: Option<(u16, u16)>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub cursor_shape: CursorShape,
    /// Whether the program in this pane has asked for mouse reporting.
    ///
    /// Only the daemon can know — it is parsing that program's output — and
    /// only the client can act on it: a client draws its own text selection,
    /// and over `vim` or `htop` a drag belongs to the program instead. Without
    /// this a client has to choose one behaviour for every pane and be wrong
    /// about half of them.
    ///
    /// Additive and defaulted, so an older client simply keeps selecting.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wants_mouse: bool,
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Tab,
    BackTab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Insert,
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct KeyMods {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ctrl: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub alt: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shift: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct KeyEvent {
    pub code: KeyCode,
    #[serde(default, skip_serializing_if = "is_default")]
    pub mods: KeyMods,
}

impl KeyEvent {
    pub fn char(c: char) -> Self {
        Self { code: KeyCode::Char(c), mods: KeyMods::default() }
    }

    pub fn ctrl(c: char) -> Self {
        Self { code: KeyCode::Char(c), mods: KeyMods { ctrl: true, ..Default::default() } }
    }
}

/// Which button a [`InputEvent::MouseDown`] came from. Left is the default so
/// the field can be skipped on the wire, keeping left-click frames byte-for-byte
/// what they were before right-click existed (see `is_default`).
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    #[default]
    Left,
    /// Opens the workbench context menu; never forwarded to a pane.
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum InputEvent {
    Key(KeyEvent),
    Paste(String),
    /// Mouse: click-to-focus, drag-to-select, and wheel scroll. `alt` marks an
    /// Alt-held press so the workbench can force a text selection over a pane
    /// whose app has grabbed the mouse. `button` distinguishes the context-menu
    /// press; it is absent on the wire for the common left-click.
    MouseDown {
        x: u16,
        y: u16,
        #[serde(default)]
        alt: bool,
        #[serde(default, skip_serializing_if = "is_default")]
        button: MouseButton,
    },
    /// Left button held and moved (extends a text selection). `alt` forces the
    /// selection even over a mouse-hungry app.
    MouseDrag {
        x: u16,
        y: u16,
        #[serde(default)]
        alt: bool,
    },
    /// Left button released (finishes a selection).
    MouseUp {
        x: u16,
        y: u16,
    },
    ScrollUp {
        x: u16,
        y: u16,
    },
    ScrollDown {
        x: u16,
        y: u16,
    },
}

// ---------------------------------------------------------------------------
// Commands (the vocabulary shared by keybindings, palette, and GUI clients)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum SplitDir {
    /// Side by side (new pane to the right).
    Horizontal,
    /// Stacked (new pane below).
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum PaneKind {
    Terminal { command: Option<String> },
    Editor { path: Option<PathBuf> },
    FileTree,
    Git,
    Diff,
    Agent { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum Command {
    SplitPane {
        dir: SplitDir,
        kind: PaneKind,
    },
    ClosePane,
    FocusDir(Dir),
    FocusPane(PaneId),
    ZoomToggle,
    /// Open the CHANGES rail's `g` command menu — every git operation that does
    /// not fit in the rail's three hint rows.
    GitMenu,
    ResizePane {
        dir: Dir,
        cells: i16,
    },
    ScrollPage(i16),
    NewWindow,
    NextWindow,
    PrevWindow,
    SelectWindow(usize),
    RenameWindow(String),
    NewSession {
        name: Option<String>,
        /// Accepted and ignored. Layout presets described pane splits, and the
        /// workbench has fixed rails — which is also why `ApplyLayout` is
        /// refused. Kept on the wire because shipped clients send it.
        layout: Option<String>,
    },
    KillSession(String),
    ListSessions,
    /// Stop the daemon, keeping the persisted session so the next start comes
    /// back to the workspaces that were open. This is the default because a
    /// restart is not a decision to throw work away.
    KillServer,
    /// Stop the daemon **and forget** the persisted session, so the next start
    /// comes up empty.
    ///
    /// A separate variant rather than a field on [`Command::KillServer`]: unit
    /// variants go over the wire as the bare string `"kill_server"`, and adding
    /// a payload would have made every existing client's message unparseable.
    KillServerClear,
    ApplyLayout(String),
    OpenFile(PathBuf),
    ListAgents,
    SpawnAgent(String),
    /// Pin the agent that the AGENTS `+` button spawns without asking; `None`
    /// clears the pin and puts the picker back. Persisted to `config.toml`.
    SetDefaultAgent(Option<String>),
    /// Start a managed process row in the workspace (v2 workbench).
    NewProcess {
        name: String,
        command: String,
    },
    ReloadConfig,
    /// Switch the chrome palette live and persist the choice to `config.toml`.
    /// The daemon composes every frame, so this repaints all attached clients.
    SetTheme(String),
    /// Built-in and user theme names, for discovering what `SetTheme` accepts.
    ListThemes,
    /// Show/hide the ALL AGENTS panel under the changes rail (v2 workbench).
    ToggleAllAgents,
    /// Write a file into the workspace's scratch directory and paste its
    /// absolute path where this client's input would have gone — the "paste an
    /// image into an agent" gesture, which agent CLIs accept as a path.
    ///
    /// A command rather than a REST route on purpose. The TUI reaches a remote
    /// daemon through a single `ssh host butai proxy` stdio channel, so putting
    /// this on `POST /v1/workspaces/{id}/upload` would cost a second ssh
    /// channel per paste; the framed connection is already open. The two routes
    /// also differ in intent: `upload` writes into the *workspace* and shows up
    /// in the changes rail, which is right for a file you meant to add and
    /// wrong for a screenshot.
    ///
    /// Paste the image on the *client's* clipboard into the focused pane.
    ///
    /// Only the client can read its own clipboard, so the daemon cannot do this
    /// itself: it answers with [`ServerMsg::ReadClipboardImage`] and the client
    /// completes the round trip with [`Command::PutFile`]. It is a command
    /// rather than a client-side keybinding so the gesture lives in the one
    /// vocabulary the keymap, the `:` prompt and the help overlay all read from
    /// — a client that cannot read a clipboard simply ignores the request.
    PasteImage,
    /// Replied to with [`ServerMsg::FilePut`].
    PutFile {
        /// Suggested file name. Only its basename is used, and the daemon
        /// prefixes a counter — a client cannot choose the final path.
        name: String,
        /// The file's bytes, base64 (standard alphabet, padding optional).
        ///
        /// Base64 rather than a byte array so JSON and MessagePack clients send
        /// the identical structure, which is the promise the framing section of
        /// `docs/protocol.md` makes about the two encodings. Capped at
        /// [`MAX_PUT_FILE_BYTES`] decoded.
        data: String,
    },
}

/// Largest file [`Command::PutFile`] will accept, decoded.
///
/// Frames max out at 32 MiB and base64 costs 4/3, so this is not the binding
/// constraint — the point is to fail a phone's 40 MP camera roll with a
/// readable error instead of a rejected frame.
pub const MAX_PUT_FILE_BYTES: usize = 8 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    #[default]
    Json,
    Msgpack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum AttachTarget {
    /// Attach to an existing session by name.
    Attach { name: String },
    /// Create a new session (and attach). `name` defaults to a generated one.
    /// `layout` is accepted and ignored — see [`Command::NewSession`].
    New { name: Option<String>, layout: Option<String> },
    /// Attach to the most recent session, creating one if none exist.
    Default,
    /// Control-only connection: no viewport, no frames (CLI one-shots, GUIs
    /// that only want structured state).
    Control,
    /// Stream a single pane's grid full-bleed at this connection's size,
    /// independent of workbench chrome — the web client's "stage". Input is
    /// routed straight to the pane. Multiple viewers share the pane (its PTY
    /// holds one size; latest resize wins), like a shared terminal.
    Pane { pane: PaneId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct SessionInfo {
    pub id: SessionId,
    pub name: String,
    pub windows: usize,
    pub attached_clients: usize,
    pub cwd: PathBuf,
}

/// Client -> server messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum ClientMsg {
    /// Must be the first message, always JSON-encoded.
    Hello {
        proto_version: u32,
        /// Encoding for all subsequent frames (both directions).
        encoding: Encoding,
        cols: u16,
        rows: u16,
        target: AttachTarget,
        cwd: PathBuf,
    },
    Input(InputEvent),
    Resize {
        cols: u16,
        rows: u16,
    },
    Command(Command),
    /// Point this connection at a different pane, without reconnecting.
    ///
    /// Only meaningful on an [`AttachTarget::Pane`] connection, which is how a
    /// client streams the one pane it is showing. Answered with a full
    /// [`ServerMsg::Frame`] for the new pane, exactly like a fresh attach.
    ///
    /// A client that shows one pane at a time has to change which one — and
    /// without this the only way is to tear the connection down and dial again,
    /// which is what `web/butai-stage.js`'s `setPane()` does today. That is a
    /// visible stall on a link with any latency, for something that is a
    /// bookkeeping change on the daemon's side.
    ///
    /// On any other kind of connection, or for a pane that does not exist, the
    /// daemon answers [`ServerMsg::Error`] and **keeps streaming what it was**
    /// — a failed re-point should not cost you the pane you already had. Added
    /// in 0.6; a client that never sends one is unaffected, so
    /// `PROTOCOL_VERSION` is unchanged.
    Watch {
        pane: PaneId,
    },
    Detach,
    /// Something the client needs to tell the user but has nowhere to say it.
    ///
    /// The daemon composes every frame, so a client has no footer of its own —
    /// which is fine until the client is the half of the pair that failed, as
    /// when [`ServerMsg::ReadClipboardImage`] finds no image on the clipboard.
    /// Shown as a footer flash, exactly like a server-side error. Truncated to
    /// [`MAX_NOTICE_CHARS`].
    Notice(String),
}

/// Longest [`ClientMsg::Notice`] the daemon will show. A flash is one line of a
/// footer; anything past this is a client bug rather than a message.
pub const MAX_NOTICE_CHARS: usize = 200;

/// The [`ServerMsg::Detached`] reason that means *the daemon is going*, rather
/// than this pane or this workspace.
///
/// A constant on both sides rather than a literal on each, because a client
/// branches on it: everything else means the thing you were watching is gone
/// and the stage should empty, while this one means the thing you were watching
/// is very likely still there and you have merely stopped hearing about it. The
/// TUI keeps its last frame and says so; a client that ignores the distinction
/// still behaves correctly, it just cannot draw the difference.
///
/// The string is what shipped, so this names the wire rather than changing it.
pub const DETACH_SERVER_SHUTDOWN: &str = "server shutting down";

/// Server -> client messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum ServerMsg {
    /// Always JSON-encoded (it acknowledges the encoding switch).
    Hello {
        proto_version: u32,
        session: Option<SessionInfo>,
        /// The daemon's own build version (`CARGO_PKG_VERSION`), so a client can
        /// tell a *stale daemon* from a broken one.
        ///
        /// `proto_version` cannot carry this. The versioning rule is that
        /// additive changes do not bump it — [`ClientMsg::Watch`] says so in its
        /// own doc — so a daemon and a client many releases apart both report
        /// `1` and the handshake sees nothing wrong. What the user sees instead
        /// is the *consequences*: commands the daemon has never heard of, and,
        /// before the tolerance fix in [`crate::framing`]'s callers, a
        /// disconnect on every one of them.
        ///
        /// Additive and therefore free: `None` when absent, skipped when unset,
        /// so a daemon that sets it looks byte-identical to one that does not
        /// (`wire_shape_is_stable` pins that). **`None` is itself the signal** —
        /// it means the daemon predates this field, which is strictly older than
        /// the client reading it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        server_version: Option<String>,
    },
    Frame(FrameUpdate),
    SessionList(Vec<SessionInfo>),
    AgentList(Vec<String>),
    ThemeList(Vec<String>),
    /// Generic success acknowledgment for state-changing control commands.
    Ok,
    /// This connection is over, and why.
    ///
    /// **The reason is not only a message.** Almost every one of them means
    /// "there is nothing on the other end any more" — the pane closed, the
    /// workspace closed, you asked to detach — and a client should empty its
    /// stage. [`DETACH_SERVER_SHUTDOWN`] is the exception, and telling it apart
    /// is what stops a daemon restart from looking like every agent exiting at
    /// once.
    Detached {
        reason: String,
    },
    Error(String),
    /// Text the client should place on the system clipboard (copy). The TUI
    /// client emits it as an OSC 52 sequence.
    SetClipboard(String),
    /// Ask the client to ring its terminal bell (an agent needs input).
    Bell,
    /// Where [`Command::PutFile`] landed. The path has already been pasted into
    /// the pane; this is so the client can say so — a `pane`-target client has
    /// no workbench footer to read a flash out of.
    FilePut {
        path: PathBuf,
    },
    /// Read the image on your clipboard and send it back as
    /// [`Command::PutFile`] — the daemon's half of [`Command::PasteImage`].
    ///
    /// The mirror of [`ServerMsg::SetClipboard`]: that one asks the client to
    /// *write* the local clipboard, this one to read it. A client with no
    /// clipboard to read ignores it; one that looks and finds no image says so
    /// with [`ClientMsg::Notice`], because a keypress that does nothing at all
    /// is indistinguishable from a broken one.
    ReadClipboardImage,
}

// ---------------------------------------------------------------------------
// Optional conversions from crossterm (used by the built-in TUI client)
// ---------------------------------------------------------------------------

#[cfg(feature = "crossterm")]
mod crossterm_conv {
    use super::*;
    use crossterm::event as ct;

    impl KeyEvent {
        /// Convert a crossterm key event; returns `None` for keys the
        /// protocol does not carry (media keys, releases, etc.).
        pub fn from_crossterm(ev: &ct::KeyEvent) -> Option<Self> {
            if ev.kind == ct::KeyEventKind::Release {
                return None;
            }
            let code = match ev.code {
                ct::KeyCode::Char(c) => KeyCode::Char(c),
                ct::KeyCode::Enter => KeyCode::Enter,
                ct::KeyCode::Esc => KeyCode::Esc,
                ct::KeyCode::Backspace => KeyCode::Backspace,
                ct::KeyCode::Tab => KeyCode::Tab,
                ct::KeyCode::BackTab => KeyCode::BackTab,
                ct::KeyCode::Left => KeyCode::Left,
                ct::KeyCode::Right => KeyCode::Right,
                ct::KeyCode::Up => KeyCode::Up,
                ct::KeyCode::Down => KeyCode::Down,
                ct::KeyCode::Home => KeyCode::Home,
                ct::KeyCode::End => KeyCode::End,
                ct::KeyCode::PageUp => KeyCode::PageUp,
                ct::KeyCode::PageDown => KeyCode::PageDown,
                ct::KeyCode::Delete => KeyCode::Delete,
                ct::KeyCode::Insert => KeyCode::Insert,
                ct::KeyCode::F(n) => KeyCode::F(n),
                _ => return None,
            };
            let m = ev.modifiers;
            Some(KeyEvent {
                code,
                mods: KeyMods {
                    ctrl: m.contains(ct::KeyModifiers::CONTROL),
                    alt: m.contains(ct::KeyModifiers::ALT),
                    shift: m.contains(ct::KeyModifiers::SHIFT),
                },
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(v: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(v).unwrap();
        let back: T = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, v, "json roundtrip failed for {json}");
        let mp = rmp_serde::to_vec_named(v).unwrap();
        let back: T = rmp_serde::from_slice(&mp).unwrap();
        assert_eq!(&back, v, "msgpack roundtrip failed");
    }

    #[test]
    fn message_roundtrips() {
        roundtrip(&ClientMsg::Hello {
            proto_version: PROTOCOL_VERSION,
            encoding: Encoding::Json,
            cols: 80,
            rows: 24,
            target: AttachTarget::Default,
            cwd: PathBuf::from("/tmp"),
        });
        roundtrip(&ClientMsg::Input(InputEvent::Key(KeyEvent::ctrl('b'))));
        roundtrip(&ClientMsg::Command(Command::SplitPane {
            dir: SplitDir::Horizontal,
            kind: PaneKind::Terminal { command: None },
        }));
        // The image-paste round trip, both directions and both encodings. The
        // payload-free halves are the ones worth pinning: serde writes them as
        // bare strings, so they are the shape a hand-written client parser is
        // most likely to get wrong.
        roundtrip(&ClientMsg::Command(Command::PasteImage));
        roundtrip(&ClientMsg::Command(Command::PutFile {
            name: "clipboard.png".into(),
            data: b64::encode(b"\x89PNG\r\n\x1a\n"),
        }));
        roundtrip(&ClientMsg::Notice("no image on the clipboard".into()));
        roundtrip(&ServerMsg::ReadClipboardImage);
        roundtrip(&ServerMsg::FilePut { path: "/home/me/.butai/scratch/p-1/000001-x.png".into() });
        assert_eq!(
            serde_json::to_string(&ClientMsg::Command(Command::PasteImage)).unwrap(),
            r#"{"command":"paste_image"}"#
        );
        assert_eq!(
            serde_json::to_string(&ServerMsg::ReadClipboardImage).unwrap(),
            r#""read_clipboard_image""#
        );
        roundtrip(&ServerMsg::Frame(FrameUpdate {
            full: true,
            cells: vec![CellRun {
                x: 0,
                y: 0,
                cells: vec![Cell {
                    ch: "a".into(),
                    fg: Color::Indexed(2),
                    bg: Color::Rgb(10, 20, 30),
                    mods: Mods { bold: true, ..Default::default() },
                }],
            }],
            cursor: Some((3, 4)),
            cursor_shape: CursorShape::Bar,
            wants_mouse: false,
        }));
        roundtrip(&ServerMsg::SessionList(vec![SessionInfo {
            id: SessionId(1),
            name: "main".into(),
            windows: 2,
            attached_clients: 1,
            cwd: PathBuf::from("/home"),
        }]));
    }

    #[test]
    fn wire_shape_is_stable() {
        // Frozen wire shapes: a change here is a protocol break — bump
        // PROTOCOL_VERSION and update docs/protocol.md.
        let msg = ClientMsg::Input(InputEvent::Key(KeyEvent::ctrl('c')));
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"input":{"key":{"code":{"char":"c"},"mods":{"ctrl":true}}}}"#
        );
        // An ordinary frame serializes to exactly what it always did: every
        // field added since is defaulted and skipped, so a client built against
        // an older butai sees byte-identical frames.
        let msg = ServerMsg::Frame(FrameUpdate {
            full: false,
            cells: vec![],
            cursor: None,
            cursor_shape: CursorShape::default(),
            wants_mouse: false,
        });
        assert_eq!(serde_json::to_string(&msg).unwrap(), r#"{"frame":{"cells":[],"cursor":null}}"#);
        // A left click must still serialize without a `button` key, so a daemon
        // built before right-click existed sees byte-identical frames.
        let msg = ClientMsg::Input(InputEvent::MouseDown {
            x: 3,
            y: 4,
            alt: false,
            button: MouseButton::Left,
        });
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"input":{"mouse_down":{"x":3,"y":4,"alt":false}}}"#
        );
        let msg = ClientMsg::Input(InputEvent::MouseDown {
            x: 3,
            y: 4,
            alt: false,
            button: MouseButton::Right,
        });
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"input":{"mouse_down":{"x":3,"y":4,"alt":false,"button":"right"}}}"#
        );
    }

    /// `server_version` has to be free: it was added to a shipped handshake, so
    /// a daemon that sets it must look byte-identical to one that does not, and
    /// a client built before it existed must still decode a hello that has it.
    #[test]
    fn the_hello_carries_a_server_version_without_changing_its_shape() {
        let bare = ServerMsg::Hello { proto_version: 1, session: None, server_version: None };
        assert_eq!(
            serde_json::to_string(&bare).unwrap(),
            r#"{"hello":{"proto_version":1,"session":null}}"#,
            "an unset server_version must not appear on the wire at all"
        );

        // The other direction: a hello *without* the field still decodes, which
        // is what a current client does with an old daemon's greeting. `None` is
        // the signal, so it must survive as `None` rather than failing to parse.
        let old = r#"{"hello":{"proto_version":1,"session":null}}"#;
        let decoded: ServerMsg = serde_json::from_str(old).unwrap();
        assert_eq!(decoded, bare);

        let stamped = ServerMsg::Hello {
            proto_version: 1,
            session: None,
            server_version: Some("9.9.9".into()),
        };
        assert_eq!(
            serde_json::to_string(&stamped).unwrap(),
            r#"{"hello":{"proto_version":1,"session":null,"server_version":"9.9.9"}}"#
        );
    }

    #[test]
    fn mouse_down_without_button_decodes_as_left() {
        // The pre-right-click wire shape, as an older client still emits it.
        let msg: ClientMsg =
            serde_json::from_str(r#"{"input":{"mouse_down":{"x":1,"y":2}}}"#).unwrap();
        assert_eq!(
            msg,
            ClientMsg::Input(InputEvent::MouseDown {
                x: 1,
                y: 2,
                alt: false,
                button: MouseButton::Left,
            })
        );
    }

    #[test]
    fn right_click_roundtrips() {
        // rmp-serde is stricter than serde_json about struct-variant fields, so
        // pin the msgpack path too — it is what attached clients actually use.
        roundtrip(&ClientMsg::Input(InputEvent::MouseDown {
            x: 7,
            y: 9,
            alt: true,
            button: MouseButton::Right,
        }));
    }
}
