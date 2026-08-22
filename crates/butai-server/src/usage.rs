//! Account standing for every configured agent CLI — what the USAGE page draws.
//!
//! # What this can and cannot know
//!
//! **No agent CLI reports its account limits through a subcommand**, and asking
//! a provider for them would mean authenticating as the user. But `claude` does
//! not need to be asked: it writes the numbers its own `/usage` screen draws
//! into `~/.claude.json` under `cachedUsageUtilization`, in plain JSON, and
//! refreshes them while it runs. Reading that key is what lets this page show a
//! **session limit with a percentage and a reset instant** rather than a token
//! total with no denominator. See [`published`].
//!
//! So the daemon reports, in order of how much it can stand behind:
//!
//! - **the limits the provider published**, when the CLI cached them *recently
//!   enough to still be about now*. These are proportions (`unit: Percent`,
//!   `of: Some(100)`) with a real reset instant, and they are the only numbers
//!   here that come from the provider rather than from arithmetic done on this
//!   machine ([`UsageSource::Published`]). **A cached limit is a snapshot, not a
//!   feed** — `claude` refreshes it only when it runs and decides to fetch, so
//!   it goes stale without bound. See [`window_span`] for the rule that decides
//!   when a snapshot has stopped describing the window it names.
//! - **what has been spent in a rolling window**, by summing the CLI's own
//!   transcripts, for CLIs that publish nothing but do record what a turn cost.
//!   `gemini` is the case here: no ceiling anywhere on disk, but every assistant
//!   turn carries its token counts. That total has no denominator unless the
//!   user declares one, and it says so ([`CliState::Counted`] vs
//!   [`CliState::Metered`]) rather than inventing a ceiling.
//! - **installed or not**, and the version, by resolving and probing the binary;
//! - **the account and plan**, where the CLI writes them somewhere that is not a
//!   secret. `~/.claude.json` records `oauthAccount.emailAddress` and
//!   `organizationRateLimitTier`; `~/.gemini/google_accounts.json` records the
//!   active address and `settings.json` how it signed in. All plain JSON, so
//!   reading them costs no trust. **A credential store is never opened** —
//!   neither `.credentials.json` nor `oauth_creds.json` nor
//!   `antigravity-oauth-token`. Authenticating to a provider *as the user* is a
//!   decision they have not made, and it is not made here.
//!
//! And where the answer is *nothing*, it says which kind of nothing. `agy` is
//! the interesting one: it **has** a quota and never writes it down — its
//! `quota_manager` pulls a user quota summary into an in-memory cache on each
//! run — and its sessions record no per-turn cost either, so there is neither a
//! limit to read nor turns to total. That is [`CliState::Unknown`], and it is a
//! different fact from `aider`'s [`CliState::NoAccount`].
//!
//! The *counted* windows are five hours and seven days because those are the
//! shapes the real subscription limits come in. They are *rolling* — with
//! nothing published, the provider's window boundary is not knowable from here —
//! which is why they are labelled "last 5h" rather than "session". A published
//! window has a genuine boundary and is labelled for it.
//!
//! # A stale snapshot is not a small number
//!
//! The two kinds of window can appear **together**, and that is the point of
//! [`claude_windows`]. When a cached limit has outlived the window it describes,
//! dropping it silently would leave the page emptier than the machine — so
//! whatever the snapshot can no longer speak for is counted from the transcripts
//! instead, and the note says both halves.
//!
//! This is the fix for a real defect: the cache was trusted unconditionally, so
//! a snapshot taken before a five-hour window rolled over drew a confident
//! `session 0%` — with a bar, the `Published` badge and `Metered` state, the
//! most authoritative rendering this page has — on a machine that had spent
//! millions of tokens in that very window. **The freshness was read and spent
//! only on prose.** A number nobody can act on is worse than a blank, and a
//! wrong one wearing the provider's authority is worse than either.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use butai_protocol::api::{
    CliState, CliUsageDto, UsageDto, UsageSource, UsageUnit, UsageWindowDto,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::config::{AgentDef, BudgetDef};
use crate::core::Event;

/// How often the roster is rebuilt. Slower than the SYSTEM sampler by two
/// orders of magnitude: a version does not change while you watch, and the
/// window totals move in minutes, not frames.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(60);

/// A `--version` probe that hangs must not wedge the sampler. Every known CLI
/// answers in well under this.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

const FIVE_HOURS_MS: u64 = 5 * 60 * 60 * 1000;
const SEVEN_DAYS_MS: u64 = 7 * 24 * 60 * 60 * 1000;

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// One [`Counter`] per CLI, keyed by command.
///
/// Per CLI rather than one shared counter, because a counter *is* a set of
/// counted turns: pouring two CLIs' transcripts into one would report each
/// account's spend as the sum of both. Keyed on the command for the reason
/// [`one`] branches on it — the `[[agents]]` name is the user's label and can
/// be anything.
pub type Counters = HashMap<String, Counter>;

/// Rebuild the roster every [`SAMPLE_INTERVAL`] and hand it to the core.
///
/// Owns its [`Counters`] across ticks, which is what keeps the transcript scan
/// cheap: an append-only transcript is read only past where the last pass
/// stopped, and a rewritten one is skipped entirely unless its mtime moved.
pub fn spawn_sampler(tx: UnboundedSender<Event>, agents: Vec<AgentDef>, budgets: Vec<BudgetDef>) {
    tokio::spawn(async move {
        let mut counters = Counters::new();
        loop {
            let dto = sample(&agents, &budgets, &mut counters).await;
            if tx.send(Event::Usage(dto)).is_err() {
                return; // core is gone
            }
            tokio::time::sleep(SAMPLE_INTERVAL).await;
        }
    });
}

/// One pass: probe every configured agent and count what is countable.
pub async fn sample(
    agents: &[AgentDef],
    budgets: &[BudgetDef],
    counters: &mut Counters,
) -> UsageDto {
    let now = now_ms();
    let mut clis = Vec::new();
    for def in agents {
        let counter = counters.entry(def.command.clone()).or_default();
        clis.push(one(def, budgets, counter, now).await);
    }
    UsageDto { clis, sampled_ms: now }
}

async fn one(
    def: &AgentDef,
    budgets: &[BudgetDef],
    counter: &mut Counter,
    now: u64,
) -> CliUsageDto {
    let mut dto = CliUsageDto {
        name: def.name.clone(),
        command: def.command.clone(),
        state: CliState::Absent,
        version: None,
        account: None,
        plan: None,
        windows: Vec::new(),
        panes: Vec::new(),
        source: UsageSource::None,
        note: None,
    };

    let Some(program) = resolve(&def.command) else {
        dto.note = Some(format!(
            "not installed — no `{}` on PATH, or where a pane would look for it",
            def.command
        ));
        return dto;
    };
    dto.version = probe_version(&program).await;

    // Per-CLI knowledge, keyed on the command rather than the `[[agents]]` name:
    // the name is the user's label and can be anything, the binary is what
    // decides where the transcripts and the account file live.
    match def.command.as_str() {
        "claude" => {
            let account = claude_account();
            dto.account = account.as_ref().and_then(|a| a.email.clone());
            dto.plan = account.as_ref().and_then(|a| a.plan.clone());
            let (windows, source, state, note) = claude_windows(counter, now);
            dto.windows = windows;
            dto.source = source;
            dto.state = state;
            dto.note = Some(note);
        }
        // Gemini publishes no ceiling anywhere on disk, but its sessions do
        // record what each turn cost, so the windows are countable.
        "gemini" => {
            let account = gemini_account();
            dto.account = account.email;
            dto.plan = account.plan;
            counter.refresh_rewritten(&gemini_dir(), now, gemini_sessions, parse_gemini_session);
            dto.windows = counter.windows(now);
            dto.source = UsageSource::Transcripts;
            dto.state = CliState::Counted;
            dto.note = Some(
                "counted from this machine's sessions — gemini publishes no limit to compare it against"
                    .into(),
            );
        }
        // Antigravity *has* a quota and refuses to write it down: its
        // `quota_manager` fetches a user quota summary into an in-memory cache
        // on each run and persists nothing, so there is no file to read and no
        // subcommand to ask. Its sessions record no per-turn cost either, so
        // unlike gemini there is nothing to total up instead.
        "agy" => {
            dto.state = CliState::Unknown;
            dto.note = Some(
                "agy fetches its quota per run and keeps it in memory — nothing on disk".into(),
            );
        }
        // Bring-your-own-key, billed by the provider: there is no account here
        // to have a limit, and saying so is the answer.
        "aider" => {
            dto.state = CliState::NoAccount;
            dto.note = Some("runs on your own API key — the provider bills you directly".into());
        }
        _ => {
            dto.state = CliState::Unknown;
            dto.note = Some("butai cannot read this CLI's usage yet".into());
        }
    }

    apply_budgets(&mut dto, budgets);
    dto
}

