//! End-to-end tests over a real Unix socket: the daemon accept loop, JSON
//! framing, handshake, detach/reattach, and kill-server — driven exactly
//! the way a third-party client would.
//!
//! Anything that reads a screen reads it through a [`AttachTarget::Pane`]
//! connection, because a pane is the only thing the daemon draws. The workbench
//! around it is JSON on `/v1/*` and every client composes its own, so a test
//! that wanted to see a rail would be asserting on a picture no client is sent.

use std::path::PathBuf;
use std::time::Duration;

use butai_protocol::framing::{decode, encode, length_codec, MAX_CONSECUTIVE_BAD_FRAMES};
use butai_protocol::{
    AttachTarget, ClientMsg, Command, Encoding, FrameUpdate, InputEvent, KeyCode, KeyEvent, PaneId,
    ServerMsg, PROTOCOL_VERSION,
};
use butai_server::config::Config;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::codec::Framed;

type Client = Framed<UnixStream, tokio_util::codec::LengthDelimitedCodec>;

async fn start_daemon(tmp: &tempfile::TempDir) -> PathBuf {
    let socket = tmp.path().join("butai.sock");
    start_daemon_with_store(&socket, None).await;
    socket
}

/// Start a daemon on a specific socket with an explicit session-store path,
/// so a test can restart it (fresh socket, same store) to exercise restore.
async fn start_daemon_with_store(socket: &std::path::Path, store: Option<PathBuf>) {
    let listener = UnixListener::bind(socket).unwrap();
    let mut config = Config::with_defaults();
    config.general.default_shell = Some("/bin/sh".into());
    tokio::spawn(butai_server::daemon::serve(listener, config, store));
}

/// A daemon whose only configured agent is a plain shell, so restore can be
/// exercised without one of the real agent CLIs installed on the machine.
async fn start_daemon_with_agent(socket: &std::path::Path, store: PathBuf) {
    // No resume flag: `sh` has nothing to reopen. The pane still comes back
    // painted, which is the half that test asserts on.
    start_daemon_with_agent_args(socket, store, &[], &[]).await
}

/// The same, with the launcher's argv spelled out — so a test can stand in for
/// a real agent CLI's session flags with a shell script that echoes what it was
/// given.
async fn start_daemon_with_agent_args(
    socket: &std::path::Path,
    store: PathBuf,
    args: &[&str],
    resume_args: &[&str],
) {
    let listener = UnixListener::bind(socket).unwrap();
    let mut config = Config::with_defaults();
    config.general.default_shell = Some("/bin/sh".into());
    config.agents.clear();
    config.agents.push(butai_server::config::AgentDef {
        name: "sh".into(),
        command: "/bin/sh".into(),
        args: args.iter().map(|s| s.to_string()).collect(),
        resume_args: resume_args.iter().map(|s| s.to_string()).collect(),
        env: Default::default(),
        waiting_pattern: None,
        busy_pattern: None,
    });
    tokio::spawn(butai_server::daemon::serve(listener, config, Some(store)));
}

/// Every `session_id` the store recorded, in workspace/agent order.
fn session_ids(saved: &str) -> Vec<String> {
    const KEY: &str = "\"session_id\": \"";
    saved
        .match_indices(KEY)
        .map(|(i, _)| {
            let rest = &saved[i + KEY.len()..];
            rest[..rest.find('"').expect("terminated string")].to_string()
        })
        .collect()
}

async fn connect(socket: &PathBuf) -> Client {
    let stream = UnixStream::connect(socket).await.unwrap();
    Framed::new(stream, length_codec())
}

async fn send(client: &mut Client, msg: &ClientMsg) {
    client.send(encode(msg, Encoding::Json).unwrap()).await.unwrap();
}

async fn recv(client: &mut Client) -> ServerMsg {
    let frame = tokio::time::timeout(Duration::from_secs(10), client.next())
        .await
        .expect("timed out waiting for server message")
        .expect("server closed connection")
        .expect("read error");
    decode(&frame, Encoding::Json).unwrap()
}

fn hello(target: AttachTarget) -> ClientMsg {
    ClientMsg::Hello {
        proto_version: PROTOCOL_VERSION,
        encoding: Encoding::Json,
        cols: 80,
        rows: 24,
        target,
        cwd: PathBuf::from("/"),
    }
}

/// Apply frame updates to a text grid so tests can assert on screen content.
///
/// One pane, at the size the connection asked for — the daemon resizes the pane
/// to the client that is streaming it.
struct Screen {
    grid: Vec<Vec<char>>,
}

impl Screen {
    fn new() -> Self {
        Self { grid: vec![vec![' '; 80]; 24] }
    }

    fn apply(&mut self, f: &FrameUpdate) {
        let (cols, rows) = (self.grid[0].len(), self.grid.len());
        for run in &f.cells {
            for (i, cell) in run.cells.iter().enumerate() {
                let (x, y) = (run.x as usize + i, run.y as usize);
                if y < rows && x < cols {
                    self.grid[y][x] = cell.ch.chars().next().unwrap_or(' ');
                }
            }
        }
    }

    fn text(&self) -> String {
        self.grid.iter().map(|r| r.iter().collect::<String>()).collect::<Vec<_>>().join("\n")
    }
}

async fn await_text(client: &mut Client, screen: &mut Screen, needle: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if screen.text().contains(needle) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never saw {needle:?}; screen:\n{}",
            screen.text()
        );
        if let ServerMsg::Frame(f) = recv(client).await {
            screen.apply(&f);
        }
    }
}

/// One HTTP GET on the same socket the framed protocol uses.
///
/// A socket test needs a little REST: *which* pane a workspace is showing is a
/// question about state, and state is JSON. The daemon sniffs the first byte to
/// tell the two apart, so this is the same socket rather than a second surface.
async fn http_get(socket: &PathBuf, path: &str) -> String {
    let mut stream = UnixStream::connect(socket).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: butai\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("http read timed out")
        .unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    text.split("\r\n\r\n").nth(1).unwrap_or("").to_string()
}

