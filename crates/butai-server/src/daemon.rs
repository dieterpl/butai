//! Daemon lifecycle: flock single-instance guard, socket bind, accept loop,
//! signal handling, file logging.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use crate::config::Config;
use anyhow::{Context, Result};
use butai_protocol::paths;
use rustix::fs::{flock, FlockOperation};
use tokio::net::UnixListener;
use tokio::sync::mpsc::unbounded_channel;
use tracing::info;

use crate::core::{CoreMode, Event, ServerCore};

/// Foreground daemon entry point (the `butai daemon` subcommand). Blocks
/// until the last session dies or a termination signal arrives.
pub fn run(socket_path: &Path) -> Result<()> {
    let log_dir = paths::log_dir();
    fs::create_dir_all(&log_dir).ok();
    let file_appender = tracing_appender::rolling::daily(&log_dir, "daemon.log");
    let (writer, log_guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_ansi(false)
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let restart_into = rt.block_on(async move {
        let dir = socket_path.parent().context("socket path has no parent")?;
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        restrict_dir_permissions(dir)?;

        // Single-instance guard: hold an exclusive flock for our lifetime.
        let lock_path = paths::lock_path_for(socket_path);
        let lock_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| format!("open {}", lock_path.display()))?;
        flock(&lock_file, FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| anyhow::anyhow!("another butai daemon is already running"))?;

        // We hold the lock: any existing socket file is stale.
        let _ = fs::remove_file(socket_path);
        let listener = UnixListener::bind(socket_path)
            .with_context(|| format!("bind {}", socket_path.display()))?;
        info!("daemon listening on {}", socket_path.display());

        let (config, warnings) = Config::load();
        for w in warnings {
            tracing::warn!("config: {w}");
        }

        let restart_into = serve(listener, config, Some(paths::session_state_path())).await;

        let _ = fs::remove_file(socket_path);
        drop(lock_file); // releases the flock
        match &restart_into {
            Some(staged) => info!("update: installing {} and restarting", staged.version),
            None => info!("daemon exit"),
        }
        Ok::<_, anyhow::Error>(restart_into)
    })?;

    // Outside the runtime, and after the log guard goes: `tracing_appender`'s
    // non-blocking writer is drained by dropping its guard, and `exec` drops
    // nothing — so the line above, which is the whole record of an unattended
    // update, would never reach the file.
    drop(rt);
    drop(log_guard);

    // Nothing of this daemon is left holding the socket, the lock or the log,
    // and the session snapshot is on disk. Only now can the binary be replaced
    // and this process become the new one.
    //
    // `restart` execs with our own arguments, so a daemon started as
    // `butai --socket X daemon` comes back on the same socket. It does not
    // return; an error means the exec itself failed, and by then the swap has
    // happened, so the next plain `butai` starts the new build and restores the
    // session.
    if let Some(staged) = restart_into {
        let install = staged.install_path().to_path_buf();
        butai_update::swap(&staged)?;
        return Err(butai_update::restart(&install));
    }
    Ok(())
}

fn restrict_dir_permissions(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = fs::Permissions::from_mode(0o700);
    fs::set_permissions(dir, perms).with_context(|| format!("chmod 700 {}", dir.display()))
}

/// How long to let in-flight replies reach their clients before this process
/// is replaced.
///
/// Only spent on the update path, and only because of one reply: the 202 for
/// `POST /v1/update` is written by a hyper task, and `exec` does not wait for
/// it. Everything else about the shutdown is already ordered — clients are
/// detached and the session is snapshotted before the core loop returns.
const RESTART_GRACE: std::time::Duration = std::time::Duration::from_millis(300);

/// Accept loop + core actor. Public so integration tests can run a daemon
/// on a temp socket in-process.
///
/// Returns the binary this daemon stopped in order to become, when it stopped
/// for a self-update — see [`run`], which is the only caller that acts on it.
/// `butai standalone` ignores it, and turns the request off in its config
/// rather than relying on that.
pub async fn serve(
    listener: UnixListener,
    config: Config,
    session_store: Option<PathBuf>,
) -> Option<butai_update::Staged> {
    let (event_tx, event_rx) = unbounded_channel::<Event>();
    // PTY output rides a separate bounded channel so a flooding pane throttles
    // its own reader thread instead of burying control events on `event_tx`.
    let (output_tx, output_rx) = tokio::sync::mpsc::channel(crate::core::OUTPUT_CHANNEL_CAP);
    // Taken before the core swallows the config: the usage sampler probes the
    // same launchers the core spawns, and both need their own copy.
    let (agents, budgets) = (config.agents.clone(), config.budgets.clone());
    let mut core = ServerCore::new(config, event_tx.clone(), output_tx, CoreMode::Daemon);
    // Panes are handed this as `$BUTAI_SOCKET`, so it has to be the path the
    // listener is really on rather than whatever the daemon's own environment
    // happened to say.
    if let Some(path) =
        listener.local_addr().ok().and_then(|a| a.as_pathname().map(Path::to_path_buf))
    {
        core.set_socket(path);
    }
    if let Some(path) = session_store {
        core.set_session_store(path);
    }
    // After the session store, so configured hosts' tabs land after the
    // restored local ones — the same order the bar draws them in.
    let core_task = tokio::spawn(core.run(event_rx, output_rx));
    crate::sys::spawn_sampler(event_tx.clone());
    crate::usage::spawn_sampler(event_tx.clone(), agents, budgets);
    crate::sys::spawn_ticker(event_tx.clone());
    crate::sys::spawn_fast_ticker(event_tx.clone());

    let accept_tx = event_tx.clone();
    let accept_task = tokio::spawn(async move {
        let mut next_client: u64 = 1;
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let id = next_client;
                    next_client += 1;
                    let tx = accept_tx.clone();
                    tokio::spawn(crate::client_conn::handle_connection(stream, id, tx));
                }
                Err(e) => {
                    tracing::warn!("accept failed: {e}");
                    break;
                }
            }
        }
    });

    // Wait for the core to finish (last session died / kill-server), or for
    // a termination signal, whichever first.
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    tokio::pin!(core_task);
    let restart_into = tokio::select! {
        r = &mut core_task => r.ok().flatten(),
        _ = tokio::signal::ctrl_c() => {
            info!("SIGINT: shutting down");
            let _ = event_tx.send(Event::Shutdown);
            core_task.await.ok().flatten()
        }
        _ = async { match sigterm.as_mut() { Some(s) => { s.recv().await; } None => std::future::pending().await } } => {
            info!("SIGTERM: shutting down");
            let _ = event_tx.send(Event::Shutdown);
            core_task.await.ok().flatten()
        }
    };
    accept_task.abort();
    // The client that asked for the update is still waiting on its 202, on a
    // connection this process is about to `exec` out from under. Held here
    // rather than before the `exec` so the accept loop is already down and no
    // new client can arrive into the gap.
    if restart_into.is_some() {
        tokio::time::sleep(RESTART_GRACE).await;
    }
    restart_into
}
