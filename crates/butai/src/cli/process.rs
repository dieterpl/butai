//! `butai process` — the managed processes of a workspace.
//!
//! `status` exists to be used in a condition: it reports a failed process
//! through its exit code, so `butai process status -q || butai agent send 7 "the
//! dev server died"` works without parsing anything.

use anyhow::Result;
use butai_protocol::api::ProcessDto;
use clap::Subcommand;

use super::pane::resolve_ws;
use super::Ctx;
use crate::exit;

#[derive(Subcommand)]
pub enum ProcCmd {
    /// List a workspace's processes and their statuses
    #[command(visible_alias = "list")]
    Ls {
        /// Workspace id or name (defaults to --ws)
        target: Option<String>,
    },
    /// Like `ls`, but exit non-zero if any process has failed
    Status {
        /// Workspace id or name (defaults to --ws)
        target: Option<String>,
    },
    /// Start a managed process
    Start {
        /// Label for the process rail
        name: String,
        /// Command to run
        command: Vec<String>,
    },
}

pub async fn run(cmd: ProcCmd, ctx: &Ctx) -> Result<u8> {
    match cmd {
        ProcCmd::Ls { target } => list(ctx, target, false).await,
        ProcCmd::Status { target } => list(ctx, target, true).await,
        ProcCmd::Start { name, command } => start(ctx, &name, command).await,
    }
}

async fn list(ctx: &Ctx, target: Option<String>, strict: bool) -> Result<u8> {
    let ws = resolve_ws(&ctx.api, target.or_else(|| ctx.ws.clone()), "the process list").await?;
    let body = ctx.api.get(&format!("/v1/workspaces/{ws}/processes")).await?;
    let procs: Vec<ProcessDto> = butai_client::api::parse(&body)?;

    ctx.out.emit(&body, |w| {
        if procs.is_empty() {
            writeln!(w, "no processes in workspace {ws}")?;
        }
        for p in &procs {
            writeln!(w, "{}\t{}\t{}\t{}", p.pane, p.status, p.name, p.command)?;
        }
        Ok(())
    })?;

    // `FAIL(<code>)` is the daemon's own wording for a process that exited
    // non-zero and left a corpse in the rail.
    let failed =
        procs.iter().any(|p| p.status.starts_with("FAIL") || p.exited.is_some_and(|c| c != 0));
    Ok(if strict && failed { exit::EXITED } else { exit::OK })
}

async fn start(ctx: &Ctx, name: &str, command: Vec<String>) -> Result<u8> {
    if command.is_empty() {
        return exit::usage("no command given: `butai process start <name> <command…>`");
    }
    let ws = resolve_ws(&ctx.api, ctx.ws.clone(), "the new process").await?;
    let body = serde_json::json!({ "name": name, "command": command.join(" ") });
    ctx.api.post(&format!("/v1/workspaces/{ws}/processes"), &body).await?;
    Ok(exit::OK)
}
