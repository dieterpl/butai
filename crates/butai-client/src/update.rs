//! The socket-aware half of updating butai in place.
//!
//! Everything about *what* to install — the GitHub release, which of the seven
//! artifacts belongs to this machine, the checksum, the swap and the exec —
//! lives in [`butai_update`] and is re-exported here, so a call site that says
//! `update::check()` still says `update::check()`.
//!
//! What stays is the one step that needs a daemon socket: stopping the daemon
//! before its binary is replaced. A *client* does that by asking
//! (`kill-server`, then waiting for the socket to go quiet). A daemon updating
//! itself cannot ask itself, and does it by falling out of its own event loop
//! instead — which is why this is here and not beside the rest.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Result};
use butai_protocol::{Command, ServerMsg};

pub use butai_update::*;

/// How long to wait for the old daemon to finish exiting before giving up on
/// the swap. It has a session snapshot to write first, so this is generous.
const DAEMON_EXIT_TIMEOUT: Duration = Duration::from_secs(15);

/// Stop the daemon, then put the new binary in place.
///
/// After this returns the machine is on the new build and nothing of butai is
/// running. The caller either execs into it ([`restart`]) or says so and exits.
pub async fn apply(staged: &Staged, socket: &Path) -> Result<()> {
    stop_daemon(socket).await?;
    butai_update::swap(staged)
}

/// `kill-server`, then wait for the socket to stop answering.
///
/// The wait is the part that matters. `kill-server` is acknowledged before the
/// daemon has finished writing its session snapshot and closing the socket, and
/// a new client that connects in that window attaches to the *old* build on its
/// way out — the version skew this whole feature exists to end.
///
/// Public because the workbench stops a daemon for the other reason: not to
/// replace the binary, but because the one running is already a different build
/// from the client. Same window, same trap.
pub async fn stop_daemon(socket: &Path) -> Result<()> {
    // Nothing listening: there is no daemon to stop, and asking would start
    // one, because `control_request` connects-or-spawns like every other
    // client call.
    if tokio::net::UnixStream::connect(socket).await.is_err() {
        return Ok(());
    }

    match crate::conn::control_request(socket, Command::KillServer).await {
        // `Ok` and the detach acknowledgement both mean it heard us; which one
        // arrives depends on how far the shutdown got before it answered.
        Ok(ServerMsg::Ok | ServerMsg::Detached { .. }) => {}
        Ok(ServerMsg::Error(e)) => bail!("stopping the daemon: {e}"),
        Ok(other) => bail!("stopping the daemon: unexpected reply: {other:?}"),
        Err(e) => return Err(e.context("stop the daemon")),
    }

    let deadline = tokio::time::Instant::now() + DAEMON_EXIT_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if tokio::net::UnixStream::connect(socket).await.is_err() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!(
        "the daemon did not stop within {}s — nothing has been changed; \
         `butai kill-server` and try again",
        DAEMON_EXIT_TIMEOUT.as_secs()
    )
}
