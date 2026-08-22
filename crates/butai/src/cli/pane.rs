//! `butai pane` — list, read and drive the panes of a workspace.
//!
//! These are the verbs a program shells out to, and the program butai most cares
//! about is an agent running inside one of its own panes: `butai pane read 7`
//! beats teaching a model a `curl --unix-socket` incantation, and it is the same
//! surface a shell-out plugin would use.
//!
//! Target resolution happens here, client-side, the way `butai workspace` already
//! resolves a workspace name. A bare pane id needs no scope at all — butai
//! allocates pane ids from one daemon-wide counter — so the common case is a
//! single round-trip.

use anyhow::{Context, Result};
use butai_protocol::api::{PaneOutputDto, WorkspaceDetail, WorkspaceSummary};
use butai_protocol::{PaneId, SessionId};
use clap::{Subcommand, ValueEnum};
use serde::Serialize;

use super::Ctx;
use crate::exit;
use crate::target::{Leaf, Target};
use butai_client::api::Api;

#[derive(Subcommand)]
pub enum PaneCmd {
    /// List the panes of a workspace
    #[command(visible_alias = "list")]
    Ls {
        /// Workspace id or name (defaults to --ws)
        target: Option<String>,
    },
    /// Print a pane's output as text
    Read {
        /// Pane id, agent name, or `stage` — optionally `<workspace>:<leaf>`
        target: String,
        /// Maximum rows to print, counting back from the live screen
        #[arg(short, long, default_value_t = 200)]
        lines: usize,
        /// Which band to read
        #[arg(long, value_enum, default_value_t = ReadSource::Scrollback)]
        source: ReadSource,
        /// Keep colors as escape sequences
        #[arg(long, value_enum, default_value_t = ReadFormat::Text)]
        format: ReadFormat,
    },
    /// Type into a pane, as if the keyboard had
    Send {
        /// Pane id, agent name, or `stage`
        target: String,
        /// Text to type. Submitted with Enter unless --no-enter is given
        text: Vec<String>,
        /// Send a named key instead of text (enter, esc, up, ctrl-c, …)
        #[arg(long, value_name = "KEY")]
        key: Option<String>,
        /// Do not press Enter after the text
        #[arg(long)]
        no_enter: bool,
    },
}

/// Which band of a pane `read` returns. Mirrors the route's `?source=`.
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
pub enum ReadSource {
    /// Recent history ending at the live screen
    Scrollback,
    /// Exactly the visible viewport, blank rows and all
    Screen,
    /// The band the agent-state detector scans
    Footer,
}

impl ReadSource {
    fn as_str(self) -> &'static str {
        match self {
            ReadSource::Scrollback => "scrollback",
            ReadSource::Screen => "screen",
            ReadSource::Footer => "footer",
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
pub enum ReadFormat {
    /// Plain text, escapes stripped
    Text,
    /// SGR-formatted, colors preserved
    Ansi,
}

impl ReadFormat {
    fn as_str(self) -> &'static str {
        match self {
            ReadFormat::Text => "text",
            ReadFormat::Ansi => "ansi",
        }
    }
}

pub async fn run(cmd: PaneCmd, ctx: &Ctx) -> Result<u8> {
    match cmd {
        PaneCmd::Ls { target } => ls(ctx, target).await,
        PaneCmd::Read { target, lines, source, format } => {
            read(ctx, &target, lines, source, format).await
        }
        PaneCmd::Send { target, text, key, no_enter } => {
            send(ctx, &target, text, key, no_enter).await
        }
    }
}

// ---------------------------------------------------------------------------
// Target resolution
// ---------------------------------------------------------------------------

/// A target turned into the ids the routes actually take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub ws: SessionId,
    pub pane: PaneId,
}

impl Resolved {
    /// The route prefix for this pane, e.g. `/v1/workspaces/1/panes/7`.
    pub fn route(&self) -> String {
        format!("/v1/workspaces/{}/panes/{}", self.ws, self.pane)
    }
}