/// Turn declared ceilings into denominators.
///
/// A budget the user wrote is the only ceiling butai ever draws a proportion
/// against, and matching it by label keeps the config readable at the cost of
/// nothing: an unmatched window simply does not apply, which is the right
/// failure for a typo in a number nothing can validate anyway.
fn apply_budgets(dto: &mut CliUsageDto, budgets: &[BudgetDef]) {
    let mut any = false;
    for w in &mut dto.windows {
        if let Some(b) = budgets.iter().find(|b| b.agent == dto.name && b.window == w.label) {
            w.of = Some(b.tokens);
            any = true;
        }
    }
    if any {
        dto.state = CliState::Metered;
        dto.source = UsageSource::Declared;
        dto.note = Some("measured against the budget in config.toml, not a published limit".into());
    }
}

/// The binary a pane would launch for this agent, or `None` if there is none.
///
/// **`PATH` alone is the wrong question**, and answering it is what made this
/// page report a working install as missing. The daemon inherits its
/// environment from whatever started it — a desktop session, a systemd unit,
/// the client that auto-spawned it — which is rarely the login shell the user
/// installed their agents from. On the machine this was found on the daemon's
/// `PATH` was `~/.local/bin:/usr/bin:/bin` while `claude` and `gemini` live
/// under `~/.nvm/versions/node/*/bin`: every pane launched them, because the
/// spawner falls back to the directories a login shell would have added, and
/// USAGE called both of them uninstalled.
///
/// So resolution is the spawner's own [`resolve_program`], not a second copy of
/// it — this page reports on the binary that would actually run, or it is
/// describing a different machine.
///
/// [`resolve_program`]: crate::pane::terminal::resolve_program
fn resolve(cmd: &str) -> Option<PathBuf> {
    let program = crate::pane::terminal::resolve_program(cmd);
    // The name comes back unchanged when nothing was found, so a bare one still
    // has to be looked up; a path the fallback did find needs only the same
    // executability check `which` would have made.
    if program.contains('/') {
        let p = PathBuf::from(program);
        return is_exec(&p).then_some(p);
    }
    which(&program)
}

/// First match for `cmd` on `PATH`. An absolute or relative path is taken as
/// given, matching how the pane spawner would launch it.
fn which(cmd: &str) -> Option<PathBuf> {
    if cmd.contains('/') {
        let p = PathBuf::from(cmd);
        return is_exec(&p).then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(cmd)).find(|p| is_exec(p))
}

