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
///
/// ## `BUTAI_HOME`
///
/// One variable moves the whole tree, and it is the supported way to run a
/// second butai beside the one you use — a build off a feature branch, tried
/// against real work before it is merged.
///
/// Overriding `HOME` would do the same thing and is what the test suite does,
/// but it is the wrong tool for a build you mean to *use*: it takes away the
/// ssh config, the shell profile and the git identity that make the run worth
/// anything. This takes away only butai's own state, so a dev daemon gets its
/// own socket, session store, pane dumps and logs while everything that makes
/// the machine yours stays where it is.
///
/// It is read here rather than beside each path so the halves of a session
/// cannot separate: socket, config, logs, `session.json` and `panes/` all
/// derive from this one answer, and there is no combination of variables that
/// points them at two different butais.
pub fn butai_dir() -> PathBuf {
    dir_for(env_path("BUTAI_HOME"), dirs::home_dir())
}

/// [`butai_dir`] with its two inputs handed in, so the choice between them is
/// testable without writing to the process environment — which cargo runs
/// tests in parallel threads of, and which the whole crate reads.
fn dir_for(overridden: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = overridden {
        return dir;
    }
    match home {
        Some(home) => home.join(".butai"),
        None => {
            let uid = rustix::process::getuid().as_raw();
            PathBuf::from(format!("/tmp/butai-{uid}"))
        }
    }
}

/// A path from the environment, if it is set to something.
fn env_path(var: &str) -> Option<PathBuf> {
    override_from(std::env::var(var).ok())
}

