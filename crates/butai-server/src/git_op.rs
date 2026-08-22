//! Running `git` on the user's behalf, without ever hanging the daemon.
//!
//! Everything that writes the repository beyond the index goes through here.
//! The reason is the same one [`git_push`](super) had before it: the user's
//! remotes, `push.default`, credential helpers, ssh-agent, hooks, signing
//! config and sequencer state all belong to the real `git` binary, and libgit2
//! is built in this workspace with `default-features = false` — no SSH or HTTPS
//! transport is compiled in at all, so it *cannot* reach a network.
//!
//! Two things make this different from shelling out naively:
//!
//! **It is `tokio::process`, not `spawn_blocking` + `Command::output()`.** A
//! blocking-pool thread parked in `wait()` cannot be cancelled or timed out, so
//! a single credential prompt used to park one forever with no way back. An
//! async child can be raced against a timer and killed.
//!
//! **Argument handling is a pure function.** [`argv`] never spawns anything, so
//! the whole injection surface is unit-testable. `Command` does not go through a
//! shell, so quoting, `;` and `$()` are not the threat; the threats are a value
//! git parses as an *option* and a value git parses as a *different kind of
//! argument*, and both are handled by validating before argv is built.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use butai_protocol::api::{GitOp, RepoState, ResetMode, SequenceAction};
use butai_protocol::SessionId;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::core::Event;

/// How long a caller waits for the operation before it is answered
/// "accepted, poll me". Long enough that the millisecond-scale operations
/// (taking a side of a conflict, dropping a stash) answer synchronously with
/// their real result; short enough that no HTTP request is ever held open by a
/// network round trip.
pub const GRACE: Duration = Duration::from_millis(300);

/// Kill an operation that has produced no output at all for this long. This is
/// the real backstop against a hang: whatever git or ssh decided to wait on, it
/// stopped telling us about it.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Kill an operation that has run this long even if it is still chattering. A
/// clone of a very large repository over a slow link is the case this has to
/// tolerate, so it is generous.
pub const HARD_TIMEOUT: Duration = Duration::from_secs(600);

/// Emit at most one progress event per interval. `git push` prints a line per
/// percent; relayed unthrottled that is tens of events a second to every SSE
/// subscriber and a repaint of every attached TUI, for text nobody can read at
/// that rate.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// Keep this much of git's output for the failure message. Enough for a real
/// error (a rejected push explains itself in a dozen lines), bounded so a
/// pathological command cannot grow the actor's memory.
const TAIL_LINES: usize = 64;

// -- validation -------------------------------------------------------------

/// Reject a value that git would read as an option rather than data.
///
/// The leading-`-` check is the whole point: `--upload-pack=<cmd>` and
/// `--exec=<cmd>` are argument-position options that run a command, so a branch
/// name that starts with `-` is remote code execution. `--` separators handle
/// pathspecs; nothing else in this module ever puts user text in option
/// position.
fn not_an_option(what: &str, s: &str) -> Result<(), String> {
    if s.starts_with('-') {
        return Err(format!("{what} may not start with '-': {s:?}"));
    }
    if s.is_empty() {
        return Err(format!("empty {what}"));
    }
    if s.len() > 255 {
        return Err(format!("{what} is too long"));
    }
    if s.chars().any(|c| c.is_ascii_control()) {
        return Err(format!("{what} contains a control character"));
    }
    Ok(())
}

/// Validate a ref name against git's own rules (`git check-ref-format`).
///
/// Deliberately not a permissive character allowlist: real branch names contain
/// `/` and `.` (`feature/thing`, `release-1.2`), so an allowlist either rejects
/// ordinary work or is so wide it stops meaning anything. These are the rules
/// git itself enforces.
pub fn valid_ref_name(s: &str) -> Result<&str, String> {
    not_an_option("branch name", s)?;
    let bad = |why: &str| Err(format!("invalid branch name {s:?}: {why}"));
    if s.contains("..") {
        return bad("contains '..'");
    }
    if s.contains("@{") {
        return bad("contains '@{'");
    }
    if s == "@" {
        return bad("is '@'");
    }
    if s.contains("//") {
        return bad("contains '//'");
    }
    if s.starts_with('/') || s.ends_with('/') {
        return bad("starts or ends with '/'");
    }
    if s.ends_with('.') {
        return bad("ends with '.'");
    }
    if s.ends_with(".lock") {
        return bad("ends with '.lock'");
    }
    if s.split('/').any(|part| part.is_empty() || part.starts_with('.')) {
        return bad("has an empty or dot-leading component");
    }
    if s.chars().any(|c| matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\')) {
        return bad("contains one of ' ~^:?*[\\'");
    }
    Ok(s)
}

