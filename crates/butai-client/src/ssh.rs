//! Reaching a daemon on another machine, by making its socket a local path.
//!
//! **Why a forward rather than a pipe.** `butai-connect` can already run
//! `ssh host butai proxy` and speak the framed protocol down its stdio, and that
//! is enough to stream one pane. It is not enough to *be a client*: a client
//! also needs REST and an event stream, which are HTTP, and HTTP over one pipe
//! means one request at a time with no way to hold the event stream open beside
//! them. `ssh -L` gives a Unix socket instead, and from there the far daemon is
//! a local path like any other — the same shape the macOS client uses, and the
//! reason [`crate::endpoints`] takes sockets rather than ssh commands.
//!
//! The socket path comes from the far side itself, two ways. A `butai` run over
//! ssh announces where its daemon listens and the daemon relays that to us, so
//! a handoff needs no configuration at all; a machine picked out of
//! `~/.ssh/config` has announced nothing, so [`remote_socket`] asks it. Either
//! way the path is never guessed — `~/.butai/butai.sock` is not guaranteed (with
//! no home directory the daemon lives under `/tmp`), and `-L` forwards the path
//! verbatim with no shell expansion to save us.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::{Child, Command};

/// How long to wait for `ssh -L` to bind the local socket.
///
/// It is a full ssh connection — DNS, TCP, the key exchange, possibly a
/// passphrase-less agent round trip — so it is sized for a bad link rather than
/// a LAN. Nothing is blocked meanwhile: the caller awaits this while the rest
/// of the workbench keeps drawing.
const BIND_TIMEOUT: Duration = Duration::from_secs(15);

/// A live `ssh -L` forward. Dropping it kills the ssh child and removes the
/// socket, so a host that goes away leaves nothing behind.
pub struct Forward {
    child: Child,
    socket: PathBuf,
}

impl Forward {
    /// The local path the far daemon is now reachable at.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Whether the ssh carrying this forward is still running.
    ///
    /// Non-blocking, so the loop can ask on every event without stalling.
    ///
    /// **Necessary but not sufficient**, and the reconnect trigger treats it
    /// that way. A link that went half-open is still a live child until
    /// `ServerAliveInterval` gives up on it (see
    /// [`crate::dial::control_master_opts`]), so a machine can be unreachable
    /// for the better part of a minute while this still says yes. What it
    /// catches is the other half: an ssh that exited — a refused forward, a
    /// dropped route, a far host that rebooted — which the event stream alone
    /// cannot distinguish from a daemon that is merely restarting.
    pub fn is_alive(&mut self) -> bool {
        // An error from `try_wait` means the child is unwaitable, which is not
        // a running ssh either.
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        // `kill_on_drop` handles the child; the socket is ours to clean up.
        std::fs::remove_file(&self.socket).ok();
    }
}

/// How long to wait for the far host to say where its daemon listens.
///
/// Longer than it sounds, because the answer is not only a round trip: `butai`
/// starts the far daemon if none is running, and a cold start reads a session
/// file and reopens its workspaces before the CLI's own reply comes back.
const WHOAMI_TIMEOUT: Duration = Duration::from_secs(20);

/// Bring `target` within reach, discovering its socket when we do not know it.
///
/// The two entry points differ only here: a handoff arrives with the far
/// daemon's socket attached, and a machine chosen from `~/.ssh/config` has told
/// us nothing, so it is asked.
pub async fn dial(target: &str, args: &[String], socket: Option<&str>) -> Result<Forward> {
    let socket = match socket.filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => remote_socket(target, args).await?,
    };
    forward(target, args, &socket).await
}

