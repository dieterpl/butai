//! The butai daemon: owns sessions, PTYs, pane state, and rendering.
//! Clients speak the `butai-protocol` API over a Unix socket.

pub mod client_conn;
pub mod config;
pub mod core;
pub mod daemon;
pub mod git_op;
pub mod git_worktree;
pub mod http_conn;
mod ids;
pub mod input;
pub mod pane;
mod render;
pub mod search;
pub mod sys;
#[cfg(test)]
mod testenv;
pub mod usage;
pub mod workbench;

/// Run the daemon on `socket_path` in the foreground.
pub fn run_daemon(socket_path: &std::path::Path) -> anyhow::Result<()> {
    daemon::run(socket_path)
}