/// Resolve a target string to a workspace and pane.
///
/// A bare pane id is looked up daemon-wide, so `butai pane read 7` works from
/// anywhere without naming a workspace. Everything else needs a scope, taken
/// from the target's own `<workspace>:` prefix, then `--ws`, then
/// `$BUTAI_WORKSPACE` — which is why a pane knowing its own workspace matters.
///
/// A scope given alongside a numeric leaf is an *assertion*: `1:7` fails if pane
/// 7 does not belong to workspace 1, so a script that cached an id across a
/// teardown gets an error instead of acting on whatever inherited the number.
pub async fn resolve(api: &Api, spec: &str, default_ws: Option<&str>) -> Result<Resolved> {
    let target = match Target::parse(spec) {
        Ok(t) => t,
        Err(e) => return exit::usage(e.to_string()),
    };

    if target.is_direct() {
        let Leaf::Pane(n) = target.leaf else { unreachable!("is_direct implies a pane id") };
        return find_pane_anywhere(api, PaneId(n)).await;
    }

    let scope = target.scope.clone().or_else(|| default_ws.map(str::to_string));
    let ws = resolve_ws(api, scope, spec).await?;
    let detail: WorkspaceDetail = api
        .get_as(&format!("/v1/workspaces/{ws}"))
        .await
        .with_context(|| format!("looking up workspace {ws}"))?;

    match &target.leaf {
        Leaf::Pane(n) => {
            let pane = PaneId(*n);
            if panes_of(&detail).any(|(p, _)| p == pane) {
                Ok(Resolved { ws, pane })
            } else {
                exit::not_found(format!(
                    "pane {pane} is not in workspace {ws} ({})",
                    describe_panes(&detail)
                ))
            }
        }
        Leaf::Stage => match detail.stage {
            Some(pane) => Ok(Resolved { ws, pane }),
            None => exit::not_found(format!("workspace {ws} has nothing on its stage")),
        },
        Leaf::Name(name) => {
            let needle = name.to_lowercase();
            let hits: Vec<(PaneId, String)> = panes_of(&detail)
                .filter(|(_, label)| label.to_lowercase().contains(&needle))
                .collect();
            match hits.as_slice() {
                [(pane, _)] => Ok(Resolved { ws, pane: *pane }),
                [] => exit::not_found(format!(
                    "nothing in workspace {ws} is named {name:?} ({})",
                    describe_panes(&detail)
                )),
                many => {
                    let all: Vec<String> = many.iter().map(|(p, l)| format!("{p} ({l})")).collect();
                    // Ambiguity is an error rather than a coin flip: sending a
                    // prompt to the wrong agent is not recoverable.
                    exit::usage(format!(
                        "{} panes in workspace {ws} match {name:?}; use an id: {}",
                        many.len(),
                        all.join(", ")
                    ))
                }
            }
        }
    }
}

/// Every addressable pane in a workspace, with the label a target may name it by.
fn panes_of(detail: &WorkspaceDetail) -> impl Iterator<Item = (PaneId, String)> + '_ {
    let agents = detail.agents.iter().map(|a| (a.pane, a.title.clone()));
    let procs = detail.processes.iter().map(|p| (p.pane, p.name.clone()));
    agents.chain(procs)
}

fn describe_panes(detail: &WorkspaceDetail) -> String {
    let all: Vec<String> = panes_of(detail).map(|(p, l)| format!("{p} {l}")).collect();
    if all.is_empty() {
        "it has no panes".into()
    } else {
        format!("it has: {}", all.join(", "))
    }
}

/// Find a pane by id across every workspace.
///
/// Pane ids come from one daemon-wide counter, so this cannot be ambiguous —
/// it is a scan only because the routes are all workspace-scoped.
async fn find_pane_anywhere(api: &Api, pane: PaneId) -> Result<Resolved> {
    let list: Vec<WorkspaceSummary> = api.get_as("/v1/workspaces").await?;
    for ws in &list {
        let detail: WorkspaceDetail = api.get_as(&format!("/v1/workspaces/{}", ws.id)).await?;
        if panes_of(&detail).any(|(p, _)| p == pane) {
            return Ok(Resolved { ws: ws.id, pane });
        }
    }
    exit::not_found(format!("no pane {pane} in any open workspace"))
}

