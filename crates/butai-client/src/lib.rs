//! The butai TUI client: raw-mode terminal, input forwarding, and the
//! workbench it draws from the daemon's `/v1/*` DTOs.

pub mod api;
pub mod chrome;
pub mod clipboard;
pub mod config;
pub mod conn;
pub mod daemon;
pub mod dial;
pub mod git_menu;
pub mod graph;
pub mod hit;
pub mod keymap;
pub mod keys;
pub mod layout;
pub mod links;
pub mod reference;
pub mod selection;
pub mod ssh;
pub mod ssh_config;
pub mod syntax;
pub mod term;
pub mod theme;
pub mod tui;
pub mod verbs;
pub mod workbench;

use std::path::{Path, PathBuf};

use anyhow::Result;
use butai_protocol::AttachTarget;

/// Refuse to attach to the daemon we are already inside. The daemon sets `BUTAI`
/// in every pane's environment, to its own socket path; nesting a client inside
/// its own session double-draws and captures keys for the wrong layer.
///
/// `target` is the socket about to be attached to, or `None` for a client with
/// no socket identity at all (`butai standalone`), which refuses any nesting.
///
/// Attaching a *different* daemon from inside a pane is deliberately allowed:
/// opening a remote workbench from a local pane is the primary multi-host
/// gesture, and it is a different daemon drawing a different screen, so none of
/// the reasons above apply. Comparing sockets rather than merely testing for
/// `BUTAI` is what makes that possible without telling users to `unset BUTAI`,
/// which would throw away the guard for the case it actually protects.
pub fn guard_against_nesting(target: Option<&Path>) -> Result<()> {
    let Some(inside) = std::env::var_os("BUTAI").filter(|v| !v.is_empty()) else {
        return Ok(());
    };
    let inside = PathBuf::from(inside);
    match target {
        Some(target) if !same_socket(&inside, target) => Ok(()),
        _ => anyhow::bail!(
            "already inside this butai ({}) — attach a different --socket, \
             unset BUTAI to force, or detach first",
            inside.display()
        ),
    }
}

/// Whether two paths name the same daemon socket.
///
/// Canonicalized when both resolve, so the same socket reached by different
/// spellings — a symlinked home, a relative `--socket` — still compares equal.
/// A path that cannot be canonicalized does not exist, and so cannot be the
/// daemon we are running inside; the literal comparison is then the answer.
fn same_socket(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The workspace a target names, for the client-drawn path.
///
/// It resolves a workspace over REST rather than attaching to one, so all it
/// needs from the target is the name.
fn target_workspace(target: &AttachTarget) -> Option<String> {
    match target {
        AttachTarget::Attach { name } => Some(name.clone()),
        AttachTarget::New { name, .. } => name.clone(),
        _ => None,
    }
}

/// Every daemon the client should connect to: the local socket first, then
/// whatever `[[remote]]` entries name a socket reachable from here.
///
/// A remote is a *socket*, not an ssh command, because that is what makes
/// several machines share one tab bar without anything relaying: forward the far
/// daemon's socket (`ssh -L`) and it is a local path like any other. It is the
/// same shape the macOS client uses, and it is why the tab bar can span hosts
/// with no daemon acting as a client of another.
fn endpoints(local: PathBuf) -> Vec<workbench::Endpoint> {
    let mut out = vec![workbench::Endpoint { host: None, socket: local }];
    let (config, _) = crate::config::Config::load();
    for r in &config.remote {
        let Some(socket) = r.socket.as_deref() else { continue };
        let host = r
            .name
            .clone()
            .or_else(|| r.host.clone())
            .unwrap_or_else(|| socket.rsplit('/').next().unwrap_or(socket).to_string());
        out.push(workbench::Endpoint { host: Some(host), socket: PathBuf::from(socket) });
    }
    out
}

/// The `[[remote]]` blocks that name an ssh destination rather than a socket.
///
/// These are the machines `[+ host]` remembered. They are *not* endpoints: a
/// socket in the config is already reachable, while these have to be forwarded
/// first, and an ssh connection is seconds of DNS, TCP and key exchange. So they
/// are handed to the workbench separately and dialled on their own tasks after
/// the first frame is up — the alternative is a client that shows nothing for
/// twenty seconds because one remembered machine is asleep.
fn remotes() -> Vec<workbench::RemoteDial> {
    let (config, _) = crate::config::Config::load();
    config
        .remote
        .iter()
        .filter_map(|r| {
            let host = r.host.clone()?;
            Some(workbench::RemoteDial {
                label: r.name.clone().unwrap_or_else(|| host.clone()),
                target: host,
                args: r.ssh_args.clone(),
                socket_path: r.socket_path.clone(),
            })
        })
        .collect()
}

/// Attach to (or spawn) the daemon at `socket` and run the workbench until
/// detach.
pub fn run_client(socket: &Path, target: AttachTarget) -> Result<()> {
    guard_against_nesting(Some(socket))?;
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let reason = rt.block_on(attach(socket.to_path_buf(), target))?;
    println!("[butai: {reason}]");
    Ok(())
}

/// [`run_client`] for a caller that already has a runtime — `butai standalone`,
/// which starts a daemon of its own first. Returns the detach reason rather
/// than printing it.
pub fn run_client_on(socket: &Path, target: AttachTarget) -> Result<String> {
    guard_against_nesting(Some(socket))?;
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(attach(socket.to_path_buf(), target))
    })
}

/// Spawn the daemon if it is not up, then talk to it the way every other client
/// does: REST and an event stream for everything structured, one framed
/// connection for the pane on the stage.
async fn attach(socket: PathBuf, target: AttachTarget) -> Result<String> {
    drop(conn::connect_or_spawn(&socket).await?);
    let ws = target_workspace(&target);
    ensure_workspace(&socket, &target).await?;
    workbench::run(endpoints(socket), remotes(), ws).await
}

/// Open the workspace the target asks for, if it is not open already.
///
/// The daemon used to do this as part of attaching: a session target *was* the
/// request. Now the client never sends one — it holds REST plus a pane
/// connection like every other client — so somebody has to ask, and the client
/// is the one that knows which directory the user typed `butai` in.
///
/// `POST /v1/workspaces` rather than a new route: it is the same call the
/// folder browser makes, and the same one Caliper makes on startup.
async fn ensure_workspace(socket: &Path, target: &AttachTarget) -> Result<()> {
    use butai_protocol::api::WorkspaceSummary;
    let api = api::Api::new(socket.to_path_buf());
    let open: Vec<WorkspaceSummary> = api.get_as("/v1/workspaces").await?;
    let want = target_workspace(target);
    // A named target that is already open needs nothing; an unnamed one is
    // satisfied by *any* open workspace, which is what "default" means.
    let satisfied = match &want {
        Some(name) => open.iter().any(|w| &w.name == name),
        None => !open.is_empty(),
    };
    if satisfied {
        return Ok(());
    }
    // `New` with no name, and `Default` with nothing open, both mean "here".
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let mut body = serde_json::json!({ "path": cwd.to_string_lossy() });
    if let Some(name) = want {
        body["name"] = serde_json::Value::String(name);
    }
    api.post("/v1/workspaces", &body).await?;
    Ok(())
}
