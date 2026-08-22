//! `butai standalone`: a daemon and a workbench in one process lifetime, on a
//! socket nobody else can find.
//!
//! It used to bridge the two with in-memory channels, which made it the one
//! configuration whose message flow was not a socket's. That was defensible
//! while the TUI's only input was a stream of composed frames — the channels
//! carried exactly what a socket would. It stopped being defensible when the
//! client became an ordinary API client: a workbench now needs REST and an
//! event stream as well as a pane connection, and there is no honest way to
//! serve three of those over one pair of channels.
//!
//! So it binds a real socket in a private directory and runs the same client
//! against it that `butai` runs against `~/.butai/butai.sock`. The point of the
//! mode is unchanged — nothing outside this process can reach it, and it all
//! goes away together — but it is now the *same* code path as everything else
//! rather than a parallel one that has to be kept in step.

use std::path::PathBuf;

use anyhow::{Context, Result};
use butai_protocol::AttachTarget;
use butai_server::config::Config;
use tokio::net::UnixListener;

pub fn run(target: AttachTarget) -> Result<()> {
    // No shared socket, so there is no "different daemon" this could
    // legitimately be nested inside.
    butai_client::guard_against_nesting(None)?;
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let dir = private_dir()?;
        let socket = dir.path().join("butai.sock");
        let (config, warnings) = Config::load();
        for w in &warnings {
            eprintln!("butai: config: {w}");
        }

        let listener =
            UnixListener::bind(&socket).with_context(|| format!("bind {}", socket.display()))?;
        // No session store: a standalone workbench is deliberately not
        // persisted, so the next one starts empty rather than reopening
        // whatever this one happened to have.
        let daemon = tokio::spawn(butai_server::daemon::serve(listener, config, None));

        let reason = butai_client::run_client_on(&socket, target)?;
        println!("[butai: {reason}]");
        daemon.abort();
        Ok(())
    })
}

/// A directory only this user can read, holding one socket for one run.
///
/// `0700` on the directory is the same protection the shared socket has; the
/// pid in the name is what keeps two standalone sessions apart.
fn private_dir() -> Result<TempDir> {
    let base =
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let path = base.join(format!("butai-standalone-{}", std::process::id()));
    std::fs::create_dir_all(&path).with_context(|| format!("create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(TempDir(path))
}

/// Removes its directory on the way out, however that happens.
struct TempDir(PathBuf);

impl TempDir {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}
