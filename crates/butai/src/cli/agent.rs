//! `butai agent` — inspect, drive, and *wait on* the agents in a workspace.
//!
//! `wait` is the verb that matters. Listing and sending let a program poke at
//! its neighbours; waiting is what turns poking into coordination — spawn a
//! helper, hand it a task, block until it is done, read what it produced.
//!
//! It is implemented client-side, by polling `GET /v1/workspaces/{ws}/agents`.
//! Two reasons it is not the SSE stream: `ApiEvent::Workspaces` carries only
//! *counts*, never per-pane state, so a subscriber would still have to follow
//! every event with a GET; and a clean agent exit removes the row without
//! emitting a notification at all, so an edge-only waiter would hang on the most
//! ordinary success. Agent state is recomputed on the daemon's ~2s sampler tick,
//! so polling faster than that buys nothing anyway.

use std::time::{Duration, Instant};

use anyhow::Result;
use butai_protocol::api::{AgentDto, AgentState, NotificationsDto, PaneOutputDto};
use butai_protocol::PaneId;
use clap::Subcommand;
use serde::Serialize;

use super::pane::{self, ReadFormat, ReadSource, Resolved};
use super::Ctx;
use crate::exit;
use butai_client::api::Api;

/// How often to re-read agent state while waiting. The daemon recomputes on a
/// ~2s tick, so this is already finer-grained than the truth it is polling.
const POLL_MIN: Duration = Duration::from_millis(400);
const POLL_MAX: Duration = Duration::from_millis(1000);

/// How long a freshly spawned agent gets to draw before `--prompt` gives up
/// waiting and types anyway. Generous: it bounds a startup that is normally
/// under two seconds, and the cost of overrunning it is a warning, not a
/// failure.
const READY_TIMEOUT: Duration = Duration::from_millis(15_000);
/// How often to look for the first thing the agent draws. Finer than
/// [`POLL_MIN`] because this one is racing a startup, not a turn.
const READY_POLL: Duration = Duration::from_millis(100);

#[derive(Subcommand)]
pub enum AgentCmd {
    /// List the agents of a workspace and their states
    #[command(visible_alias = "list")]
    Ls {
        /// Workspace id or name (defaults to --ws)
        target: Option<String>,
    },
    /// Start an agent, printing its new pane id
    Spawn {
        /// Agent type, as configured (claude, codex, gemini, aider, …)
        kind: String,
        /// Do not take the stage — leave the human's view where it is
        #[arg(long)]
        background: bool,
        /// Send this prompt once the agent is up
        #[arg(long, value_name = "TEXT")]
        prompt: Option<String>,
        /// With --prompt, block until the agent finishes it
        #[arg(long)]
        wait: bool,
        /// Give up after this many milliseconds when waiting
        #[arg(long, value_name = "MS", default_value_t = 300_000)]
        timeout: u64,
    },
    /// Type a prompt into an agent, optionally blocking until it answers
    Send {
        /// Pane id, agent name, or `stage`
        target: String,
        /// The prompt
        text: Vec<String>,
        /// Block until the agent reaches --until
        #[arg(long)]
        wait: bool,
        /// States that end the wait (comma-separated, or `done`/`attention`)
        #[arg(long, value_name = "SET")]
        until: Option<String>,
        /// Give up after this many milliseconds
        #[arg(long, value_name = "MS", default_value_t = 300_000)]
        timeout: u64,
    },
    /// Print an agent's output as text (the same read as `butai pane read`)
    Read {
        /// Pane id, agent name, or `stage`
        target: String,
        #[arg(short, long, default_value_t = 200)]
        lines: usize,
        #[arg(long, value_enum, default_value_t = ReadSource::Scrollback)]
        source: ReadSource,
        #[arg(long, value_enum, default_value_t = ReadFormat::Text)]
        format: ReadFormat,
    },
    /// Block until an agent reaches one of a set of states
    Wait {
        /// Pane id, agent name, or `stage`
        target: String,
        /// States that end the wait (comma-separated, or `done`/`attention`).
        /// Defaults to `finished,exited`
        #[arg(long, value_name = "SET")]
        until: Option<String>,
        /// Give up after this many milliseconds
        #[arg(long, value_name = "MS", default_value_t = 300_000)]
        timeout: u64,
        /// Only accept a state reached *after* this notification sequence.
        /// Makes the wait edge-correct when a previous turn left the agent in
        /// the state being waited for
        #[arg(long, value_name = "N")]
        since_seq: Option<u64>,
    },
    /// Kill an agent's pane
    Kill {
        /// Pane id, agent name, or `stage`
        target: String,
    },
}