/// Validate a remote *name*.
///
/// Colons are refused outright, which is what keeps this from being a remote
/// code execution hole: `git fetch 'ext::sh -c whoami'` runs a shell, and
/// `ssh://` and `user@host:path` are equally not names.
///
/// Every route in this daemon takes a configured remote's *name*. Exactly one
/// takes a URL — `GitOp::RemoteAdd` — and it goes through
/// [`valid_remote_url`], which is a transport allowlist rather than this
/// rule. Nothing else should ever accept one.
pub fn valid_remote(s: &str) -> Result<&str, String> {
    not_an_option("remote name", s)?;
    if s.contains(':') {
        return Err(format!("remote must be a configured name, not a URL: {s:?}"));
    }
    valid_ref_name(s).map_err(|_| format!("invalid remote name {s:?}"))
}

/// Validate a remote *URL*.
///
/// **This reopens a door [`valid_remote`] deliberately closed**, and the
/// allowlist below is the only thing holding it. `git remote add` takes a URL,
/// and a URL is an arbitrary-code-execution vector: `ext::sh -c whoami` makes
/// git run a shell, and any `<helper>::<rest>` form dispatches to
/// `git-remote-<helper>` on `$PATH`. That is why adding a remote was not
/// exposed at all until worktrees and remote management were asked for.
///
/// So this is an allowlist of transports, not a denylist of bad strings:
/// anything that is not one of the shapes below is refused, and the caller also
/// passes `-c protocol.ext.allow=never` as a second line of defence. A
/// denylist here would be wrong — the set of helper names is whatever is
/// installed on the machine.
pub fn valid_remote_url(s: &str) -> Result<&str, String> {
    not_an_option("remote url", s)?;
    if s.len() > 2048 {
        return Err("remote url is too long".into());
    }
    if s.contains(char::is_whitespace) {
        return Err(format!("remote url contains whitespace: {s:?}"));
    }
    // The transport-helper form, and the whole reason this function exists.
    // Checked before the scheme allowlist because `ext::` would otherwise slip
    // through anything that only looks for `://`.
    if let Some(pos) = s.find("::") {
        return Err(format!("refusing a transport helper {:?} in {s:?}", &s[..pos]));
    }
    const SCHEMES: [&str; 6] = ["https://", "http://", "ssh://", "git://", "file://", "git+ssh://"];
    if SCHEMES.iter().any(|p| s.starts_with(p)) {
        let rest = s.split_once("://").map(|(_, r)| r).unwrap_or("");
        if rest.is_empty() {
            return Err(format!("remote url has no host: {s:?}"));
        }
        return Ok(s);
    }
    // An absolute local path is a legitimate remote, and it is what every test
    // fixture in this repository uses.
    if s.starts_with('/') {
        return Ok(s);
    }
    // scp-style `user@host:path` — no scheme, exactly one `:`, and a host
    // before it. This is the form that makes a plain denylist hopeless, since
    // it is a colon-bearing string that *is* legitimate.
    if let Some((host, path)) = s.split_once(':') {
        let name = host.rsplit('@').next().unwrap_or(host);
        let host_ok = !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
        if host_ok && !path.is_empty() && !path.contains(':') {
            return Ok(s);
        }
    }
    Err(format!("unsupported remote url {s:?}"))
}

/// Validate a revision — looser than a ref name, because `HEAD~2`, `abc123^`
/// and `origin/main` are all legitimate, but with the same option guard.
pub fn valid_rev(s: &str) -> Result<&str, String> {
    not_an_option("revision", s)?;
    if s.chars().any(|c| matches!(c, ' ' | ';' | '|' | '&' | '$' | '`' | '\n')) {
        return Err(format!("invalid revision {s:?}"));
    }
    Ok(s)
}

/// Validate the revision `GET .../show?id=` is about.
///
/// Stricter than [`valid_rev`] on purpose, and the difference is one character:
/// **`:` stays out**, because `git show <rev>:<path>` reads a *file* out of a
/// tree, and this endpoint's contract is a whole-commit diff. An allowlist
/// rather than a denylist since the answer is rendered back to the user.
///
/// `@`, `{` and `}` are in it. Without them the reflog forms — `stash@{0}`,
/// `main@{upstream}`, `HEAD@{2}` — were rejected, which is what made a Stashes
/// list impossible to show a diff for. They are inert here: nothing is passed
/// through a shell (`Command` takes argv directly), a leading `-` is refused
/// separately, and without `:` there is no path to traverse.
pub fn valid_show_rev(s: &str) -> Result<&str, String> {
    not_an_option("revision", s)?;
    let ok = !s.is_empty()
        && s.len() <= 100
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '-' | '.' | '/' | '~' | '^' | '@' | '{' | '}')
        });
    if ok {
        Ok(s)
    } else {
        Err(format!("bad revision {s:?}"))
    }
}

// -- argv -------------------------------------------------------------------