/// Ask `target` where its daemon listens, starting one if none is.
///
/// `butai --json whoami` reports the socket this invocation *would* talk to, so
/// it answers outside a pane too — which is the whole reason it reports a
/// resolved path rather than `$BUTAI_SOCKET`. `butai ls` in front of it is what
/// makes the daemon exist: connecting to a machine that is not running butai yet
/// has always started one (that is what `ssh host butai proxy` did), and a host
/// picker that only worked on machines you had already visited would be a
/// smaller thing than the one being replaced.
pub async fn remote_socket(target: &str, args: &[String]) -> Result<String> {
    anyhow::ensure!(!target.is_empty(), "no ssh target to ask");
    let script = format!(
        r#"{}; "$BUTAI" ls >/dev/null 2>&1; exec "$BUTAI" --json whoami"#,
        crate::dial::find_binary()
    );
    let mut cmd = Command::new("ssh");
    // No tty: this is one JSON document, not a session.
    cmd.arg("-T");
    cmd.arg("-o").arg("BatchMode=yes");
    for opt in crate::dial::control_master_opts() {
        cmd.arg("-o").arg(opt);
    }
    cmd.args(args);
    cmd.arg(target).arg(script);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);

    // None of these name the target: the caller has already put it in front of
    // whatever comes back, and the footer is one line — a live run of this
    // spent a third of it on "gpu-box: gpu-box: ssh: ...".
    let out = tokio::time::timeout(WHOAMI_TIMEOUT, cmd.output())
        .await
        .map_err(|_| anyhow::anyhow!("no answer within {WHOAMI_TIMEOUT:?}"))?
        .context("spawn ssh")?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        // The one failure worth naming, because it is the only one the user can
        // do something about and the shell's own words for it are useless.
        if why.contains(crate::dial::NOT_INSTALLED) {
            // Names every binary that *would* have answered, because the
            // rename made this message a liar: it said "not installed" to
            // machines carrying a perfectly good install under the old name.
            // They are found now, so a machine that still reaches here really
            // has none of them — and the message is the only place the user
            // learns which names were looked for.
            let names = butai_protocol::names::BINARIES.join(" or ");
            anyhow::bail!(
                "no {names} there — install it on that machine \
                 (~/.local/bin/{} or on its PATH)",
                butai_protocol::names::BINARY
            );
        }
        let why = why.lines().find(|l| !l.trim().is_empty()).unwrap_or("no output");
        anyhow::bail!("{}", why.trim());
    }
    socket_from_whoami(&out.stdout).context("it did not say where its daemon is")
}

/// Pull the socket path out of `butai --json whoami`.
///
/// Tolerant of anything printed in front of it, because a login shell's rc
/// files write to stdout more often than their authors think and one `echo`
/// would otherwise make the machine unreachable.
fn socket_from_whoami(stdout: &[u8]) -> Result<String> {
    let text = String::from_utf8_lossy(stdout);
    let start = text.find('{').context("no JSON in the reply")?;
    let value: serde_json::Value =
        serde_json::from_str(text[start..].trim()).context("reply was not JSON")?;
    let socket = value.get("socket").and_then(|s| s.as_str()).context("no socket in the reply")?;
    anyhow::ensure!(!socket.is_empty(), "the socket in the reply is empty");
    Ok(socket.to_string())
}

/// Forward `remote_socket` on `target` to a fresh local socket.
///
/// `args` are the arguments the far side was reached with — recovered by the
/// daemon from the pane's own `ssh` process — so this goes back the same way,
/// through the same jump hosts, with the same key.
pub async fn forward(target: &str, args: &[String], remote_socket: &str) -> Result<Forward> {
    anyhow::ensure!(!target.is_empty(), "no ssh target to dial back on");
    anyhow::ensure!(!remote_socket.is_empty(), "the far side did not say where its socket is");

    let socket = local_socket_path(target);
    // A stale socket from a killed client would make ssh refuse to bind.
    std::fs::remove_file(&socket).ok();
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }

    let mut cmd = Command::new("ssh");
    // `-N` runs no command: this connection exists only to carry the forward.
    // `-T` keeps ssh from allocating a tty we would then have to manage.
    cmd.arg("-N").arg("-T");
    cmd.arg("-o").arg("BatchMode=yes");
    cmd.arg("-o").arg("ExitOnForwardFailure=yes");
    // Reuse the connection [`remote_socket`] just opened, when there was one.
    // A forward asked of an existing master costs no key exchange, which is the
    // difference between the picker feeling instant and it costing two full ssh
    // handshakes back to back.
    for opt in crate::dial::control_master_opts() {
        cmd.arg("-o").arg(opt);
    }
    cmd.arg("-L").arg(format!("{}:{remote_socket}", socket.display()));
    cmd.args(args);
    cmd.arg(target);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);

    let child = cmd.spawn().context("spawn ssh -L")?;
    let mut fwd = Forward { child, socket };
    wait_for_socket(&mut fwd).await?;
    Ok(fwd)
}