/// The pane a workspace has on its stage, once it is one this test has not seen.
///
/// Polled, and past a pane rather than merely for one, because both ends race:
/// a workspace exists before the shell it opens with does, and a spawn moves the
/// stage a moment after the command that asked for it returns. `previous` is
/// `None` the first time and the pane being left behind after that.
async fn stage_pane_past(socket: &PathBuf, ws: u32, previous: Option<PaneId>) -> PaneId {
    const KEY: &str = "\"stage\":";
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let body = http_get(socket, &format!("/v1/workspaces/{ws}")).await;
        let staged: Option<PaneId> = body.split(KEY).nth(1).and_then(|rest| {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok().map(PaneId)
        });
        if let Some(pane) = staged.filter(|p| Some(*p) != previous) {
            return pane;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "workspace {ws} never staged a pane past {previous:?}; body: {body}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The pane a workspace has on its stage.
async fn stage_pane(socket: &PathBuf, ws: u32) -> PaneId {
    stage_pane_past(socket, ws, None).await
}

/// A connection streaming one pane, with the grid its frames paint into.
///
/// This is what every client does with a pane — the web stage, both native
/// apps, and since the TUI draws its own chrome, that too. A test that wants to
/// see a program's output asks for the program's pane.
async fn watch_pane(socket: &PathBuf, pane: PaneId) -> (Client, Screen) {
    let mut c = connect(socket).await;
    send(&mut c, &hello(AttachTarget::Pane { pane })).await;
    let first = recv(&mut c).await;
    let ServerMsg::Hello { .. } = first else { panic!("expected hello, got {first:?}") };
    (c, Screen::new())
}

/// Watch whichever pane the workspace has on its stage *now*.
///
/// Not `stage_pane` followed by [`watch_pane`]: those are two round trips, and
/// a daemon that is still restoring can replace the staged pane between them —
/// an agent whose conversation will not reopen is torn down and started fresh,
/// which retires the pane id the first call just read. Attaching to a pane that
/// has since gone is answered with `Error` + `Detached` rather than a hello
/// (`core.rs`, `AttachTarget::Pane`), so the stale id surfaces as "expected
/// hello" three frames away from the cause.
///
/// Re-reading the stage on that answer keeps the race out of the assertion. It
/// cannot mask a broken restart: the workspace has to end up with a live staged
/// pane within the deadline, and the caller still asserts on what that pane
/// paints.
/// Like [`recv`], but `None` when the connection ends instead of a panic — a
/// pane being replaced closes its viewers, which is a thing to react to rather
/// than fail on.
async fn recv_opt(client: &mut Client) -> Option<ServerMsg> {
    let next = tokio::time::timeout(Duration::from_secs(10), client.next()).await.ok()?;
    let frame = next?.ok()?;
    Some(decode(&frame, Encoding::Json).unwrap())
}

/// Wait for `needle` on whatever the workspace is showing, following the stage
/// across a pane replacement.
///
/// A restart can retire the pane twice over. The id read before the attach goes
/// stale — [`watch_stage_pane`] covers that — and the pane attached to can still
/// be torn down afterwards, because an agent whose resume fails is given one
/// fresh start and the daemon detaches the viewers of the pane it replaces. The
/// viewer then sees `Detached` and EOF, which surfaced as "server closed
/// connection" raised from inside [`await_text`], nowhere near the restart that
/// caused it.
///
/// Re-attaching is what a real client does when the stage moves, so this follows
/// the behaviour under test rather than working around it: the workspace still
/// has to paint `needle` before the deadline or this fails.
///
/// Returns everything painted, every pane it watched and not just the last —
/// a caller asserting that some text never appeared would otherwise go blind
/// exactly when a pane is replaced, which is the case it most needs to see.
async fn await_stage_text(socket: &PathBuf, ws: u32, needle: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut seen = String::new();
    while tokio::time::Instant::now() < deadline {
        let (mut c, mut screen) = watch_stage_pane(socket, ws).await;
        loop {
            if screen.text().contains(needle) {
                seen.push_str(&screen.text());
                return seen;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            match recv_opt(&mut c).await {
                Some(ServerMsg::Frame(f)) => screen.apply(&f),
                // Detached or closed: the pane went away, so go back to the
                // stage and pick up whatever replaced it.
                Some(ServerMsg::Detached { .. }) | None => break,
                Some(_) => {}
            }
        }
        seen.push_str(&screen.text());
        seen.push('\n');
    }
    panic!("workspace {ws} never painted {needle:?}; everything seen:\n{seen}");
}

async fn watch_stage_pane(socket: &PathBuf, ws: u32) -> (Client, Screen) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let pane = stage_pane(socket, ws).await;
        let mut c = connect(socket).await;
        send(&mut c, &hello(AttachTarget::Pane { pane })).await;
        match recv(&mut c).await {
            ServerMsg::Hello { .. } => return (c, Screen::new()),
            ServerMsg::Error(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            other => panic!("attaching to staged pane {pane} of workspace {ws}: {other:?}"),
        }
    }
}

/// Re-point an open pane connection at `pane` and start a fresh grid for it.
///
/// Spawning an agent moves the stage, so a test following the work follows the
/// stage — the same gesture `<butai-stage>`'s `setPane()` makes, and the reason
/// [`ClientMsg::Watch`] exists.
async fn watch(client: &mut Client, pane: PaneId) -> Screen {
    send(client, &ClientMsg::Watch { pane }).await;
    Screen::new()
}

fn type_line(text: &str) -> Vec<ClientMsg> {
    let mut out: Vec<ClientMsg> =
        text.chars().map(|c| ClientMsg::Input(InputEvent::Key(KeyEvent::char(c)))).collect();
    out.push(ClientMsg::Input(InputEvent::Key(KeyEvent {
        code: KeyCode::Enter,
        mods: Default::default(),
    })));
    out
}

#[tokio::test]
async fn detach_and_reattach_preserves_screen() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    // Open the workspace, then stream the shell it came up with.
    let mut ctl = connect(&socket).await;
    send(&mut ctl, &hello(AttachTarget::New { name: Some("work".into()), layout: None })).await;
    let ServerMsg::Hello { session, .. } = recv(&mut ctl).await else {
        panic!("expected hello");
    };
    assert_eq!(session.unwrap().name, "work");
    let pane = stage_pane(&socket, 1).await;

    let (mut c1, mut screen) = watch_pane(&socket, pane).await;
    for msg in type_line("echo E2E_MARKER") {
        send(&mut c1, &msg).await;
    }
    await_text(&mut c1, &mut screen, "E2E_MARKER").await;

    // Abrupt disconnect (terminal window closed).
    drop(c1);
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Reattach: the first frame is a full one, so the marker is still there.
    let (mut c2, mut screen2) = watch_pane(&socket, pane).await;
    await_text(&mut c2, &mut screen2, "E2E_MARKER").await;

    // Explicit detach gets an acknowledgment.
    send(&mut c2, &ClientMsg::Detach).await;
    loop {
        match recv(&mut c2).await {
            ServerMsg::Detached { .. } => break,
            ServerMsg::Frame(_) => continue,
            other => panic!("unexpected: {other:?}"),
        }
    }
}

/// Two people on one pane see the same program. Each gets its own diff stream,
/// so the second one is not a replay of the first — it is rendered against what
/// *that* connection was last sent.
#[tokio::test]
async fn two_clients_mirror_one_pane() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    let mut ctl = connect(&socket).await;
    send(&mut ctl, &hello(AttachTarget::New { name: Some("dup".into()), layout: None })).await;
    recv(&mut ctl).await; // hello
    let pane = stage_pane(&socket, 1).await;

    let (mut c1, mut s1) = watch_pane(&socket, pane).await;
    let (mut c2, mut s2) = watch_pane(&socket, pane).await;

    for msg in type_line("echo MIRRORED") {
        send(&mut c1, &msg).await;
    }
    await_text(&mut c1, &mut s1, "MIRRORED").await;
    await_text(&mut c2, &mut s2, "MIRRORED").await;
}