pub async fn run(cmd: AgentCmd, ctx: &Ctx) -> Result<u8> {
    match cmd {
        AgentCmd::Ls { target } => ls(ctx, target).await,
        AgentCmd::Spawn { kind, background, prompt, wait, timeout } => {
            spawn(ctx, &kind, background, prompt, wait, timeout).await
        }
        AgentCmd::Send { target, text, wait, until, timeout } => {
            send(ctx, &target, text, wait, until, timeout).await
        }
        AgentCmd::Read { target, lines, source, format } => {
            pane::run(pane::PaneCmd::Read { target, lines, source, format }, ctx).await
        }
        AgentCmd::Wait { target, until, timeout, since_seq } => {
            let until = parse_until(until.as_deref())?;
            let at = match pane::resolve(&ctx.api, &target, ctx.ws.as_deref()).await {
                Ok(at) => at,
                // An agent that has already gone is `exited`, not "no such
                // pane". A wait that spans the exit has always said so —
                // `wait_for` returns `exited` when the row disappears under it
                // — so starting half a second later, on the same situation,
                // must not report a different code. Only for a target that
                // names a pane outright: a name nothing matches is a typo, and
                // still a 404.
                Err(e) if exit::code_for(&e) == exit::NOT_FOUND => match bare_pane(&target) {
                    Some(pane) => {
                        let gone = WaitOutcome {
                            pane,
                            state: "exited",
                            exited: None,
                            timed_out: false,
                            waited_ms: 0,
                        };
                        return report(ctx, &gone);
                    }
                    None => return Err(e),
                },
                Err(e) => return Err(e),
            };
            pane::refuse_self(at, "wait")?;
            let outcome = wait_for(&ctx.api, at, &until, timeout, since_seq).await?;
            report(ctx, &outcome)
        }
        AgentCmd::Kill { target } => {
            let at = pane::resolve(&ctx.api, &target, ctx.ws.as_deref()).await?;
            pane::refuse_self(at, "kill")?;
            ctx.api.delete(&at.route()).await?;
            Ok(exit::OK)
        }
    }
}

// ---------------------------------------------------------------------------
// --until
// ---------------------------------------------------------------------------

/// Parse `--until` into the set of states that end a wait.
///
/// Accepts the daemon's own state names plus two aliases that exist only here:
/// `done` for "stopped doing anything", `attention` for "wants a human". They
/// stay CLI-side deliberately — the wire should carry states, not opinions.
///
/// The default is `finished,exited`, and notably *not* `idle`: a freshly spawned
/// agent starts out idle, so waiting for it would return immediately.
fn parse_until(spec: Option<&str>) -> Result<Vec<AgentState>> {
    use AgentState::*;
    let Some(spec) = spec else { return Ok(vec![Finished, Exited]) };
    let mut out = Vec::new();
    for word in spec.split(',').map(str::trim).filter(|w| !w.is_empty()) {
        let states: &[AgentState] = match word.to_lowercase().as_str() {
            "waiting" => &[Waiting],
            "working" => &[Working],
            "finished" => &[Finished],
            "idle" => &[Idle],
            "exited" => &[Exited],
            "done" => &[Finished, Idle, Exited],
            "attention" => &[Waiting, Finished, Exited],
            other => {
                return exit::usage(format!(
                    "unknown state {other:?} in --until; expected some of \
                     waiting, working, finished, idle, exited, or the aliases \
                     done and attention"
                ))
            }
        };
        for s in states {
            if !out.contains(s) {
                out.push(*s);
            }
        }
    }
    if out.is_empty() {
        return exit::usage("--until was given but named no states");
    }
    Ok(out)
}

/// The pane id of a target that names one outright — `7`, but not `web:7`, not
/// `stage`, not an agent's name. Those carry a scope or a lookup that can fail
/// for reasons of their own, so only this form can read a miss as "it exited".
fn bare_pane(spec: &str) -> Option<PaneId> {
    spec.trim().parse::<u64>().ok().map(PaneId)
}

fn state_name(state: AgentState) -> &'static str {
    match state {
        AgentState::Waiting => "waiting",
        AgentState::Working => "working",
        AgentState::Finished => "finished",
        AgentState::Idle => "idle",
        AgentState::Exited => "exited",
    }
}

// ---------------------------------------------------------------------------
// wait
// ---------------------------------------------------------------------------