/// Wait until the forwarded socket accepts a connection, or ssh gives up.
///
/// Existence is not enough — ssh creates the socket before the connection is
/// usable — so this connects, which is also what the caller is about to do.
async fn wait_for_socket(fwd: &mut Forward) -> Result<()> {
    let deadline = tokio::time::Instant::now() + BIND_TIMEOUT;
    loop {
        if tokio::net::UnixStream::connect(&fwd.socket).await.is_ok() {
            return Ok(());
        }
        // A dead ssh will never bind, and its stderr says why (a bad key, an
        // unknown host, a refused forward). Reporting that beats a timeout.
        if let Some(status) = fwd.child.try_wait().context("wait on ssh")? {
            let why = drain_stderr(fwd).await;
            anyhow::bail!("ssh exited ({status}){why}");
        }
        if tokio::time::Instant::now() >= deadline {
            let why = drain_stderr(fwd).await;
            anyhow::bail!("ssh did not forward the socket within {BIND_TIMEOUT:?}{why}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Whatever ssh complained about, trimmed to one line for a footer.
async fn drain_stderr(fwd: &mut Forward) -> String {
    use tokio::io::AsyncReadExt;
    let Some(mut err) = fwd.child.stderr.take() else { return String::new() };
    let mut text = String::new();
    // Never block on this: ssh may still hold the pipe open.
    let read = tokio::time::timeout(Duration::from_millis(200), err.read_to_string(&mut text));
    read.await.ok();
    match text.lines().find(|l| !l.trim().is_empty()) {
        Some(line) => format!(": {}", line.trim()),
        None => String::new(),
    }
}

/// A private path for one forward.
///
/// Under the user's runtime directory when there is one, because a socket in
/// `/tmp` is world-listable and this one reaches a daemon that can run
/// commands. The name carries the target so two forwards to different machines
/// cannot collide, and the pid so two clients cannot.
fn local_socket_path(target: &str) -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("butai-forwards");
    let safe: String = target
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    // Socket paths have a hard length limit (~108 bytes on Linux) and it counts
    // the whole path, so the variable part is kept short rather than
    // descriptive.
    let safe: String = safe.chars().take(24).collect();
    dir.join(format!("{safe}-{}.sock", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forward_path_is_private_and_collision_proof() {
        let a = local_socket_path("user@host.example.com");
        let b = local_socket_path("other");
        assert_ne!(a, b, "two targets must not share a socket");
        assert_eq!(a.parent(), b.parent());
        // Nothing in the name can escape the directory or confuse a shell.
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!name.contains('/'), "{name}");
        assert!(!name.contains('@'), "{name}");
        assert!(name.ends_with(".sock"), "{name}");
        // Unix socket paths are length-limited, and the limit counts the whole
        // path — a long ssh alias must not push it over.
        let long = local_socket_path(&"x".repeat(200));
        assert!(long.as_os_str().len() < 100, "{}", long.display());
    }

    /// The half of "the machine went away" that the event stream cannot see.
    /// A dead ssh and a daemon that is merely restarting produce the same
    /// stream errors; only the child says which one happened.
    #[tokio::test]
    async fn a_forward_knows_when_its_ssh_has_gone() {
        let mut alive = Forward {
            child: Command::new("sleep").arg("30").kill_on_drop(true).spawn().expect("spawn sleep"),
            // Never bound, and `Drop` only unlinks — nothing to clean up.
            socket: std::env::temp_dir().join("butai-is-alive-test.sock"),
        };
        assert!(alive.is_alive(), "a running ssh must read as alive");

        let mut dead = Forward {
            child: Command::new("true").kill_on_drop(true).spawn().expect("spawn true"),
            socket: std::env::temp_dir().join("butai-is-dead-test.sock"),
        };
        dead.child.wait().await.expect("reap");
        assert!(!dead.is_alive(), "an ssh that exited must not read as alive");
    }

    #[tokio::test]
    async fn a_forward_refuses_without_somewhere_to_dial() {
        assert!(forward("", &[], "/run/butai.sock").await.is_err());
        assert!(forward("host", &[], "").await.is_err());
        assert!(remote_socket("", &[]).await.is_err());
    }

    /// Verbatim from a real `butai --json whoami` on a machine with no daemon
    /// running yet, so a change to the CLI's reply shape breaks this rather
    /// than the host picker.
    #[test]
    fn a_socket_is_read_out_of_whoami() {
        let reply = br#"{"inside_butai":false,"pane":null,"socket":"/tmp/cr3/home/.butai/butai.sock","workspace":null}"#;
        assert_eq!(socket_from_whoami(reply).unwrap(), "/tmp/cr3/home/.butai/butai.sock");
    }

    #[test]
    fn a_chatty_login_shell_does_not_hide_the_socket() {
        // `~/.bashrc` printing something is the ordinary case, not a broken
        // machine, and it must not be the reason a host is unreachable.
        let reply = b"Welcome to gpu-box!\nlast login: never\n{\"socket\":\"/run/b.sock\"}\n";
        assert_eq!(socket_from_whoami(reply).unwrap(), "/run/b.sock");
    }

    #[test]
    fn a_reply_with_no_socket_in_it_is_an_error() {
        assert!(socket_from_whoami(b"command not found: butai\n").is_err());
        assert!(socket_from_whoami(br#"{"inside_butai":false}"#).is_err());
        assert!(socket_from_whoami(br#"{"socket":""}"#).is_err());
    }
}