/// An environment value read as an override.
///
/// Empty is *not* one. `BUTAI_HOME=` from a shell that exports it
/// unconditionally means "no opinion", and taking it literally would resolve
/// every path in this module against the working directory — a daemon whose
/// socket, session store and logs land wherever it happened to be started.
/// All three variables here have always read it that way; this is that rule
/// written once.
fn override_from(value: Option<String>) -> Option<PathBuf> {
    value.filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// `~/.butai/config.toml`.
///
/// One file, read by both sides into different structs — the daemon takes
/// `[general]`'s shell and scrollback keys plus `[[agents]]`, the client takes
/// the rest. It lives here so the two cannot disagree about *which* file.
pub fn config_path() -> PathBuf {
    butai_dir().join("config.toml")
}

/// The daemon socket path.
///
/// Four answers, in order:
///
/// 1. `--socket`, which the CLI resolves before it ever asks this.
/// 2. `BUTAI_HOME` — a whole butai, its socket included.
/// 3. `BUTAI_SOCKET` — this one socket, wherever the rest of it lives.
/// 4. `~/.butai/butai.sock`.
///
/// **`BUTAI_HOME` beating `BUTAI_SOCKET` is deliberate**, and it is the one
/// order here that is not obvious. The two are not set by the same kind of act.
/// `BUTAI_SOCKET` is exported into *every pane a daemon creates*, so every
/// command run inside butai already has one, pointing at the daemon drawing the
/// pane it is running in. Nobody typed it.
///
/// Someone who types `BUTAI_HOME=~/.butai-dev butai` in that shell is asking
/// for the other butai, and the ambient value is the single most likely thing
/// to be in their way. Reading it first is not a near miss either — it hands
/// them the exact daemon they were stepping away from: the dev daemon tries to
/// bind the real socket, refuses because it is taken, and the client attaches
/// to the real one with an unused state directory sitting beside it. Which is
/// to say it silently does the opposite of what was asked.
///
/// Setting both on purpose still works, and still means what it says — it is
/// only the *inherited* one that loses, and only to a variable that has to have
/// been typed to be there at all.
pub fn socket_path() -> PathBuf {
    socket_for(env_path("BUTAI_HOME"), env_path("BUTAI_SOCKET"), dirs::home_dir())
}

/// [`socket_path`] with its inputs handed in, for the same reason [`dir_for`]
/// takes its own: the order between these two variables is the part worth a
/// test, and the process environment is not a thing parallel tests can move.
fn socket_for(
    home_override: Option<PathBuf>,
    socket: Option<PathBuf>,
    home: Option<PathBuf>,
) -> PathBuf {
    if home_override.is_some() {
        return dir_for(home_override, home).join("butai.sock");
    }
    socket.unwrap_or_else(|| dir_for(None, home).join("butai.sock"))
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
/// `BUTAI_SESSION_FILE` overrides it on its own, for a daemon that wants a
/// different store and the same everything else. Moving a whole butai aside is
/// [`BUTAI_HOME`](butai_dir)'s job and takes this with it.
///
/// Unlike [`socket_path`], this one is *not* outranked by `BUTAI_HOME`, and the
/// difference is where the value came from. Nothing exports
/// `BUTAI_SESSION_FILE` — it is only ever there because somebody set it, so
/// there is no ambient value for `BUTAI_HOME` to have to win against. The rule
/// across this module is that `BUTAI_HOME` beats what butai exports at you and
/// yields to what you typed.
///
/// This deliberately does *not* key off `BUTAI_SOCKET`: a client auto-spawning
/// the daemon always passes the socket through the environment, so keying off
/// it put every normal session's workspace list beside the socket — under
/// `/tmp` back when that is where the socket lived, where a reboot wiped it.
pub fn session_state_path() -> PathBuf {
    env_path("BUTAI_SESSION_FILE").unwrap_or_else(|| butai_dir().join("session.json"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn butai_home_wins_over_the_home_directory() {
        // The whole point of it: a build off a branch gets its own socket,
        // session store, logs and pane dumps without giving up the real $HOME
        // and everything under it that makes the run worth doing.
        let dir = dir_for(Some(PathBuf::from("/x/.butai-dev")), Some(PathBuf::from("/home/me")));
        assert_eq!(dir, PathBuf::from("/x/.butai-dev"));
    }

    #[test]
    fn without_an_override_it_is_the_home_directory() {
        let dir = dir_for(None, Some(PathBuf::from("/home/me")));
        assert_eq!(dir, PathBuf::from("/home/me/.butai"));
    }

    #[test]
    fn with_no_home_at_all_it_is_scoped_to_this_user() {
        // Never a shared path another user could have created first.
        let dir = dir_for(None, None);
        let uid = rustix::process::getuid().as_raw();
        assert_eq!(dir, PathBuf::from(format!("/tmp/butai-{uid}")));

        // An override still wins there — the fallback is the last resort, not
        // a special case that outranks what somebody asked for.
        let dir = dir_for(Some(PathBuf::from("/x")), None);
        assert_eq!(dir, PathBuf::from("/x"));
    }

    #[test]
    fn an_empty_variable_is_not_an_override() {
        // `BUTAI_HOME=` from a profile that exports it unconditionally. Taken
        // literally it would resolve every path here against the working
        // directory, so a daemon's socket and session store would land wherever
        // it was started from.
        assert_eq!(override_from(Some(String::new())), None);
        assert_eq!(override_from(None), None);
        assert_eq!(override_from(Some("/x/.butai".into())), Some(PathBuf::from("/x/.butai")));
    }

    #[test]
    fn butai_home_outranks_an_inherited_socket() {
        // The trap this exists for. `$BUTAI_SOCKET` is exported into every pane,
        // so `BUTAI_HOME=~/.butai-dev butai` typed *inside* butai already has a
        // socket in scope pointing at the daemon drawing that pane. Reading it
        // first attaches to the very daemon the command was stepping away from,
        // and does it silently.
        let ambient = PathBuf::from("/home/me/.butai/butai.sock");
        let sock = socket_for(
            Some(PathBuf::from("/x/.butai-dev")),
            Some(ambient.clone()),
            Some(PathBuf::from("/home/me")),
        );
        assert_eq!(sock, PathBuf::from("/x/.butai-dev/butai.sock"));
    }

    #[test]
    fn without_butai_home_the_socket_variable_is_the_answer() {
        // Unchanged for everything that does not set BUTAI_HOME: the test
        // suite, a forwarded socket, `butai standalone`.
        let sock = socket_for(
            None,
            Some(PathBuf::from("/tmp/bt/.butai/butai.sock")),
            Some(PathBuf::from("/home/me")),
        );
        assert_eq!(sock, PathBuf::from("/tmp/bt/.butai/butai.sock"));

        // And with neither, the one under the home directory.
        let sock = socket_for(None, None, Some(PathBuf::from("/home/me")));
        assert_eq!(sock, PathBuf::from("/home/me/.butai/butai.sock"));
    }

    #[test]
    fn every_path_hangs_off_the_one_directory() {
        // Asserted as a shape rather than by moving the environment: the halves
        // of a session must not be able to separate, so anything added here
        // belongs under the same root.
        let root = butai_dir();
        for path in [config_path(), log_dir()] {
            assert!(path.starts_with(&root), "{} is not under {}", path.display(), root.display());
        }
        assert_eq!(lock_path_for(&root.join("butai.sock")), root.join("butai.lock"));
    }

    #[test]
    fn the_pane_dumps_follow_the_session_file() {
        // `panes/` beside `session.json`, wherever that turned out to be — the
        // two halves of a restore travel together or a dev daemon replays the
        // real session's output into its panes.
        assert_eq!(panes_dir(), session_state_path().parent().unwrap().join("panes"));
    }
}
