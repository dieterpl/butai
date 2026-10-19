//! Letting ssh ask the workbench its questions.
//!
//! Every ssh the client starts runs with no terminal: stdin is null or a pipe,
//! and the real tty belongs to the workbench in raw mode. ssh therefore cannot
//! ask whether to trust a new host, or for a passphrase, or for a one-time
//! code — which is why those ran with `BatchMode=yes` and simply failed with
//! `Host key verification failed`.
//!
//! OpenSSH has a hook for exactly this: when `SSH_ASKPASS` names a program and
//! `SSH_ASKPASS_REQUIRE=force`, every prompt is put to that program instead of
//! a tty, as its first argument, and whatever it prints is the answer. The
//! program here is `butai askpass`, a few lines that forward the question over
//! a private unix socket to the workbench, which shows it as a
//! [`crate::chrome::DialogOverlay`] and sends the typed answer back.
//!
//! The socket lives in a directory only this user can enter, and the answer
//! crosses it once and is dropped; nothing here logs a prompt's reply.

use anyhow::{Context, Result};
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};

/// What sort of answer ssh wants, from `SSH_ASKPASS_PROMPT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskKind {
    /// A secret or a code: the default when ssh says nothing.
    Secret,
    /// A yes/no, such as trusting a new host key.
    Confirm,
    /// Something to read, with nothing to answer.
    Notice,
}

impl AskKind {
    pub fn from_env(value: Option<&str>) -> Self {
        match value {
            Some("confirm") => Self::Confirm,
            Some("none") => Self::Notice,
            _ => Self::Secret,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::Confirm => "confirm",
            Self::Notice => "notice",
        }
    }

    fn parse(name: &str) -> Self {
        match name {
            "confirm" => Self::Confirm,
            "notice" => Self::Notice,
            _ => Self::Secret,
        }
    }
}

/// One question from one ssh, waiting for an answer.
///
/// Dropping `reply` without sending is "cancelled": the helper exits non-zero
/// and ssh gives up on that prompt.
#[derive(Debug)]
pub struct AskRequest {
    pub prompt: String,
    pub kind: AskKind,
    pub reply: oneshot::Sender<Option<String>>,
}

/// The environment that makes an ssh ask the workbench instead of a tty.
#[derive(Debug, Clone)]
pub struct AskpassEnv {
    script: PathBuf,
    socket: PathBuf,
}

impl AskpassEnv {
    /// Point `cmd`'s ssh at the helper.
    pub fn apply(&self, cmd: &mut tokio::process::Command) {
        cmd.env("SSH_ASKPASS", &self.script)
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("BUTAI_ASKPASS_SOCK", &self.socket);
        // OpenSSH before 8.4 ignores `SSH_ASKPASS_REQUIRE` and only runs the
        // helper when `DISPLAY` is set and there is no tty. Any value will do.
        if std::env::var_os("DISPLAY").is_none() {
            cmd.env("DISPLAY", "butai");
        }
        // Without a controlling terminal ssh has nowhere to ask but the helper.
        // `SSH_ASKPASS_REQUIRE` says the same on 8.4 and later; older releases
        // open `/dev/tty` first whenever there is one, which here is the
        // workbench's own screen in raw mode.
        #[cfg(unix)]
        // SAFETY: `setsid` is async-signal-safe and touches no memory of ours;
        // it runs between fork and exec, where that is all that is allowed.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
}

/// The listening half: owns the socket, and yields what ssh asks.
pub struct Askpass {
    pub env: AskpassEnv,
    pub requests: mpsc::UnboundedReceiver<AskRequest>,
    _dir: tempfile::TempDir,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Askpass {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(unix)]
impl Askpass {
    /// Bind the socket and write the helper script. Must run inside a tokio
    /// runtime.
    pub fn start() -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;

        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("butai-forwards");
        std::fs::create_dir_all(&base).with_context(|| format!("create {}", base.display()))?;
        let dir = tempfile::Builder::new().prefix("ask-").tempdir_in(&base)?;
        // Private to this user: the answers to ssh's questions pass through it.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;