#[tokio::test]
async fn control_client_lists_and_kills() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    let mut c1 = connect(&socket).await;
    send(&mut c1, &hello(AttachTarget::New { name: Some("victim".into()), layout: None })).await;
    recv(&mut c1).await;

    let mut ctl = connect(&socket).await;
    send(&mut ctl, &hello(AttachTarget::Control)).await;
    let ServerMsg::Hello { session, .. } = recv(&mut ctl).await else { panic!() };
    assert!(session.is_none());

    send(&mut ctl, &ClientMsg::Command(Command::ListSessions)).await;
    let ServerMsg::SessionList(list) = recv(&mut ctl).await else { panic!() };
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "victim");
    assert_eq!(list[0].attached_clients, 1);

    send(&mut ctl, &ClientMsg::Command(Command::KillSession("victim".into()))).await;
    // The attached client gets detached; the control client gets Ok.
    loop {
        match recv(&mut ctl).await {
            ServerMsg::Ok => break,
            ServerMsg::Frame(_) => continue,
            other => panic!("unexpected: {other:?}"),
        }
    }
    loop {
        match recv(&mut c1).await {
            ServerMsg::Detached { reason } => {
                assert!(reason.contains("closed"), "reason: {reason}");
                break;
            }
            ServerMsg::Frame(_) => continue,
            other => panic!("unexpected: {other:?}"),
        }
    }
}

/// A pane flooding output at full tilt must not starve control commands.
/// Regression for the incident where a high-output pane (e.g. `docker logs -f`
/// on a chatty container) pinned the daemon at 100% CPU and left `kill-server`
/// unresponsive: PTY output now rides a bounded channel with backpressure and
/// control events are serviced first, so list/kill-server stay prompt.
// Multi-threaded like the real daemon (`new_multi_thread`), so the core loop and
// client I/O run on separate workers — the setting under which the incident and
// the fix are meaningful.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_stays_responsive_under_pty_flood() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    // A workspace whose stage shell floods stdout as fast as the OS allows.
    let mut opener = connect(&socket).await;
    send(&mut opener, &hello(AttachTarget::New { name: Some("flood".into()), layout: None })).await;
    let ServerMsg::Hello { session, .. } = recv(&mut opener).await else { panic!() };
    assert_eq!(session.unwrap().name, "flood");
    let pane = stage_pane(&socket, 1).await;

    let (mut c1, mut screen) = watch_pane(&socket, pane).await;
    for msg in type_line("yes FLOODING") {
        send(&mut c1, &msg).await;
    }

    // Confirm the flood command is running (its marker reached a frame) before we
    // measure — otherwise the test could pass without ever exercising the path.
    await_text(&mut c1, &mut screen, "FLOODING").await;

    // Keep this client's frame queue drained in the background so the thing we
    // measure is the daemon's own responsiveness, not our read speed.
    tokio::spawn(async move { while c1.next().await.is_some() {} });

    // A separate control client must still get a prompt answer. The fix services
    // control on the next loop turn (~one frame), so a 1s bound is ~60x the
    // expected latency yet still fails loudly if control is ever starved again.
    let mut ctl = connect(&socket).await;
    send(&mut ctl, &hello(AttachTarget::Control)).await;
    let ServerMsg::Hello { .. } = recv(&mut ctl).await else { panic!() };

    send(&mut ctl, &ClientMsg::Command(Command::ListSessions)).await;
    let list = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let ServerMsg::SessionList(list) = recv(&mut ctl).await {
                return list;
            }
        }
    })
    .await
    .expect("ListSessions starved by PTY flood");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "flood");

    // kill-server — the reported-broken path — must also complete promptly. It
    // detaches every client, so the control client sees a Detached.
    send(&mut ctl, &ClientMsg::Command(Command::KillServer)).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let ServerMsg::Detached { .. } = recv(&mut ctl).await {
                return;
            }
        }
    })
    .await
    .expect("kill-server starved by PTY flood");
}

