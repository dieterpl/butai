//! Client-side transports for the butai daemon.
//!
//! Everything that wants to *talk to* a daemon lives here: the TUI, the CLI's
//! one-shots, and — since remote workspaces landed in the tab bar — the daemon
//! itself, which is a client of every daemon it relays. That last consumer is
//! why this is its own crate rather than part of `butai-client`: pulling the
//! connect path out of the TUI crate means `butai-server` can reuse it without
//! taking a dependency on crossterm, ratatui's backend, and the clipboard.
//!
//! The layering that makes remote work at all: [`Transport`] is a pair of
//! message channels, so nothing above it knows what the bytes travel over, and
//! [`into_transport`] is generic over the stream, so nothing below it is tied
//! to a Unix socket. A local socket, an `ssh host butai proxy` child, and a
//! forwarded socket are then the same thing with different plumbing — see
//! [`Dial`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use butai_protocol::framing::{decode, encode, length_codec, MAX_CONSECUTIVE_BAD_FRAMES};
use butai_protocol::{AttachTarget, ClientMsg, Command, Encoding, ServerMsg, PROTOCOL_VERSION};
use futures::{SinkExt, StreamExt};
use rustix::fs::{flock, FlockOperation};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixStream;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio_util::codec::Framed;

pub use crate::dial::{ssh_transport, Dial, SshChild};

/// A connection to a daemon, reduced to two message channels.
///
/// Deliberately not a trait: the only thing a consumer needs is somewhere to
/// put a [`ClientMsg`] and somewhere to take a [`ServerMsg`] from, and keeping
/// it a plain struct is what lets `butai standalone` build one over in-process
/// channels with no socket underneath at all.
pub struct Transport {
    pub to_server: UnboundedSender<ClientMsg>,
    pub from_server: UnboundedReceiver<ServerMsg>,
}

/// Connect, spawning the daemon if none is running. Returns the raw stream.
pub async fn connect_or_spawn(socket: &Path) -> Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(socket).await {
        return Ok(stream);
    }
    spawn_daemon(socket)?;
    // The daemon needs a moment to bind; retry with backoff.
    for attempt in 0..40u32 {
        tokio::time::sleep(Duration::from_millis(50 + 10 * u64::from(attempt))).await;
        if let Ok(stream) = UnixStream::connect(socket).await {
            return Ok(stream);
        }
    }
    anyhow::bail!("daemon did not come up on {}", socket.display())
}

/// Connect to a daemon that is expected to be running already, without
/// spawning one.
///
/// The relay path wants this rather than [`connect_or_spawn`]: a forwarded
/// socket that is not answering means the tunnel is down or the far daemon is
/// gone, and starting a *local* daemon on the far end's socket path would be
/// the wrong answer to both.
pub async fn connect_existing(socket: &Path) -> Result<UnixStream> {
    UnixStream::connect(socket).await.with_context(|| format!("connect to {}", socket.display()))
}

/// Spawn `butai daemon` detached. A shared lock on the daemon's lock file
/// tells us whether one is already alive (the daemon holds it exclusively).
fn spawn_daemon(socket: &Path) -> Result<()> {
    let dir = socket.parent().context("socket path has no parent")?;
    std::fs::create_dir_all(dir)?;
    let lock_path = butai_protocol::paths::lock_path_for(socket);
    let lock_file =
        std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(&lock_path)?;
    if flock(&lock_file, FlockOperation::NonBlockingLockShared).is_err() {
        // A daemon holds the exclusive lock; it just isn't accepting yet.
        return Ok(());
    }
    // No daemon alive. Release our probe lock before spawning.
    let _ = flock(&lock_file, FlockOperation::NonBlockingUnlock);

    let exe = std::env::current_exe().context("locate butai binary")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .env("BUTAI_SOCKET", socket);
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            rustix::process::setsid().map_err(std::io::Error::from)?;
            Ok(())
        });
    }
    cmd.spawn().context("spawn butai daemon")?;
    Ok(())
}

/// Wrap a connected stream into the channel-based [`Transport`]. `encoding`
/// governs all frames after the JSON Hello exchange.
///
/// Generic over the stream because the bytes do not have to come from a socket:
/// the same pumps drive an `ssh` child's stdio (see [`ssh_transport`]), where
/// the read and write halves are two different pipes joined back together.
pub fn into_transport<S>(stream: S, encoding: Encoding) -> Transport
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let framed = Framed::new(stream, length_codec());
    let (mut sink, mut source) = framed.split();
    let (out_tx, mut out_rx) = unbounded_channel::<ClientMsg>();
    let (in_tx, in_rx) = unbounded_channel::<ServerMsg>();

    tokio::spawn(async move {
        let mut first = true;
        while let Some(msg) = out_rx.recv().await {
            let enc = if first { Encoding::Json } else { encoding };
            first = false;
            match encode(&msg, enc) {
                Ok(bytes) => {
                    if sink.send(bytes).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = sink.close().await;
    });

    tokio::spawn(async move {
        let mut first = true;
        // Symmetric with the daemon's inbound loop: a message this client does
        // not know is skipped, not fatal. `docs/protocol.md` says additive
        // changes do not bump `proto_version`, so a client talking to a *newer*
        // daemon must expect to meet a `ServerMsg` variant it has never heard
        // of, and dropping the connection over one is how a version gap becomes
        // a reconnect loop rather than a message.
        let mut consecutive_bad = 0u32;
        while let Some(frame) = source.next().await {
            let Ok(bytes) = frame else { break };
            let enc = if first { Encoding::Json } else { encoding };
            first = false;
            let msg = match decode::<ServerMsg>(&bytes, enc) {
                Ok(m) => {
                    consecutive_bad = 0;
                    m
                }
                Err(_) => {
                    consecutive_bad += 1;
                    if consecutive_bad >= MAX_CONSECUTIVE_BAD_FRAMES {
                        break;
                    }
                    continue;
                }
            };
            if in_tx.send(msg).is_err() {
                break;
            }
        }
    });

    Transport { to_server: out_tx, from_server: in_rx }
}

/// The opening frame, with the fields a caller rarely varies filled in.
///
/// Every connection sends one of these and they differ in three fields at
/// most, so building it by hand at each site was four lines of noise around
/// the one that mattered.
pub fn hello(target: AttachTarget, cols: u16, rows: u16, encoding: Encoding) -> ClientMsg {
    ClientMsg::Hello {
        proto_version: PROTOCOL_VERSION,
        encoding,
        cols,
        rows,
        target,
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
    }
}

/// One-shot control connection: send a single command, collect the reply.
pub async fn control_request(socket: &Path, command: Command) -> Result<ServerMsg> {
    let stream = connect_or_spawn(socket).await?;
    let mut framed = Framed::new(stream, length_codec());
    let hello = hello(AttachTarget::Control, 0, 0, Encoding::Json);
    framed.send(encode(&hello, Encoding::Json)?).await?;
    // Server hello first.
    let Some(Ok(bytes)) = framed.next().await else {
        anyhow::bail!("daemon closed the connection during handshake");
    };
    let _hello: ServerMsg = decode(&bytes, Encoding::Json)?;
    framed.send(encode(&ClientMsg::Command(command), Encoding::Json)?).await?;
    let reply = tokio::time::timeout(Duration::from_secs(5), framed.next())
        .await
        .context("daemon did not reply")?;
    match reply {
        Some(Ok(bytes)) => Ok(decode(&bytes, Encoding::Json)?),
        _ => anyhow::bail!("daemon closed the connection"),
    }
}