/// The result of a wait. Shaped so that a future server-side
/// `GET .../panes/{pane}/wait` can return exactly this JSON and the CLI's
/// `--json` output does not change when it switches over.
#[derive(Serialize)]
pub struct WaitOutcome {
    pub pane: PaneId,
    pub state: &'static str,
    pub exited: Option<u32>,
    pub timed_out: bool,
    pub waited_ms: u64,
}

impl WaitOutcome {
    /// Exit code for this outcome.
    ///
    /// `exited` is code 4 even when the caller asked to wait for it: `exited`
    /// is in the default set so the wait *terminates*, not because a dead agent
    /// is a success. `butai agent wait 7 -q && ./deploy.sh` must not deploy
    /// because the agent's process fell over.
    pub fn code(&self) -> u8 {
        if self.timed_out {
            exit::TIMED_OUT
        } else if self.state == "exited" {
            exit::EXITED
        } else {
            exit::OK
        }
    }
}

/// Poll until the agent at `at` is in one of `until`, or `timeout_ms` elapses.
///
/// When `since_seq` is given, a state only counts once the daemon has emitted a
/// notification for this pane past that sequence — the fix for the level-vs-edge
/// trap, where a wait started right after a prompt returns instantly on the
/// *previous* turn's `finished`. A state that differs from the one observed on
/// the first poll also counts, since `idle` never produces a notification.
async fn wait_for(
    api: &Api,
    at: Resolved,
    until: &[AgentState],
    timeout_ms: u64,
    since_seq: Option<u64>,
) -> Result<WaitOutcome> {
    let started = Instant::now();
    let deadline = started + Duration::from_millis(timeout_ms);
    let agents_path = format!("/v1/workspaces/{}/agents", at.ws);

    let mut seq = since_seq;
    let mut first_state: Option<AgentState> = None;
    let mut last = (AgentState::Idle, None);
    let mut backoff = POLL_MIN;

    loop {
        let agents: Vec<AgentDto> = api.get_as(&agents_path).await?;
        match agents.iter().find(|a| a.pane == at.pane) {
            Some(a) => {
                last = (a.state, a.exited);
                let first = *first_state.get_or_insert(a.state);
                // Without a sequence to beat, any matching state ends the wait.
                // With one, the state has to be demonstrably new: either the
                // daemon notified about this pane since, or the state has
                // changed under us (which covers `idle`, the one state that
                // never notifies).
                let fresh = match seq {
                    None => true,
                    Some(s) => a.state != first || notified_since(api, at.pane, s).await?,
                };
                if fresh && until.contains(&a.state) {
                    return Ok(WaitOutcome {
                        pane: at.pane,
                        state: state_name(a.state),
                        exited: a.exited,
                        timed_out: false,
                        waited_ms: started.elapsed().as_millis() as u64,
                    });
                }
                if fresh {
                    // Stop re-checking the feed once freshness is established.
                    seq = None;
                }
            }
            // The row is gone. A *clean* agent exit removes it and notifies
            // nothing, so this disappearance is the only evidence there is —
            // and the reason this waits on state rather than on events.
            None => {
                return Ok(WaitOutcome {
                    pane: at.pane,
                    state: "exited",
                    exited: last.1,
                    timed_out: false,
                    waited_ms: started.elapsed().as_millis() as u64,
                })
            }
        }

        if Instant::now() >= deadline {
            return Ok(WaitOutcome {
                pane: at.pane,
                state: state_name(last.0),
                exited: last.1,
                timed_out: true,
                waited_ms: started.elapsed().as_millis() as u64,
            });
        }
        tokio::time::sleep(backoff.min(deadline.saturating_duration_since(Instant::now()))).await;
        backoff = (backoff * 2).min(POLL_MAX);
    }
}

/// Whether the daemon has reported anything about `pane` past sequence `seq`.
async fn notified_since(api: &Api, pane: PaneId, seq: u64) -> Result<bool> {
    let feed: NotificationsDto = api.get_as(&format!("/v1/notifications?since={seq}")).await?;
    Ok(feed.items.iter().any(|n| n.pane == pane))
}

/// The feed's current head, so a wait can ignore everything already in it.
async fn notification_head(api: &Api) -> Result<u64> {
    let feed: NotificationsDto =
        api.get_as(&format!("/v1/notifications?since={}", u64::MAX)).await?;
    Ok(feed.head)
}

fn report(ctx: &Ctx, outcome: &WaitOutcome) -> Result<u8> {
    ctx.out.emit_owned(outcome, |w| {
        if outcome.timed_out {
            writeln!(
                w,
                "pane {} still {} after {}ms",
                outcome.pane, outcome.state, outcome.waited_ms
            )?;
        } else {
            writeln!(w, "pane {} {}", outcome.pane, outcome.state)?;
        }
        Ok(())
    })?;
    Ok(outcome.code())
}

