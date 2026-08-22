//! `butai workspace` — the REST face's workspace routes, on the command line.
//!
//! A workspace is butai's unit of project: one per directory, holding the agent
//! and process rails and the git changes view. These are the same
//! `/v1/workspaces` routes every GUI client uses; nothing here is CLI-only.

use anyhow::{Context, Result};
use butai_protocol::api::{WorkspaceDetail, WorkspaceSummary};
use butai_protocol::SessionId;
use clap::Subcommand;

use super::Ctx;
use butai_client::api::Api;

#[derive(Subcommand)]
pub enum WsCmd {
    /// List workspaces with their agent, process, and change counts
    #[command(visible_alias = "list")]
    Ls,
    /// Show one workspace's agents, processes, and changes
    Show {
        /// Workspace id or name (defaults to --ws)
        target: Option<String>,
    },
    /// Create a workspace
    #[command(visible_alias = "new")]
    Create {
        /// Directory to open (defaults to the current directory)
        #[arg(long, value_name = "PATH")]
        cwd: Option<String>,
        /// Workspace name (defaults to the directory's basename)
        #[arg(long)]
        name: Option<String>,
        /// Layout preset to apply (e.g. "ide")
        #[arg(long)]
        layout: Option<String>,
    },
    /// Close a workspace and kill everything in it
    #[command(visible_alias = "kill")]
    Rm {
        /// Workspace id or name (defaults to --ws)
        target: Option<String>,
    },
}

pub async fn run(cmd: WsCmd, ctx: &Ctx) -> Result<()> {
    match cmd {
        WsCmd::Ls => ls(ctx).await,
        WsCmd::Show { target } => show(ctx, target).await,
        WsCmd::Create { cwd, name, layout } => create(ctx, cwd, name, layout).await,
        WsCmd::Rm { target } => rm(ctx, target).await,
    }
}

async fn ls(ctx: &Ctx) -> Result<()> {
    let body = ctx.api.get("/v1/workspaces").await?;
    let list: Vec<WorkspaceSummary> = butai_client::api::parse(&body)?;
    ctx.out.emit(&body, |w| {
        if list.is_empty() {
            writeln!(w, "no workspaces")?;
        }
        for ws in &list {
            // Only mention the states that are actually populated: a quiet
            // workspace should read as quiet, not as a row of zeroes.
            let mut marks = Vec::new();
            if ws.waiting > 0 {
                marks.push(format!("{} waiting", ws.waiting));
            }
            if ws.working > 0 {
                marks.push(format!("{} working", ws.working));
            }
            if ws.finished > 0 {
                marks.push(format!("{} done", ws.finished));
            }
            if ws.exited > 0 {
                marks.push(format!("{} exited", ws.exited));
            }
            let agents = if marks.is_empty() {
                format!("{} agent{}", ws.agents, plural(ws.agents))
            } else {
                format!("{} agent{} ({})", ws.agents, plural(ws.agents), marks.join(", "))
            };
            writeln!(
                w,
                "{}\t{}\t{}, {} process{}, {} change{}\t[{}]",
                ws.id,
                ws.name,
                agents,
                ws.processes,
                if ws.processes == 1 { "" } else { "es" },
                ws.changes,
                plural(ws.changes),
                ws.cwd,
            )?;
        }
        Ok(())
    })
}

