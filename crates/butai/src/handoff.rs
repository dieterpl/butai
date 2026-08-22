//! Typing `butai` after `ssh`, and having that machine's projects appear in the
//! tab bar you already had.
//!
//! The problem is that a `butai` on the far end of an ssh session has no way to
//! know it is inside a butai pane. `$BUTAI` does not survive ssh (it would need
//! `SendEnv`/`AcceptEnv` on both sides), and there is no other environment
//! channel. What *does* survive is the terminal: the pane's PTY is the far
//! side's stdout, and butai's daemon parses every byte a pane writes in order to
//! answer terminal queries it would otherwise block on.
//!
//! So the handshake is a terminal query. butai answers Secondary DA with `98`
//! (`b`) in the identifying field — the way tmux answers with `84` (`T`) — and
//! that is the whole detection:
//!
//! 1. Far side writes `ESC[>c` and reads the answer.
//! 2. **Every** terminal answers DA2, so the far side learns yes *or* no
//!    promptly. This is why it is DA2 rather than a private query with a
//!    private reply: a terminal that is not butai would simply not answer one of
//!    those, and every non-butai `ssh host` + `butai` would pay the full timeout.
//! 3. If the answer is butai's, the far side writes a one-way APC saying where
//!    it is, prints a line, and exits. The near daemon sees the APC in the
//!    pane's output and dials back.
//!
//! Nothing is written to the terminal until the gate in [`should_probe`] passes,
//! and the APC is only ever written after a *confirmed* butai answer — so a
//! plain terminal sees one DA2 query, which is invisible, and nothing else.

use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use rustix::termios::{self, OptionalActions, Termios};

/// `Pp` in the DA2 reply that identifies a butai pane. Must match
/// `butai_server::pane::terminal::DA2_BUTAI_ID`; the test at the bottom of this
/// file is what keeps the two from drifting.
const BUTAI_DA2_PREFIX: &[u8] = b"\x1b[>98;";

/// Give up waiting for a DA2 answer after this long.
///
/// Only reached when the terminal answers nothing at all, since any real one
/// answers in milliseconds and the read returns the moment bytes arrive. It is
/// still a network round trip — the query goes out through ssh and the answer
/// comes back — so it is sized for a bad link rather than a LAN.
const DA2_TIMEOUT: Duration = Duration::from_millis(1500);

/// Whether to probe at all.
///
/// Both conditions matter. Without a tty there is no terminal to ask. Without
/// `$SSH_CONNECTION` we are not on the far end of anything, so even a positive
/// answer would leave nothing to dial back to — and that is the common case, so
/// gating on it means a local `butai` never pays for this.
fn should_probe() -> bool {
    if std::env::var_os("BUTAI_NO_HANDOFF").is_some_and(|v| !v.is_empty() && v != "0") {
        return false;
    }
    if !std::env::var("SSH_CONNECTION").is_ok_and(|v| !v.trim().is_empty()) {
        return false;
    }
    termios::isatty(std::io::stdin()) && termios::isatty(std::io::stdout())
}

/// Detect an enclosing butai and hand this machine to it.
///
/// Returns `true` when the announcement went out and the caller should exit
/// instead of starting a TUI.
pub fn try_handoff(socket: &Path) -> bool {
    if !should_probe() {
        return false;
    }
    if !ask_terminal_is_butai() {
        return false;
    }
    let Some(hint) = dial_back_hint() else { return false };
    // One-way: there is nothing to wait for. If the near daemon cannot reach us
    // it says so on its own footer, which the user is looking at.
    // Announced under the current name; the near daemon accepts every name this
    // program has shipped under, so an older one on this side is still heard.
    let announce =
        format!("\x1b_{};here;{hint};{}\x1b\\", butai_protocol::names::BINARY, socket.display());
    let mut out = std::io::stdout();
    if out.write_all(announce.as_bytes()).is_err() || out.flush().is_err() {
        return false;
    }
    println!("[butai: opened in your local butai — this machine's projects are in its tab bar]");
    true
}

/// Ask the terminal who it is, and wait for the answer.
fn ask_terminal_is_butai() -> bool {
    let stdin = std::io::stdin();
    // Raw mode so the answer arrives unbuffered and, crucially, unechoed: DA2
    // replies as printable text, and a terminal echoing it would paint
    // `[>98;1;0c` across the user's screen.
    let Ok(saved) = enter_raw(&stdin) else { return false };
    let answer = query_da2(&stdin);
    let _ = termios::tcsetattr(&stdin, OptionalActions::Now, &saved);
    answer
}