#[cfg(unix)]
fn is_exec(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_exec(p: &Path) -> bool {
    p.is_file()
}

/// `<program> --version`, first line, trimmed.
///
/// The CLIs disagree on the shape of the answer (`2.1.4 (Claude Code)`,
/// `0.53.1`), so only the noise every one of them adds is stripped: the version
/// itself is passed through as written rather than parsed into something this
/// code would then have to keep true.
///
/// Run with the `PATH` a pane's child gets, because that is what this probe is:
/// an npm-installed CLI is a `#!/usr/bin/env node` launcher, and on the
/// daemon's inherited `PATH` that `node` is the distribution's — v10 on the
/// machine this was found on, which cannot parse the file. Probing without the
/// repair reports no version for a CLI that runs fine in a pane.
async fn probe_version(program: &Path) -> Option<String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.arg("--version");
    if let Some(path) = crate::pane::terminal::child_path() {
        cmd.env("PATH", path);
    }
    let out = tokio::time::timeout(PROBE_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

struct Account {
    email: Option<String>,
    plan: Option<String>,
}

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

fn claude_projects_dir() -> PathBuf {
    home().unwrap_or_default().join(".claude/projects")
}

fn claude_json_path() -> PathBuf {
    home().unwrap_or_default().join(".claude.json")
}

/// What this machine's transcripts say has been spent, as rolling windows.
fn counted_windows(counter: &mut Counter, now: u64) -> Vec<UsageWindowDto> {
    counter.refresh(&claude_projects_dir(), now);
    counter.windows(now)
}

/// Claude's windows: the limits it published, plus a count for whatever those
/// no longer describe.
///
/// The three outcomes are three different sentences, and the note says which:
///
/// - **nothing cached** — an old `claude`, or one that has never run here. Count
///   everything, exactly as a CLI that publishes nothing is handled.
/// - **cached and current** — use the provider's numbers and nothing else. They
///   have real boundaries and a real denominator, which no arithmetic here can
///   produce.
/// - **cached but outrun** — keep the windows the snapshot still speaks for and
///   count the rest. A week-long window barely moves in the hours a five-hour
///   one takes to roll over completely, so the two halves of one cache go stale
///   at wildly different rates and are worth separating.
///
/// Returned as a tuple rather than written into the dto, so the decision can be
/// asserted without building one — what makes this right or wrong is which
/// source answers for which window, not where the fields land.
fn claude_windows(
    counter: &mut Counter,
    now: u64,
) -> (Vec<UsageWindowDto>, UsageSource, CliState, String) {
    let Some(p) = published(&claude_json_path(), now) else {
        return (
            counted_windows(counter, now),
            UsageSource::Transcripts,
            CliState::Counted,
            "counted from this machine's transcripts — claude has not cached its limits here yet"
                .into(),
        );
    };
    let age = ago(now.saturating_sub(p.fetched_ms));

    // Every window outrun: the snapshot describes nothing that still exists, so
    // it is worth exactly as much as no snapshot at all — but for a different
    // reason, and the reader is owed the difference.
    if p.windows.is_empty() {
        return (
            counted_windows(counter, now),
            UsageSource::Transcripts,
            CliState::Counted,
            format!(
                "counted from this machine's transcripts — claude's cached limits are {age} old and describe windows that have since rolled over"
            ),
        );
    }

    let mut windows = p.windows;
    if p.rolled_over {
        windows.extend(counted_windows(counter, now));
        return (
            windows,
            UsageSource::Published,
            CliState::Metered,
            format!(
                "published by claude {age} ago — the windows that snapshot has outlived are counted from this machine's transcripts instead"
            ),
        );
    }
    (
        windows,
        UsageSource::Published,
        CliState::Metered,
        format!("published by claude, read {age} ago from ~/.claude.json"),
    )
}

/// The limits `claude` cached, as windows the page can draw.
pub struct Published {
    /// When the CLI last refreshed them, epoch millis. On the note, because a
    /// limit is only as good as its age.
    pub fetched_ms: u64,
    pub windows: Vec<UsageWindowDto>,
    /// At least one window was dropped because the snapshot no longer describes
    /// it — either the cache has outlived the window's own span, or the window's
    /// reset instant has passed.
    ///
    /// The caller counts transcripts to fill the hole, so this is not merely a
    /// flag for a message: it is the difference between a page that admits what
    /// it cannot see and one that draws a stale zero.
    pub rolled_over: bool,
}

/// The account limits `claude` last cached, out of `~/.claude.json`.
///
/// **This is the only number on the page the provider actually stated.**
/// `cachedUsageUtilization` is what the CLI's own `/usage` screen renders — a
/// percentage per window and the instant each one resets — refreshed whenever
/// claude runs. It sits in the same plain config file the account and plan come
/// from, so reading it opens no credential store and asks the provider nothing.
///
/// The `limits` array is read rather than the sibling per-window objects
/// (`five_hour`, `seven_day`, `seven_day_opus`, and a rotating cast of internal
/// codenames): it is the normalised shape, it carries the scope a window applies
/// to, and it does not grow a new key every time a plan gains a tier.
///
/// Undocumented internal config, so every field is optional on the way out: a
/// shape this does not recognise yields no windows and the caller falls back to
/// counting transcripts, which is the same thing that happens on an old claude
/// that never wrote the key.
pub fn published(path: &Path, now: u64) -> Option<Published> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let cached = v.get("cachedUsageUtilization")?;
    let fetched_ms = cached.get("fetchedAtMs").and_then(|f| f.as_u64()).unwrap_or(0);
    let limits = cached.get("utilization")?.get("limits")?.as_array()?;

    // How old the snapshot is. Every trust decision below is made against this
    // one reading rather than the clock, so a slow parse cannot move a window
    // across the boundary mid-loop.
    let age = now.saturating_sub(fetched_ms);

    let mut windows = Vec::new();
    let mut rolled_over = false;
    for l in limits {
        let kind = l.get("kind").and_then(|k| k.as_str()).unwrap_or_default();
        let percent = l.get("percent").and_then(|p| p.as_u64()).unwrap_or(0);
        let active = l.get("is_active").and_then(|a| a.as_bool()).unwrap_or(false);
        // Session and the all-model week are the two every account has; drawing
        // them at 0% is the answer to "how much room do I have". A scoped
        // window is per-model and only exists once that model has been used —
        // an always-present `week · opus` row on an account that never touches
        // opus is a row that says nothing, the same call `Counter::windows`
        // makes about its own opus row.
        let universal = matches!(kind, "session" | "weekly_all");
        if !universal && !active && percent == 0 {
            continue;
        }
        let resets_ms = l.get("resets_at").and_then(|r| r.as_str()).and_then(parse_rfc3339_ms);
        // A window whose reset instant has passed has emptied, and the cached
        // percentage describes a window that no longer exists. Claude refreshes
        // this file only when it runs, so this is reached whenever nobody has
        // run an agent since the boundary — exactly when a stale percentage
        // would be most misleading.
        let expired = resets_ms.is_some_and(|r| r <= now);
        // And a boundary is not the only way to lose a window. A `resets_at` of
        // `null` is common — an idle session window has none — so the boundary
        // check alone lets an arbitrarily old percentage through untouched.
        // Ageing the snapshot against the window's own span catches it.
        let outrun = window_span(kind).is_some_and(|span| age > span);
        if expired || outrun {
            rolled_over = true;
            continue;
        }
        windows.push(UsageWindowDto {
            label: window_label(kind, l.get("scope")),
            used: percent,
            of: Some(100),
            unit: UsageUnit::Percent,
            resets_ms,
        });
    }
    Some(Published { fetched_ms, windows, rolled_over })
}

/// How long the window a `kind` names runs for, where that is knowable.
///
/// **This is the whole trust rule.** A cached percentage is evidence about *now*
/// only while the snapshot is younger than the window it describes: once the
/// cache has outlived a five-hour window, every second of that window happened
/// after the reading, and the number is about a period that has entirely gone.
/// The same cache can be perfectly good for the seven-day row at the same
/// instant, which is why the rule is per window rather than per file.
///
/// It is deliberately the window's *own* span and not a tunable: any constant
/// here would be a guess about how often `claude` refreshes, and the CLI makes
/// no promise about that. A kind with no known span yields `None` and is trusted
/// as before — inventing a span for a window this has never seen would throw
/// away a number on the strength of a guess.
fn window_span(kind: &str) -> Option<u64> {
    match kind {
        "session" => Some(FIVE_HOURS_MS),
        k if k.starts_with("weekly") => Some(SEVEN_DAYS_MS),
        _ => None,
    }
}

/// `weekly_scoped` + a scope naming Opus -> `week · opus`.
///
/// The scope's model is whatever the provider called it, lowercased to sit
/// beside the other labels rather than shouting a proper noun mid-row.
fn window_label(kind: &str, scope: Option<&serde_json::Value>) -> String {
    let model = scope
        .and_then(|s| s.get("model"))
        .and_then(|m| m.get("display_name"))
        .and_then(|n| n.as_str())
        .map(str::to_lowercase);
    match kind {
        "session" => "session".into(),
        "weekly_all" => "week · all models".into(),
        "weekly_scoped" => match model {
            Some(m) => format!("week · {m}"),
            None => "week · scoped".into(),
        },
        // A kind this has never seen still draws: the percentage is the useful
        // part, and inventing a prettier name for it would only hide it.
        other => other.replace('_', " "),
    }
}

/// A coarse age for the provenance note — `4m`, `2h`, `3d`.
fn ago(ms: u64) -> String {
    let secs = ms / 1000;
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

/// The account and plan out of `~/.claude.json`.
///
/// Plain config, not a credential store: `.credentials.json` sits beside it and
/// is deliberately never opened. The plan arrives as an internal tier string
/// (`default_claude_max_5x`), so it is tidied into something a person reads.
fn claude_account() -> Option<Account> {
    let text = std::fs::read_to_string(home()?.join(".claude.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let oauth = v.get("oauthAccount")?;
    let email = oauth.get("emailAddress").and_then(|e| e.as_str()).map(str::to_string);
    let tier = oauth
        .get("organizationRateLimitTier")
        .or_else(|| oauth.get("userRateLimitTier"))
        .and_then(|t| t.as_str())
        .map(plan_label);
    Some(Account { email, plan: tier })
}

/// `default_claude_max_5x` -> `max 5x`. Unknown shapes pass through unchanged
/// rather than being forced into the pattern — a tier this has never seen is
/// still more useful on screen than a blank.
fn plan_label(tier: &str) -> String {
    tier.trim_start_matches("default_").trim_start_matches("claude_").replace('_', " ")
}

fn gemini_dir() -> PathBuf {
    home().unwrap_or_default().join(".gemini")
}

/// The signed-in Google account and how it authenticates, out of `~/.gemini`.
///
/// `google_accounts.json` records the active address and `settings.json` the
/// auth type it was chosen with — both plain config the CLI wrote itself.
/// `oauth_creds.json` sits beside them holding the actual token and is
/// deliberately never opened, exactly as `.credentials.json` is not for claude.
fn gemini_account() -> Account {
    let dir = gemini_dir();
    let email = std::fs::read_to_string(dir.join("google_accounts.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("active")?.as_str().map(str::to_string))
        .filter(|s| !s.is_empty());
    // `oauth-personal` -> `oauth personal`. Not a plan — Gemini's tier is not
    // written down anywhere here — but it is the difference between a personal
    // login and a workspace one, which is what a reader is asking.
    let plan = std::fs::read_to_string(dir.join("settings.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.get("security")?
                .get("auth")?
                .get("selectedType")?
                .as_str()
                .map(|s| s.replace('-', " "))
        });
    Account { email, plan }
}

/// Every gemini chat session touched since `cutoff`.
///
/// One level deeper than claude's layout — `tmp/<project>/chats/*.json` rather
/// than `projects/<project>/*.jsonl` — and the files are whole JSON documents
/// rewritten in place as a session grows, not append-only lines.
fn gemini_sessions(dir: &Path, cutoff: u64) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(dir.join("tmp")) else { return out };
    for project in projects.flatten() {
        let Ok(files) = std::fs::read_dir(project.path().join("chats")) else { continue };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if mtime_ms(&f).is_none_or(|m| m >= cutoff) {
                out.push(path);
            }
        }
    }
    out
}

/// One gemini session file's assistant turns.
///
/// **`cached` comes off the total**, for the reason claude's cache reads do:
/// `total` is `input + output + thoughts + tool`, and the cached slice of
/// `input` is context replayed rather than work done. Verified against this
/// machine's 1,491 recorded turns — `total` matches that sum and `cached` never
/// exceeds `input` — so the subtraction cannot go negative on real data, and
/// saturates rather than wrapping if it ever did.
fn parse_gemini_session(text: &str) -> Vec<Entry> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    let Some(messages) = v.get("messages").and_then(|m| m.as_array()) else { return Vec::new() };
    let mut out = Vec::new();
    for m in messages {
        let Some(t) = m.get("tokens").filter(|t| t.is_object()) else { continue };
        let n = |k: &str| t.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        let Some(ms) = m.get("timestamp").and_then(|t| t.as_str()).and_then(parse_rfc3339_ms)
        else {
            continue;
        };
        // The message id is the dedup key: a session file is re-read whole
        // whenever it grows, so every earlier turn is seen again each pass.
        let Some(id) = m.get("id").and_then(|i| i.as_str()) else { continue };
        out.push(Entry {
            ms,
            tokens: n("total").saturating_sub(n("cached")),
            opus: false, // gemini has no model family metered on its own
            request: id.to_string(),
        });
    }
    out
}

/// A directory entry's mtime in epoch millis.
fn mtime_ms(f: &std::fs::DirEntry) -> Option<u64> {
    let t = f.metadata().ok()?.modified().ok()?;
    Some(t.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64)
}

/// One assistant turn's token cost, as recorded in a transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    ms: u64,
    tokens: u64,
    /// True for the model family with a limit of its own.
    opus: bool,
    /// The provider's request id, for deduplication.
    request: String,
}

/// Incremental transcript reader.
///
/// Two layouts, one accumulator. Claude's transcripts are append-only, so each
/// pass reads from where the last one stopped ([`Self::refresh`]). Gemini
/// rewrites a session file whole every time it grows, so there is no offset to
/// resume from and the file is re-read when its mtime moves
/// ([`Self::refresh_rewritten`]). Either way the parsed entries are kept, which
/// is what makes a 60-second sampler affordable on a machine with a year of
/// sessions on disk: the first pass pays for the retention window, every pass
/// after it pays only for what actually changed.
#[derive(Default)]
pub struct Counter {
    /// Bytes already consumed, per append-only transcript.
    offsets: HashMap<PathBuf, u64>,
    /// Last-seen mtime, per rewritten transcript.
    mtimes: HashMap<PathBuf, u64>,
    entries: Vec<Entry>,
    seen: HashSet<String>,
}

impl Counter {
    /// Re-read every rewritten session under `dir` whose mtime has moved.
    ///
    /// Deduplication is by entry id rather than by position, because a rewritten
    /// file presents all of its earlier turns again on every read.
    fn refresh_rewritten(
        &mut self,
        dir: &Path,
        now: u64,
        list: fn(&Path, u64) -> Vec<PathBuf>,
        parse: fn(&str) -> Vec<Entry>,
    ) {
        let cutoff = now.saturating_sub(SEVEN_DAYS_MS);
        for path in list(dir, cutoff) {
            let stamp = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            // Unchanged since the last pass: its turns are already counted.
            if self.mtimes.get(&path) == Some(&stamp) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            self.mtimes.insert(path, stamp);
            for entry in parse(&text) {
                if self.seen.insert(entry.request.clone()) {
                    self.entries.push(entry);
                }
            }
        }
        self.prune(cutoff);
    }

    /// Read whatever is new under `dir` and drop anything older than the widest
    /// window.
    fn refresh(&mut self, dir: &Path, now: u64) {
        let cutoff = now.saturating_sub(SEVEN_DAYS_MS);
        for file in transcripts(dir, cutoff) {
            self.read_file(&file);
        }
        self.prune(cutoff);
    }

    fn read_file(&mut self, path: &Path) {
        let Ok(text) = std::fs::read_to_string(path) else { return };
        let len = text.len() as u64;
        let offset = self.offsets.entry(path.to_path_buf()).or_insert(0);
        // A transcript that shrank was rewritten under us; start it over rather
        // than slicing at an offset that now means something else.
        if len < *offset {
            *offset = 0;
        }
        let fresh = &text[*offset as usize..];
        *offset = len;
        // A partial last line (the CLI is mid-write) would parse as garbage and
        // be lost; rewind to the last newline so it is read whole next pass.
        let end = fresh.rfind('\n').map(|i| i + 1).unwrap_or(0);
        if end == 0 {
            *self.offsets.get_mut(path).expect("just inserted") = len - fresh.len() as u64;
            return;
        }
        *self.offsets.get_mut(path).expect("just inserted") = len - (fresh.len() - end) as u64;
        for line in fresh[..end].lines() {
            if let Some(entry) = parse_entry(line) {
                if self.seen.insert(entry.request.clone()) {
                    self.entries.push(entry);
                }
            }
        }
    }

    fn prune(&mut self, cutoff: u64) {
        self.entries.retain(|e| e.ms >= cutoff);
        self.seen = self.entries.iter().map(|e| e.request.clone()).collect();
    }

    /// The windows the page draws. The opus row appears only when there is opus
    /// usage to draw — an always-present zero would be a row that says nothing.
    fn windows(&self, now: u64) -> Vec<UsageWindowDto> {
        let sum = |since: u64, opus_only: bool| -> u64 {
            self.entries
                .iter()
                .filter(|e| e.ms >= since && (!opus_only || e.opus))
                .map(|e| e.tokens)
                .sum()
        };
        let five = now.saturating_sub(FIVE_HOURS_MS);
        let week = now.saturating_sub(SEVEN_DAYS_MS);
        let mut out =
            vec![window("last 5h", sum(five, false)), window("last 7d", sum(week, false))];
        let opus = sum(week, true);
        if opus > 0 {
            out.push(window("last 7d · opus", opus));
        }
        out
    }
}

/// A rolling window: counted, no ceiling, and no reset instant — the provider's
/// boundary is not knowable from here, and a made-up one would be read as fact.
fn window(label: &str, used: u64) -> UsageWindowDto {
    UsageWindowDto { label: label.into(), used, of: None, unit: UsageUnit::Tokens, resets_ms: None }
}

/// Every `*.jsonl` under `dir/*/` touched since `cutoff`.
///
/// The mtime filter is what keeps the first pass proportional to the retention
/// window rather than to everything ever recorded.
fn transcripts(dir: &Path, cutoff: u64) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(dir) else { return out };
    for project in projects.flatten() {
        let Ok(files) = std::fs::read_dir(project.path()) else { continue };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let fresh = f
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64 >= cutoff)
                .unwrap_or(true);
            if fresh {
                out.push(path);
            }
        }
    }
    out
}