/// Turn a workspace id or name into an id.
///
/// `what` names the thing being addressed, so the "which workspace?" error can
/// say what it was trying to do.
pub async fn resolve_ws(api: &Api, spec: Option<String>, what: &str) -> Result<SessionId> {
    let Some(spec) = spec else {
        return exit::usage(format!(
            "no workspace given for {what}: pass --ws, write the target as \
             <workspace>:<leaf>, or run inside a butai pane"
        ));
    };
    let spec = spec.trim();
    if let Ok(id) = spec.parse::<u64>() {
        return Ok(SessionId(id));
    }
    let list: Vec<WorkspaceSummary> = api.get_as("/v1/workspaces").await?;
    let matches: Vec<&WorkspaceSummary> = list.iter().filter(|ws| ws.name == spec).collect();
    match matches.as_slice() {
        [ws] => Ok(ws.id),
        [] => exit::not_found(format!("no workspace named {spec:?}")),
        many => {
            let ids: Vec<String> = many.iter().map(|ws| ws.id.to_string()).collect();
            exit::usage(format!(
                "{} workspaces are named {spec:?}; use an id instead ({})",
                many.len(),
                ids.join(", ")
            ))
        }
    }
}

/// The pane the caller is running in, if it is running in one.
///
/// `$BUTAI_PANE` is set by the daemon when it spawns a pane. Its absence is the
/// test for "not inside butai" — no separate marker variable is needed, because
/// a pane id is strictly more informative than a boolean.
pub fn own_pane() -> Option<PaneId> {
    std::env::var("BUTAI_PANE").ok()?.trim().parse().ok().map(PaneId)
}