fn enter_raw(stdin: &std::io::Stdin) -> rustix::io::Result<Termios> {
    let saved = termios::tcgetattr(stdin)?;
    let mut raw = saved.clone();
    raw.make_raw();
    // A read returns as soon as anything is available, or after 100ms with
    // nothing; the loop above decides when to give up overall.
    raw.special_codes[termios::SpecialCodeIndex::VMIN] = 0;
    raw.special_codes[termios::SpecialCodeIndex::VTIME] = 1;
    termios::tcsetattr(stdin, OptionalActions::Now, &raw)?;
    Ok(saved)
}

/// Write `ESC[>c` and look for butai's signature in the reply.
fn query_da2(stdin: &std::io::Stdin) -> bool {
    let mut out = std::io::stdout();
    if out.write_all(b"\x1b[>c").is_err() || out.flush().is_err() {
        return false;
    }
    let mut seen: Vec<u8> = Vec::with_capacity(64);
    let deadline = Instant::now() + DA2_TIMEOUT;
    let mut handle = stdin.lock();
    let mut chunk = [0u8; 64];
    while Instant::now() < deadline {
        let n = match handle.read(&mut chunk) {
            Ok(0) => continue, // VTIME expiry; the deadline decides when to stop
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        };
        seen.extend_from_slice(&chunk[..n]);
        // A DA2 reply ends in `c`. Waiting for the terminator rather than
        // matching the prefix alone means a reply split across two reads is
        // still judged once, and completely.
        if let Some(end) = seen.iter().position(|b| *b == b'c') {
            return seen[..=end].windows(BUTAI_DA2_PREFIX.len()).any(|w| w == BUTAI_DA2_PREFIX);
        }
        // Cap the buffer: a terminal that streams something other than a DA2
        // reply must not make us grow without bound.
        if seen.len() > 1024 {
            return false;
        }
    }
    false
}

/// `user@host` for the near side to fall back on.
///
/// `$SSH_CONNECTION` is `client-ip client-port server-ip server-port`, so the
/// third field is the address the client reached us on — the best guess this
/// side can make about how to be reached again. It is only a fallback: the near
/// daemon prefers the `ssh` command line its own pane is running, which is
/// correct even when this address is a NAT-internal one.
fn dial_back_hint() -> Option<String> {
    let conn = std::env::var("SSH_CONNECTION").ok()?;
    let server_ip = conn.split_whitespace().nth(2)?;
    if server_ip.is_empty() {
        return None;
    }
    let user = std::env::var("USER").ok().filter(|u| !u.trim().is_empty());
    Some(match user {
        Some(user) => format!("{user}@{server_ip}"),
        None => server_ip.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two halves of the handshake live in different crates and are matched
    /// by a magic number; this is the only thing stopping one from moving.
    #[test]
    fn da2_signature_matches_what_the_daemon_answers() {
        let expected = format!("\x1b[>{};", butai_server::pane::terminal::DA2_BUTAI_ID);
        assert_eq!(BUTAI_DA2_PREFIX, expected.as_bytes());
    }

    #[test]
    fn hint_prefers_user_at_server_address() {
        // `SSH_CONNECTION` is client-ip client-port server-ip server-port.
        temp_env(
            &[("SSH_CONNECTION", Some("10.0.0.9 51234 10.0.0.5 22")), ("USER", Some("paul"))],
            || {
                assert_eq!(dial_back_hint().as_deref(), Some("paul@10.0.0.5"));
            },
        );
    }

    #[test]
    fn hint_without_a_user_is_just_the_address() {
        temp_env(&[("SSH_CONNECTION", Some("10.0.0.9 51234 10.0.0.5 22")), ("USER", None)], || {
            assert_eq!(dial_back_hint().as_deref(), Some("10.0.0.5"));
        });
    }

    #[test]
    fn no_ssh_connection_means_no_hint() {
        temp_env(&[("SSH_CONNECTION", None)], || assert_eq!(dial_back_hint(), None));
    }

    #[test]
    fn a_truncated_ssh_connection_is_not_guessed_at() {
        temp_env(&[("SSH_CONNECTION", Some("10.0.0.9 51234")), ("USER", Some("paul"))], || {
            assert_eq!(dial_back_hint(), None);
        });
    }

    /// Set env vars, run, restore. Tests touching the environment must not run
    /// concurrently with each other, so they share one lock.
    fn temp_env(vars: &[(&str, Option<&str>)], f: impl FnOnce()) {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(String, Option<String>)> =
            vars.iter().map(|(k, _)| ((*k).to_string(), std::env::var(k).ok())).collect();
        for (k, v) in vars {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        f();
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(&k, v),
                None => std::env::remove_var(&k),
            }
        }
    }
}
