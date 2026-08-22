//! Filesystem locations. One daemon per user, and one directory holding
//! everything that daemon reads or writes outside a project: see [`butai_dir`].

use std::path::PathBuf;

/// Root of everything butai stores — `~/.butai`:
///
/// ```text
/// ~/.butai/config.toml    user config
/// ~/.butai/themes/        user themes
/// ~/.butai/logs/          daemon logs, rotated daily
/// ~/.butai/session.json   open workspaces, restored on restart
/// ~/.butai/panes/         per-pane output dumps, replayed on restart
/// ~/.butai/scratch/       files pasted in from a client (images), per workspace
/// ~/.butai/butai.sock     daemon socket
/// ~/.butai/butai.lock     spawn-race lock
/// ```
///
/// One home-relative directory rather than the XDG split (`~/.config/butai`,
/// `~/.local/state/butai`, `$XDG_RUNTIME_DIR/butai`): `$XDG_RUNTIME_DIR` is set
/// for a login shell but routinely absent from a non-interactive `ssh host
/// butai ...`, so the socket moved between the two and a remote client would
/// spawn a second, empty daemon instead of attaching to the running one.
///
/// The daemon chmods this `0700` when it binds the socket. Without a home
/// directory to resolve, falls back to a uid-scoped directory under `/tmp` —
/// never a shared path another user could have created first.
pub fn butai_dir() -> PathBuf {
    match dirs::home_dir() {
        Some(home) => home.join(".butai"),
        None => {
            let uid = rustix::process::getuid().as_raw();
            PathBuf::from(format!("/tmp/butai-{uid}"))
        }
    }
}

/// `~/.butai/config.toml`.
///
/// One file, read by both sides into different structs — the daemon takes
/// `[general]`'s shell and scrollback keys plus `[[agents]]`, the client takes
/// the rest. It lives here so the two cannot disagree about *which* file.
pub fn config_path() -> PathBuf {
    butai_dir().join("config.toml")
}

/// The daemon socket path; `BUTAI_SOCKET` overrides (tests, and a second daemon
/// run deliberately alongside the real one).
pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("BUTAI_SOCKET") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    butai_dir().join("butai.sock")
}

/// Lock file guarding daemon spawn races; lives beside the socket.
pub fn lock_path_for(socket: &std::path::Path) -> PathBuf {
    socket.with_extension("lock")
}

/// Daemon log file location.
pub fn log_dir() -> PathBuf {
    butai_dir().join("logs")
}

/// Persisted list of open workspaces, restored when the daemon restarts.
///
/// `BUTAI_SESSION_FILE` overrides it, matching the `BUTAI_THEME_DIR` convention:
/// an alternate daemon (tests, experiments) sets it to keep its own store
/// instead of clobbering the real one. This deliberately does *not* key off
/// `BUTAI_SOCKET`: a client auto-spawning the daemon always passes the socket
/// through the environment, so keying off it put every normal session's
/// workspace list beside the socket — under `/tmp` back when that is where the
/// socket lived, where a reboot wiped it.
pub fn session_state_path() -> PathBuf {
    if let Ok(p) = std::env::var("BUTAI_SESSION_FILE") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    butai_dir().join("session.json")
}

/// Directory holding per-pane output dumps, one subdirectory per persisted
/// workspace. Replayed into the fresh panes when the daemon restarts.
///
/// Derived from [`session_state_path`] rather than from [`butai_dir`] directly,
/// so the two halves of a restore always travel together: an alternate daemon
/// pointing `BUTAI_SESSION_FILE` at its own store gets its own dumps beside it
/// instead of replaying the real session's output into its panes.
pub fn panes_dir() -> PathBuf {
    let session = session_state_path();
    match session.parent() {
        Some(dir) => dir.join("panes"),
        None => butai_dir().join("panes"),
    }
}