/// Workspaces open in one daemon come back when a fresh daemon starts against
/// the same session store — the reboot-persistence contract.
#[tokio::test]
async fn workspaces_persist_across_daemon_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("session.json");
    // A real project directory to (re)open a workspace for.
    let proj = tmp.path().join("proj");
    std::fs::create_dir(&proj).unwrap();

    // Daemon #1: opening `butai` in `proj` creates that folder's workspace.
    let socket1 = tmp.path().join("butai1.sock");
    start_daemon_with_store(&socket1, Some(store.clone())).await;
    let mut c1 = connect(&socket1).await;
    send(
        &mut c1,
        &ClientMsg::Hello {
            proto_version: PROTOCOL_VERSION,
            encoding: Encoding::Json,
            cols: 80,
            rows: 24,
            target: AttachTarget::Default,
            cwd: proj.clone(),
        },
    )
    .await;
    loop {
        match recv(&mut c1).await {
            ServerMsg::Hello { session, .. } => {
                assert!(session.is_some(), "landed on a workspace");
                break;
            }
            _ => continue,
        }
    }

    // The store now records the open workspace by directory.
    let saved = std::fs::read_to_string(&store).expect("session file written");
    assert!(saved.contains("proj"), "session file names the project dir: {saved}");

    // Simulate a reboot: abandon the client and start a brand-new daemon on a
    // fresh socket but the SAME store.
    drop(c1);
    let socket2 = tmp.path().join("butai2.sock");
    start_daemon_with_store(&socket2, Some(store.clone())).await;

    // A control client sees the restored workspace, named for its directory.
    let mut ctl = connect(&socket2).await;
    send(&mut ctl, &hello(AttachTarget::Control)).await;
    let ServerMsg::Hello { .. } = recv(&mut ctl).await else { panic!() };
    send(&mut ctl, &ClientMsg::Command(Command::ListSessions)).await;
    let ServerMsg::SessionList(list) = recv(&mut ctl).await else { panic!() };
    assert_eq!(list.len(), 1, "one workspace restored");
    assert_eq!(list[0].name, "proj", "restored under its directory name");
}

/// The other half of restart restore: not just *that* a workspace comes back
/// but that the work in it does. An agent started by hand is respawned, and
/// both it and the shell come back showing the output they had on screen.
#[tokio::test]
async fn agents_and_scrollback_come_back_across_daemon_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("session.json");
    let proj = tmp.path().join("proj");
    std::fs::create_dir(&proj).unwrap();

    let socket1 = tmp.path().join("butai1.sock");
    start_daemon_with_agent(&socket1, store.clone()).await;
    let mut c1 = connect(&socket1).await;
    send(
        &mut c1,
        &ClientMsg::Hello {
            proto_version: PROTOCOL_VERSION,
            encoding: Encoding::Json,
            cols: 80,
            rows: 24,
            target: AttachTarget::Default,
            cwd: proj.clone(),
        },
    )
    .await;
    loop {
        match recv(&mut c1).await {
            ServerMsg::Hello { session, .. } => {
                assert!(session.is_some(), "landed on a workspace");
                break;
            }
            _ => continue,
        }
    }

    // Work in the shell the workspace opens with...
    let shell = stage_pane(&socket1, 1).await;
    let (mut watcher, mut screen) = watch_pane(&socket1, shell).await;
    for msg in type_line("echo SHELL_MARKER") {
        send(&mut watcher, &msg).await;
    }
    await_text(&mut watcher, &mut screen, "SHELL_MARKER").await;

    // ...then start an agent by hand (not from `.butai.toml`, so only the
    // session store can know about it) and work in that too. Spawning takes the
    // stage, so following the work means following the stage.
    send(&mut c1, &ClientMsg::Command(Command::SpawnAgent("sh".into()))).await;
    let agent = stage_pane_past(&socket1, 1, Some(shell)).await;
    let mut screen = watch(&mut watcher, agent).await;
    for msg in type_line("echo AGENT_MARKER") {
        send(&mut watcher, &msg).await;
    }
    await_text(&mut watcher, &mut screen, "AGENT_MARKER").await;

    // Graceful shutdown: the daemon takes its final snapshot on the way down.
    send(&mut c1, &ClientMsg::Command(Command::KillServer)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(c1);

    // The store records the agent by its launcher name, and the dumps landed
    // beside it rather than in the user's real `~/.butai`.
    let saved = std::fs::read_to_string(&store).expect("session file written");
    assert!(saved.contains("\"agent\": \"sh\""), "agent recorded: {saved}");
    assert!(tmp.path().join("panes").is_dir(), "dumps written beside the store");

    // Reboot onto the same store.
    let socket2 = tmp.path().join("butai2.sock");
    start_daemon_with_agent(&socket2, store.clone()).await;
    let mut c2 = connect(&socket2).await;
    send(
        &mut c2,
        &ClientMsg::Hello {
            proto_version: PROTOCOL_VERSION,
            encoding: Encoding::Json,
            cols: 80,
            rows: 24,
            target: AttachTarget::Default,
            cwd: proj.clone(),
        },
    )
    .await;
    loop {
        match recv(&mut c2).await {
            ServerMsg::Hello { session, .. } => {
                assert!(session.is_some(), "landed on the restored workspace");
                break;
            }
            _ => continue,
        }
    }

    // The agent held the stage when the daemon went down, so that is what the
    // restored workspace is showing — with its transcript replayed into it.
    drop(c2);
    await_stage_text(&socket2, 1, "AGENT_MARKER").await;
}

/// Open a workspace on `proj` and return the client attached to it.
async fn attach_to(socket: &PathBuf, proj: &std::path::Path) -> Client {
    let mut c = connect(socket).await;
    send(
        &mut c,
        &ClientMsg::Hello {
            proto_version: PROTOCOL_VERSION,
            encoding: Encoding::Json,
            cols: 80,
            rows: 24,
            target: AttachTarget::Default,
            cwd: proj.to_path_buf(),
        },
    )
    .await;
    loop {
        match recv(&mut c).await {
            ServerMsg::Hello { session, .. } => {
                assert!(session.is_some(), "landed on a workspace");
                return c;
            }
            _ => continue,
        }
    }
}

