//! How to reach a daemon that is not the local one.
//!
//! Two ways, and they are the two `docs/building-a-client.md` §2.3 already
//! documents — this module is only the Rust side of them:
//!
//! * [`Dial::Ssh`] runs `ssh host butai proxy` and speaks the protocol over the
//!   child's stdio. SSH is both the transport and the authentication, which is
//!   why the daemon has never needed to listen on TCP.
//! * [`Dial::Socket`] connects to a socket that is already reachable — a
//!   `ssh -N -L …` forward, or another daemon on the same box.
//!
//! Neither spawns a daemon on the far end. A far socket that does not answer
//! means the tunnel is down or the daemon is gone, and both are worth
//! reporting rather than papering over with a second, empty daemon.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use butai_protocol::{names, AttachTarget, Encoding, ServerMsg};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::conn::{connect_existing, hello, into_transport, Transport};

/// How long to wait for the far daemon's Hello before giving up. Generous
/// because the clock starts before ssh has authenticated, and an agent-less
/// key or a cold ControlMaster can take a few seconds.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// The shell fragment that puts the far host's binary in `$BUTAI`.
///
/// `$HOME/.local/bin` first because that is where a `cargo install` lands and
/// it is routinely missing from a non-interactive ssh's `PATH`; `command -v`
/// second for a system install.
///
/// Every name in [`names::BINARIES`] is tried, in order, because a rename here
/// does not rename the binary on machines you have not upgraded — and the one
/// that shipped as `bmux` answers `whoami`, `ls` and `proxy` just as well. This
/// is the difference between the rename costing a reinstall on every machine
/// and it costing every machine.
///
/// Public because it is not only `proxy` that has to find the far binary: the
/// client's host picker asks the same machine where its daemon listens
/// (`--json whoami`), and the two must resolve the same binary or they would be
/// talking about different daemons.
pub fn find_binary() -> String {
    let names = names::BINARIES.join(" ");
    // Say the failure in one word rather than letting the shell fail on its
    // own. An empty `$BUTAI` runs as `"" ls`, and what came back was `: command
    // not found` with no host and no hint — a message that reads like a bug in
    // the client rather than a machine that has never had this on it.
    format!(
        r#"BUTAI=""; for n in {names}; do if [ -x "$HOME/.local/bin/$n" ]; then BUTAI="$HOME/.local/bin/$n"; break; fi; BUTAI="$(command -v "$n" 2>/dev/null)"; [ -n "$BUTAI" ] && break; done; [ -n "$BUTAI" ] || {{ echo "{NOT_INSTALLED}" >&2; exit 127; }}"#
    )
}

/// What the far side prints when it cannot find anything to run.
pub const NOT_INSTALLED: &str = "butai-not-installed";