// ---------------------------------------------------------------------------
// The other verbs
// ---------------------------------------------------------------------------

async fn ls(ctx: &Ctx, target: Option<String>) -> Result<u8> {
    pane::run(pane::PaneCmd::Ls { target }, ctx).await
}

async fn spawn(
    ctx: &Ctx,
    kind: &str,
    background: bool,
    prompt: Option<String>,
    wait: bool,
    timeout: u64,
) -> Result<u8> {
    let ws = pane::resolve_ws(&ctx.api, ctx.ws.clone(), "the new agent").await?;
    let path = format!("/v1/workspaces/{ws}/agents");

    // The spawn route answers `{"ok":true}`, not the new pane id, so the id has
    // to be recovered by diffing. Pane ids come from one daemon-wide counter, so
    // "the highest id that was not there before" is unambiguous.
    let before: Vec<AgentDto> = ctx.api.get_as(&path).await?;
    let known: Vec<PaneId> = before.iter().map(|a| a.pane).collect();

    let head = if wait { Some(notification_head(&ctx.api).await?) } else { None };
    ctx.api.post(&path, &serde_json::json!({ "type": kind, "background": background })).await?;

    let after: Vec<AgentDto> = ctx.api.get_as(&path).await?;
    let Some(fresh) = after.iter().map(|a| a.pane).filter(|p| !known.contains(p)).max() else {
        anyhow::bail!("the daemon accepted the spawn but no new agent appeared");
    };
    let at = Resolved { ws, pane: fresh };

    // The pane id on stdout, bare, so `P=$(butai agent spawn claude)` works —
    // the same shape `butai ws create` already has.
    ctx.out.emit_owned(&serde_json::json!({ "pane": fresh }), |w| {
        writeln!(w, "{fresh}")?;
        Ok(())
    })?;

    let Some(prompt) = prompt else { return Ok(exit::OK) };
    // Never onto stdout: that is the pane id and nothing else, so a caller can
    // keep writing `P=$(butai agent spawn claude --prompt …)`.
    if !wait_ready(&ctx.api, at, READY_TIMEOUT).await? {
        eprintln!(
            "pane {fresh} drew nothing in {}ms; sending the prompt anyway",
            READY_TIMEOUT.as_millis()
        );
    }
    deliver(ctx, at, &prompt).await?;
    if !wait {
        return Ok(exit::OK);
    }
    let until = parse_until(None)?;
    let outcome = wait_for(&ctx.api, at, &until, timeout, head).await?;
    // The pane id was already printed; report the wait to stderr so stdout
    // stays exactly the id.
    if outcome.timed_out {
        eprintln!("pane {} still {} after {}ms", outcome.pane, outcome.state, outcome.waited_ms);
    }
    Ok(outcome.code())
}

async fn send(
    ctx: &Ctx,
    target: &str,
    text: Vec<String>,
    wait: bool,
    until: Option<String>,
    timeout: u64,
) -> Result<u8> {
    let at = pane::resolve(&ctx.api, target, ctx.ws.as_deref()).await?;
    pane::refuse_self(at, "send")?;
    if text.is_empty() {
        return exit::usage("nothing to send: pass the prompt as arguments");
    }
    let until = parse_until(until.as_deref())?;

    // Read the feed's head *before* injecting, so the wait can tell this turn's
    // `finished` from the one already on screen.
    let head = if wait { Some(notification_head(&ctx.api).await?) } else { None };
    deliver(ctx, at, &text.join(" ")).await?;
    if !wait {
        return Ok(exit::OK);
    }
    let outcome = wait_for(&ctx.api, at, &until, timeout, head).await?;
    report(ctx, &outcome)
}

