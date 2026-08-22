//! The command tree and its dispatch.
//!
//! Commands fall into three groups, and which transport they use follows from
//! which group they are in:
//!
//! * **Attaching** (`new`, `attach`, bare `butai`) — hand off to `butai-client`,
//!   which speaks the framed protocol and owns the terminal.
//! * **Structured control** (`workspace`, `pane`, `agent`, `process`) — the REST
//!   face over the same socket, via [`butai_client::api`]. This is also the surface an
//!   agent running *inside* a pane shells out to; see `skills/butai/SKILL.md`.
//! * **Process modes** (`daemon`, `proxy`, `reset`, `standalone`) — no daemon
//!   conversation at all; they *are* the process.
//!
//! The legacy one-shots (`ls`, `kill-session`, `kill-server`) predate the REST
//! face and stay on the framed control path: their output is a documented
//! contract the test suite drives, and moving them would change it for no gain.

use std::path::PathBuf;

use anyhow::Result;
use butai_protocol::{AttachTarget, Command, ServerMsg};
use clap::{Parser, Subcommand};

use crate::out::Out;
use butai_client::api::Api;

pub mod agent;
pub mod pane;
pub mod process;
pub mod workspace;

#[derive(Parser)]
#[command(
    name = "butai",
    version,
    about = "A terminal multiplexer with built-in editor, git tooling, and agent panes"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Cmd>,

    /// Emit JSON instead of human-readable text.
    ///
    /// The daemon's own response body is passed through unmodified, so this is
    /// exactly what the REST API returns.
    #[arg(long, global = true)]
    pub json: bool,

    /// Daemon socket to talk to.
    #[arg(long, global = true, env = "BUTAI_SOCKET", value_name = "PATH")]
    pub socket: Option<PathBuf>,

    /// Workspace to act in, by id or name.
    ///
    /// Defaults to `$BUTAI_WORKSPACE`, which every pane carries — so a command
    /// run inside butai acts on its own workspace without being told which.
    #[arg(short = 'w', long = "ws", global = true, env = "BUTAI_WORKSPACE", value_name = "WS")]
    pub ws: Option<String>,

    /// Print nothing on success; the exit code is the answer.
    #[arg(short, long, global = true)]
    pub quiet: bool,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Create a new session and attach to it
    New {
        /// Session name
        #[arg(short = 's', long)]
        session: Option<String>,
    },
    /// Attach to an existing session
    Attach {
        /// Target session name (most recent when omitted)
        #[arg(short = 't', long)]
        target: Option<String>,
    },
    /// List sessions
    Ls,
    /// Inspect and manage workspaces
    #[command(subcommand, visible_alias = "ws")]
    Workspace(workspace::WsCmd),
    /// Inspect, read and drive panes
    #[command(subcommand)]
    Pane(pane::PaneCmd),
    /// Inspect, drive and wait on agents
    #[command(subcommand)]
    Agent(agent::AgentCmd),
    /// Inspect and start managed processes
    #[command(subcommand, visible_alias = "proc")]
    Process(process::ProcCmd),
    /// Report which pane and workspace this command is running in
    Whoami,
    /// Kill a session
    KillSession {
        #[arg(short = 't', long)]
        target: String,
    },
    /// Kill the server and all sessions. The open workspaces are remembered, so
    /// the next start comes back to them
    KillServer {
        /// Forget the remembered workspaces too, so the next start comes up
        /// empty
        #[arg(long)]
        clear: bool,
    },
    /// Run the daemon in the foreground (normally spawned automatically)
    Daemon,
    /// Bridge stdin/stdout to the daemon socket (for `ssh host butai proxy`)
    Proxy,
    /// Put a terminal left in mouse/raw mode by a killed or crashed butai back
    /// to normal
    Reset,
    /// Run a single-process session without the daemon (no detach support)
    Standalone,
}

/// What a structured-control command needs: somewhere to talk, somewhere to
/// print, and the default workspace scope.
pub struct Ctx {
    pub api: Api,
    pub out: Out,
    pub ws: Option<String>,
}