/// The shell fragment run on the far host.
///
/// `exec` so the shell does not linger between us and the process whose stdio
/// we are reading.
///
/// Deliberately *not* `nc -U` or `socat`: `butai proxy` means nothing extra has
/// to be installed on the far host, which matters because some distributions
/// (Raspberry Pi OS, for one) ship no `nc -U` at all.
fn remote_cmd() -> String {
    format!(r#"{}; exec "$BUTAI" proxy"#, find_binary())
}

#[cfg(test)]
mod find_butai_tests {
    /// Run the fragment against a directory holding `names`, and report what it
    /// put in `$BUTAI`.
    ///
    /// Everything here goes through a real `sh` rather than eyeballing the
    /// quoting: a stray quote fails on the far side, where nobody can see it,
    /// and the fragment is now a loop rather than two assignments.
    fn resolve(names: &[&str], on_path: bool) -> std::process::Output {
        let dir = std::env::temp_dir().join(format!("butai-find-{}", names.join("-")));
        std::fs::create_dir_all(&dir).unwrap();
        for name in names {
            let fake = dir.join(name);
            std::fs::write(&fake, "#!/bin/sh\nexit 0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        // Either the binaries are on `PATH` (a system install) or they are in
        // `$HOME/.local/bin` (a `cargo install`, which is routinely *not* on a
        // non-interactive ssh's PATH). The fragment has to find both.
        let (path, home) = if on_path {
            (dir.clone(), std::path::PathBuf::from("/nonexistent"))
        } else {
            let home = dir.parent().unwrap().join(format!("butai-home-{}", names.join("-")));
            let bin = home.join(".local/bin");
            std::fs::create_dir_all(&bin).unwrap();
            for name in names {
                std::fs::copy(dir.join(name), bin.join(name)).unwrap();
            }
            (std::path::PathBuf::from("/nonexistent"), home)
        };
        std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                r#"PATH="{}" HOME="{}"; {}; echo "$BUTAI""#,
                path.display(),
                home.display(),
                super::find_binary()
            ))
            .output()
            .expect("run sh")
    }

    #[test]
    fn a_machine_without_any_of_them_says_so_and_fails() {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                r#"PATH=/nonexistent HOME=/nonexistent; {}; echo reached"#,
                super::find_binary()
            ))
            .output()
            .expect("run sh");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(super::NOT_INSTALLED), "stderr was {err:?}");
        assert!(
            !String::from_utf8_lossy(&out.stdout).contains("reached"),
            "it must stop rather than run the next command with an empty $BUTAI"
        );
        assert_eq!(out.status.code(), Some(127), "127 is `command not found`");
    }

    /// And when there *is* one, it finds it and carries on — from either place.
    #[test]
    fn a_machine_with_the_current_name_carries_on() {
        for on_path in [true, false] {
            let out = resolve(&["butai"], on_path);
            assert!(out.status.success(), "stderr {:?}", String::from_utf8_lossy(&out.stderr));
            assert!(
                String::from_utf8_lossy(&out.stdout).trim().ends_with("butai"),
                "it should have named the binary it found (on_path={on_path})"
            );
        }
    }

    /// The one this whole loop exists for. A machine that was never upgraded
    /// past `bmux` is still a machine you can open, and it was reported as
    /// "no butai there — install it", about a working install.
    #[test]
    fn a_machine_still_carrying_the_old_name_is_reachable() {
        for on_path in [true, false] {
            let out = resolve(&["bmux"], on_path);
            assert!(
                out.status.success(),
                "an un-upgraded machine must still resolve (on_path={on_path}): {:?}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                String::from_utf8_lossy(&out.stdout).trim().ends_with("bmux"),
                "it should have fallen back to the old name"
            );
        }
    }

    /// A half-upgraded machine has both. The new one is the one to talk to:
    /// they are two different daemons on two different sockets, and the old
    /// binary is the one that is about to be deleted.
    #[test]
    fn a_machine_with_both_prefers_the_current_name() {
        let out = resolve(&["butai", "bmux"], true);
        assert!(out.status.success());
        assert!(
            String::from_utf8_lossy(&out.stdout).trim().ends_with("butai"),
            "the current name wins: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dial {
    Ssh {
        /// The ssh destination — an alias from `~/.ssh/config`, or `user@host`.
        target: String,
        /// Extra arguments placed before the destination (`-p`, `-i`, `-J`, …).
        args: Vec<String>,
        /// `BUTAI_SOCKET` for the far daemon. Normally `None`: left unset, the
        /// far `butai` resolves its own default and attaches to the daemon
        /// already running there, instead of starting a second one on a path
        /// nothing else uses.
        socket: Option<PathBuf>,
    },
    /// A socket already reachable from here — an `ssh -L` forward, or a second
    /// local daemon.
    Socket(PathBuf),
}

impl Dial {
    /// A short label for the tab badge and error messages.
    pub fn label(&self) -> String {
        match self {
            Dial::Ssh { target, .. } => target.clone(),
            Dial::Socket(path) => path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
        }
    }
}

/// A live connection to a far daemon.
pub struct Dialed {
    pub transport: Transport,
    /// `None` for [`Dial::Socket`]. Held so the ssh process is killed when the
    /// connection is dropped, and so a caller can await its death.
    pub child: Option<SshChild>,
}

/// The `ssh` process behind a [`Dial::Ssh`] connection.
///
/// Owns the stderr tail as well as the process: ssh reports the things a user
/// most needs to see — `Permission denied (publickey)`, `Host key
/// verification failed` — on stderr and then exits, so without capturing it a
/// failed connection is an unexplained blank panel.
pub struct SshChild {
    child: Child,
    stderr: Arc<Mutex<String>>,
}

impl SshChild {
    /// Wait for ssh to exit, and describe why in one line.
    pub async fn wait(&mut self) -> String {
        let status = self.child.wait().await;
        let tail = self.last_stderr();
        if !tail.is_empty() {
            return tail;
        }
        match status {
            Ok(s) if s.success() => "connection closed".to_string(),
            Ok(s) => format!("ssh exited with {s}"),
            Err(e) => format!("ssh failed: {e}"),
        }
    }

    /// The most recent non-empty line ssh wrote to stderr, if any.
    pub fn last_stderr(&self) -> String {
        self.stderr.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

impl Drop for SshChild {
    fn drop(&mut self) {
        // `kill_on_drop` covers the tokio-managed case, but only once the
        // runtime reaps it; asking directly means a disconnected host does not
        // leave an ssh holding a ControlMaster open behind it.
        let _ = self.child.start_kill();
    }
}

/// Open a connection to a far daemon and complete the protocol handshake.
///
/// Returns once the far daemon's [`ServerMsg::Hello`] has arrived, so a
/// successful return means the far end really is a butai daemon that accepted
/// our version — not merely that ssh connected.
pub async fn open(
    dial: &Dial,
    target: AttachTarget,
    cols: u16,
    rows: u16,
    encoding: Encoding,
) -> Result<Dialed> {
    let mut dialed = match dial {
        Dial::Socket(path) => {
            let stream = connect_existing(path).await?;
            Dialed { transport: into_transport(stream, encoding), child: None }
        }
        Dial::Ssh { target, args, socket } => {
            ssh_transport(target, args, socket.as_deref(), encoding)?
        }
    };

    dialed
        .transport
        .to_server
        .send(hello(target, cols, rows, encoding))
        .map_err(|_| anyhow::anyhow!("connection closed before handshake"))?;

    let reply = tokio::time::timeout(HANDSHAKE_TIMEOUT, dialed.transport.from_server.recv())
        .await
        .map_err(|_| anyhow::anyhow!("{}", handshake_timeout_reason(&dialed)))?;

    match reply {
        Some(ServerMsg::Hello { .. }) => Ok(dialed),
        // The daemon reports a version mismatch this way before detaching, and
        // it is the one handshake failure with a useful message in it.
        Some(ServerMsg::Error(e)) => anyhow::bail!("{e}"),
        Some(other) => anyhow::bail!("unexpected first message: {other:?}"),
        None => anyhow::bail!("{}", closed_reason(&dialed)),
    }
}

/// Why the handshake never completed. Prefers ssh's own complaint.
fn handshake_timeout_reason(dialed: &Dialed) -> String {
    match dialed.child.as_ref().map(SshChild::last_stderr) {
        Some(tail) if !tail.is_empty() => tail,
        _ => "timed out waiting for the daemon".to_string(),
    }
}

fn closed_reason(dialed: &Dialed) -> String {
    match dialed.child.as_ref().map(SshChild::last_stderr) {
        Some(tail) if !tail.is_empty() => tail,
        _ => "connection closed during handshake".to_string(),
    }
}

/// Spawn `ssh <args> <target> butai proxy` and wrap its stdio as a transport.
pub fn ssh_transport(
    target: &str,
    args: &[String],
    socket: Option<&std::path::Path>,
    encoding: Encoding,
) -> Result<Dialed> {
    let mut cmd = Command::new("ssh");
    // No tty: we speak a binary protocol over this stdio, and a pty would both
    // echo and translate it. It also suppresses the login MOTD.
    cmd.arg("-T");
    for opt in control_master_opts() {
        cmd.arg("-o").arg(opt);
    }
    cmd.args(args);
    cmd.arg(target);
    match socket {
        Some(path) => cmd.arg(format!("BUTAI_SOCKET={} {}", shell_quote(path), remote_cmd())),
        None => cmd.arg(remote_cmd()),
    };
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);

    let mut child = cmd.spawn().context("spawn ssh")?;
    let stdin = child.stdin.take().context("ssh stdin")?;
    let stdout = child.stdout.take().context("ssh stdout")?;
    let stderr = child.stderr.take().context("ssh stderr")?;

    let tail = Arc::new(Mutex::new(String::new()));
    spawn_stderr_pump(stderr, Arc::clone(&tail), target.to_string());

    // Two pipes rejoined into the one duplex `Framed` wants. This is the whole
    // reason `into_transport` is generic over its stream.
    let duplex = tokio::io::join(stdout, stdin);
    Ok(Dialed {
        transport: into_transport(duplex, encoding),
        child: Some(SshChild { child, stderr: tail }),
    })
}

/// Share one ssh connection across the several channels a host opens. Without
/// this, every workspace you switch to pays a full key exchange.
///
/// Public because the client opens ssh connections of its own that want the
/// same master: connecting a machine from the host picker asks it where its
/// daemon listens and then forwards that socket, which is two `ssh` runs to one
/// host back to back — the second should not repeat the first's key exchange.
///
/// `%C` rather than `%r@%h:%p` because a control socket is still a Unix socket
/// and still bound by the ~104-byte `sun_path` limit; the hash form is fixed
/// width no matter how long the destination is.
///
/// **The keepalives are here rather than in their own function because they
/// belong to whichever connection becomes the master.** A laptop that sleeps or
/// changes network leaves TCP half-open: nothing is delivered, nothing is
/// refused, and ssh notices only when the kernel gives up — which can be hours.
/// The `-N` forward is long-lived, so it is usually the master, and a wedged
/// master is worse than a dead one: every later `ssh` multiplexes onto it and
/// hangs too, so even a deliberate reconnect could not get out. Three missed
/// 15s probes end it in about 45 seconds, the `ControlPath` socket goes with
/// it, and the next dial is a clean connection.
pub fn control_master_opts() -> Vec<String> {
    let dir = butai_protocol::paths::butai_dir();
    // Best-effort: without the directory ssh just fails to make the control
    // socket and each channel opens its own connection, which still works.
    let _ = std::fs::create_dir_all(&dir);
    vec![
        "ControlMaster=auto".to_string(),
        format!("ControlPath={}/ssh-%C", dir.display()),
        "ControlPersist=60".to_string(),
        "ServerAliveInterval=15".to_string(),
        "ServerAliveCountMax=3".to_string(),
    ]
}

/// Log ssh's stderr and keep the last line for the disconnected panel.
fn spawn_stderr_pump(
    stderr: tokio::process::ChildStderr,
    tail: Arc<Mutex<String>>,
    target: String,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            tracing::warn!(host = %target, "ssh: {line}");
            if let Ok(mut tail) = tail.lock() {
                *tail = line;
            }
        }
    });
}

