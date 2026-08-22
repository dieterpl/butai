//! Pane types behind one dispatch enum.
//!
//! Two kinds survive, and they divide on where their content lives. A
//! [`TerminalPane`] owns a PTY, so the daemon is the only thing that can say
//! what is on its screen. A [`GitPane`] owns *state* — a worktree's status,
//! cached because computing it is a full walk — which the daemon serves as
//! JSON and every client draws for itself.
//!
//! What the split removed: the editor, the diff view and the file tree. Each
//! was a cursor sitting in text the daemon happened to have read, and each is
//! now the client's, against `GET .../file`, `GET .../diff` and
//! `GET .../tree`. Nothing here interprets a keystroke except the terminal,
//! which forwards it to a program.

pub mod git;
pub mod term_emu;
pub mod terminal;

use butai_protocol::InputEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use self::git::GitPane;
use self::terminal::TerminalPane;

/// The lint wants the terminal variant boxed, because it is the larger of the
/// two by about 200 bytes. These live one per pane in a `HashMap`, so the whole
/// saving on a busy workspace is a few kilobytes — against an allocation and a
/// pointer hop on the path that paints every frame of every terminal. Not a
/// trade worth making, and worth saying so rather than re-deciding it each time
/// a field is added to [`TerminalPane`].
#[allow(clippy::large_enum_variant)]
pub enum PaneState {
    Terminal(TerminalPane),
    Git(GitPane),
}

impl PaneState {
    /// Deliver input, if the pane is the kind of pane input means anything to.
    ///
    /// Only a terminal is: the keystroke goes to the program on the PTY. A git
    /// pane's state is mutated through the API — staging a file is
    /// `POST .../git/apply`, not a `Space` keypress — so a key pressed over one
    /// is the client's to interpret, not ours.
    pub fn handle_input(&mut self, ev: &InputEvent) {
        if let PaneState::Terminal(t) = self {
            t.handle_input(ev);
        }
    }

    /// Paint the pane, if it is the kind of pane that has a picture.
    ///
    /// Only a terminal is. A git pane holds *state* — a worktree's status —
    /// and state crosses the wire as JSON for the client to draw. A terminal
    /// cannot: what is on its screen is the accumulated effect of every byte a
    /// program has written, and reconstructing that needs a VT emulator, which
    /// is the whole reason this side draws anything at all.
    ///
    /// `false` means there was nothing to paint, so a client asking to stream
    /// the other kind can be told rather than shown an empty grid.
    pub fn render(&mut self, buf: &mut Buffer, area: Rect) -> bool {
        match self {
            PaneState::Terminal(t) => {
                t.render(buf, area);
                true
            }
            _ => false,
        }
    }

    /// Resize the pane, if it has a size. A terminal does — the PTY's window
    /// size is something programs read and react to. A status listing does
    /// not: how many of its rows fit on a screen is the question of whichever
    /// client is drawing it, and two clients can disagree.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        if let PaneState::Terminal(t) = self {
            t.resize(rows, cols);
        }
    }

    /// Pane-relative cursor (col, row), `None` when hidden.
    pub fn cursor(&self) -> Option<(u16, u16)> {
        match self {
            PaneState::Terminal(t) => t.cursor(),
            _ => None,
        }
    }

    pub fn is_dead(&self) -> bool {
        match self {
            PaneState::Terminal(t) => t.is_dead(),
            _ => false,
        }
    }

    pub fn scroll_page(&mut self, pages: i16) {
        if let PaneState::Terminal(t) = self {
            t.scroll_page(pages);
        }
    }

    /// Scroll by lines rather than screens — what the wheel moves, as against
    /// the page a key asks for.
    pub fn scroll_lines(&mut self, lines: i32) {
        if let PaneState::Terminal(t) = self {
            t.scroll_lines(lines);
        }
    }
}
