//! Owning the real terminal: raw mode, the alternate screen, mouse tracking,
//! and putting all of it back afterwards.
//!
//! What is left after the client stopped being a frame-blitter. This file used
//! to be the TUI — an input thread, a message loop and a painter for the cell
//! runs the daemon composed — and all three moved: input and the loop into
//! [`crate::workbench`], the painting into [`crate::chrome`]. What remains is
//! the part that is genuinely about *this terminal* rather than about butai, and
//! it is shared by everything that draws.

use std::io::{self, Write};
use std::sync::{Once, OnceLock};

use anyhow::{Context, Result};
use crossterm::{cursor, event, execute, terminal};

use crate::term;

/// Restores the terminal even on panic or error paths. Signals are covered
/// separately, by [`crate::term`].
pub(crate) struct TerminalGuard;

impl TerminalGuard {
    pub(crate) fn enter() -> Result<Self> {
        // Before raw mode: `install` saves the settings it will hand back, and
        // those are the cooked ones.
        term::install();
        terminal::enable_raw_mode().context("enable raw mode")?;
        let mut out = io::stdout();
        execute!(out, terminal::EnterAlternateScreen, event::EnableBracketedPaste, cursor::Hide,)?;
        // Not crossterm's EnableMouseCapture — see [`term::ENABLE`].
        out.write_all(term::ENABLE)?;
        out.flush()?;
        install_panic_hook();
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// The thread that entered the guard, i.e. the one painting frames.
static UI_THREAD: OnceLock<std::thread::ThreadId> = OnceLock::new();

fn install_panic_hook() {
    let _ = UI_THREAD.set(std::thread::current().id());
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let was_live = term::is_armed();
            // Leave the alternate screen first, so the panic message lands on
            // the normal screen where it can actually be read.
            restore_terminal();
            default_hook(info);
            // Unwinding only unwinds the panicking thread. If that isn't the
            // one driving the UI — the input thread, a tokio worker — the main
            // loop would carry on painting into the terminal we just handed
            // back to the shell, so take the whole process down instead.
            if was_live && UI_THREAD.get() != Some(&std::thread::current().id()) {
                std::process::exit(101); // rustc's exit code for a panic
            }
        }));
    });
}

/// Idempotent: the panic hook calls this and then unwinding drops the guard,
/// which calls it again.
fn restore_terminal() {
    let mut out = io::stdout();
    // A superset of what we turned on, so a terminal left half-configured by
    // an earlier butai comes back clean too. Subsumes DisableMouseCapture,
    // DisableBracketedPaste, LeaveAlternateScreen and cursor::Show.
    let _ = out.write_all(term::RESTORE);
    let _ = out.flush();
    let _ = terminal::disable_raw_mode();
    term::disarm();
}

/// Put `text` on the terminal's clipboard with OSC 52.
///
/// Not arboard: this has to work over ssh and with no display server, which is
/// where the TUI lives. The terminal emulator is the one with a desktop
/// attached, and it forwards the sequence to the real clipboard.
///
/// Inside tmux the bare sequence is not enough, and this is the common case
/// rather than an exotic one: tmux only honours an application's OSC 52 when
/// `set-clipboard` is `on`, and the default — on this machine's tmux 3.0a and
/// on every release since — is `external`, which drops it. Not forwarded, not
/// even kept as a paste buffer. So a copy out of butai under stock tmux went
/// nowhere at all, silently, while the footer said "copied 1 line".
///
/// The way past it is tmux's DCS passthrough, which relays the wrapped bytes to
/// the outer terminal whatever `set-clipboard` says. The plain sequence goes out
/// too: it is what works outside tmux, it is what a `set-clipboard on` config
/// wants (that also files the text as a tmux paste buffer), and a clipboard set
/// twice with the same text is the same clipboard.
pub fn set_clipboard(text: &str) -> Result<()> {
    let mut out = io::stdout().lock();
    let b64 = butai_protocol::b64::encode(text.as_bytes());
    let osc = format!("\x1b]52;c;{b64}\x07");
    out.write_all(osc.as_bytes())?;
    if std::env::var_os("TMUX").is_some() {
        out.write_all(tmux_passthrough(&osc).as_bytes())?;
    }
    out.flush()?;
    Ok(())
}

/// Wrap a sequence so tmux hands it to the terminal it is drawing on.
///
/// `ESC P tmux; <payload> ESC \`, with every `ESC` in the payload doubled —
/// that is how tmux tells the end of the payload from an escape inside it.
///
/// tmux 3.3 put this behind `allow-passthrough`, which is off by default, so on
/// a newer tmux than this machine's the wrapped copy is dropped and the plain
/// one alongside it is what has to land. That is the config where
/// `set-clipboard on` is still worth setting.
fn tmux_passthrough(seq: &str) -> String {
    format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The doubling is the whole trick: an OSC 52 payload starts with an `ESC`
    /// of its own, and a single one ends tmux's passthrough right there — so an
    /// unescaped wrap sends tmux the two bytes `ESC P tmux;` and then leaks the
    /// clipboard sequence into the pane as text.
    #[test]
    fn the_passthrough_wrapper_doubles_every_escape() {
        assert_eq!(
            tmux_passthrough("\x1b]52;c;aGk=\x07"),
            "\x1bPtmux;\x1b\x1b]52;c;aGk=\x07\x1b\\"
        );
    }
}