pub fn run(cli: Cli) -> Result<u8> {
    // Destructured up front so the global flags stay usable after `command` is
    // moved into the match.
    let Cli { command, json, socket, ws, quiet } = cli;
    let socket = socket.unwrap_or_else(butai_protocol::paths::socket_path);
    let ctx_for = |socket| Ctx { api: Api::new(socket), out: Out::new(json, quiet), ws };
    match command {
        // Bare `butai`, and only bare `butai`: over ssh from inside a pane this
        // hands the machine to the butai you are already looking at, instead of
        // drawing a second workbench inside the first. An explicit `attach` or
        // `new` is a request for a TUI *here*, and is left alone.
        None if crate::handoff::try_handoff(&socket) => Ok(crate::exit::OK),
        None => butai_client::run_client(&socket, AttachTarget::Default).map(|_| crate::exit::OK),
        Some(Cmd::New { session }) => {
            butai_client::run_client(&socket, AttachTarget::New { name: session, layout: None })
                .map(|_| crate::exit::OK)
        }
        Some(Cmd::Attach { target: Some(name) }) => {
            butai_client::run_client(&socket, AttachTarget::Attach { name })
                .map(|_| crate::exit::OK)
        }
        Some(Cmd::Attach { target: None }) => {
            butai_client::run_client(&socket, AttachTarget::Default).map(|_| crate::exit::OK)
        }
        Some(Cmd::Ls) => list_sessions(&socket, &Out::new(json, quiet)).map(|_| crate::exit::OK),
        Some(Cmd::KillSession { target }) => {
            control(&socket, Command::KillSession(target)).map(|_| crate::exit::OK)
        }
        Some(Cmd::KillServer { clear }) => {
            let cmd = if clear { Command::KillServerClear } else { Command::KillServer };
            control(&socket, cmd).map(|_| crate::exit::OK)
        }
        Some(Cmd::Workspace(cmd)) => {
            let ctx = ctx_for(socket);
            block_on(async move { workspace::run(cmd, &ctx).await.map(|_| crate::exit::OK) })
        }
        Some(Cmd::Pane(cmd)) => {
            let ctx = ctx_for(socket);
            block_on(async move { pane::run(cmd, &ctx).await })
        }
        Some(Cmd::Agent(cmd)) => {
            let ctx = ctx_for(socket);
            block_on(async move { agent::run(cmd, &ctx).await })
        }
        Some(Cmd::Process(cmd)) => {
            let ctx = ctx_for(socket);
            block_on(async move { process::run(cmd, &ctx).await })
        }
        Some(Cmd::Whoami) => whoami(&Out::new(json, quiet), &socket).map(|_| crate::exit::OK),
        Some(Cmd::Daemon) => butai_server::run_daemon(&socket).map(|_| crate::exit::OK),
        Some(Cmd::Proxy) => crate::proxy::run(&socket).map(|_| crate::exit::OK),
        // Stands alone on purpose: no nesting guard, no daemon, so it works
        // from whatever shell the wedged terminal left you in.
        Some(Cmd::Reset) => butai_client::term::reset_terminal().map(|_| crate::exit::OK),
        Some(Cmd::Standalone) => {
            let target = AttachTarget::New { name: Some("standalone".into()), layout: None };
            crate::standalone::run(target).map(|_| crate::exit::OK)
        }
    }
}

/// `butai whoami` — answer "where am I?" for a program running inside a pane.
///
/// The whole agent-facing surface rests on a pane being able to identify
/// itself, so this is the first thing a skill file tells an agent to run. When
/// `$BUTAI_PANE` is unset the answer is "not inside butai", which is exactly the
/// check a caller should make before issuing any control command.
///
/// `socket` is the *resolved* socket this invocation would talk to, not
/// `$BUTAI_SOCKET`, so it is always answered — outside a pane too, where the env
/// var is unset. That makes `ssh host butai --json whoami` the way to learn a
/// remote daemon's socket path, which `ssh -L` needs and cannot guess: it
/// forwards the path verbatim without shell expansion, and `~/.butai/butai.sock`
/// is not guaranteed anyway (with no home directory it lives under `/tmp`).
fn whoami(out: &Out, socket: &std::path::Path) -> Result<()> {
    let pane = crate::cli::pane::own_pane();
    let ws = std::env::var("BUTAI_WORKSPACE").ok().filter(|s| !s.trim().is_empty());
    let inside = pane.is_some();
    let value = serde_json::json!({
        "inside_butai": inside,
        "pane": pane.map(|p| p.0),
        "workspace": ws,
        "socket": socket.display().to_string(),
    });
    out.emit_owned(&value, |w| {
        match pane {
            Some(p) => {
                writeln!(w, "pane {p}")?;
                if let Some(ws) = &ws {
                    writeln!(w, "workspace {ws}")?;
                }
            }
            None => writeln!(w, "not inside a butai pane ($BUTAI_PANE is unset)")?,
        }
        writeln!(w, "socket {}", socket.display())?;
        Ok(())
    })
}

/// Run one async command to completion.
///
/// A fresh runtime per invocation: the CLI does a handful of round-trips and
/// exits, so startup cost dwarfs anything a shared runtime would save.
pub fn block_on<T, F: std::future::Future<Output = Result<T>>>(fut: F) -> Result<T> {
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(fut)
}

/// `butai ls` — the framed control path, with `--json` layered on top.
///
/// Sessions come back as [`butai_protocol::SessionInfo`], which is not a REST DTO
/// and has no route, so the JSON here is serialized by the CLI rather than
/// passed through from the daemon. Everything structured that *does* have a
/// route goes through `butai workspace` instead.
fn list_sessions(socket: &std::path::Path, out: &Out) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let reply = rt.block_on(butai_client::conn::control_request(socket, Command::ListSessions))?;
    let sessions = match reply {
        ServerMsg::SessionList(sessions) => sessions,
        ServerMsg::Error(e) => anyhow::bail!("{e}"),
        other => anyhow::bail!("unexpected reply: {other:?}"),
    };
    out.emit_owned(&sessions, |w| {
        if sessions.is_empty() {
            writeln!(w, "no sessions")?;
        }
        for s in &sessions {
            writeln!(
                w,
                "{}: {} window{} ({} client{}) [{}]",
                s.name,
                s.windows,
                if s.windows == 1 { "" } else { "s" },
                s.attached_clients,
                if s.attached_clients == 1 { "" } else { "s" },
                s.cwd.display(),
            )?;
        }
        Ok(())
    })
}

/// One-shot framed control command whose only interesting outcome is success.
fn control(socket: &std::path::Path, command: Command) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        match butai_client::conn::control_request(socket, command).await? {
            ServerMsg::Ok => Ok(()),
            // kill-server acknowledgment.
            ServerMsg::Detached { .. } => Ok(()),
            ServerMsg::Error(e) => anyhow::bail!("{e}"),
            other => anyhow::bail!("unexpected reply: {other:?}"),
        }
    })
}