async fn show(ctx: &Ctx, target: Option<String>) -> Result<()> {
    let id = resolve(&ctx.api, target.or_else(|| ctx.ws.clone())).await?;
    let body = ctx.api.get(&format!("/v1/workspaces/{id}")).await?;
    let ws: WorkspaceDetail = butai_client::api::parse(&body)?;
    ctx.out.emit(&body, |w| {
        writeln!(w, "{} ({}) [{}]", ws.name, ws.id, ws.cwd)?;
        if !ws.agents.is_empty() {
            writeln!(w, "\nAGENTS")?;
            for a in &ws.agents {
                let state = match a.exited {
                    Some(code) => format!("exited({code})"),
                    None => format!("{:?}", a.state).to_lowercase(),
                };
                let question = if a.question { " ?" } else { "" };
                writeln!(w, "  {}\t{}\t{}{}", a.pane, state, a.title, question)?;
            }
        }
        if !ws.processes.is_empty() {
            writeln!(w, "\nPROCESSES")?;
            for p in &ws.processes {
                writeln!(w, "  {}\t{}\t{}\t{}", p.pane, p.status, p.name, p.command)?;
            }
        }
        if let Some(changes) = &ws.changes {
            if !changes.staged.is_empty() || !changes.unstaged.is_empty() {
                writeln!(w, "\nCHANGES ({})", changes.branch)?;
                // Staged first, marked, so `butai ws show` answers "what would a
                // commit take?" without a second look at git.
                for f in &changes.staged {
                    writeln!(w, "  {} {}\t+{} -{}\tstaged", f.code, f.path, f.added, f.deleted)?;
                }
                for f in &changes.unstaged {
                    writeln!(w, "  {} {}\t+{} -{}", f.code, f.path, f.added, f.deleted)?;
                }
            }
        }
        Ok(())
    })
}

async fn create(
    ctx: &Ctx,
    cwd: Option<String>,
    name: Option<String>,
    layout: Option<String>,
) -> Result<()> {
    let path = match cwd {
        Some(p) => Some(p),
        None => Some(
            std::env::current_dir()
                .context("read the current directory")?
                .to_string_lossy()
                .into_owned(),
        ),
    };
    let mut params = serde_json::Map::new();
    if let Some(name) = name {
        params.insert("name".into(), name.into());
    }
    if let Some(layout) = layout {
        params.insert("layout".into(), layout.into());
    }
    if let Some(path) = path {
        params.insert("path".into(), path.into());
    }
    let body = ctx.api.post("/v1/workspaces", &params.into()).await?;
    let created: serde_json::Value = butai_client::api::parse(&body)?;
    let id = created.get("id").and_then(|v| v.as_u64());
    ctx.out.emit(&body, |w| {
        match id {
            Some(id) => writeln!(w, "{id}")?,
            None => writeln!(w, "created")?,
        }
        Ok(())
    })
}

async fn rm(ctx: &Ctx, target: Option<String>) -> Result<()> {
    let id = resolve(&ctx.api, target.or_else(|| ctx.ws.clone())).await?;
    let body = ctx.api.delete(&format!("/v1/workspaces/{id}")).await?;
    ctx.out.emit(&body, |w| {
        writeln!(w, "killed workspace {id}")?;
        Ok(())
    })
}

/// Turn a workspace id or name into an id.
///
/// A numeric spec is taken at face value — the route answers 404 if it is stale,
/// which is a better error than "no workspace named 7". A name is matched
/// against the list, case-sensitively, and an ambiguous name is an error rather
/// than a coin flip: killing the wrong workspace is not recoverable.
async fn resolve(api: &Api, spec: Option<String>) -> Result<SessionId> {
    let spec = spec.context(
        "no workspace given: pass one as an argument, set --ws, or run inside a butai pane",
    )?;
    let spec = spec.trim();
    if let Ok(id) = spec.parse::<u64>() {
        return Ok(SessionId(id));
    }
    let list: Vec<WorkspaceSummary> = api.get_as("/v1/workspaces").await?;
    let matches: Vec<&WorkspaceSummary> = list.iter().filter(|ws| ws.name == spec).collect();
    match matches.as_slice() {
        [ws] => Ok(ws.id),
        [] => {
            let known: Vec<&str> = list.iter().map(|ws| ws.name.as_str()).collect();
            if known.is_empty() {
                anyhow::bail!("no workspace named {spec:?} (none are open)")
            }
            anyhow::bail!("no workspace named {spec:?} (open: {})", known.join(", "))
        }
        many => {
            let ids: Vec<String> = many.iter().map(|ws| ws.id.to_string()).collect();
            anyhow::bail!(
                "{} workspaces are named {spec:?}; use an id instead ({})",
                many.len(),
                ids.join(", ")
            )
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}