        let exe = std::env::current_exe().context("find butai's own path")?;
        let script = dir.path().join("p");
        let quoted = exe.to_string_lossy().replace('\'', r"'\''");
        std::fs::write(&script, format!("#!/bin/sh\nexec '{quoted}' askpass \"$@\"\n"))?;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;

        let socket = dir.path().join("s");
        let listener = tokio::net::UnixListener::bind(&socket)
            .with_context(|| format!("bind {}", socket.display()))?;
        let (tx, requests) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, tx.clone()));
            }
        });
        Ok(Self { env: AskpassEnv { script, socket }, requests, _dir: dir, task })
    }
}

#[cfg(not(unix))]
impl Askpass {
    /// ssh on Windows prompts through its own console and cannot be redirected
    /// this way; those dials stay non-interactive.
    pub fn start() -> Result<Self> {
        anyhow::bail!("ssh prompts are only forwarded on unix")
    }
}

/// One helper connection: read its question, wait for the workbench's answer,
/// write it back.
#[cfg(unix)]
async fn serve(stream: tokio::net::UnixStream, tx: mpsc::UnboundedSender<AskRequest>) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let (read, mut write) = stream.into_split();
    // A prompt is a few lines; the cap is for a stray process on the socket.
    let mut line = String::new();
    if BufReader::new(read.take(64 * 1024)).read_line(&mut line).await.is_err() {
        return;
    }
    let Ok(wire) = serde_json::from_str::<serde_json::Value>(&line) else { return };
    let prompt = wire.get("prompt").and_then(|p| p.as_str()).unwrap_or_default().to_string();
    let kind = AskKind::parse(wire.get("kind").and_then(|k| k.as_str()).unwrap_or_default());

    let (reply, answer) = oneshot::channel();
    let answer = if tx.send(AskRequest { prompt, kind, reply }).is_ok() {
        answer.await.ok().flatten()
    } else {
        None
    };
    let body = match answer {
        Some(a) => serde_json::json!({ "answer": a }),
        None => serde_json::json!({ "cancel": true }),
    };
    let _ = write.write_all(format!("{body}\n").as_bytes()).await;
}

/// The helper's side: put `prompt` to the workbench and wait.
///
/// `Ok(None)` is a cancel. Blocking and synchronous on purpose — it runs in a
/// process that exists for this one question, and pulling a runtime in for a
/// socket round trip would be most of its start-up time.
#[cfg(unix)]
pub fn ask(prompt: &str) -> Result<Option<String>> {
    use std::io::{BufRead, BufReader, Write};

    let path = std::env::var_os("BUTAI_ASKPASS_SOCK")
        .context("BUTAI_ASKPASS_SOCK is not set — `butai askpass` is run by ssh, not by hand")?;
    let mut stream = std::os::unix::net::UnixStream::connect(&path)
        .with_context(|| format!("connect {}", PathBuf::from(&path).display()))?;
    let kind = AskKind::from_env(std::env::var("SSH_ASKPASS_PROMPT").ok().as_deref());
    let wire = serde_json::json!({ "prompt": prompt, "kind": kind.name() });
    writeln!(stream, "{wire}")?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    let reply: serde_json::Value = serde_json::from_str(&reply).context("the workbench's reply")?;
    Ok(reply.get("answer").and_then(|a| a.as_str()).map(str::to_string))
}