/// Turn an operation into the arguments following `git`.
///
/// Pure: it spawns nothing and touches no filesystem, so the validation above
/// is exercised by ordinary unit tests. The caller adds `-C <root>` and the
/// hardening flags; see [`spawn_op`].
pub fn argv(op: &GitOp) -> Result<Vec<String>, String> {
    let mut a: Vec<String> = Vec::new();
    match op {
        GitOp::Fetch { remote, all, prune } => {
            a.push("fetch".into());
            // `--progress` because the daemon is not a terminal, and without it
            // git prints nothing until it finishes — which would leave the idle
            // watchdog looking at silence during a perfectly healthy transfer.
            a.push("--progress".into());
            if *prune {
                a.push("--prune".into());
            }
            if *all {
                a.push("--all".into());
            } else if let Some(r) = remote {
                a.push(valid_remote(r)?.to_string());
            }
        }
        GitOp::Pull { remote, branch, rebase, ff_only } => {
            a.push("pull".into());
            a.push("--progress".into());
            if *rebase {
                a.push("--rebase".into());
            }
            if *ff_only {
                a.push("--ff-only".into());
            }
            if let Some(r) = remote {
                a.push(valid_remote(r)?.to_string());
                // A branch without a remote is meaningless to `git pull`, so it
                // is only accepted alongside one.
                if let Some(b) = branch {
                    a.push(valid_ref_name(b)?.to_string());
                }
            }
        }
        GitOp::Push { remote, branch, set_upstream, force_with_lease } => {
            a.push("push".into());
            a.push("--progress".into());
            if *force_with_lease {
                a.push("--force-with-lease".into());
            }
            if *set_upstream {
                a.push("--set-upstream".into());
            }
            if let Some(r) = remote {
                a.push(valid_remote(r)?.to_string());
                if let Some(b) = branch {
                    a.push(valid_ref_name(b)?.to_string());
                }
            } else if *set_upstream {
                // `--set-upstream` with no remote has nothing to record.
                return Err("--set-upstream needs a remote".into());
            }
        }
        GitOp::Stash { message, include_untracked } => {
            a.push("stash".into());
            a.push("push".into());
            if *include_untracked {
                a.push("--include-untracked".into());
            }
            if let Some(m) = message {
                // A message is the *value* of `-m`, so it can hold anything a
                // commit message can — but a leading `-` would still be read as
                // an option if the value were ever detached from its flag.
                not_an_option("stash message", m)?;
                a.push("-m".into());
                a.push(m.clone());
            }
        }
        GitOp::StashApply { index, pop } => {
            a.push("stash".into());
            a.push(if *pop { "pop".into() } else { "apply".into() });
            a.push(stash_ref(*index));
        }
        GitOp::StashDrop { index } => {
            a.push("stash".into());
            a.push("drop".into());
            a.push(stash_ref(*index));
        }
        GitOp::Amend { message } => {
            a.push("commit".into());
            a.push("--amend".into());
            match message {
                Some(m) => {
                    not_an_option("commit message", m)?;
                    a.push("-m".into());
                    a.push(m.clone());
                }
                // No message means keep the old one. `--no-edit` is essential:
                // without it git opens an editor the daemon cannot answer, and
                // the operation hangs until the idle watchdog kills it.
                None => a.push("--no-edit".into()),
            }
        }
        GitOp::Reset { rev, mode } => {
            a.push("reset".into());
            a.push(
                match mode {
                    ResetMode::Soft => "--soft",
                    ResetMode::Mixed => "--mixed",
                    ResetMode::Hard => "--hard",
                }
                .into(),
            );
            if let Some(r) = rev {
                a.push(valid_rev(r)?.to_string());
            }
        }
        GitOp::Revert { rev } => {
            a.push("revert".into());
            // The daemon has no editor to open, so the message is taken as-is.
            a.push("--no-edit".into());
            a.push(valid_rev(rev)?.to_string());
        }
        GitOp::CherryPick { rev } => {
            a.push("cherry-pick".into());
            a.push(valid_rev(rev)?.to_string());
        }
        GitOp::Merge { branch, no_ff } => {
            a.push("merge".into());
            a.push("--no-edit".into());
            if *no_ff {
                a.push("--no-ff".into());
            }
            a.push(valid_rev(branch)?.to_string());
        }
        GitOp::Rebase { onto } => {
            a.push("rebase".into());
            a.push(valid_rev(onto)?.to_string());
        }
        GitOp::Sequence { action } => {
            // Which subcommand depends on what is running; the caller resolves
            // that from `RepoState` and passes it in via `sequence_argv`.
            return Err(format!(
                "internal: a {action:?} needs the repository state to name its subcommand"
            ));
        }
        GitOp::Tag { name, rev, message } => {
            a.push("tag".into());
            if let Some(m) = message {
                not_an_option("tag message", m)?;
                a.push("-a".into());
                a.push("-m".into());
                a.push(m.clone());
            }
            a.push(valid_ref_name(name)?.to_string());
            if let Some(r) = rev {
                a.push(valid_rev(r)?.to_string());
            }
        }
        GitOp::TagDelete { name } => {
            a.push("tag".into());
            a.push("-d".into());
            a.push(valid_ref_name(name)?.to_string());
        }
        GitOp::WorktreeAdd { path, branch, new_branch } => {
            a.push("worktree".into());
            a.push("add".into());
            if *new_branch {
                let Some(b) = branch else {
                    return Err("a new worktree branch needs a name".into());
                };
                a.push("-b".into());
                a.push(valid_ref_name(b)?.to_string());
            }
            // `--` before the path: it is the one argument here that is neither
            // a flag nor a ref, and it comes from a text prompt.
            a.push("--".into());
            a.push(crate::git_worktree::valid_path(path)?.to_string());
            if !*new_branch {
                if let Some(b) = branch {
                    a.push(valid_rev(b)?.to_string());
                }
            }
        }
        GitOp::WorktreeRemove { path, force } => {
            a.push("worktree".into());
            a.push("remove".into());
            if *force {
                a.push("--force".into());
            }
            a.push("--".into());
            a.push(crate::git_worktree::valid_path(path)?.to_string());
        }
        GitOp::WorktreePrune => {
            a.push("worktree".into());
            a.push("prune".into());
        }
        GitOp::RemoteAdd { name, url } => {
            a.push("remote".into());
            a.push("add".into());
            a.push("--".into());
            a.push(valid_remote(name)?.to_string());
            a.push(valid_remote_url(url)?.to_string());
        }
        GitOp::RemoteRemove { name } => {
            a.push("remote".into());
            a.push("remove".into());
            // **No `--` here, and the asymmetry with `add` above is git's, not
            // ours.** `git remote add` takes the separator; `git remote remove`
            // rejects it outright — `usage: git remote remove <name>`, exit 129
            // — so the belt-and-braces `--` that is free everywhere else made
            // this the one route that could never work. It answered 200 with
            // `ok: false`, because the operation genuinely ran and genuinely
            // failed, so nothing about the status code said so.
            //
            // Nothing is given up by dropping it: `valid_remote` already
            // refuses a name starting with `-` (see `not_an_option`), which is
            // the entire threat the separator was covering. Do not "restore
            // consistency" by adding it back.
            a.push(valid_remote(name)?.to_string());
        }
    }
    Ok(a)
}