/// One transcript line, if it records what an assistant turn cost.
///
/// **Cache reads are excluded on purpose.** They are twenty times the rest put
/// together on a long session and an order of magnitude cheaper per token, so
/// including them produces a number that moves with how much context was
/// replayed rather than with how much work was done — the opposite of what
/// somebody watching a limit wants to see.
fn parse_entry(line: &str) -> Option<Entry> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let msg = v.get("message")?;
    let usage = msg.get("usage")?;
    let n = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    let tokens = n("input_tokens") + n("cache_creation_input_tokens") + n("output_tokens");
    let model = msg.get("model").and_then(|m| m.as_str()).unwrap_or_default();
    // The request id is the deduplication key: a transcript repeats a turn when
    // it is resumed or forked, and counting it twice inflates the window.
    let request = v
        .get("requestId")
        .and_then(|r| r.as_str())
        .or_else(|| msg.get("id").and_then(|i| i.as_str()))?
        .to_string();
    Some(Entry {
        ms: parse_rfc3339_ms(v.get("timestamp")?.as_str()?)?,
        tokens,
        opus: model.contains("opus"),
        request,
    })
}

/// `2026-08-11T20:16:00.016Z` -> epoch millis.
///
/// Hand-rolled rather than taking a date dependency for one field: the date
/// half is fixed-width and the conversion is Howard Hinnant's `days_from_civil`,
/// which is exact for every date this will ever see.
///
/// The tail is *not* fixed-width, and both writers this reads are in it: a
/// transcript timestamp ends `.016Z`, while a cached limit's `resets_at` ends
/// `.901236+00:00` — microseconds and an explicit zero offset. Truncating the
/// fraction at three digits and subtracting the offset covers both, and an
/// offset that is not zero is subtracted rather than ignored, so a claude
/// configured to write local time does not land the reset six hours out.
fn parse_rfc3339_ms(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' {
        return None;
    }
    let num = |from: usize, to: usize| s.get(from..to)?.parse::<i64>().ok();
    let (y, m, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hh, mm, ss) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);

    // Fractional seconds, to whatever precision the writer chose.
    let mut i = 19;
    let mut millis = 0;
    if b.get(i) == Some(&b'.') {
        let start = i + 1;
        let mut end = start;
        while b.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == start {
            return None; // a decimal point with no digits after it
        }
        // `.9` is 900ms, not 9 — take at most three digits and scale.
        let frac = s.get(start..end.min(start + 3))?;
        millis = frac.parse::<i64>().ok()? * 10_i64.pow(3 - frac.len() as u32);
        i = end;
    }

    // Zone. `Z` and `+00:00` mean the same thing; a real offset comes off to
    // get UTC. Anything else is not a timestamp this understands.
    let offset_min = match b.get(i) {
        None | Some(&b'Z') | Some(&b'z') => 0,
        Some(&sign @ (b'+' | b'-')) => {
            let oh = num(i + 1, i + 3)?;
            let om =
                if b.get(i + 3) == Some(&b':') { num(i + 4, i + 6)? } else { num(i + 3, i + 5)? };
            let mag = oh * 60 + om;
            if sign == b'+' {
                mag
            } else {
                -mag
            }
        }
        _ => return None,
    };

    let days = days_from_civil(y, m, d);
    let secs = days * 86_400 + hh * 3_600 + mm * 60 + ss - offset_min * 60;
    u64::try_from(secs * 1000 + millis).ok()
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testenv::EnvGuard;

    fn def(name: &str, command: &str) -> AgentDef {
        AgentDef {
            name: name.into(),
            command: command.into(),
            args: Vec::new(),
            resume_args: Vec::new(),
            env: HashMap::new(),
            waiting_pattern: None,
            busy_pattern: None,
        }
    }

    #[test]
    fn civil_dates_match_known_epochs() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        // A leap day, which the mp/doy shift is the whole reason for.
        assert_eq!(days_from_civil(2024, 2, 29), 19782);
    }

    #[test]
    fn rfc3339_parses_to_millis() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(parse_rfc3339_ms("2026-08-11T20:16:00.016Z"), Some(1786479360016));
        // Without the fractional part, which some writers omit.
        assert_eq!(parse_rfc3339_ms("2026-08-11T20:16:00Z"), Some(1786479360000));
        assert_eq!(parse_rfc3339_ms("not a date"), None);
    }

    #[test]
    fn rfc3339_reads_the_shape_claude_writes_a_reset_in() {
        // Microseconds and an explicit zero offset, as `resets_at` arrives.
        assert_eq!(
            parse_rfc3339_ms("2026-08-16T19:59:59.901236+00:00"),
            Some(1786910399901),
            "six fractional digits truncate to millis rather than overflowing them"
        );
        // A fraction shorter than three digits scales rather than being read raw.
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00.9Z"), Some(900));
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00.05Z"), Some(50));
        // A real offset comes off; the same instant two ways must agree.
        assert_eq!(
            parse_rfc3339_ms("2026-08-11T22:16:00+02:00"),
            parse_rfc3339_ms("2026-08-11T20:16:00Z")
        );
        assert_eq!(
            parse_rfc3339_ms("2026-08-11T15:16:00-05:00"),
            parse_rfc3339_ms("2026-08-11T20:16:00Z")
        );
        // Offsets without the colon are legal too.
        assert_eq!(
            parse_rfc3339_ms("2026-08-11T22:16:00+0200"),
            parse_rfc3339_ms("2026-08-11T20:16:00Z")
        );
        assert_eq!(parse_rfc3339_ms("2026-08-11T20:16:00.Z"), None, "a point with no digits");
        assert_eq!(parse_rfc3339_ms("2026-08-11T20:16:00 UTC"), None, "an unparsed zone is not 0");
    }

    /// A `~/.claude.json` carrying the shape the real one does.
    fn claude_json(dir: &Path, fetched_ms: u64, limits: &str) -> PathBuf {
        let path = dir.join(".claude.json");
        std::fs::write(
            &path,
            format!(
                r#"{{"oauthAccount":{{"emailAddress":"you@example.com"}},
                    "cachedUsageUtilization":{{"fetchedAtMs":{fetched_ms},
                      "utilization":{{"limits":[{limits}]}}}}}}"#
            ),
        )
        .unwrap();
        path
    }

    #[test]
    fn published_limits_become_windows_with_a_ceiling_and_a_reset() {
        let dir = tempdir();
        let now = parse_rfc3339_ms("2026-08-12T10:00:00Z").unwrap();
        // Fetched a minute ago: fresh enough that every window is still about
        // now, which is what this test is holding still while it checks shape.
        let fetched = now - 60_000;
        let path = claude_json(
            &dir,
            fetched,
            r#"{"kind":"session","group":"session","percent":42,"severity":"normal",
                "resets_at":"2026-08-12T14:30:00.000000+00:00","scope":null,"is_active":true},
               {"kind":"weekly_all","group":"weekly","percent":56,"severity":"normal",
                "resets_at":"2026-08-16T19:59:59.901236+00:00","scope":null,"is_active":true},
               {"kind":"weekly_scoped","group":"weekly","percent":13,"severity":"normal",
                "resets_at":null,"scope":{"model":{"id":null,"display_name":"Opus"}},
                "is_active":true}"#,
        );
        let p = published(&path, now).expect("the key parses");
        assert_eq!(p.fetched_ms, fetched);
        assert!(!p.rolled_over, "a minute-old snapshot has outlived nothing");

        let labels: Vec<&str> = p.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["session", "week · all models", "week · opus"]);
        assert!(
            p.windows.iter().all(|w| w.of == Some(100) && w.unit == UsageUnit::Percent),
            "a published limit is a proportion, so it has a denominator"
        );
        assert_eq!(p.windows[0].used, 42);
        assert_eq!(p.windows[0].resets_ms, parse_rfc3339_ms("2026-08-12T14:30:00Z"));
        assert_eq!(p.windows[2].resets_ms, None, "a scoped window can lack a boundary");
    }

    #[test]
    fn a_window_whose_reset_has_passed_is_dropped_not_zeroed() {
        let dir = tempdir();
        let now = parse_rfc3339_ms("2026-08-12T10:00:00Z").unwrap();
        let path = claude_json(
            &dir,
            now - 60_000,
            r#"{"kind":"session","group":"session","percent":87,"severity":"warning",
                "resets_at":"2026-08-12T09:00:00.000000+00:00","scope":null,"is_active":true}"#,
        );
        // An hour after the window emptied, with nothing having refreshed the file.
        let p = published(&path, now).expect("the key parses");
        assert!(
            p.windows.is_empty(),
            "the 87% described a window that no longer exists, and 0% would be a guess \
             wearing the provider's authority"
        );
        assert!(p.rolled_over, "so the caller has to count the transcripts instead");
    }

    /// The defect this rule exists for: `resets_at` is `null` on an idle session
    /// window, so the boundary check cannot fire, and an arbitrarily old `0`
    /// used to sail through as a confident `session 0%`.
    #[test]
    fn a_snapshot_older_than_a_window_stops_speaking_for_it() {
        let dir = tempdir();
        let now = parse_rfc3339_ms("2026-08-12T10:00:00Z").unwrap();
        let limits = r#"{"kind":"session","group":"session","percent":0,"severity":"normal",
                "resets_at":null,"scope":null,"is_active":false},
               {"kind":"weekly_all","group":"weekly","percent":56,"severity":"normal",
                "resets_at":"2026-08-16T19:59:59.901236+00:00","scope":null,"is_active":true}"#;

        // Six hours old: past the five-hour window entirely, nowhere near the week.
        let stale = claude_json(&dir, now - 6 * 60 * 60 * 1000, limits);
        let p = published(&stale, now).expect("the key parses");
        let labels: Vec<&str> = p.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(
            labels,
            ["week · all models"],
            "every second of the five-hour window happened after this reading"
        );
        assert_eq!(p.windows[0].used, 56, "the week is barely dented by six hours");
        assert!(p.rolled_over);

        // The same file four hours old keeps both: the rule is the window's own
        // span, not a blanket freshness cutoff.
        let fresh = claude_json(&dir, now - 4 * 60 * 60 * 1000, limits);
        let p = published(&fresh, now).expect("the key parses");
        assert_eq!(p.windows.len(), 2, "four hours has not outrun a five-hour window");
        assert!(!p.rolled_over);

        // Eight days old and even the week is gone.
        let ancient = claude_json(&dir, now - 8 * 24 * 60 * 60 * 1000, limits);
        let p = published(&ancient, now).expect("the key parses");
        assert!(p.windows.is_empty());
        assert!(p.rolled_over);
    }

    #[test]
    fn a_window_span_is_known_only_for_the_kinds_that_have_one() {
        assert_eq!(window_span("session"), Some(FIVE_HOURS_MS));
        assert_eq!(window_span("weekly_all"), Some(SEVEN_DAYS_MS));
        assert_eq!(window_span("weekly_scoped"), Some(SEVEN_DAYS_MS));
        assert_eq!(
            window_span("something_new"),
            None,
            "a kind with no known span is trusted rather than thrown away on a guess"
        );
    }

    #[test]
    fn the_universal_windows_are_kept_at_zero_and_scoped_ones_are_not() {
        let dir = tempdir();
        let path = claude_json(
            &dir,
            1_000,
            r#"{"kind":"session","group":"session","percent":0,"severity":"normal",
                "resets_at":null,"scope":null,"is_active":false},
               {"kind":"weekly_all","group":"weekly","percent":0,"severity":"normal",
                "resets_at":null,"scope":null,"is_active":false},
               {"kind":"weekly_scoped","group":"weekly","percent":0,"severity":"normal",
                "resets_at":null,"scope":{"model":{"id":null,"display_name":"Fable"}},
                "is_active":false}"#,
        );
        let p = published(&path, 10_000).expect("the key parses");
        let labels: Vec<&str> = p.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(
            labels,
            ["session", "week · all models"],
            "an idle scoped window is a row that says nothing"
        );
    }

    #[test]
    fn a_claude_that_never_cached_its_limits_yields_nothing_to_publish() {
        let dir = tempdir();
        let path = dir.join(".claude.json");
        std::fs::write(&path, r#"{"oauthAccount":{"emailAddress":"you@example.com"}}"#).unwrap();
        assert!(published(&path, 10_000).is_none(), "no key, so the caller falls back to counting");
        assert!(published(&dir.join("nope.json"), 10_000).is_none(), "and no file at all");

        // A shape this does not recognise is the same answer, not a panic.
        std::fs::write(&path, r#"{"cachedUsageUtilization":{"utilization":{}}}"#).unwrap();
        assert!(published(&path, 10_000).is_none());
    }

    /// Both halves of a fake `HOME`: what claude cached, and what it recorded
    /// spending. [`claude_windows`] reads exactly these two files.
    fn claude_home(dir: &Path, cached: Option<(u64, &str)>, turns: &[(&str, u64)]) {
        match cached {
            Some((fetched, limits)) => {
                claude_json(dir, fetched, limits);
            }
            None => std::fs::write(
                dir.join(".claude.json"),
                r#"{"oauthAccount":{"emailAddress":"you@example.com"}}"#,
            )
            .unwrap(),
        }
        let project = dir.join(".claude/projects/proj");
        std::fs::create_dir_all(&project).unwrap();
        let lines: String = turns
            .iter()
            .enumerate()
            .map(|(i, (ts, tokens))| {
                format!(
                    r#"{{"timestamp":"{ts}","requestId":"r{i}","message":{{"model":"claude-sonnet-5","usage":{{"output_tokens":{tokens}}}}}}}
"#
                )
            })
            .collect();
        std::fs::write(project.join("a.jsonl"), lines).unwrap();
    }

    const LIMITS: &str = r#"{"kind":"session","group":"session","percent":0,"severity":"normal",
            "resets_at":null,"scope":null,"is_active":false},
           {"kind":"weekly_all","group":"weekly","percent":56,"severity":"normal",
            "resets_at":"2026-08-16T19:59:59.901236+00:00","scope":null,"is_active":true}"#;

    /// The bug, end to end: a six-hour-old cache saying `session 0%` on a
    /// machine that spent two million tokens in that very window.
    #[test]
    fn a_stale_snapshot_keeps_the_week_and_counts_the_hours() {
        let dir = tempdir();
        let now = parse_rfc3339_ms("2026-08-12T10:00:00Z").unwrap();
        claude_home(
            &dir,
            Some((now - 6 * 60 * 60 * 1000, LIMITS)),
            &[("2026-08-12T09:30:00.000Z", 2_000_000)],
        );
        let _g = EnvGuard::set(&[("HOME", dir.to_str().unwrap())]);

        let mut c = Counter::default();
        let (windows, source, state, note) = claude_windows(&mut c, now);
        let labels: Vec<&str> = windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(
            labels,
            ["week · all models", "last 5h", "last 7d"],
            "the week survives, and the hours the snapshot cannot speak for are counted"
        );
        assert_eq!(windows[0].used, 56, "still the provider's number");
        assert_eq!(
            windows[1].used, 2_000_000,
            "and the five hours read as what was actually spent, not as 0%"
        );
        assert!(!windows.iter().any(|w| w.label == "session"), "no stale zero survives");
        assert_eq!(source, UsageSource::Published);
        assert_eq!(state, CliState::Metered);
        assert!(note.contains("outlived"), "the note owns up to the mix: {note:?}");
    }

    #[test]
    fn a_cache_that_has_outrun_every_window_reads_as_counted() {
        let dir = tempdir();
        let now = parse_rfc3339_ms("2026-08-12T10:00:00Z").unwrap();
        claude_home(
            &dir,
            Some((now - 8 * 24 * 60 * 60 * 1000, LIMITS)),
            &[("2026-08-12T09:30:00.000Z", 7)],
        );
        let _g = EnvGuard::set(&[("HOME", dir.to_str().unwrap())]);

        let mut c = Counter::default();
        let (windows, source, state, note) = claude_windows(&mut c, now);
        let labels: Vec<&str> = windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["last 5h", "last 7d"], "nothing published is left to draw");
        assert_eq!(source, UsageSource::Transcripts);
        assert_eq!(state, CliState::Counted);
        assert!(
            note.contains("rolled over"),
            "and it is a different sentence from never having cached: {note:?}"
        );
    }

    #[test]
    fn a_current_snapshot_answers_alone() {
        let dir = tempdir();
        let now = parse_rfc3339_ms("2026-08-12T10:00:00Z").unwrap();
        claude_home(&dir, Some((now - 60_000, LIMITS)), &[("2026-08-12T09:30:00.000Z", 2_000_000)]);
        let _g = EnvGuard::set(&[("HOME", dir.to_str().unwrap())]);

        let mut c = Counter::default();
        let (windows, source, state, note) = claude_windows(&mut c, now);
        let labels: Vec<&str> = windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(
            labels,
            ["session", "week · all models"],
            "a fresh snapshot needs no help, and counted rows beside it would be noise"
        );
        assert_eq!(source, UsageSource::Published);
        assert_eq!(state, CliState::Metered);
        assert!(note.contains("read 1m ago"), "{note:?}");
    }

    #[test]
    fn a_claude_that_cached_nothing_is_a_different_sentence_from_one_that_went_stale() {
        let dir = tempdir();
        let now = parse_rfc3339_ms("2026-08-12T10:00:00Z").unwrap();
        claude_home(&dir, None, &[("2026-08-12T09:30:00.000Z", 42)]);
        let _g = EnvGuard::set(&[("HOME", dir.to_str().unwrap())]);

        let mut c = Counter::default();
        let (windows, source, state, note) = claude_windows(&mut c, now);
        assert_eq!(windows.len(), 2, "the rolling pair, counted");
        assert_eq!(source, UsageSource::Transcripts);
        assert_eq!(state, CliState::Counted);
        assert!(note.contains("has not cached its limits here yet"), "{note:?}");
    }

    /// A gemini session file: whole JSON, rewritten as the session grows.
    fn gemini_session(dir: &Path, project: &str, name: &str, messages: &str) -> PathBuf {
        let chats = dir.join("tmp").join(project).join("chats");
        std::fs::create_dir_all(&chats).unwrap();
        let path = chats.join(name);
        std::fs::write(&path, format!(r#"{{"sessionId":"s","messages":[{messages}]}}"#)).unwrap();
        path
    }

    /// One assistant turn in gemini's shape.
    fn gem_msg(id: &str, ts: &str, input: u64, output: u64, cached: u64) -> String {
        let total = input + output;
        format!(
            r#"{{"id":"{id}","timestamp":"{ts}","type":"gemini","model":"gemini-3-flash-preview",
                "tokens":{{"input":{input},"output":{output},"cached":{cached},
                "thoughts":0,"tool":0,"total":{total}}}}}"#
        )
    }

    #[test]
    fn a_gemini_turn_is_counted_without_its_replayed_context() {
        let entries = parse_gemini_session(&format!(
            r#"{{"messages":[{}]}}"#,
            gem_msg("m1", "2026-08-11T20:16:00.000Z", 7581, 60, 3008)
        ));
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].tokens,
            7641 - 3008,
            "the cached slice of the input is replayed context, not work done"
        );
        assert!(!entries[0].opus, "gemini has no model family metered on its own");
        assert_eq!(entries[0].request, "m1");
    }

    #[test]
    fn a_gemini_message_with_no_tokens_is_skipped_not_counted_as_zero() {
        // A user turn, an error turn, and a turn with a malformed stamp: none
        // of them is an assistant turn with a cost.
        let entries = parse_gemini_session(
            r#"{"messages":[
                {"id":"u1","timestamp":"2026-08-11T20:16:00.000Z","type":"user",
                 "content":[{"text":"hi"}]},
                {"id":"e1","timestamp":"nonsense","type":"gemini",
                 "tokens":{"input":1,"output":1,"cached":0,"thoughts":0,"tool":0,"total":2}}
            ]}"#,
        );
        assert!(entries.is_empty());
        assert!(parse_gemini_session("not json").is_empty(), "a corrupt file is not a panic");
    }

    #[test]
    fn a_rewritten_session_counts_each_turn_once_however_often_it_is_reread() {
        let dir = tempdir();
        let path = gemini_session(
            &dir,
            "proj",
            "session-a.json",
            &gem_msg("m1", "2026-08-11T20:16:00.000Z", 100, 10, 0),
        );
        let now = parse_rfc3339_ms("2026-08-11T21:00:00.000Z").unwrap();
        let mut c = Counter::default();
        c.refresh_rewritten(&dir, now, gemini_sessions, parse_gemini_session);
        assert_eq!(c.windows(now)[0].used, 110);

        // The session grows: the file is rewritten whole, so the first turn is
        // presented again alongside the new one.
        std::fs::write(
            &path,
            format!(
                r#"{{"messages":[{},{}]}}"#,
                gem_msg("m1", "2026-08-11T20:16:00.000Z", 100, 10, 0),
                gem_msg("m2", "2026-08-11T20:30:00.000Z", 50, 5, 0)
            ),
        )
        .unwrap();
        // mtime resolution is coarse enough that a same-millisecond rewrite
        // could be skipped; move it forward explicitly so the read is forced.
        let later = std::time::SystemTime::now() + Duration::from_secs(2);
        filetime_set(&path, later);
        c.refresh_rewritten(&dir, now, gemini_sessions, parse_gemini_session);
        assert_eq!(c.windows(now)[0].used, 165, "m1 is counted once, not twice");
        assert_eq!(c.entries.len(), 2);
    }

    #[test]
    fn an_unchanged_session_is_not_reread() {
        let dir = tempdir();
        let path = gemini_session(
            &dir,
            "proj",
            "session-a.json",
            &gem_msg("m1", "2026-08-11T20:16:00.000Z", 100, 10, 0),
        );
        let now = parse_rfc3339_ms("2026-08-11T21:00:00.000Z").unwrap();
        // Pinned to a whole second before the first read: `touch` cannot restore
        // sub-second precision, so a stamp taken from the filesystem could not
        // be put back exactly and the file would look changed for the wrong
        // reason.
        let stamp = SystemTime::now() - Duration::from_secs(1);
        filetime_set(&path, stamp);
        let mut c = Counter::default();
        c.refresh_rewritten(&dir, now, gemini_sessions, parse_gemini_session);

        // Swap in content the counter has never seen, then put the mtime back
        // where it was. Only the skip can keep this turn out of the total —
        // dedup cannot, because `m2` is new.
        std::fs::write(
            &path,
            format!(
                r#"{{"messages":[{}]}}"#,
                gem_msg("m2", "2026-08-11T20:30:00.000Z", 900, 90, 0)
            ),
        )
        .unwrap();
        filetime_set(&path, stamp);
        c.refresh_rewritten(&dir, now, gemini_sessions, parse_gemini_session);
        assert_eq!(c.windows(now)[0].used, 110, "a file whose mtime did not move is not reopened");
    }

    /// Push a file's mtime forward. `std::fs` cannot set one, and the crate
    /// does not depend on `filetime` — touching through the shell is enough for
    /// a test that only needs the stamp to differ.
    fn filetime_set(path: &Path, when: SystemTime) {
        let secs = when.duration_since(UNIX_EPOCH).unwrap().as_secs();
        let _ = std::process::Command::new("touch")
            .arg("-d")
            .arg(format!("@{secs}"))
            .arg(path)
            .status();
    }

    #[test]
    fn ages_read_as_a_glance() {
        assert_eq!(ago(30_000), "30s");
        assert_eq!(ago(240_000), "4m");
        assert_eq!(ago(7_200_000), "2h");
        assert_eq!(ago(3 * 86_400_000), "3d");
    }

    #[test]
    fn entry_sums_input_and_output_but_not_cache_reads() {
        let line = r#"{"timestamp":"2026-08-11T20:16:00.016Z","requestId":"req_1","message":
            {"model":"claude-opus-5","usage":{"input_tokens":2,"cache_creation_input_tokens":100,
            "cache_read_input_tokens":90000,"output_tokens":50}}}"#;
        let e = parse_entry(line).expect("parses");
        assert_eq!(e.tokens, 152, "cache reads must not be in the total");
        assert!(e.opus);
        assert_eq!(e.request, "req_1");
    }

    #[test]
    fn a_repeated_request_is_counted_once() {
        let dir = tempdir();
        let project = dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let line = |req: &str, out: u64| {
            format!(
                r#"{{"timestamp":"2026-08-11T20:16:00.000Z","requestId":"{req}","message":{{"model":"claude-opus-5","usage":{{"output_tokens":{out}}}}}}}"#
            )
        };
        std::fs::write(
            project.join("a.jsonl"),
            format!("{}\n{}\n{}\n", line("r1", 10), line("r1", 10), line("r2", 5)),
        )
        .unwrap();

        let now = parse_rfc3339_ms("2026-08-11T21:00:00.000Z").unwrap();
        let mut c = Counter::default();
        c.refresh(&dir, now);
        assert_eq!(c.entries.len(), 2, "the duplicate request id is dropped");
        assert_eq!(c.windows(now)[0].used, 15);
    }

    #[test]
    fn a_second_pass_reads_only_what_was_appended() {
        let dir = tempdir();
        let project = dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join("a.jsonl");
        let line = |req: &str| {
            format!(
                r#"{{"timestamp":"2026-08-11T20:16:00.000Z","requestId":"{req}","message":{{"model":"claude-sonnet-5","usage":{{"output_tokens":7}}}}}}"#
            )
        };
        std::fs::write(&path, format!("{}\n", line("r1"))).unwrap();
        let now = parse_rfc3339_ms("2026-08-11T21:00:00.000Z").unwrap();
        let mut c = Counter::default();
        c.refresh(&dir, now);
        let after_first = c.offsets[&path];

        std::fs::write(&path, format!("{}\n{}\n", line("r1"), line("r2"))).unwrap();
        c.refresh(&dir, now);
        assert!(c.offsets[&path] > after_first, "the offset advanced past the new line");
        assert_eq!(c.entries.len(), 2);
        assert_eq!(c.windows(now)[0].used, 14);
        // No opus turn, so the page gets no opus row to look at.
        assert!(c.windows(now).iter().all(|w| w.label != "last 7d · opus"));
    }

    #[test]
    fn a_half_written_line_is_read_whole_on_the_next_pass() {
        let dir = tempdir();
        let project = dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join("a.jsonl");
        let whole = r#"{"timestamp":"2026-08-11T20:16:00.000Z","requestId":"r1","message":{"model":"m","usage":{"output_tokens":9}}}"#;
        let (head, tail) = whole.split_at(40);
        std::fs::write(&path, head).unwrap();
        let now = parse_rfc3339_ms("2026-08-11T21:00:00.000Z").unwrap();
        let mut c = Counter::default();
        c.refresh(&dir, now);
        assert_eq!(c.entries.len(), 0, "a partial line is not parsed");

        std::fs::write(&path, format!("{head}{tail}\n")).unwrap();
        c.refresh(&dir, now);
        assert_eq!(c.entries.len(), 1, "and is picked up once it is complete");
        assert_eq!(c.windows(now)[0].used, 9);
    }

    #[test]
    fn windows_drop_what_falls_out_of_them() {
        let dir = tempdir();
        let project = dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let at = |ts: &str, req: &str| {
            format!(
                r#"{{"timestamp":"{ts}","requestId":"{req}","message":{{"model":"claude-opus-5","usage":{{"output_tokens":100}}}}}}"#
            )
        };
        std::fs::write(
            project.join("a.jsonl"),
            format!(
                "{}\n{}\n",
                at("2026-08-11T20:00:00.000Z", "recent"),
                at("2026-08-11T10:00:00.000Z", "older")
            ),
        )
        .unwrap();
        let now = parse_rfc3339_ms("2026-08-11T21:00:00.000Z").unwrap();
        let mut c = Counter::default();
        c.refresh(&dir, now);
        let w = c.windows(now);
        assert_eq!(w[0].used, 100, "only the turn inside the 5h window");
        assert_eq!(w[1].used, 200, "both are inside the 7d window");
        assert_eq!(w[2].label, "last 7d · opus");
        assert_eq!(w[2].used, 200);
        assert!(w.iter().all(|x| x.of.is_none()), "nothing published a ceiling");
    }

    #[tokio::test]
    async fn an_uninstalled_cli_is_listed_as_absent() {
        let mut c = Counter::default();
        let dto = one(&def("nope", "butai-no-such-binary"), &[], &mut c, now_ms()).await;
        assert_eq!(dto.state, CliState::Absent);
        assert!(dto.version.is_none());
        assert!(dto.note.unwrap().contains("not installed"));
    }

    /// An executable script in a fake home's `bin` directory.
    fn fake_bin(home: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = home.join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let path = bin.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// The bug this page shipped with: the daemon's inherited `PATH` is not a
    /// login shell's, so an agent installed under nvm or in `~/.local/bin` was
    /// reported `absent` — while the AGENTS rail launched it perfectly well,
    /// because the pane spawner has always looked in those directories.
    #[tokio::test]
    async fn an_agent_a_pane_could_launch_is_never_absent() {
        let tmp = tempdir();
        fake_bin(&tmp, "butai-fake-agent", "#!/bin/sh\necho '9.9.9 (Fake)'\n");
        let _g = EnvGuard::set(&[
            ("HOME", tmp.to_str().unwrap()),
            // Deliberately without the directory the agent is in: this is the
            // short `PATH` a daemon started outside a login shell inherits.
            ("PATH", "/usr/bin:/bin"),
        ]);

        let mut c = Counter::default();
        let dto = one(&def("fake", "butai-fake-agent"), &[], &mut c, now_ms()).await;
        assert_ne!(dto.state, CliState::Absent, "a binary a pane would find is installed");
        assert_eq!(
            dto.version.as_deref(),
            Some("9.9.9 (Fake)"),
            "and the probe has to run the binary that was found, not the bare name"
        );
    }

    /// The probe is a pane's child in miniature and needs a pane's `PATH`: an
    /// npm-installed CLI is a `#!/usr/bin/env node` launcher, and the node the
    /// daemon inherited is routinely too old to parse it. `butai-fake-node`
    /// stands in for the interpreter — reachable only if the `PATH` handed to
    /// the probe was repaired.
    #[tokio::test]
    async fn a_version_probe_runs_with_the_path_a_pane_would_have_given() {
        let tmp = tempdir();
        fake_bin(&tmp, "butai-fake-node", "#!/bin/sh\necho '1.2.3 (via interpreter)'\n");
        fake_bin(&tmp, "butai-fake-agent2", "#!/bin/sh\nexec butai-fake-node\n");
        let _g = EnvGuard::set(&[("HOME", tmp.to_str().unwrap()), ("PATH", "/usr/bin:/bin")]);

        let mut c = Counter::default();
        let dto = one(&def("fake", "butai-fake-agent2"), &[], &mut c, now_ms()).await;
        assert_eq!(
            dto.version.as_deref(),
            Some("1.2.3 (via interpreter)"),
            "the probe's child could not find its interpreter on the daemon's own PATH"
        );
    }

    #[tokio::test]
    async fn an_installed_cli_butai_cannot_read_is_unknown_not_no_account() {
        // `sh` stands in for any CLI that is present and unparsed: the state has
        // to say "we cannot see it", never "it has no limits".
        let mut c = Counter::default();
        let dto = one(&def("shell", "sh"), &[], &mut c, now_ms()).await;
        assert_eq!(dto.state, CliState::Unknown);
        assert_eq!(dto.source, UsageSource::None);
    }

    #[test]
    fn a_declared_budget_turns_a_count_into_a_proportion() {
        let budgets =
            [BudgetDef { agent: "claude".into(), window: "last 5h".into(), tokens: 20_000_000 }];
        let mut dto = CliUsageDto {
            name: "claude".into(),
            command: "claude".into(),
            state: CliState::Counted,
            version: None,
            account: None,
            plan: None,
            windows: vec![window("last 5h", 5_000_000), window("last 7d", 9)],
            panes: Vec::new(),
            source: UsageSource::Transcripts,
            note: None,
        };
        apply_budgets(&mut dto, &budgets);
        assert_eq!(dto.state, CliState::Metered);
        assert_eq!(dto.windows[0].of, Some(20_000_000));
        assert_eq!(dto.windows[1].of, None, "an undeclared window keeps no ceiling");
    }

    #[test]
    fn an_unmatched_budget_label_changes_nothing() {
        let budgets = [BudgetDef { agent: "claude".into(), window: "last 6h".into(), tokens: 1 }];
        let mut dto = CliUsageDto {
            name: "claude".into(),
            command: "claude".into(),
            state: CliState::Counted,
            version: None,
            account: None,
            plan: None,
            windows: vec![window("last 5h", 5)],
            panes: Vec::new(),
            source: UsageSource::Transcripts,
            note: None,
        };
        apply_budgets(&mut dto, &budgets);
        assert_eq!(dto.state, CliState::Counted, "a typo must not claim to be metered");
        assert_eq!(dto.windows[0].of, None);
    }

    #[test]
    fn plan_tiers_are_tidied_for_reading() {
        assert_eq!(plan_label("default_claude_max_5x"), "max 5x");
        assert_eq!(plan_label("claude_pro"), "pro");
        assert_eq!(plan_label("something_new"), "something new");
    }

    #[test]
    fn which_finds_a_real_binary_and_misses_a_fake_one() {
        assert!(which("sh").is_some());
        assert!(which("butai-no-such-binary").is_none());
    }

    /// A scratch directory of this test's own. Keeps the transcript tests off
    /// the real `~/.claude`.
    ///
    /// Counter, not a timestamp: the tests run in parallel threads of one
    /// process, so a pid-plus-millis name collides whenever two of them start
    /// in the same millisecond — and then they write each other's `a.jsonl` and
    /// fail in ways that look like counter bugs.
    fn tempdir() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base =
            std::env::temp_dir().join(format!("butai-usage-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }
}