/// The regression test for the bug this whole mechanism exists to fix.
///
/// Every resume flag a CLI offers by default — `claude --continue`,
/// `gemini --resume latest` — means "the most recent conversation *in this
/// directory*". Two agents in one workspace is butai's normal case, and it is
/// exactly where that is ambiguous: both would reopen the same transcript.
/// So each pane gets a conversation of its own, and keeps it across a restart.
#[tokio::test]
async fn each_agent_keeps_its_own_conversation_across_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("session.json");
    let proj = tmp.path().join("proj");
    std::fs::create_dir(&proj).unwrap();

    // A stand-in for a real agent CLI: it prints the conversation it was told
    // to open and then stays alive, so the id butai chose is visible on screen.
    let args = ["-c", "echo OPENED {session_id}; exec cat"];
    let resume_args = ["-c", "echo REOPENED {session_id}; exec cat"];

    let socket1 = tmp.path().join("butai1.sock");
    start_daemon_with_agent_args(&socket1, store.clone(), &args, &resume_args).await;
    let mut c1 = attach_to(&socket1, &proj).await;

    let shell = stage_pane(&socket1, 1).await;
    let (mut watcher, _) = watch_pane(&socket1, shell).await;

    send(&mut c1, &ClientMsg::Command(Command::SpawnAgent("sh".into()))).await;
    let first = stage_pane_past(&socket1, 1, Some(shell)).await;
    let mut screen = watch(&mut watcher, first).await;
    await_text(&mut watcher, &mut screen, "OPENED ").await;
    // Speak to it. A conversation is only written once the agent is addressed,
    // so this is what makes the id worth reopening — an agent that was never
    // typed into is deliberately restored fresh instead (see `AgentMeta::spoke`).
    for msg in type_line("hello") {
        send(&mut watcher, &msg).await;
    }

    send(&mut c1, &ClientMsg::Command(Command::SpawnAgent("sh".into()))).await;
    let second = stage_pane_past(&socket1, 1, Some(first)).await;
    let mut screen = watch(&mut watcher, second).await;
    await_text(&mut watcher, &mut screen, "OPENED ").await;
    for msg in type_line("hello") {
        send(&mut watcher, &msg).await;
    }
    // Let the second `hello` reach the agent before the daemon is told to stop:
    // it is what marks the agent spoken to, and the store below is asserted on.
    tokio::time::sleep(Duration::from_millis(500)).await;

    send(&mut c1, &ClientMsg::Command(Command::KillServer)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(c1);

    let saved = std::fs::read_to_string(&store).expect("session file written");
    let ids = session_ids(&saved);
    assert_eq!(ids.len(), 2, "both agents recorded a conversation: {saved}");
    assert_ne!(ids[0], ids[1], "the two agents must not share one conversation");
    assert!(ids.iter().all(|id| id.len() == 36), "ids are UUIDs: {ids:?}");

    // Reboot onto the same store: the agent that held the stage comes back
    // reopening ITS conversation, by name, not "the most recent one".
    let socket2 = tmp.path().join("butai2.sock");
    start_daemon_with_agent_args(&socket2, store.clone(), &args, &resume_args).await;
    drop(attach_to(&socket2, &proj).await);
    await_stage_text(&socket2, 1, &format!("REOPENED {}", ids[1])).await;

    // And the ids are stable, so a second restart resumes the same two rather
    // than forking a new pair every time the daemon bounces.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = std::fs::read_to_string(&store).expect("session file rewritten");
    assert_eq!(session_ids(&after), ids, "conversations survive unchanged");
}

/// A conversation can be gone by the time butai asks for it — aged out of the
/// CLI's retention, cleared by hand, or never really started. The CLIs do not
/// degrade in that case, they exit. One clean restart beats a dead pane.
#[tokio::test]
async fn an_agent_that_cannot_reopen_its_conversation_is_restarted_fresh() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("session.json");
    let proj = tmp.path().join("proj");
    std::fs::create_dir(&proj).unwrap();

    // `resume_args` that always fail, standing in for a conversation the CLI
    // can no longer find; `args` that work, as the fallback.
    let args = ["-c", "echo STARTED {session_id}; exec cat"];
    let resume_args = ["-c", "exit 9"];

    let socket1 = tmp.path().join("butai1.sock");
    start_daemon_with_agent_args(&socket1, store.clone(), &args, &resume_args).await;
    let mut c1 = attach_to(&socket1, &proj).await;
    let shell = stage_pane(&socket1, 1).await;
    let (mut watcher, _) = watch_pane(&socket1, shell).await;
    send(&mut c1, &ClientMsg::Command(Command::SpawnAgent("sh".into()))).await;
    let agent = stage_pane_past(&socket1, 1, Some(shell)).await;
    let mut screen = watch(&mut watcher, agent).await;
    await_text(&mut watcher, &mut screen, "STARTED ").await;
    // Give it a conversation to lose: only an agent that has been spoken to is
    // asked to reopen one, so without this the restart below would take the
    // ordinary fresh-start path and never exercise the fallback.
    for msg in type_line("hello") {
        send(&mut watcher, &msg).await;
    }

    send(&mut c1, &ClientMsg::Command(Command::KillServer)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(c1);

    // Restart: the resume exits 9, and butai gives the pane one fresh start.
    let socket2 = tmp.path().join("butai2.sock");
    start_daemon_with_agent_args(&socket2, store.clone(), &args, &resume_args).await;
    drop(attach_to(&socket2, &proj).await);
    await_stage_text(&socket2, 1, "STARTED ").await;

    // The replacement is a live agent on a new conversation, not a corpse row.
    let after = std::fs::read_to_string(&store).expect("session file written");
    assert_eq!(session_ids(&after).len(), 1, "one agent, restarted not dropped: {after}");
}

/// An agent that was opened and never typed into holds an id naming a
/// conversation that was never written: both CLIs create the transcript on the
/// first user message, so `claude --resume <that id>` prints "No conversation
/// found" and exits 1.
///
/// That made the fallback the *common* path rather than the rare one — every
/// idle agent died on restart and came back through a restart it should never
/// have needed. So the resume is not attempted at all here, which the failing
/// `resume_args` below prove: if it were, the pane would exit 9.
#[tokio::test]
async fn an_agent_that_was_never_spoken_to_is_not_asked_to_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("session.json");
    let proj = tmp.path().join("proj");
    std::fs::create_dir(&proj).unwrap();

    // Resuming is fatal here, so reaching it at all is the failure this test
    // is looking for.
    let args = ["-c", "echo STARTED {session_id}; exec cat"];
    let resume_args = ["-c", "echo RESUMED {session_id}; exit 9"];

    let socket1 = tmp.path().join("butai1.sock");
    start_daemon_with_agent_args(&socket1, store.clone(), &args, &resume_args).await;
    let mut c1 = attach_to(&socket1, &proj).await;
    let shell = stage_pane(&socket1, 1).await;
    let (mut watcher, _) = watch_pane(&socket1, shell).await;
    send(&mut c1, &ClientMsg::Command(Command::SpawnAgent("sh".into()))).await;
    let agent = stage_pane_past(&socket1, 1, Some(shell)).await;
    let mut screen = watch(&mut watcher, agent).await;
    await_text(&mut watcher, &mut screen, "STARTED ").await;
    // Deliberately no input: this is the agent you open and leave alone.

    send(&mut c1, &ClientMsg::Command(Command::KillServer)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(c1);

    let saved = std::fs::read_to_string(&store).expect("session file written");
    assert_eq!(session_ids(&saved).len(), 1, "the agent is still recorded: {saved}");
    assert!(
        saved.contains("\"spoke\": false"),
        "an agent that was never typed into is saved as unspoken: {saved}"
    );

    // Restart: it comes back on a fresh conversation, without the failed launch.
    let socket2 = tmp.path().join("butai2.sock");
    start_daemon_with_agent_args(&socket2, store.clone(), &args, &resume_args).await;
    drop(attach_to(&socket2, &proj).await);
    let restored = await_stage_text(&socket2, 1, "STARTED ").await;
    assert!(!restored.contains("RESUMED"), "the doomed resume must not be attempted: {restored}");
}