/// `stash@{N}`. Built from a number, so it can never carry user text.
fn stash_ref(index: usize) -> String {
    format!("stash@{{{index}}}")
}

/// The argv for driving whatever sequence `state` says is running.
///
/// A merge, rebase, cherry-pick and revert each continue and abort with their
/// own subcommand, but a user only ever means "carry on" or "back out" — so the
/// API has one pair and the state picks the verb.
pub fn sequence_argv(state: RepoState, action: SequenceAction) -> Result<Vec<String>, String> {
    let cmd = match state {
        RepoState::Merge => "merge",
        RepoState::Rebase => "rebase",
        RepoState::CherryPick => "cherry-pick",
        RepoState::Revert => "revert",
        RepoState::Clean => return Err("nothing in progress".into()),
        RepoState::Bisect => return Err("bisect is not driven from here".into()),
        RepoState::Unknown => {
            return Err("git is in a state butai does not recognise — use a shell".into())
        }
    };
    let verb = match action {
        SequenceAction::Continue => "--continue",
        SequenceAction::Abort => "--abort",
        SequenceAction::Skip => {
            if matches!(state, RepoState::Merge) {
                return Err("a merge has nothing to skip".into());
            }
            "--skip"
        }
    };
    // No `--no-edit` here: `git merge --continue` refuses it and answers with
    // its usage text. The editor is neutralised by `GIT_EDITOR` in `run`
    // instead, which works for every verb.
    Ok(vec![cmd.to_string(), verb.to_string()])
}

// -- running ----------------------------------------------------------------

/// The outcome of one operation: git's closing line, or the reason it failed.
pub type OpResult = Result<String, String>;

/// Spawn `op` against `root`.
///
/// `reply` receives `Some(result)` if the operation finishes inside [`GRACE`]
/// and `None` if it is still running — so a millisecond-scale operation behaves
/// like an ordinary synchronous call and a network one never holds an HTTP
/// connection open. `None` is a distinct answer from `Some(Ok(""))`: an
/// operation really can succeed silently, and reporting that as "still running"
/// would leave a client polling something already finished.
///
/// Progress and completion also arrive as [`Event`]s regardless, so every
/// attached client learns about it, not only whoever started it.
#[allow(clippy::too_many_arguments)]
pub fn spawn_op(
    tx: UnboundedSender<Event>,
    root: PathBuf,
    ws: SessionId,
    seq: u64,
    op: GitOp,
    args: Vec<String>,
    reply: oneshot::Sender<Option<OpResult>>,
    cancel: oneshot::Receiver<()>,
) {
    tokio::spawn(async move {
        let (grace_tx, grace_rx) = oneshot::channel::<OpResult>();
        // The grace race lives here rather than in the caller so the caller
        // cannot forget to answer its oneshot.
        tokio::spawn(async move {
            let _ = reply.send(match tokio::time::timeout(GRACE, grace_rx).await {
                Ok(Ok(result)) => Some(result),
                // Timed out, or the op task died without reporting: either way
                // the caller is told it is still in flight and should poll.
                Ok(Err(_)) | Err(_) => None,
            });
        });

        let result = run(&root, &args, &tx, ws, seq, &op, cancel).await;
        let _ = grace_tx.send(result.clone());
        let _ = tx.send(Event::GitOpDone { root, ws, seq, result });
    });
}