/// Block until a freshly spawned agent has drawn something, or `timeout`.
/// Answers whether it drew.
///
/// The spawn route returns as soon as the PTY exists, but an agent CLI needs
/// about a second more before it is reading input — and what is typed into that
/// gap does not simply queue. The paste survives, because it lands in the input
/// box the moment the box exists; the Enter after it does not, because the
/// startup drains what is already buffered. The result is a prompt sitting
/// unsubmitted in the agent's box while `--wait` blocks for the full timeout on
/// a turn that was never started.
///
/// A non-blank footer is the signal. It is the band the daemon's own state
/// detector scans, so it costs a route that already exists, and a TUI paints
/// *because* its input loop is running — the two were simultaneous on every
/// measurement, never the other way round.
async fn wait_ready(api: &Api, at: Resolved, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    let path = format!("{}/output?lines=8&source=footer&format=text", at.route());
    loop {
        let dto: PaneOutputDto = api.get_as(&path).await?;
        if dto.lines.iter().any(|l| !l.trim().is_empty()) {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(READY_POLL.min(deadline.saturating_duration_since(Instant::now())))
            .await;
    }
}

/// Paste a prompt into a pane and submit it.
async fn deliver(ctx: &Ctx, at: Resolved, text: &str) -> Result<()> {
    let input = format!("{}/input", at.route());
    ctx.api.post(&input, &serde_json::json!({ "paste": text })).await?;
    ctx.api.post(&input, &serde_json::json!({ "key": { "code": "enter" } })).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_set_is_finished_and_exited_but_never_idle() {
        let until = parse_until(None).unwrap();
        assert!(until.contains(&AgentState::Finished));
        assert!(until.contains(&AgentState::Exited));
        // A fresh agent starts idle, so this would return immediately.
        assert!(!until.contains(&AgentState::Idle), "idle must not be a default");
    }

    #[test]
    fn states_and_aliases_both_parse() {
        assert_eq!(parse_until(Some("waiting")).unwrap(), vec![AgentState::Waiting]);
        let done = parse_until(Some("done")).unwrap();
        assert!(done.contains(&AgentState::Finished) && done.contains(&AgentState::Idle));
        let attention = parse_until(Some("attention")).unwrap();
        assert!(attention.contains(&AgentState::Waiting));
        assert!(!attention.contains(&AgentState::Working));
    }

    #[test]
    fn a_set_is_deduplicated_and_order_independent() {
        let a = parse_until(Some("finished,exited,finished")).unwrap();
        assert_eq!(a.len(), 2, "duplicates collapse: {a:?}");
        assert_eq!(parse_until(Some(" finished , exited ")).unwrap(), a, "spacing is ignored");
    }

    #[test]
    fn an_unknown_state_is_a_usage_error_listing_the_real_ones() {
        let err = parse_until(Some("finished,sideways")).unwrap_err();
        assert_eq!(exit::code_for(&err), exit::USAGE);
        assert!(err.to_string().contains("sideways"), "name the offender: {err}");
        assert!(err.to_string().contains("finished"), "list the alternatives: {err}");
        assert_eq!(exit::code_for(&parse_until(Some(" , ")).unwrap_err()), exit::USAGE);
    }

    /// Only a target that names a pane outright can have its absence read as
    /// "it exited"; a name or a scoped target keeps the 404.
    #[test]
    fn only_a_bare_pane_id_can_be_reported_as_gone() {
        assert_eq!(bare_pane("7"), Some(PaneId(7)));
        assert_eq!(bare_pane(" 7 "), Some(PaneId(7)), "spacing is the caller's");
        assert_eq!(bare_pane("1:7"), None, "a scope can fail on its own");
        assert_eq!(bare_pane("stage"), None);
        assert_eq!(bare_pane("claude"), None, "a name nothing matches is a typo");
        assert_eq!(bare_pane("-1"), None);
    }

    /// A pane that has gone reports the same code whether the wait spanned the
    /// exit or started after it. The two paths build this outcome separately,
    /// so the agreement is worth pinning.
    #[test]
    fn a_pane_that_is_already_gone_exits_like_one_that_goes_mid_wait() {
        let gone = WaitOutcome {
            pane: PaneId(7),
            state: "exited",
            exited: None,
            timed_out: false,
            waited_ms: 0,
        };
        assert_eq!(gone.code(), exit::EXITED);
        assert_ne!(gone.code(), exit::NOT_FOUND, "the same situation, the same code");
    }

    #[test]
    fn an_exit_is_never_reported_as_success() {
        let dead = WaitOutcome {
            pane: PaneId(7),
            state: "exited",
            exited: Some(1),
            timed_out: false,
            waited_ms: 10,
        };
        assert_eq!(dead.code(), exit::EXITED, "`wait && deploy` must not fire on a dead agent");

        let ok = WaitOutcome {
            pane: PaneId(7),
            state: "finished",
            exited: None,
            timed_out: false,
            waited_ms: 10,
        };
        assert_eq!(ok.code(), exit::OK);

        let slow = WaitOutcome {
            pane: PaneId(7),
            state: "working",
            exited: None,
            timed_out: true,
            waited_ms: 300_000,
        };
        assert_eq!(slow.code(), exit::TIMED_OUT);
    }
}
