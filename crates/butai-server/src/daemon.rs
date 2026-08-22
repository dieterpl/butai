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
    let (writer, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_ansi(false)
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
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

        serve(listener, config, Some(paths::session_state_path())).await;

        let _ = fs::remove_file(socket_path);
        drop(lock_file); // releases the flock
        info!("daemon exit");
        Ok(())
    })
}

fn restrict_dir_permissions(dir: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = fs::Permissions::from_mode(0o700);
    fs::set_permissions(dir, perms).with_context(|| format!("chmod 700 {}", dir.display()))
}

/// Accept loop + core actor. Public so integration tests can run a daemon
/// on a temp socket in-process.
pub async fn serve(listener: UnixListener, config: Config, session_store: Option<PathBuf>) {
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
    tokio::select! {
        _ = &mut core_task => {}
        _ = tokio::signal::ctrl_c() => {
            info!("SIGINT: shutting down");
            let _ = event_tx.send(Event::Shutdown);
            let _ = core_task.await;
        }
        _ = async { match sigterm.as_mut() { Some(s) => { s.recv().await; } None => std::future::pending().await } } => {
            info!("SIGTERM: shutting down");
            let _ = event_tx.send(Event::Shutdown);
            let _ = core_task.await;
        }
    }
    accept_task.abort();
}