async fn run(
    root: &Path,
    args: &[String],
    tx: &UnboundedSender<Event>,
    ws: SessionId,
    seq: u64,
    op: &GitOp,
    mut cancel: oneshot::Receiver<()>,
) -> OpResult {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).arg("--no-pager");
    // `core.askPass` in the user's config overrides the GIT_ASKPASS env var, so
    // clearing the variable alone would not close the door.
    cmd.arg("-c").arg("core.askPass=");
    // Belt and braces behind `valid_remote_url`: a transport helper is how a
    // remote URL turns into "run this program", and `ext` is the one that takes
    // a shell command outright. The allowlist should already have refused
    // anything containing `::`; this makes a miss inert rather than fatal, and
    // it also covers a helper URL configured outside butai entirely.
    cmd.arg("-c").arg("protocol.ext.allow=never");
    cmd.args(args);

    // Never block on a prompt. The daemon has no terminal to prompt on and no
    // way to relay one, so an operation that needs a credential must fail
    // quickly and say so rather than wait for an answer that cannot come.
    //
    // Deliberately *not* set: `GIT_SSH_COMMAND`. It would let us force
    // `BatchMode=yes`, but it overrides the user's own `core.sshCommand`, and
    // silently ignoring someone's `ssh -i ~/.keys/work` is a worse failure than
    // the one it prevents. An ssh that still finds something to wait on is
    // caught by IDLE_TIMEOUT instead.
    // A no-op editor rather than a `--no-edit` flag per subcommand: `git merge
    // --continue` rejects `--no-edit` outright, `git rebase --continue` takes
    // it, and knowing which is which for every verb is exactly the kind of
    // detail that rots. `true` exits 0 without touching the file, so git keeps
    // whatever message it had — and no command can ever block on an editor the
    // daemon has no way to answer. `GIT_SEQUENCE_EDITOR` covers the rebase todo
    // list the same way.
    cmd.env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/bin/false")
        .env("SSH_ASKPASS", "/bin/false")
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env("GIT_FLUSH", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err(format!("git: {e}")),
    };
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();

    let mut out = String::new();
    let mut tail: Vec<String> = Vec::new();
    let mut last_emit = tokio::time::Instant::now();
    // One buffer per pipe: `select!` holds both read futures alive at once.
    let mut ebuf = [0u8; 4096];
    let mut obuf = [0u8; 4096];
    let mut partial = String::new();

    let started = tokio::time::Instant::now();
    let outcome = loop {
        let idle = tokio::time::sleep(IDLE_TIMEOUT);
        let hard = tokio::time::sleep_until(started + HARD_TIMEOUT);
        tokio::pin!(idle, hard);

        tokio::select! {
            // Progress and errors both arrive on stderr; git writes them
            // separated by `\r`, not `\n`, so this cannot use `lines()`.
            n = read_some(&mut stderr, &mut ebuf) => {
                let Some(n) = n else { break wait_for(&mut child).await };
                partial.push_str(&String::from_utf8_lossy(&ebuf[..n]));
                for line in take_lines(&mut partial) {
                    if line.is_empty() { continue }
                    if tail.len() == TAIL_LINES { tail.remove(0); }
                    tail.push(line.clone());
                    if last_emit.elapsed() >= PROGRESS_INTERVAL {
                        last_emit = tokio::time::Instant::now();
                        let _ = tx.send(Event::GitOpProgress { ws, seq, line });
                    }
                }
            }
            n = read_some(&mut stdout, &mut obuf) => {
                match n {
                    Some(n) => out.push_str(&String::from_utf8_lossy(&obuf[..n])),
                    // stdout closing does not mean the process is done; stop
                    // polling this branch by dropping the handle.
                    None => stdout = None,
                }
            }
            status = child.wait() => break status.map_err(|e| format!("git: {e}")),
            _ = &mut cancel => {
                let _ = child.kill().await;
                return Err("cancelled".into());
            }
            _ = &mut idle => {
                let _ = child.kill().await;
                return Err(format!("no output for {}s — giving up", IDLE_TIMEOUT.as_secs()));
            }
            _ = &mut hard => {
                let _ = child.kill().await;
                return Err(format!("still running after {}s — giving up", HARD_TIMEOUT.as_secs()));
            }
        }
    };

    let status = outcome?;
    // Prefer stderr's closing line: git says "Everything up-to-date" and every
    // rejection reason there, and leaves stdout empty for most operations.
    let summary = tail
        .iter()
        .rev()
        .find(|l| !l.trim().is_empty())
        .cloned()
        .or_else(|| out.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string))
        .unwrap_or_default();

    if status.success() {
        Ok(if summary.is_empty() { format!("{} ok", op.kind()) } else { summary })
    } else {
        // A failed operation's last line is often just a summary of a longer
        // explanation ("error: failed to push some refs"), so keep the tail.
        let detail: Vec<&str> = tail.iter().map(String::as_str).rev().take(4).collect();
        let detail = detail.into_iter().rev().collect::<Vec<_>>().join("; ");
        Err(if detail.is_empty() { format!("{} failed", op.kind()) } else { detail })
    }
}