/// Single-quote a path for the far host's shell.
fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_label_is_the_destination() {
        let d = Dial::Ssh { target: "gpu-box".into(), args: vec![], socket: None };
        assert_eq!(d.label(), "gpu-box");
    }

    #[test]
    fn socket_label_is_the_file_stem() {
        assert_eq!(Dial::Socket("/tmp/gpu-box.sock".into()).label(), "gpu-box");
    }

    #[test]
    fn quoting_survives_an_apostrophe() {
        assert_eq!(shell_quote(std::path::Path::new("/tmp/a")), "'/tmp/a'");
        assert_eq!(shell_quote(std::path::Path::new("/tmp/it's")), r"'/tmp/it'\''s'");
    }

    #[test]
    fn control_path_stays_under_the_sockaddr_limit() {
        // `%C` is a 40-char hash, so the bound is the directory plus "/ssh-".
        let opts = control_master_opts();
        let path = opts.iter().find(|o| o.starts_with("ControlPath=")).expect("control path");
        assert!(path.len() - "ControlPath=".len() + 40 < 104, "{path} is too long");
    }

    /// A half-open link must end itself. Without these a slept laptop leaves a
    /// master that is neither alive nor dead, and every later ssh — including
    /// the reconnect that would have fixed it — multiplexes onto it and hangs.
    #[test]
    fn every_ssh_carries_keepalives() {
        let opts = control_master_opts();
        assert!(opts.iter().any(|o| o == "ServerAliveInterval=15"), "{opts:?}");
        assert!(opts.iter().any(|o| o == "ServerAliveCountMax=3"), "{opts:?}");
    }

    /// The link must give up well inside the re-dial backoff's first step, or
    /// the first attempt is spent waiting on a connection that is already gone.
    #[test]
    fn the_keepalive_budget_is_under_a_minute() {
        let opts = control_master_opts();
        let num = |key: &str| -> u64 {
            opts.iter()
                .find_map(|o| o.strip_prefix(key))
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| panic!("no {key} in {opts:?}"))
        };
        let budget = num("ServerAliveInterval=") * num("ServerAliveCountMax=");
        assert!(budget <= 60, "a dead link takes {budget}s to notice");
    }
}