/// A directory that does not resolve at daemon start is not proof the work is
/// gone — it is what a network share or an external disk looks like before it
/// has finished mounting, and daemon start is exactly when that is most likely.
///
/// Restore used to read it as "deleted", skip the workspace, and then rewrite
/// the store from the workspaces that *did* come up — so a single transient
/// miss permanently destroyed the entry, its agents and their conversation ids,
/// with nothing left to recover from. Observed on a real SMB mount, where every
/// workspace on the share was lost at once.
#[tokio::test]
async fn a_workspace_whose_directory_is_unreadable_is_kept_for_the_next_start() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("session.json");
    // Stands in for the mount point; `proj` is what lives on it.
    let mount = tmp.path().join("mnt");
    let proj = mount.join("proj");
    std::fs::create_dir_all(&proj).unwrap();

    let args = ["-c", "echo OPENED {session_id}; exec cat"];
    let resume_args = ["-c", "echo REOPENED {session_id}; exec cat"];

    let socket1 = tmp.path().join("butai1.sock");
    start_daemon_with_agent_args(&socket1, store.clone(), &args, &resume_args).await;
    let mut c1 = attach_to(&socket1, &proj).await;
    let shell = stage_pane(&socket1, 1).await;
    let (mut watcher, _) = watch_pane(&socket1, shell).await;
    send(&mut c1, &ClientMsg::Command(Command::SpawnAgent("sh".into()))).await;
    let agent = stage_pane_past(&socket1, 1, Some(shell)).await;
    let mut screen = watch(&mut watcher, agent).await;
    await_text(&mut watcher, &mut screen, "OPENED ").await;
    // Speak to it, so the conversation is one worth reopening later.
    for msg in type_line("hello") {
        send(&mut watcher, &msg).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    send(&mut c1, &ClientMsg::Command(Command::KillServer)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(c1);

    let saved = std::fs::read_to_string(&store).expect("session file written");
    let ids = session_ids(&saved);
    assert_eq!(ids.len(), 1, "the agent recorded a conversation: {saved}");

    // The share goes away. Start a daemon while it is down.
    std::fs::rename(&mount, tmp.path().join("mnt-unplugged")).unwrap();
    let socket2 = tmp.path().join("butai2.sock");
    start_daemon_with_agent_args(&socket2, store.clone(), &args, &resume_args).await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let after_outage = std::fs::read_to_string(&store).expect("session file still there");
    assert!(
        after_outage.contains(proj.to_str().unwrap()),
        "the unreachable workspace is kept, not forgotten: {after_outage}"
    );
    assert_eq!(
        session_ids(&after_outage),
        ids,
        "and so is the conversation its agent was holding: {after_outage}"
    );

    // The share comes back. The workspace restores from the entry that survived,
    // reopening the same conversation rather than starting over.
    std::fs::rename(tmp.path().join("mnt-unplugged"), &mount).unwrap();
    let socket3 = tmp.path().join("butai3.sock");
    start_daemon_with_agent_args(&socket3, store.clone(), &args, &resume_args).await;
    drop(attach_to(&socket3, &proj).await);
    await_stage_text(&socket3, 1, &format!("REOPENED {}", ids[0])).await;
}

/// The pane dumps are rewritten every sampler tick; the roster they are keyed
/// against has to keep up, or a hard kill restores one against the other.
///
/// Nothing is shut down gracefully here — the daemon is dropped, exactly as a
/// crash or a power cut would leave it — so the only thing that can have
/// recorded the second agent is the periodic write.
#[tokio::test]
async fn the_agent_roster_survives_a_daemon_that_never_shut_down() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("session.json");
    let proj = tmp.path().join("proj");
    std::fs::create_dir(&proj).unwrap();

    let args = ["-c", "echo OPENED {session_id}; exec cat"];
    let resume_args = ["-c", "echo REOPENED {session_id}; exec cat"];

    let socket1 = tmp.path().join("butai1.sock");
    start_daemon_with_agent_args(&socket1, store.clone(), &args, &resume_args).await;
    let mut c1 = attach_to(&socket1, &proj).await;
    let shell = stage_pane(&socket1, 1).await;
    let (mut watcher, _) = watch_pane(&socket1, shell).await;
    send(&mut c1, &ClientMsg::Command(Command::SpawnAgent("sh".into()))).await;
    let agent = stage_pane_past(&socket1, 1, Some(shell)).await;
    let mut screen = watch(&mut watcher, agent).await;
    await_text(&mut watcher, &mut screen, "OPENED ").await;
    send(&mut c1, &ClientMsg::Command(Command::SpawnAgent("sh".into()))).await;

    // Long enough for a sampler tick to carry the roster to disk.
    tokio::time::sleep(Duration::from_secs(3)).await;
    drop(c1);

    let saved = std::fs::read_to_string(&store).expect("session file written");
    assert_eq!(
        session_ids(&saved).len(),
        2,
        "both agents recorded without a graceful shutdown: {saved}"
    );

    // And closing one has to be recorded just as promptly. The dumps are keyed
    // by position, so a roster that still lists the agent that went away is the
    // case where a restore repaints a pane with its neighbour's screen.
    let mut c2 = attach_to(&socket1, &proj).await;
    send(&mut c2, &ClientMsg::Command(Command::ClosePane)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(c2);

    let after = std::fs::read_to_string(&store).expect("session file rewritten");
    assert_eq!(session_ids(&after).len(), 1, "the closed agent is gone from the store: {after}");
}

/// An explicit `kill-server` (graceful teardown) must NOT wipe the session
/// store: the workspaces that were open come back when the daemon restarts.
#[tokio::test]
async fn workspaces_survive_kill_server() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("session.json");
    let proj = tmp.path().join("proj");
    std::fs::create_dir(&proj).unwrap();

    // Daemon #1: open a workspace, then ask the server to shut down.
    let socket1 = tmp.path().join("butai1.sock");
    start_daemon_with_store(&socket1, Some(store.clone())).await;
    let mut c1 = connect(&socket1).await;
    send(
        &mut c1,
        &ClientMsg::Hello {
            proto_version: PROTOCOL_VERSION,
            encoding: Encoding::Json,
            cols: 80,
            rows: 24,
            target: AttachTarget::Default,
            cwd: proj.clone(),
        },
    )
    .await;
    loop {
        match recv(&mut c1).await {
            ServerMsg::Hello { session, .. } => {
                assert!(session.is_some(), "landed on a workspace");
                break;
            }
            _ => continue,
        }
    }
    send(&mut c1, &ClientMsg::Command(Command::KillServer)).await;
    drop(c1);
    // Give the teardown loop a moment to run.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The store still names the workspace despite every workspace being killed.
    let saved = std::fs::read_to_string(&store).expect("session file kept after kill-server");
    assert!(saved.contains("proj"), "kill-server preserved the workspace: {saved}");

    // A fresh daemon on the same store restores it.
    let socket2 = tmp.path().join("butai2.sock");
    start_daemon_with_store(&socket2, Some(store.clone())).await;
    let mut ctl = connect(&socket2).await;
    send(&mut ctl, &hello(AttachTarget::Control)).await;
    let ServerMsg::Hello { .. } = recv(&mut ctl).await else { panic!() };
    send(&mut ctl, &ClientMsg::Command(Command::ListSessions)).await;
    let ServerMsg::SessionList(list) = recv(&mut ctl).await else { panic!() };
    assert_eq!(list.len(), 1, "workspace restored after kill-server");
    assert_eq!(list[0].name, "proj");
}