/// Refuse a target that is the caller itself.
///
/// Typing into your own pane appends to the prompt you are composing; waiting on
/// yourself can never return, because you are `working` precisely *because* you
/// are running the wait. Both are silent traps, so they are errors.
pub fn refuse_self(resolved: Resolved, verb: &str) -> Result<()> {
    if own_pane() == Some(resolved.pane) {
        return exit::usage(format!(
            "pane {} is this pane — `{verb}` on yourself would {}",
            resolved.pane,
            if verb == "wait" { "never return" } else { "type into your own prompt" }
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Verbs
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct PaneRow {
    pane: PaneId,
    kind: &'static str,
    label: String,
    status: String,
    staged: bool,
}

async fn ls(ctx: &Ctx, target: Option<String>) -> Result<u8> {
    let spec = target.or_else(|| ctx.ws.clone());
    let ws = resolve_ws(&ctx.api, spec, "the pane list").await?;
    let detail: WorkspaceDetail = ctx.api.get_as(&format!("/v1/workspaces/{ws}")).await?;

    let mut rows: Vec<PaneRow> = Vec::new();
    for a in &detail.agents {
        rows.push(PaneRow {
            pane: a.pane,
            kind: "agent",
            label: a.title.clone(),
            status: format!("{:?}", a.state).to_lowercase(),
            staged: detail.stage == Some(a.pane),
        });
    }
    for p in &detail.processes {
        rows.push(PaneRow {
            pane: p.pane,
            kind: "process",
            label: p.name.clone(),
            status: p.status.clone(),
            staged: detail.stage == Some(p.pane),
        });
    }

    let me = own_pane();
    ctx.out.emit_owned(&rows, |w| {
        if rows.is_empty() {
            writeln!(w, "no panes in workspace {ws}")?;
        }
        for r in &rows {
            // Mark the caller's own row: the first thing an agent needs to know
            // about a pane list is which one it is looking out of.
            let mark = if me == Some(r.pane) { " <- you" } else { "" };
            let stage = if r.staged { " [stage]" } else { "" };
            writeln!(w, "{}\t{}\t{}\t{}{}{}", r.pane, r.kind, r.status, r.label, stage, mark)?;
        }
        Ok(())
    })?;
    Ok(exit::OK)
}

async fn read(
    ctx: &Ctx,
    target: &str,
    lines: usize,
    source: ReadSource,
    format: ReadFormat,
) -> Result<u8> {
    let at = resolve(&ctx.api, target, ctx.ws.as_deref()).await?;
    let path = format!(
        "{}/output?lines={lines}&source={}&format={}",
        at.route(),
        source.as_str(),
        format.as_str()
    );
    let body = ctx.api.get(&path).await?;
    let dto: PaneOutputDto = butai_client::api::parse(&body)?;
    // Human output is the lines and nothing else — no header, no pane id, no
    // colour — so `butai pane read 7 | grep …` behaves like reading a file.
    ctx.out.emit(&body, |w| {
        for line in &dto.lines {
            writeln!(w, "{line}")?;
        }
        Ok(())
    })?;
    Ok(exit::OK)
}

async fn send(
    ctx: &Ctx,
    target: &str,
    text: Vec<String>,
    key: Option<String>,
    no_enter: bool,
) -> Result<u8> {
    let at = resolve(&ctx.api, target, ctx.ws.as_deref()).await?;
    refuse_self(at, "send")?;

    if let Some(key) = key {
        if !text.is_empty() {
            return exit::usage("pass either text or --key, not both");
        }
        let event = key_event(&key)?;
        ctx.api.post(&format!("{}/input", at.route()), &event).await?;
        return Ok(exit::OK);
    }

    if text.is_empty() {
        return exit::usage("nothing to send: pass some text, or --key");
    }
    let joined = text.join(" ");
    // A paste, not a keystroke per character: it is one round-trip, and it
    // arrives inside the agent's bracketed-paste guard the way a real paste
    // would rather than looking like very fast typing.
    ctx.api.post(&format!("{}/input", at.route()), &serde_json::json!({ "paste": joined })).await?;
    if !no_enter {
        ctx.api
            .post(
                &format!("{}/input", at.route()),
                &serde_json::json!({ "key": { "code": "enter" } }),
            )
            .await?;
    }
    Ok(exit::OK)
}

/// Parse a key name into the `InputEvent` the route takes.
///
/// Deliberately small: the named keys a script actually needs, plus `ctrl-<c>`.
/// Anything else is a usage error naming what is accepted, rather than a silent
/// no-op keystroke.
fn key_event(name: &str) -> Result<serde_json::Value> {
    let name = name.trim().to_lowercase();
    if let Some(c) = name.strip_prefix("ctrl-").or_else(|| name.strip_prefix("ctrl+")) {
        let mut chars = c.chars();
        return match (chars.next(), chars.next()) {
            (Some(c), None) => Ok(serde_json::json!({
                "key": { "code": { "char": c }, "mods": { "ctrl": true } }
            })),
            _ => exit::usage(format!("ctrl- takes a single character, got {c:?}")),
        };
    }
    const NAMED: &[&str] = &[
        "enter",
        "esc",
        "tab",
        "backspace",
        "delete",
        "up",
        "down",
        "left",
        "right",
        "home",
        "end",
        "page_up",
        "page_down",
    ];
    if NAMED.contains(&name.as_str()) {
        return Ok(serde_json::json!({ "key": { "code": name } }));
    }
    exit::usage(format!("unknown key {name:?}; try one of: {}, or ctrl-<char>", NAMED.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys_become_key_events() {
        assert_eq!(key_event("enter").unwrap(), serde_json::json!({"key":{"code":"enter"}}));
        // Case and stray spacing are the caller's, not an error.
        assert_eq!(key_event(" ESC ").unwrap(), serde_json::json!({"key":{"code":"esc"}}));
    }

    #[test]
    fn ctrl_chords_carry_the_modifier() {
        let ev = key_event("ctrl-c").unwrap();
        assert_eq!(ev["key"]["code"]["char"], "c");
        assert_eq!(ev["key"]["mods"]["ctrl"], true);
        assert_eq!(key_event("ctrl+c").unwrap(), ev, "both spellings");
    }

    #[test]
    fn an_unknown_key_is_a_usage_error_that_says_what_is_allowed() {
        let err = key_event("meta-frobnicate").unwrap_err();
        assert_eq!(crate::exit::code_for(&err), crate::exit::USAGE);
        assert!(err.to_string().contains("enter"), "should list the valid keys: {err}");
        assert_eq!(crate::exit::code_for(&key_event("ctrl-").unwrap_err()), crate::exit::USAGE);
    }
}