/// Read from an optional pipe, or never resolve when it is gone. Returning
/// `None` distinguishes EOF from "there is no such pipe".
async fn read_some(pipe: &mut Option<impl AsyncReadExt + Unpin>, buf: &mut [u8]) -> Option<usize> {
    match pipe {
        Some(p) => match p.read(buf).await {
            Ok(0) | Err(_) => None,
            Ok(n) => Some(n),
        },
        // `select!` polls every branch; an absent pipe must simply never fire
        // rather than resolve immediately and spin the loop.
        None => std::future::pending().await,
    }
}

/// Split off every complete line in `partial`, treating `\r` and `\n` alike.
/// git's progress meter overwrites one line with `\r`, so a reader that only
/// splits on `\n` sees nothing until the transfer ends.
fn take_lines(partial: &mut String) -> Vec<String> {
    let mut lines = Vec::new();
    while let Some(i) = partial.find(['\r', '\n']) {
        let line: String = partial[..i].trim_end().to_string();
        partial.drain(..i + 1);
        lines.push(line);
    }
    // A progress meter can run for a long time without any terminator at all;
    // do not let the buffer grow without bound.
    if partial.len() > 8192 {
        partial.clear();
    }
    lines
}

async fn wait_for(child: &mut tokio::process::Child) -> Result<std::process::ExitStatus, String> {
    child.wait().await.map_err(|e| format!("git: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reflog forms are the whole reason this validator was widened: a
    /// Stashes list that cannot show a diff for `stash@{0}` is a list of rows
    /// that do nothing.
    #[test]
    fn show_accepts_the_reflog_forms() {
        for rev in ["stash@{0}", "main@{upstream}", "HEAD@{2}", "HEAD~2", "abc123^", "v0.8.0"] {
            assert!(valid_show_rev(rev).is_ok(), "{rev} should be a revision");
        }
    }

    /// `:` is the one character `valid_rev` allows and this must not: it turns
    /// `show` from "diff this commit" into "read this file out of that tree",
    /// which is a different endpoint's job and a wider read than this one
    /// promises.
    #[test]
    fn show_refuses_a_path_bearing_rev() {
        for rev in ["HEAD:Cargo.toml", "HEAD:../../etc/passwd", ":/secret"] {
            assert!(valid_show_rev(rev).is_err(), "{rev} should not be a revision");
        }
    }

    #[test]
    fn show_refuses_options_shell_syntax_and_absurd_lengths() {
        for rev in ["--upload-pack=evil", "-n", "a;b", "a b", "a$(id)", "a|b", ""] {
            assert!(valid_show_rev(rev).is_err(), "{rev:?} should not be a revision");
        }
        assert!(valid_show_rev(&"a".repeat(101)).is_err(), "no length cap");
    }

    fn push(remote: Option<&str>, branch: Option<&str>) -> GitOp {
        GitOp::Push {
            remote: remote.map(str::to_string),
            branch: branch.map(str::to_string),
            set_upstream: false,
            force_with_lease: false,
        }
    }

    #[test]
    fn argv_is_what_a_person_would_have_typed() {
        assert_eq!(
            argv(&GitOp::Fetch { remote: None, all: false, prune: false }).unwrap(),
            ["fetch", "--progress"]
        );
        assert_eq!(
            argv(&GitOp::Fetch { remote: Some("origin".into()), all: false, prune: true }).unwrap(),
            ["fetch", "--progress", "--prune", "origin"]
        );
        // `--all` and a named remote are mutually exclusive; `--all` wins.
        assert_eq!(
            argv(&GitOp::Fetch { remote: Some("origin".into()), all: true, prune: false }).unwrap(),
            ["fetch", "--progress", "--all"]
        );
        assert_eq!(
            argv(&push(Some("origin"), Some("feature/x"))).unwrap(),
            ["push", "--progress", "origin", "feature/x"]
        );
        assert_eq!(
            argv(&GitOp::Push {
                remote: Some("origin".into()),
                branch: Some("main".into()),
                set_upstream: true,
                force_with_lease: true,
            })
            .unwrap(),
            ["push", "--progress", "--force-with-lease", "--set-upstream", "origin", "main"]
        );
        assert_eq!(
            argv(&GitOp::Pull { remote: None, branch: None, rebase: true, ff_only: false })
                .unwrap(),
            ["pull", "--progress", "--rebase"]
        );
    }

    /// The injection surface, exhaustively. Every one of these must produce no
    /// argv at all — not a sanitised one, which would still have run.
    #[test]
    fn a_value_that_git_would_read_as_an_option_never_reaches_argv() {
        let refused = [
            // The actual remote-code-execution vectors.
            push(Some("origin"), Some("--upload-pack=touch /tmp/pwned")),
            push(Some("--exec=touch /tmp/pwned"), None),
            push(Some("ext::sh -c whoami"), None),
            push(Some("ssh://evil/repo"), None),
            push(Some("user@host:path"), None),
            // git's own ref rules.
            push(Some("origin"), Some("a..b")),
            push(Some("origin"), Some("a b")),
            push(Some("origin"), Some("x.lock")),
            push(Some("origin"), Some("HEAD@{0}")),
            push(Some("origin"), Some("with\nnewline")),
            push(Some("origin"), Some("has~tilde")),
            push(Some("origin"), Some("has:colon")),
            push(Some("origin"), Some("/leading")),
            push(Some("origin"), Some("trailing/")),
            push(Some("origin"), Some("double//slash")),
            push(Some("origin"), Some(".hidden/x")),
            push(Some("origin"), Some("")),
            push(Some("origin"), Some(&"x".repeat(300))),
        ];
        for op in refused {
            assert!(argv(&op).is_err(), "accepted a hostile value: {op:?}");
        }
    }

    /// Worktree paths come from a text prompt, so they get the same treatment
    /// as remotes and refs: git must never be able to read one as a flag.
    #[test]
    fn a_worktree_path_can_never_become_an_option() {
        let add = |path: &str, branch: Option<&str>, new: bool| GitOp::WorktreeAdd {
            path: path.into(),
            branch: branch.map(str::to_string),
            new_branch: new,
        };
        let refused = [
            add("--git-dir=/etc", None, false),
            add("-f", None, false),
            add("", None, false),
            add("relative/path", None, false),
            add("/tmp/wt", Some("--force"), true),
            add("/tmp/wt", Some("a..b"), true),
            add("/tmp/wt", Some("has space"), true),
            // `-b` with nothing to name is a mistake, not a branch called "".
            add("/tmp/wt", None, true),
            GitOp::WorktreeRemove { path: "--force".into(), force: false },
            GitOp::WorktreeRemove { path: "wt".into(), force: true },
        ];
        for op in refused {
            assert!(argv(&op).is_err(), "accepted a hostile worktree value: {op:?}");
        }

        // And the ordinary shapes produce what a person would have typed. `--`
        // separates the path from the flags, so a path can never be re-read as
        // one however it is spelled.
        assert_eq!(
            argv(&add("/home/paul/wt", Some("feat/x"), true)).unwrap(),
            ["worktree", "add", "-b", "feat/x", "--", "/home/paul/wt"]
        );
        assert_eq!(
            argv(&add("/home/paul/wt", Some("main"), false)).unwrap(),
            ["worktree", "add", "--", "/home/paul/wt", "main"]
        );
        assert_eq!(
            argv(&GitOp::WorktreeRemove { path: "/home/paul/wt".into(), force: true }).unwrap(),
            ["worktree", "remove", "--force", "--", "/home/paul/wt"]
        );
        assert_eq!(argv(&GitOp::WorktreePrune).unwrap(), ["worktree", "prune"]);
    }

    /// Adding a remote is the one place a **URL** reaches git, and a URL is the
    /// documented way to make git run a program: `ext::sh -c whoami` dispatches
    /// to `git-remote-ext`, and any `<helper>::<rest>` reaches
    /// `git-remote-<helper>` on `$PATH`. This is the test that keeps the
    /// allowlist honest.
    #[test]
    fn a_remote_url_that_would_run_a_program_is_refused() {
        let add = |url: &str| GitOp::RemoteAdd { name: "evil".into(), url: url.into() };
        for hostile in [
            "ext::sh -c whoami",
            "ext::sh",
            "fd::17/foo",
            "transport::whatever",
            // A scheme-looking prefix does not save a helper form.
            "https://x/y::ext::sh",
            "--upload-pack=touch /tmp/pwned",
            "-u",
            "",
            // Relative and scheme-less strings are not transports.
            "some/relative/path",
            "just-a-word",
            // Whitespace is how a single argument becomes several.
            "https://example.com/a b",
            "https://example.com/\nrepo",
            // A scheme with no host names nothing.
            "https://",
        ] {
            assert!(argv(&add(hostile)).is_err(), "accepted a hostile remote url: {hostile:?}");
        }

        // The legitimate transports, including the scp form that makes a plain
        // "reject colons" rule impossible.
        for ok in [
            "https://github.com/dieterpl/butai.git",
            "http://internal/repo.git",
            "ssh://git@example.com:22/repo.git",
            "git://example.com/repo",
            "file:///srv/repos/proj.git",
            "git@github.com:dieterpl/butai.git",
            "deploy_key@build-1.internal:repos/proj",
            "/srv/repos/proj.git",
        ] {
            assert!(valid_remote_url(ok).is_ok(), "refused a legitimate remote url: {ok:?}");
        }

        assert_eq!(
            argv(&GitOp::RemoteAdd {
                name: "origin".into(),
                url: "git@github.com:dieterpl/butai.git".into()
            })
            .unwrap(),
            ["remote", "add", "--", "origin", "git@github.com:dieterpl/butai.git"]
        );
        // The *name* keeps its stricter rule: it is still not a URL.
        assert!(argv(&GitOp::RemoteAdd {
            name: "ext::sh -c whoami".into(),
            url: "https://example.com/r".into()
        })
        .is_err());
        // `remove` takes **no** separator, and that is the whole bug this
        // asserts against: `git remote remove` answers its usage text and exit
        // 129 for a `--` that `add` one line up accepts happily. Asserted as
        // the exact argv *and* as the absence of the token, so re-adding it for
        // symmetry fails here rather than in a user's repository.
        let remove = argv(&GitOp::RemoteRemove { name: "origin".into() }).unwrap();
        assert_eq!(remove, ["remote", "remove", "origin"]);
        assert!(!remove.iter().any(|a| a == "--"), "`git remote remove` rejects a `--`");
        // The name is still validated, which is what makes the missing
        // separator safe: an option-shaped name never reaches argv at all.
        assert!(argv(&GitOp::RemoteRemove { name: "--upload-pack=touch /tmp/x".into() }).is_err());
    }

    #[test]
    fn ordinary_names_are_not_refused() {
        for name in ["main", "feature/x", "release-1.2", "user/JIRA-42/fix", "v2.0-rc1"] {
            assert!(valid_ref_name(name).is_ok(), "refused an ordinary branch: {name}");
        }
        for name in ["origin", "upstream", "fork2"] {
            assert!(valid_remote(name).is_ok(), "refused an ordinary remote: {name}");
        }
    }

    #[test]
    fn set_upstream_without_a_remote_is_refused() {
        let op =
            GitOp::Push { remote: None, branch: None, set_upstream: true, force_with_lease: false };
        assert!(argv(&op).is_err());
    }

    /// One "carry on" verb, four subcommands.
    ///
    /// Pinned because the flags are not interchangeable: `git merge --continue`
    /// **rejects** `--no-edit` and answers with its usage text, while
    /// `git rebase --continue` accepts it. Neither gets one — the editor is
    /// neutralised by `GIT_EDITOR` instead, which is uniform.
    #[test]
    fn a_sequence_verb_names_the_right_subcommand() {
        use SequenceAction::*;
        let cases = [
            (RepoState::Merge, Continue, vec!["merge", "--continue"]),
            (RepoState::Merge, Abort, vec!["merge", "--abort"]),
            (RepoState::Rebase, Continue, vec!["rebase", "--continue"]),
            (RepoState::Rebase, Skip, vec!["rebase", "--skip"]),
            (RepoState::CherryPick, Abort, vec!["cherry-pick", "--abort"]),
            (RepoState::Revert, Continue, vec!["revert", "--continue"]),
        ];
        for (state, action, want) in cases {
            assert_eq!(sequence_argv(state, action).unwrap(), want, "{state:?} {action:?}");
        }

        // A merge has no commit to skip, and there is nothing to carry on with
        // in a clean repository.
        assert!(sequence_argv(RepoState::Merge, Skip).is_err());
        assert!(sequence_argv(RepoState::Clean, Continue).is_err());
        // A state this version does not model must say so rather than guess.
        assert!(sequence_argv(RepoState::Unknown, Abort).is_err());
    }

    /// An operation that needs a message must never be able to sit waiting for
    /// an editor the daemon cannot answer.
    #[test]
    fn nothing_that_commits_can_open_an_editor() {
        assert_eq!(
            argv(&GitOp::Amend { message: None }).unwrap(),
            ["commit", "--amend", "--no-edit"]
        );
        assert_eq!(
            argv(&GitOp::Amend { message: Some("fixed".into()) }).unwrap(),
            ["commit", "--amend", "-m", "fixed"]
        );
        for op in [
            GitOp::Revert { rev: "abc123".into() },
            GitOp::Merge { branch: "feature".into(), no_ff: false },
        ] {
            assert!(
                argv(&op).unwrap().iter().any(|a| a == "--no-edit"),
                "{op:?} may open an editor"
            );
        }
    }

    /// git's progress meter is `\r`-separated, so a splitter that only knows
    /// `\n` reports nothing at all until the transfer finishes.
    #[test]
    fn progress_lines_split_on_carriage_returns() {
        let mut partial = String::from("Receiving: 10%\rReceiving: 50%\rdone\n");
        assert_eq!(take_lines(&mut partial), ["Receiving: 10%", "Receiving: 50%", "done"]);
        assert!(partial.is_empty());

        // A partial line is held back until its terminator arrives.
        let mut partial = String::from("Receiving: 1");
        assert!(take_lines(&mut partial).is_empty());
        partial.push_str("0%\r");
        assert_eq!(take_lines(&mut partial), ["Receiving: 10%"]);
    }
}