/// `kill-server clear` is the opt-out: it throws both halves of the restore
/// state away, so the next daemon on the same store comes up empty.
#[tokio::test]
async fn kill_server_clear_forgets_the_session() {
    let tmp = tempfile::tempdir().unwrap();
    let store = tmp.path().join("session.json");
    let panes = tmp.path().join("panes");
    let proj = tmp.path().join("proj");
    std::fs::create_dir(&proj).unwrap();

    let socket1 = tmp.path().join("butai1.sock");
    start_daemon_with_store(&socket1, Some(store.clone())).await;
    let mut c1 = connect(&socket1).await;
    send(
        &mut c1,
        &ClientMsg::Hello {
            proto_version: PROTOCOL_VERSION,
            encoding: Encoding::Json,
            cols: 80,
            rows: 24,
            target: AttachTarget::Default,
            cwd: proj.clone(),
        },
    )
    .await;
    loop {
        match recv(&mut c1).await {
            ServerMsg::Hello { session, .. } => {
                assert!(session.is_some(), "landed on a workspace");
                break;
            }
            _ => continue,
        }
    }
    // The workspace has to have been written once, or the test would pass on a
    // store that was never populated in the first place.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = std::fs::read_to_string(&store).expect("session file written while running");
    assert!(before.contains("proj"), "workspace was persisted first: {before}");

    send(&mut c1, &ClientMsg::Command(Command::KillServerClear)).await;
    drop(c1);
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(!store.exists(), "kill-server clear removed the session file");
    assert!(!panes.exists(), "kill-server clear removed the pane dumps");

    // A fresh daemon on the same store has nothing to come back to.
    let socket2 = tmp.path().join("butai2.sock");
    start_daemon_with_store(&socket2, Some(store.clone())).await;
    let mut ctl = connect(&socket2).await;
    send(&mut ctl, &hello(AttachTarget::Control)).await;
    let ServerMsg::Hello { .. } = recv(&mut ctl).await else { panic!() };
    send(&mut ctl, &ClientMsg::Command(Command::ListSessions)).await;
    let ServerMsg::SessionList(list) = recv(&mut ctl).await else { panic!() };
    assert!(list.is_empty(), "nothing restored after kill-server clear: {list:?}");
}

/// A generic `shell` process is named by the whole command line running in it,
/// not just the program name, so six shells are not six identical rows.
///
/// The rule lives in `build_processes` and reaches every client through
/// `ProcessDto.name` — the TUI's PROCESSES rail, the web client's and both
/// native ones. It used to be asserted against the composed rail, which is why
/// it reads a DTO now: the daemon draws no rail to look at.
#[tokio::test]
async fn a_shell_process_is_named_by_its_full_command() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    let mut ctl = connect(&socket).await;
    let target = AttachTarget::New { name: Some("rail".into()), layout: None };
    send(&mut ctl, &hello(target)).await;
    let ServerMsg::Hello { .. } = recv(&mut ctl).await else { panic!("expected hello") };

    let pane = stage_pane(&socket, 1).await;
    let (mut c1, mut screen) = watch_pane(&socket, pane).await;
    // A command with an argument: the row used to read a bare "sleep".
    for msg in type_line("sleep 41") {
        send(&mut c1, &msg).await;
    }
    // Wait for the shell to have actually started it, so the poll below is not
    // racing the fork.
    await_text(&mut c1, &mut screen, "sleep 41").await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let body = http_get(&socket, "/v1/workspaces/1").await;
        // Ahead of `"command"`, which holds the shell that was *launched* —
        // matching anywhere would pass on that instead.
        if body.contains(r#""name":"sleep 41""#) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the process row never named the command; body: {body}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Themes left the wire.
///
/// A theme colours a screen and the daemon has none: the only thing it draws is
/// a program's own cells, which carry the program's own colours. Every client
/// picks its palette from its own config — which is also what lets one terminal
/// be dark and another light while both watch the same workspace.
///
/// The commands stay in the vocabulary and answer with a reason, so a client
/// written against the daemon that used to own the palette is told rather than
/// left wondering why nothing changed.
#[tokio::test]
async fn themes_are_refused_because_they_are_the_clients() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    let mut ctl = connect(&socket).await;
    send(&mut ctl, &hello(AttachTarget::Control)).await;
    let ServerMsg::Hello { .. } = recv(&mut ctl).await else { panic!() };

    for cmd in [Command::ListThemes, Command::SetTheme("tokyonight".into())] {
        send(&mut ctl, &ClientMsg::Command(cmd.clone())).await;
        let ServerMsg::Error(e) = recv(&mut ctl).await else { panic!("{cmd:?} was not refused") };
        assert!(e.contains("client"), "{cmd:?} gave an unhelpful reason: {e:?}");
    }

    // And a refusal is read-only: `config.toml` must not have been written.
    assert!(!tmp.path().join(".butai/config.toml").exists(), "a refused theme wrote config");
}