#[cfg(not(unix))]
pub fn ask(_prompt: &str) -> Result<Option<String>> {
    anyhow::bail!("ssh prompts are only forwarded on unix")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_question_round_trips_through_the_socket() {
        let mut server = Askpass::start().unwrap();
        let sock = server.env.socket.clone();
        let helper = tokio::task::spawn_blocking(move || {
            use std::io::{BufRead, BufReader, Write};
            let mut s = std::os::unix::net::UnixStream::connect(sock).unwrap();
            writeln!(s, r#"{{"prompt":"user@h's password: ","kind":"secret"}}"#).unwrap();
            let mut line = String::new();
            BufReader::new(s).read_line(&mut line).unwrap();
            line
        });
        let req = server.requests.recv().await.unwrap();
        assert_eq!(req.prompt, "user@h's password: ");
        assert_eq!(req.kind, AskKind::Secret);
        req.reply.send(Some("hunter2".into())).unwrap();
        assert_eq!(helper.await.unwrap().trim(), r#"{"answer":"hunter2"}"#);
    }

    #[tokio::test]
    async fn dropping_the_reply_cancels() {
        let mut server = Askpass::start().unwrap();
        let sock = server.env.socket.clone();
        let helper = tokio::task::spawn_blocking(move || {
            use std::io::{BufRead, BufReader, Write};
            let mut s = std::os::unix::net::UnixStream::connect(sock).unwrap();
            writeln!(s, r#"{{"prompt":"Continue?","kind":"confirm"}}"#).unwrap();
            let mut line = String::new();
            BufReader::new(s).read_line(&mut line).unwrap();
            line
        });
        drop(server.requests.recv().await.unwrap());
        assert_eq!(helper.await.unwrap().trim(), r#"{"cancel":true}"#);
    }

    /// The whole chain with nothing faked but the workbench: a real `ssh` to a
    /// throwaway `sshd`, asking about a host it has never seen, through the real
    /// `butai askpass`. Run by hand — it needs `sshd` and a built `butai`:
    /// `cargo build -p butai && cargo test -p butai-client -- --ignored askpass`
    #[tokio::test]
    #[ignore = "needs sshd and a built butai binary"]
    async fn a_real_ssh_asks_about_an_unknown_host_through_the_helper() {
        use std::process::Stdio;
        let butai = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/butai");
        assert!(butai.exists(), "build butai first");
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        let keygen = |name: &str| {
            let ok = std::process::Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                .arg(p(name))
                .status()
                .unwrap()
                .success();
            assert!(ok);
        };
        keygen("host");
        keygen("client");
        std::fs::copy(p("client.pub"), p("authorized_keys")).unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        std::fs::write(
            p("sshd_config"),
            format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nPidFile {}\n\
                 AuthorizedKeysFile {}\nPasswordAuthentication no\nStrictModes no\nUsePAM no\n",
                p("host").display(),
                p("sshd.pid").display(),
                p("authorized_keys").display()
            ),
        )
        .unwrap();
        let mut sshd = tokio::process::Command::new("/usr/sbin/sshd")
            .arg("-D")
            .arg("-f")
            .arg(p("sshd_config"))
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;

        let mut server = Askpass::start().unwrap();
        // The helper is `current_exe askpass`, and the current exe is this test.
        std::fs::write(
            &server.env.script,
            format!(
                "#!/bin/sh\nexec '{}' askpass \"$@\"\n",
                butai.canonicalize().unwrap().display()
            ),
        )
        .unwrap();
        let mut cmd = tokio::process::Command::new("ssh");
        server.env.apply(&mut cmd);
        cmd.args(["-T", "-i"])
            .arg(p("client"))
            .arg("-o")
            .arg(format!("UserKnownHostsFile={}", p("known").display()))
            .args(["-o", "IdentitiesOnly=yes", "-p", &port.to_string(), "127.0.0.1", "echo hi"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = cmd.spawn().unwrap();

        let req = tokio::time::timeout(Duration::from_secs(10), server.requests.recv())
            .await
            .expect("ssh never asked")
            .unwrap();
        assert!(req.prompt.contains("authenticity of host"), "{}", req.prompt);
        let d = crate::ssh_prompts::ask_dialog(1, &req.prompt, req.kind, false);
        assert_eq!(d.title, "Unknown host");
        assert!(d.input.is_none());
        req.reply.send(Some("yes".into())).unwrap();

        let out = child.wait_with_output().await.unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hi", "{out:?}");
        assert!(std::fs::read_to_string(p("known")).unwrap().contains("ssh-ed25519"));
        sshd.kill().await.ok();
    }

    #[test]
    fn the_prompt_kind_comes_from_ssh() {
        assert_eq!(AskKind::from_env(Some("confirm")), AskKind::Confirm);
        assert_eq!(AskKind::from_env(Some("none")), AskKind::Notice);
        assert_eq!(AskKind::from_env(None), AskKind::Secret);
    }
}