/// A message from the future costs one ignored frame, not the session.
///
/// This is the `watch` failure reproduced deliberately. `watch` was added in
/// 0.6 as an *additive* change, which `docs/protocol.md` says does not bump
/// `proto_version` — correctly, because a client that never sends one is
/// unaffected. What that reasoning missed is the other pairing: a *newer client*
/// sending it to an *older daemon*, which could not decode it and dropped the
/// connection. The client re-dialled and sent another at the next stage change,
/// so a one-release gap presented as the stage blanking over and over rather
/// than as anything version-shaped. A real session was caught doing it 25 times.
///
/// Tolerance is what makes the additive rule true rather than merely stated.
#[tokio::test]
async fn a_message_the_daemon_does_not_know_does_not_end_the_connection() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    let mut ctl = connect(&socket).await;
    send(&mut ctl, &hello(AttachTarget::Control)).await;
    let ServerMsg::Hello { .. } = recv(&mut ctl).await else { panic!("expected hello") };

    // Shaped exactly like a command added in a later release: a variant this
    // build's `ClientMsg` has no arm for. Sent raw, because the point is that it
    // cannot be constructed from this build's vocabulary.
    ctl.send(bytes::Bytes::from_static(
        br#"{"annotate":{"pane":1,"note":"from a client that is newer than you"}}"#,
    ))
    .await
    .unwrap();

    // Same connection, still serving. Before the fix this timed out on a closed
    // socket rather than answering.
    send(&mut ctl, &ClientMsg::Command(Command::ListAgents)).await;
    let ServerMsg::AgentList(_) = recv(&mut ctl).await else {
        panic!("the daemon stopped serving after one unknown message")
    };
}

/// Tolerance has to stop somewhere: a stream that has stopped making sense
/// entirely is not a peer from the future, and must not be humoured forever.
#[tokio::test]
async fn a_stream_of_garbage_still_ends_the_connection() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    let mut ctl = connect(&socket).await;
    send(&mut ctl, &hello(AttachTarget::Control)).await;
    let ServerMsg::Hello { .. } = recv(&mut ctl).await else { panic!("expected hello") };

    // Well-framed, so the codec keeps handing them over; never decodable, so the
    // cap is what has to end it.
    for _ in 0..MAX_CONSECUTIVE_BAD_FRAMES + 4 {
        if ctl.send(bytes::Bytes::from_static(b"not a message at all")).await.is_err() {
            break;
        }
    }

    // The daemon hung up, so the read side is at end-of-stream. Asserted by
    // reading rather than by writing: a closed socket does not fail the first
    // write into it, so a `send` here would prove nothing and panic later.
    let ended = tokio::time::timeout(Duration::from_secs(5), ctl.next()).await;
    assert!(
        matches!(ended, Ok(None) | Ok(Some(Err(_)))),
        "an endless run of undecodable frames should have ended the connection, got {ended:?}"
    );
}

/// One notch of the wheel is three lines, not a whole screen.
///
/// `pane_wheel` — the wheel every client's pane connection arrives on — fell
/// back to `scroll_page`, so a single notch moved the viewport by the height of
/// the pane. `Terminal::handle_input` has spelled the same gesture as three
/// lines since the TUI was the only client, and the two answers were for the
/// same event; the coarse one won because pane-scoped attach is how everything
/// connects now. A notch that jumps a screen does not read as scrolling.
///
/// Asserted as an exact distance rather than "it moved": "it moved" is what the
/// page-sized version did too.
#[tokio::test]
async fn one_notch_of_the_wheel_is_three_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    let mut ctl = connect(&socket).await;
    send(&mut ctl, &hello(AttachTarget::New { name: Some("w".into()), layout: None })).await;
    let ServerMsg::Hello { .. } = recv(&mut ctl).await else { panic!("expected hello") };

    let pane = stage_pane(&socket, 1).await;
    let (mut c, mut screen) = watch_pane(&socket, pane).await;
    // Far more lines than the 24-row window, so every row on screen is a number
    // and most of the run is scrollback to move into.
    //
    // The marker is split by a pair of empty quotes so that the *echo* of this
    // line does not contain it: `sh` shows what was typed, quotes and all, and
    // only its output spells `DONE-SEQ`. Waiting on `"300"` instead matched the
    // command line the instant it was typed — before `seq` had run — and the
    // screen was then whatever the shell had got to, which is why this failed
    // one run in three.
    for msg in type_line("seq 1 300; echo DONE''-SEQ") {
        send(&mut c, &msg).await;
    }
    // Which also means output has *stopped* by the time this returns: the
    // marker is the last thing printed, so the viewport is not still moving
    // under the measurement below.
    await_text(&mut c, &mut screen, "DONE-SEQ").await;

    // The top row, as a number — which is the viewport's position, readable to
    // the line.
    let top = |s: &Screen| -> i64 {
        let first = s.text().lines().next().unwrap_or_default().trim().to_string();
        first
            .parse()
            .unwrap_or_else(|_| panic!("top row is not a line number; screen:\n{}", s.text()))
    };
    let before = top(&screen);

    send(&mut c, &ClientMsg::Input(InputEvent::ScrollUp { x: 0, y: 0 })).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while top(&screen) == before {
        assert!(tokio::time::Instant::now() < deadline, "the wheel moved nothing at all");
        if let ServerMsg::Frame(f) = recv(&mut c).await {
            screen.apply(&f);
        }
    }
    assert_eq!(
        before - top(&screen),
        3,
        "one notch moved {} lines, not three",
        before - top(&screen)
    );
}
