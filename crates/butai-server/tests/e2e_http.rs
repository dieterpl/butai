//! End-to-end tests for the HTTP/REST facade served on the same Unix socket
//! as the framed protocol (the "Docker-style" API). Drives it exactly as a
//! `curl --unix-socket` client would, including exercising the first-byte
//! sniff that separates HTTP from framed connections.

use std::path::PathBuf;
use std::time::Duration;

use butai_server::config::Config;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

async fn start_daemon(tmp: &tempfile::TempDir) -> PathBuf {
    let socket = tmp.path().join("butai.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut config = Config::with_defaults();
    config.general.default_shell = Some("/bin/sh".into());
    tokio::spawn(butai_server::daemon::serve(listener, config, None));
    socket
}

/// A daemon whose only configured agent type, `sh`, is a plain shell. The
/// built-in agent types (`claude`, `codex`, …) are not installed in CI, so this
/// is how a test drives real agent-state transitions: send it input over
/// `panes/{pane}/input` and it produces real PTY output.
async fn start_daemon_with_shell_agent(tmp: &tempfile::TempDir) -> PathBuf {
    let socket = tmp.path().join("butai.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut config = Config::with_defaults();
    config.general.default_shell = Some("/bin/sh".into());
    config.agents.clear();
    config.agents.push(butai_server::config::AgentDef {
        name: "sh".into(),
        command: "/bin/sh".into(),
        args: Vec::new(),
        resume_args: Vec::new(),
        env: Default::default(),
        waiting_pattern: None,
        busy_pattern: None,
    });
    tokio::spawn(butai_server::daemon::serve(listener, config, None));
    socket
}

/// A repo at `tmp/<name>` with one committed file (`a.txt`), then dirtied, plus
/// an untracked stray — two unstaged changes and nothing staged. `user.name` and
/// `user.email` are set because `repo.signature()` is what fails first in a bare
/// container. Returns the repo directory.
fn dirty_repo(tmp: &tempfile::TempDir, name: &str) -> PathBuf {
    let repo_dir = tmp.path().join(name);
    std::fs::create_dir_all(&repo_dir).unwrap();
    let repo = git2::Repository::init(&repo_dir).unwrap();
    let mut cfg = repo.config().unwrap();
    cfg.set_str("user.name", "Test").unwrap();
    cfg.set_str("user.email", "test@example.com").unwrap();
    std::fs::write(repo_dir.join("a.txt"), "committed\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("a.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = repo.signature().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
    std::fs::write(repo_dir.join("a.txt"), "local edit\n").unwrap();
    std::fs::write(repo_dir.join("stray.txt"), "untracked\n").unwrap();
    repo_dir
}

/// Open `path` as workspace 1 and wait for its first git scan to land. The scan
/// runs off the core loop, so a test that acts the instant the workspace exists
/// is racing it.
async fn open_repo_workspace(socket: &PathBuf, path: &std::path::Path, needle: &str) {
    let body = format!(r#"{{"name":"repo","path":"{}"}}"#, path.display());
    let (status, body) = http(socket, "POST", "/v1/workspaces", Some(&body)).await;
    assert_eq!(status, 201, "body: {body}");
    poll_until(socket, "/v1/workspaces/1/changes", needle, |b| b.contains(needle)).await;
}

/// Poll an endpoint until `pred` holds, up to ~6s (agent state is recomputed on
/// the ~2s sampler tick, and `finished` waits a further settle window).
async fn poll_until(
    socket: &PathBuf,
    path: &str,
    what: &str,
    pred: impl Fn(&str) -> bool,
) -> String {
    let mut last = String::new();
    for _ in 0..300 {
        let (status, body) = http(socket, "GET", path, None).await;
        assert_eq!(status, 200, "GET {path} failed: {body}");
        if pred(&body) {
            return body;
        }
        last = body;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}; last body: {last}");
}

/// The `pane` id of the first agent in workspace 1.
async fn first_agent_pane(socket: &PathBuf) -> u64 {
    let body =
        poll_until(socket, "/v1/workspaces/1/agents", "an agent row", |b| b.contains("\"pane\""))
            .await;
    let after = body.split("\"pane\":").nth(1).expect("pane field");
    after
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .expect("numeric pane id")
}

/// One GET with extra request headers, returning `(headers, raw body bytes)`.
///
/// Separate from [`http`] because a compressed reply is not text: decoding the
/// body before the test has decided what it is would hide the very thing being
/// asserted.
async fn http_raw(socket: &PathBuf, path: &str, extra: &str) -> (String, Vec<u8>) {
    let mut stream = UnixStream::connect(socket).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: butai\r\nConnection: close\r\n{extra}\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("http read timed out")
        .unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").expect("headers end");
    let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
    (head, raw[split + 4..].to_vec())
}

fn gunzip(bytes: &[u8]) -> String {
    use std::io::Read;
    let mut out = String::new();
    flate2::read::GzDecoder::new(bytes).read_to_string(&mut out).expect("valid gzip member");
    out
}

/// One HTTP/1.1 request over the socket with `Connection: close`, returning
/// `(status, body)`. Reading to EOF is enough — the server closes the
/// connection after the response.
async fn http(socket: &PathBuf, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let mut stream = UnixStream::connect(socket).await.unwrap();
    let payload = body.unwrap_or("");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: butai\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len()
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("http read timed out")
        .unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

/// Read the event stream for `dur`, returning the raw records seen.
///
/// The stream never ends, so this reads until the deadline rather than to EOF.
async fn read_events_for(socket: &PathBuf, dur: Duration) -> String {
    let mut stream = UnixStream::connect(socket).await.unwrap();
    let req = "GET /v1/events HTTP/1.1\r\nHost: butai\r\nAccept: text/event-stream\r\n\r\n";
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    let mut chunk = [0u8; 8192];
    let deadline = tokio::time::Instant::now() + dur;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            break;
        }
        match tokio::time::timeout(left, stream.read(&mut chunk)).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(n)) => raw.extend_from_slice(&chunk[..n]),
            Ok(Err(_)) => break,
        }
    }
    String::from_utf8_lossy(&raw).into_owned()
}

/// An idle workspace must not be re-broadcast on every frame.
///
/// `workspace_detail` is pushed on the frame clock but only when the detail
/// actually *differs* from the last one sent, which is what makes subscribing
/// to it affordable — and what lets a client draw rails from it instead of
/// polling. The guarantee is only as good as the DTO's equality, so this is the
/// test that notices when a field starts changing on its own.
///
/// A shell agent sitting at its prompt produces no output, so over a second the
/// honest number of pushes is small: the state settling after spawn, and
/// nothing else.
#[tokio::test]
async fn an_idle_workspace_is_not_rebroadcast_every_frame() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon_with_shell_agent(&tmp).await;
    let (status, _) = http(&socket, "POST", "/v1/workspaces", Some(r#"{"name":"quiet"}"#)).await;
    assert_eq!(status, 201);
    let (status, _) =
        http(&socket, "POST", "/v1/workspaces/1/agents", Some(r#"{"type":"sh"}"#)).await;
    assert_eq!(status, 200);

    // Wait for the agent to *settle*, not merely to exist. Sleeping a fixed
    // interval instead races its startup transitions (idle → working → settled)
    // on a loaded machine, and those are legitimate pushes that would be counted
    // against the thing under test.
    poll_until(&socket, "/v1/workspaces/1/agents", "a settled agent", |b| {
        b.contains("\"state\":\"idle\"") || b.contains("\"state\":\"finished\"")
    })
    .await;
    // The settle is debounced, so give the last transition room to land before
    // the window opens.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let text = read_events_for(&socket, Duration::from_millis(1000)).await;
    let pushes = text.matches("\"event\":\"workspace_detail\"").count();
    // The frame clock is ~60fps, so a DTO that is unequal to itself would land
    // somewhere near 60 here. Anything in single digits means the diff is real.
    assert!(pushes < 10, "idle workspace pushed {pushes} times in a second:\n{text}");
}

#[tokio::test]
async fn http_api_creates_queries_and_acts() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    // Empty world.
    let (status, body) = http(&socket, "GET", "/v1/workspaces", None).await;
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body.trim(), "[]");

    // Create one (201 + id).
    let (status, body) = http(&socket, "POST", "/v1/workspaces", Some(r#"{"name":"smoke"}"#)).await;
    assert_eq!(status, 201, "body: {body}");
    assert!(body.contains("\"id\":1"), "body: {body}");

    // It shows up with its shell process counted.
    let (status, body) = http(&socket, "GET", "/v1/workspaces", None).await;
    assert_eq!(status, 200);
    assert!(body.contains("\"name\":\"smoke\""), "body: {body}");
    assert!(body.contains("\"processes\":1"), "body: {body}");

    // Detail view.
    let (status, body) = http(&socket, "GET", "/v1/workspaces/1", None).await;
    assert_eq!(status, 200);
    assert!(body.contains("\"processes\""), "body: {body}");

    // System telemetry serializes.
    let (status, body) = http(&socket, "GET", "/v1/system", None).await;
    assert_eq!(status, 200);
    assert!(body.contains("\"cpu_pct\""), "body: {body}");
    // Storage is on the same payload, and the key is there before the first
    // sample lands — an embedder reading `disks` gets an empty list on a daemon
    // that has been up for a second, never a missing field.
    assert!(body.contains("\"disks\""), "body: {body}");

    // Account standing serializes, and answers before the first sample has
    // landed rather than blocking on it — a client polling a daemon that has
    // been up for a second gets an empty roster, not a stall.
    let (status, body) = http(&socket, "GET", "/v1/usage", None).await;
    assert_eq!(status, 200);
    assert!(body.contains("\"clis\""), "body: {body}");

    // Action: start a process, then confirm it appears.
    let (status, _) = http(
        &socket,
        "POST",
        "/v1/workspaces/1/processes",
        // Must outlive the assertion below: a command that exits immediately
        // races its own auto-removal on clean exit.
        Some(r#"{"name":"greet","command":"sleep 30"}"#),
    )
    .await;
    assert_eq!(status, 200);
    let (_, body) = http(&socket, "GET", "/v1/workspaces/1/processes", None).await;
    assert!(body.contains("\"greet\""), "body: {body}");

    // Unknown route + unknown workspace.
    let (status, _) = http(&socket, "GET", "/v1/nope", None).await;
    assert_eq!(status, 404);
    let (status, _) = http(&socket, "GET", "/v1/workspaces/999", None).await;
    assert_eq!(status, 404);
}

/// Discarding a file goes all the way through: route -> core -> git pane ->
/// the file on disk. Only unstaged files qualify, so a clean path is refused.
#[tokio::test]
async fn http_api_discards_a_files_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    let repo_dir = dirty_repo(&tmp, "repo");
    open_repo_workspace(&socket, &repo_dir, "a.txt").await;

    // Tracked file: restored from the index.
    let (status, body) =
        http(&socket, "POST", "/v1/workspaces/1/changes/discard", Some(r#"{"path":"a.txt"}"#))
            .await;
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(std::fs::read_to_string(repo_dir.join("a.txt")).unwrap(), "committed\n");

    // Untracked file: deleted.
    let (status, body) =
        http(&socket, "POST", "/v1/workspaces/1/changes/discard", Some(r#"{"path":"stray.txt"}"#))
            .await;
    assert_eq!(status, 200, "body: {body}");
    assert!(!repo_dir.join("stray.txt").exists(), "untracked file survived");

    // Nothing left to discard -> 400, not a silent success. The refusal is
    // decided from the rail's rows, which are rebuilt off the core loop, so
    // wait for them to catch up with the two discards above — asking too early
    // is answered from the pre-discard view and succeeds.
    poll_until(&socket, "/v1/workspaces/1/changes", "an empty rail", |b| {
        b.contains(r#""unstaged":[]"#) && b.contains(r#""staged":[]"#)
    })
    .await;
    let (status, _) =
        http(&socket, "POST", "/v1/workspaces/1/changes/discard", Some(r#"{"path":"a.txt"}"#))
            .await;
    assert_eq!(status, 400);
}

/// `DELETE .../file` removes one file, and refuses the three things that would
/// make it a bigger verb than it is.
///
/// The refusals are the point of the test. Deleting `a.txt` is one line of
/// `std::fs`; a directory removed recursively, a path that climbed out of the
/// workspace, or a second delete that reported success on a file something else
/// had already taken are the ways this route becomes dangerous.
#[tokio::test]
async fn http_api_deletes_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    let repo_dir = dirty_repo(&tmp, "repo");
    std::fs::create_dir_all(repo_dir.join("sub")).unwrap();
    std::fs::write(repo_dir.join("sub/keep.txt"), "kept\n").unwrap();
    open_repo_workspace(&socket, &repo_dir, "a.txt").await;

    // A tracked file goes, and unlike `discard` it does not come back.
    let (status, body) = http(&socket, "DELETE", "/v1/workspaces/1/file?path=a.txt", None).await;
    assert_eq!(status, 200, "body: {body}");
    assert!(!repo_dir.join("a.txt").exists(), "a.txt survived the delete");

    // Already gone: a 404 rather than a second success, so a client working
    // from a stale listing is told the file was not its to delete.
    let (status, _) = http(&socket, "DELETE", "/v1/workspaces/1/file?path=a.txt", None).await;
    assert_eq!(status, 404);

    // A directory is refused rather than removed with everything under it.
    let (status, body) = http(&socket, "DELETE", "/v1/workspaces/1/file?path=sub", None).await;
    assert_eq!(status, 400, "body: {body}");
    assert!(repo_dir.join("sub/keep.txt").exists(), "the directory's contents went");

    // And the traversal, which is the one failure that reaches outside the
    // workspace at all. `stray.txt` is the repo's own untracked file, named
    // through a path that leaves and comes back.
    let (status, _) =
        http(&socket, "DELETE", "/v1/workspaces/1/file?path=../repo/stray.txt", None).await;
    assert_eq!(status, 400);
    assert!(repo_dir.join("stray.txt").exists(), "a path that escaped was honoured");
}

/// Partial staging over REST: fetch the diff, send back one hunk of it, and
/// only that hunk is staged.
///
/// The route exists so an embedder gets `git add -p` too — Caliper relays
/// `/v1/*` verbatim, and hunk staging that only the TUI could reach would be
/// the wrong shape for an engine.
#[tokio::test]
async fn http_api_stages_one_hunk_of_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    let repo_dir = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    let repo = git2::Repository::init(&repo_dir).unwrap();
    let mut cfg = repo.config().unwrap();
    cfg.set_str("user.name", "Test").unwrap();
    cfg.set_str("user.email", "test@example.com").unwrap();
    let base: String = (1..=20).map(|i| format!("line{i}\n")).collect();
    std::fs::write(repo_dir.join("a.txt"), &base).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("a.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = repo.signature().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();

    // Two changes, far enough apart to be two hunks.
    let edited: String = (1..=20)
        .map(|i| match i {
            2 => "EARLY\n".to_string(),
            18 => "LATE\n".to_string(),
            _ => format!("line{i}\n"),
        })
        .collect();
    std::fs::write(repo_dir.join("a.txt"), &edited).unwrap();
    open_repo_workspace(&socket, &repo_dir, "a.txt").await;

    // Ask for the diff the way a client would, then keep only the second hunk.
    let (status, body) = http(&socket, "GET", "/v1/workspaces/1/diff?path=a.txt", None).await;
    assert_eq!(status, 200, "body: {body}");
    let text: String = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("patch").and_then(|t| t.as_str()).map(str::to_string))
        .expect("no patch in the diff reply");
    assert!(text.contains("LATE"), "diff did not mention the late change: {text}");

    let patch = butai_protocol::hunk::Patch::parse(&text);
    assert_eq!(patch.hunk_count(), 2, "expected two hunks:\n{text}");
    let late = patch
        .subset(&butai_protocol::hunk::Selection { file: 0, hunk: 1, lines: None })
        .expect("no subset for the second hunk");

    let payload = serde_json::json!({ "patch": late, "target": "index" }).to_string();
    let (status, body) = http(&socket, "POST", "/v1/workspaces/1/git/apply", Some(&payload)).await;
    assert_eq!(status, 200, "body: {body}");

    // The index holds the late change and not the early one; disk is untouched.
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    let entry = index.get_path(std::path::Path::new("a.txt"), 0).unwrap();
    let staged = String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap();
    assert!(staged.contains("LATE"), "late change not staged:\n{staged}");
    assert!(!staged.contains("EARLY"), "early change leaked into the index:\n{staged}");
    assert_eq!(std::fs::read_to_string(repo_dir.join("a.txt")).unwrap(), edited);

    // A patch that does not apply is a 400 rather than a silent success.
    let junk = serde_json::json!({
        "patch": "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,1 +1,1 @@\n-nope\n+yes\n",
        "target": "index",
    })
    .to_string();
    let (status, _) = http(&socket, "POST", "/v1/workspaces/1/git/apply", Some(&junk)).await;
    assert_eq!(status, 400);
}

/// Worktrees: add one, see it listed with the workspace open on it, open that
/// workspace, and remove it.
///
/// The `workspace` field is the reason this is worth having in butai at all — a
/// worktree is a directory and a butai workspace is a directory, so the list has
/// to say which worktrees already have one or a client cannot tell "open it"
/// from "go there".
#[tokio::test]
async fn worktrees_are_listed_added_and_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    let repo_dir = dirty_repo(&tmp, "repo");
    open_repo_workspace(&socket, &repo_dir, "a.txt").await;

    // Only the main worktree to begin with, and it is marked as such.
    let (status, body) = http(&socket, "GET", "/v1/workspaces/1/git/worktrees", None).await;
    assert_eq!(status, 200, "body: {body}");
    let list: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(list.len(), 1, "body: {body}");
    assert_eq!(list[0]["is_main"], true);
    assert_eq!(list[0]["workspace"], 1, "the open workspace was not matched to its worktree");

    // Add one on a new branch.
    let wt = tmp.path().join("repo-feature");
    let payload = serde_json::json!({
        "path": wt.to_string_lossy(),
        "branch": "feat/x",
        "new_branch": true,
    })
    .to_string();
    let (status, body) =
        http(&socket, "POST", "/v1/workspaces/1/git/worktree", Some(&payload)).await;
    assert!(status == 200 || status == 202, "status {status}, body: {body}");
    poll_until(&socket, "/v1/workspaces/1/git/worktrees", "the new worktree", |b| {
        b.contains("feat/x")
    })
    .await;
    assert!(wt.join("a.txt").exists(), "the worktree was not checked out");

    // It is listed, not main, and has no workspace on it yet.
    let (_, body) = http(&socket, "GET", "/v1/workspaces/1/git/worktrees", None).await;
    let list: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    let added = list.iter().find(|w| w["branch"] == "feat/x").expect("not listed");
    assert_eq!(added["is_main"], false);
    assert_eq!(added["workspace"], serde_json::Value::Null, "nothing is open on it yet");

    // Open it as a workspace, and the listing now says so.
    let body = format!(r#"{{"name":"feature","path":"{}"}}"#, wt.display());
    let (status, body) = http(&socket, "POST", "/v1/workspaces", Some(&body)).await;
    assert_eq!(status, 201, "body: {body}");
    let (_, body) = http(&socket, "GET", "/v1/workspaces/1/git/worktrees", None).await;
    let list: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    let added = list.iter().find(|w| w["branch"] == "feat/x").expect("not listed");
    assert_eq!(added["workspace"], 2, "the second workspace was not matched: {body}");

    // A hostile path never reaches git.
    let bad = serde_json::json!({ "path": "--git-dir=/etc", "branch": "x" }).to_string();
    let (status, _) = http(&socket, "POST", "/v1/workspaces/1/git/worktree", Some(&bad)).await;
    assert_eq!(status, 400);

    // Remove it. `force` because the checkout is fine but git is strict about
    // worktrees it did not create in the usual place.
    let path = wt.to_string_lossy().replace('/', "%2F");
    let (status, body) = http(
        &socket,
        "DELETE",
        &format!("/v1/workspaces/1/git/worktree?path={path}&force=true"),
        None,
    )
    .await;
    assert!(status == 200 || status == 202, "status {status}, body: {body}");
    poll_until(&socket, "/v1/workspaces/1/git/worktrees", "the worktree to go", |b| {
        !b.contains("feat/x")
    })
    .await;
}

/// A workspace opened *below* the repo root still diffs the right file.
///
/// `git status` reports paths relative to the worktree root, and the rail passes
/// them straight through to `/changes`. Anchoring `git -C` and the path-escape
/// check on the workspace cwd therefore asked for `sub/sub/b.txt` — a path that
/// does not exist — so every diff in such a workspace came back empty.
#[tokio::test]
async fn a_workspace_below_the_repo_root_diffs_the_right_file() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    let repo_dir = tmp.path().join("repo");
    std::fs::create_dir_all(repo_dir.join("sub")).unwrap();
    let repo = git2::Repository::init(&repo_dir).unwrap();
    let mut cfg = repo.config().unwrap();
    cfg.set_str("user.name", "Test").unwrap();
    cfg.set_str("user.email", "test@example.com").unwrap();
    std::fs::write(repo_dir.join("sub/b.txt"), "committed\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("sub/b.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = repo.signature().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
    std::fs::write(repo_dir.join("sub/b.txt"), "local edit\n").unwrap();

    // The workspace is the subdirectory, not the repo root.
    let body = format!(r#"{{"name":"sub","path":"{}"}}"#, repo_dir.join("sub").display());
    let (status, body) = http(&socket, "POST", "/v1/workspaces", Some(&body)).await;
    assert_eq!(status, 201, "body: {body}");

    // The rail names it `sub/b.txt`, not `b.txt` — relative to the root.
    let body =
        poll_until(&socket, "/v1/workspaces/1/changes", "the edited file", |b| b.contains("b.txt"))
            .await;
    assert!(body.contains("sub/b.txt"), "path was not repo-root relative: {body}");

    // ...and that exact path, round-tripped back, has to produce a patch.
    let (status, body) = http(&socket, "GET", "/v1/workspaces/1/diff?path=sub/b.txt", None).await;
    assert_eq!(status, 200, "body: {body}");
    assert!(body.contains("local edit"), "diff was empty: {body}");
}

/// The git operation runner, end to end, against a **local bare repository** —
/// a real `git push` with no network involved.
///
/// Also pins the two answers that only exist because operations are
/// asynchronous: a completed operation reports its outcome in the body with
/// `ok`, and a second operation is refused with 409 rather than being allowed
/// to interleave index writes with the first.
#[tokio::test]
async fn git_push_runs_against_a_local_remote_and_reports_its_outcome() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    // A bare repo standing in for `origin`, and a working clone that tracks it.
    let remote = tmp.path().join("origin.git");
    git2::Repository::init_bare(&remote).unwrap();
    let repo_dir = dirty_repo(&tmp, "repo");
    {
        let repo = git2::Repository::open(&repo_dir).unwrap();
        repo.remote("origin", remote.to_str().unwrap()).unwrap();
    }
    open_repo_workspace(&socket, &repo_dir, "a.txt").await;

    // Push the initial commit, naming the remote and branch explicitly since
    // nothing is tracking anything yet.
    let branch = {
        let repo = git2::Repository::open(&repo_dir).unwrap();
        let b = repo.head().unwrap().shorthand().unwrap().to_string();
        b
    };
    let body = format!(r#"{{"remote":"origin","branch":"{branch}","set_upstream":true}}"#);
    let (status, body) = http(&socket, "POST", "/v1/workspaces/1/git/push", Some(&body)).await;
    // 200 if it finished inside the grace window, 202 if not — a client has to
    // handle both, so the test does too.
    assert!(status == 200 || status == 202, "push answered {status}: {body}");

    // Either way the outcome is discoverable by polling.
    let body = poll_until(&socket, "/v1/workspaces/1/git/op", "the push to finish", |b| {
        b.contains("\"running\":false")
    })
    .await;
    assert!(body.contains("\"ok\":true"), "push failed: {body}");

    // ...and the commit really is in the bare repo now.
    let remote_repo = git2::Repository::open_bare(&remote).unwrap();
    assert!(
        remote_repo.find_reference(&format!("refs/heads/{branch}")).is_ok(),
        "the branch never reached the remote"
    );
}

/// A nonsense remote never becomes a command line: it is refused before
/// anything runs, with a 400 rather than a 500 or a spawned process.
#[tokio::test]
async fn a_hostile_remote_name_is_refused_before_git_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;
    let repo_dir = dirty_repo(&tmp, "repo");
    open_repo_workspace(&socket, &repo_dir, "a.txt").await;

    for (route, hostile) in [
        ("git/fetch", r#"{"remote":"ext::sh -c whoami"}"#),
        ("git/fetch", r#"{"remote":"--upload-pack=touch /tmp/pwned"}"#),
        ("git/pull", r#"{"remote":"ssh://evil/repo"}"#),
        ("git/push", r#"{"remote":"origin","branch":"--exec=touch /tmp/pwned"}"#),
        ("git/push", r#"{"remote":"origin","branch":"a..b"}"#),
    ] {
        let (status, body) =
            http(&socket, "POST", &format!("/v1/workspaces/1/{route}"), Some(hostile)).await;
        assert_eq!(status, 400, "{route} {hostile} answered {status}: {body}");
    }
    assert!(!std::path::Path::new("/tmp/pwned").exists(), "a hostile value ran");
}

/// `commit-all` stages every change and commits in one round-trip: route -> core
/// -> git pane. A clean tree is refused (400), matching the CHANGES rail's `C`.
#[tokio::test]
async fn http_api_commits_all_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    let repo_dir = dirty_repo(&tmp, "repo");
    open_repo_workspace(&socket, &repo_dir, "stray.txt").await;
    let repo = git2::Repository::open(&repo_dir).unwrap();

    // Stage everything and commit in one call.
    let (status, body) = http(
        &socket,
        "POST",
        "/v1/workspaces/1/changes/commit-all",
        Some(r#"{"message":"all in one go"}"#),
    )
    .await;
    assert_eq!(status, 200, "body: {body}");

    // The tree is clean and the new commit is on HEAD. The rail rescans off the
    // core loop, so reading `/changes` immediately races it — poll rather than
    // assume, or this fails whenever the machine is busy enough.
    let body = poll_until(&socket, "/v1/workspaces/1/changes", "all in one go", |b| {
        b.contains("all in one go") && b.contains(r#""unstaged":[]"#)
    })
    .await;
    assert!(body.contains(r#""staged":[]"#), "index not clean after commit-all: {body}");
    assert_eq!(repo.head().unwrap().peel_to_commit().unwrap().summary().unwrap(), "all in one go",);

    // Nothing left to commit -> 400, not an empty commit.
    let (status, _) =
        http(&socket, "POST", "/v1/workspaces/1/changes/commit-all", Some(r#"{"message":"nope"}"#))
            .await;
    assert_eq!(status, 400);
}

/// A bell puts an agent in `waiting`, and acknowledging it — over HTTP, with no
/// TUI anywhere — brings it back out.
///
/// This is the regression test for the bug that made a "needs you" badge
/// useless: `bell_pending()` was only ever cleared by the TUI staging a pane, so
/// for a GUI or web client an agent that rang the bell once reported `waiting`
/// forever. Two independent faults had to be fixed for this to pass — the
/// missing ack route, and `Waiting` holding in the debounced state machine even
/// after the signal cleared.
#[tokio::test]
async fn http_api_acknowledges_a_bell() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon_with_shell_agent(&tmp).await;

    let (status, body) = http(&socket, "POST", "/v1/workspaces", Some(r#"{"name":"bell"}"#)).await;
    assert_eq!(status, 201, "body: {body}");
    let (status, body) =
        http(&socket, "POST", "/v1/workspaces/1/agents", Some(r#"{"type":"sh"}"#)).await;
    assert_eq!(status, 200, "body: {body}");
    let pane = first_agent_pane(&socket).await;

    // Ring the bell from inside the agent's own PTY.
    let (status, body) = http(
        &socket,
        "POST",
        &format!("/v1/workspaces/1/panes/{pane}/input"),
        Some(r#"{"paste":"printf '\\a'\n"}"#),
    )
    .await;
    assert_eq!(status, 200, "body: {body}");

    let body = poll_until(&socket, "/v1/workspaces/1/agents", "waiting after the bell", |b| {
        b.contains("\"state\":\"waiting\"")
    })
    .await;
    // A bell is not a question — nothing was asked, so `question` stays false.
    assert!(body.contains("\"question\":false"), "a bell is not a question: {body}");

    // Ack it: the same gesture as the TUI opening the pane.
    let (status, body) =
        http(&socket, "POST", &format!("/v1/workspaces/1/panes/{pane}/ack"), None).await;
    assert_eq!(status, 200, "body: {body}");

    poll_until(&socket, "/v1/workspaces/1/agents", "waiting to clear after the ack", |b| {
        !b.contains("\"state\":\"waiting\"")
    })
    .await;

    // The workspace summary's badge count follows, so a list view agrees.
    let body = poll_until(&socket, "/v1/workspaces", "the waiting count to clear", |b| {
        b.contains("\"waiting\":0")
    })
    .await;
    assert!(body.contains("\"questions\":0"), "body: {body}");

    // Acking an unknown pane is a 404, not a silent success.
    let (status, _) = http(&socket, "POST", "/v1/workspaces/1/panes/9999/ack", None).await;
    assert_eq!(status, 404);
}

/// An agent can be killed over HTTP, via a route that says so. `KillPane` was
/// always pane-generic, but the only path to it lived under `processes`, so every
/// GUI concluded agents could not be killed.
#[tokio::test]
async fn http_api_kills_an_agent_pane() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon_with_shell_agent(&tmp).await;

    let (status, body) = http(&socket, "POST", "/v1/workspaces", Some(r#"{"name":"kill"}"#)).await;
    assert_eq!(status, 201, "body: {body}");
    let (status, _) =
        http(&socket, "POST", "/v1/workspaces/1/agents", Some(r#"{"type":"sh"}"#)).await;
    assert_eq!(status, 200);
    let pane = first_agent_pane(&socket).await;
    let (_, body) = http(&socket, "GET", "/v1/workspaces", None).await;
    assert!(body.contains("\"agents\":1"), "body: {body}");

    // The canonical route.
    let (status, body) =
        http(&socket, "DELETE", &format!("/v1/workspaces/1/panes/{pane}"), None).await;
    assert_eq!(status, 200, "body: {body}");
    let (_, body) = http(&socket, "GET", "/v1/workspaces/1/agents", None).await;
    assert_eq!(body.trim(), "[]", "agent row survived the kill");
    let (_, body) = http(&socket, "GET", "/v1/workspaces", None).await;
    assert!(body.contains("\"agents\":0"), "body: {body}");

    // Killing it twice is a 404, and so is an id from another workspace.
    let (status, _) =
        http(&socket, "DELETE", &format!("/v1/workspaces/1/panes/{pane}"), None).await;
    assert_eq!(status, 404);

    // The legacy `processes` spelling is an alias for the same handler, so a
    // client built against an older daemon keeps working.
    let (status, _) =
        http(&socket, "POST", "/v1/workspaces/1/agents", Some(r#"{"type":"sh"}"#)).await;
    assert_eq!(status, 200);
    let pane = first_agent_pane(&socket).await;
    let (status, body) =
        http(&socket, "DELETE", &format!("/v1/workspaces/1/processes/{pane}"), None).await;
    assert_eq!(status, 200, "body: {body}");
    let (_, body) = http(&socket, "GET", "/v1/workspaces/1/agents", None).await;
    assert_eq!(body.trim(), "[]", "alias route did not kill the agent");
}

/// A dead agent reports `exited`, not `idle`, and is counted apart from the live
/// buckets. Previously the state was forced to `idle`, so a client that only read
/// `state` painted a corpse as a quiet live agent.
#[tokio::test]
async fn http_api_reports_an_exited_agent_distinctly() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon_with_shell_agent(&tmp).await;

    let (status, body) = http(&socket, "POST", "/v1/workspaces", Some(r#"{"name":"dead"}"#)).await;
    assert_eq!(status, 201, "body: {body}");
    let (status, _) =
        http(&socket, "POST", "/v1/workspaces/1/agents", Some(r#"{"type":"sh"}"#)).await;
    assert_eq!(status, 200);
    let pane = first_agent_pane(&socket).await;

    // Exit non-zero: a clean exit auto-reaps the row, a failing one lingers so
    // the output stays readable — and that lingering row is what we care about.
    let (status, _) = http(
        &socket,
        "POST",
        &format!("/v1/workspaces/1/panes/{pane}/input"),
        Some(r#"{"paste":"exit 3\n"}"#),
    )
    .await;
    assert_eq!(status, 200);

    let body = poll_until(&socket, "/v1/workspaces/1/agents", "the agent to report exited", |b| {
        b.contains("\"state\":\"exited\"")
    })
    .await;
    assert!(body.contains("\"exited\":3"), "exit code missing: {body}");

    // It counts as exited and in none of the live buckets.
    let body =
        poll_until(&socket, "/v1/workspaces", "the exited count", |b| b.contains("\"exited\":1"))
            .await;
    assert!(body.contains("\"waiting\":0"), "body: {body}");
    assert!(body.contains("\"working\":0"), "body: {body}");
    assert!(body.contains("\"finished\":0"), "body: {body}");
}

/// A decision prompt on screen sets `question`, so a client can tell "it asked
/// you something" from "it rang the bell" — both of which are `waiting`.
#[tokio::test]
async fn http_api_flags_a_question_prompt() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon_with_shell_agent(&tmp).await;

    let (status, body) = http(&socket, "POST", "/v1/workspaces", Some(r#"{"name":"ask"}"#)).await;
    assert_eq!(status, 201, "body: {body}");
    let (status, _) =
        http(&socket, "POST", "/v1/workspaces/1/agents", Some(r#"{"type":"sh"}"#)).await;
    assert_eq!(status, 200);
    let pane = first_agent_pane(&socket).await;

    // Print prompt chrome and leave it on screen as the last visible line.
    // Only the bottom `FOOTER_SCAN_ROWS` of the grid are scanned — deliberately,
    // so the same phrases quoted in an agent's prose don't count — so scroll the
    // prompt down into that band first. Padding past any plausible pane height
    // keeps this independent of the daemon's default stage size.
    let pad = "\\\\n".repeat(60);
    let (status, _) = http(
        &socket,
        "POST",
        &format!("/v1/workspaces/1/panes/{pane}/input"),
        Some(&format!(r#"{{"paste":"printf '{pad}Overwrite the file? (y/n) '\n"}}"#)),
    )
    .await;
    assert_eq!(status, 200);

    poll_until(&socket, "/v1/workspaces/1/agents", "the question flag", |b| {
        b.contains("\"question\":true") && b.contains("\"state\":\"waiting\"")
    })
    .await;
    poll_until(&socket, "/v1/workspaces", "the questions count", |b| b.contains("\"questions\":1"))
        .await;
}

/// The sniff must not disturb the framed protocol: a framed control client
/// still works after HTTP clients have used the same socket.
#[tokio::test]
async fn framed_and_http_share_one_socket() {
    use butai_protocol::framing::{decode, encode, length_codec};
    use butai_protocol::{AttachTarget, ClientMsg, Command, Encoding, ServerMsg, PROTOCOL_VERSION};
    use futures::{SinkExt, StreamExt};
    use tokio_util::codec::Framed;

    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    // Create a workspace over HTTP.
    let (status, _) = http(&socket, "POST", "/v1/workspaces", Some(r#"{"name":"mixed"}"#)).await;
    assert_eq!(status, 201);

    // A framed control client sees it via the classic protocol.
    let stream = UnixStream::connect(&socket).await.unwrap();
    let mut client = Framed::new(stream, length_codec());
    let hello = ClientMsg::Hello {
        proto_version: PROTOCOL_VERSION,
        encoding: Encoding::Json,
        cols: 80,
        rows: 24,
        target: AttachTarget::Control,
        cwd: PathBuf::from("/"),
    };
    client.send(encode(&hello, Encoding::Json).unwrap()).await.unwrap();
    let _ = client.next().await.unwrap().unwrap(); // server hello

    client
        .send(encode(&ClientMsg::Command(Command::ListSessions), Encoding::Json).unwrap())
        .await
        .unwrap();
    let frame = client.next().await.unwrap().unwrap();
    let ServerMsg::SessionList(list) = decode(&frame, Encoding::Json).unwrap() else {
        panic!("expected session list");
    };
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "mixed");

    // And HTTP still works afterwards on a fresh connection.
    let (status, body) = http(&socket, "GET", "/v1/workspaces", None).await;
    assert_eq!(status, 200);
    assert!(body.contains("\"name\":\"mixed\""), "body: {body}");
}

/// `Watch` re-points a pane connection without a reconnect.
///
/// This is the message that lets a client showing one pane at a time change
/// which one — the TUI's stage, `<butai-stage>`'s `setPane()`. The properties
/// worth pinning are that the new pane's content actually arrives, that it
/// arrives as a **full** frame (a diff against the pane you just stopped
/// showing would corrupt the screen), and that a refused watch leaves the
/// connection streaming what it had rather than going blank.
#[tokio::test]
async fn watch_repoints_a_pane_connection() {
    use butai_protocol::api::WorkspaceDetail;
    use butai_protocol::framing::{decode, encode, length_codec};
    use butai_protocol::{AttachTarget, ClientMsg, Encoding, ServerMsg, PROTOCOL_VERSION};
    use futures::{SinkExt, StreamExt};
    use tokio_util::codec::Framed;

    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;
    let (status, body) = http(&socket, "POST", "/v1/workspaces", Some(r#"{"name":"w"}"#)).await;
    assert_eq!(status, 201, "body: {body}");

    // Two processes, each printing a marker that identifies its pane on sight.
    for (name, marker) in [("alpha", "ALPHA_MARKER"), ("beta", "BETA_MARKER")] {
        let body = format!(r#"{{"name":"{name}","command":"echo {marker}; sleep 60"}}"#);
        let (status, body) = http(&socket, "POST", "/v1/workspaces/1/processes", Some(&body)).await;
        assert_eq!(status, 200, "body: {body}");
    }
    let body = poll_until(&socket, "/v1/workspaces/1", "both processes", |b| {
        b.contains("\"alpha\"") && b.contains("\"beta\"")
    })
    .await;
    let detail: WorkspaceDetail = serde_json::from_str(&body).unwrap();
    let pane_of = |name: &str| {
        detail.processes.iter().find(|p| p.name == name).unwrap_or_else(|| panic!("no {name}")).pane
    };
    let (alpha, beta) = (pane_of("alpha"), pane_of("beta"));

    let stream = UnixStream::connect(&socket).await.unwrap();
    let mut client = Framed::new(stream, length_codec());
    let hello = ClientMsg::Hello {
        proto_version: PROTOCOL_VERSION,
        encoding: Encoding::Json,
        cols: 80,
        rows: 24,
        target: AttachTarget::Pane { pane: alpha },
        cwd: PathBuf::from("/"),
    };
    client.send(encode(&hello, Encoding::Json).unwrap()).await.unwrap();

    /// Read frames until one of them carries `needle`, reporting whether any of
    /// them was a full repaint.
    ///
    /// *Any*, not *the first*: a frame for the pane we are leaving can already
    /// be in flight when the watch is sent, and nothing on the wire tells a
    /// client which pane a frame belongs to. That is harmless — the socket is
    /// ordered, so a stale diff always lands before the full frame that
    /// replaces it, never after — but it does mean "the first frame after the
    /// watch" is not a property a client can observe, and a test that asserts
    /// it is testing the scheduler.
    async fn await_marker(
        client: &mut Framed<UnixStream, tokio_util::codec::LengthDelimitedCodec>,
        needle: &str,
    ) -> bool {
        let mut saw_full = false;
        for _ in 0..400 {
            let frame = tokio::time::timeout(Duration::from_secs(10), client.next())
                .await
                .expect("timed out")
                .expect("connection closed")
                .expect("read error");
            let msg: ServerMsg = decode(&frame, Encoding::Json).unwrap();
            let ServerMsg::Frame(update) = msg else { continue };
            saw_full |= update.full;
            let text: String =
                update.cells.iter().flat_map(|r| r.cells.iter()).map(|c| c.ch.as_str()).collect();
            if text.contains(needle) {
                return saw_full;
            }
        }
        panic!("never saw {needle}");
    }

    await_marker(&mut client, "ALPHA_MARKER").await;

    // A watch for a pane that does not exist is refused, and — the part that
    // matters — does not take the pane we were already streaming with it.
    //
    // Read past frames rather than demanding the error be the very next
    // message: the pane we are watching keeps producing output, so one of its
    // frames can be in flight when the refusal is sent. Insisting on message
    // order here tests the scheduler, not the daemon.
    let bogus = ClientMsg::Watch { pane: butai_protocol::PaneId(9999) };
    client.send(encode(&bogus, Encoding::Json).unwrap()).await.unwrap();
    let mut refusal = None;
    for _ in 0..50 {
        let frame = tokio::time::timeout(Duration::from_secs(5), client.next())
            .await
            .expect("timed out")
            .expect("connection closed")
            .expect("read error");
        match decode::<ServerMsg>(&frame, Encoding::Json).unwrap() {
            ServerMsg::Error(e) => {
                refusal = Some(e);
                break;
            }
            ServerMsg::Frame(_) => continue,
            other => panic!("unexpected message while awaiting the refusal: {other:?}"),
        }
    }
    let refusal = refusal.expect("the daemon never refused the bogus watch");
    assert!(refusal.contains("9999"), "unhelpful refusal: {refusal}");

    // The real thing: the new pane's output arrives, and it arrives behind a
    // full repaint rather than as a diff against the pane we were showing.
    // Nothing else would send a full frame here — no resize, no reattach — so
    // this is specifically the watch clearing the screen.
    client.send(encode(&ClientMsg::Watch { pane: beta }, Encoding::Json).unwrap()).await.unwrap();
    let saw_full = await_marker(&mut client, "BETA_MARKER").await;
    assert!(saw_full, "a watch must repaint in full, not diff against the previous pane");
}

/// A pane connection can scroll the pane it is holding open.
///
/// `ScrollPage` used to look only at the client's *workspace*, and a `pane`
/// attach has none — so scrollback was the one thing a client streaming a pane
/// could not do to the pane already on its screen. Every non-TUI client attaches
/// that way, and so does the TUI now that it draws its own chrome.
#[tokio::test]
async fn a_pane_connection_scrolls_the_pane_it_is_streaming() {
    use butai_protocol::api::WorkspaceDetail;
    use butai_protocol::framing::{decode, encode, length_codec};
    use butai_protocol::{AttachTarget, ClientMsg, Command, Encoding, ServerMsg, PROTOCOL_VERSION};
    use futures::{SinkExt, StreamExt};
    use tokio_util::codec::Framed;

    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;
    let (status, body) = http(&socket, "POST", "/v1/workspaces", Some(r#"{"name":"w"}"#)).await;
    assert_eq!(status, 201, "body: {body}");

    // Far more lines than the 24-row window, so most of them are scrollback.
    let body = r#"{"name":"counter","command":"seq 1 300; sleep 60"}"#;
    let (status, body) = http(&socket, "POST", "/v1/workspaces/1/processes", Some(body)).await;
    assert_eq!(status, 200, "body: {body}");
    let body =
        poll_until(&socket, "/v1/workspaces/1", "the process", |b| b.contains("counter")).await;
    let detail: WorkspaceDetail = serde_json::from_str(&body).unwrap();
    let pane = detail.processes.iter().find(|p| p.name == "counter").expect("counter").pane;

    let stream = UnixStream::connect(&socket).await.unwrap();
    let mut client = Framed::new(stream, length_codec());
    let hello = ClientMsg::Hello {
        proto_version: PROTOCOL_VERSION,
        encoding: Encoding::Json,
        cols: 80,
        rows: 24,
        target: AttachTarget::Pane { pane },
        cwd: PathBuf::from("/"),
    };
    client.send(encode(&hello, Encoding::Json).unwrap()).await.unwrap();

    // Frames are *cell*-level diffs, so a needle has to be looked for on a
    // reconstructed screen rather than in one frame's payload: scrolling `283`
    // to `260` sends the two characters that differ, and the number never
    // appears whole on the wire.
    let mut grid = vec![vec![' '; 80]; 24];
    async fn await_text(
        client: &mut Framed<UnixStream, tokio_util::codec::LengthDelimitedCodec>,
        grid: &mut Vec<Vec<char>>,
        needle: &str,
        what: &str,
    ) {
        let text = |g: &Vec<Vec<char>>| {
            g.iter().map(|r| r.iter().collect::<String>()).collect::<Vec<_>>().join("\n")
        };
        // One overall deadline rather than one per frame: an idle pane still
        // gets the occasional frame, so a per-frame timeout never fires and a
        // failure would take as long as the iteration count allows.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if text(grid).contains(needle) {
                return;
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(!left.is_zero(), "timed out waiting for {what}; screen:\n{}", text(grid));
            let Ok(frame) = tokio::time::timeout(left, client.next()).await else {
                panic!("timed out waiting for {what}; screen:\n{}", text(grid));
            };
            let frame = frame.expect("connection closed").expect("read error");
            let ServerMsg::Frame(update) = decode::<ServerMsg>(&frame, Encoding::Json).unwrap()
            else {
                continue;
            };
            for run in &update.cells {
                for (i, cell) in run.cells.iter().enumerate() {
                    let (x, y) = (run.x as usize + i, run.y as usize);
                    if y < grid.len() && x < grid[0].len() {
                        grid[y][x] = cell.ch.chars().next().unwrap_or(' ');
                    }
                }
            }
        }
    }

    // The tail is on screen; the window is 24 rows, so `300` is showing and
    // anything below ~277 has already scrolled off.
    await_text(&mut client, &mut grid, "300", "the end of the output").await;
    let before = grid.iter().map(|r| r.iter().collect::<String>()).collect::<Vec<_>>().join("\n");
    assert!(!before.contains("260"), "260 was already visible; the test proves nothing");

    let scroll = ClientMsg::Command(Command::ScrollPage(-1));
    client.send(encode(&scroll, Encoding::Json).unwrap()).await.unwrap();
    // A line that was *not* on screen a moment ago, so seeing it can only mean
    // the viewport moved back into the scrollback.
    await_text(&mut client, &mut grid, "260", "a line from before the visible window").await;
}

/// `Watch` on a connection that is not streaming a pane is refused rather than
/// silently reinterpreted. A control client has no viewport to re-point.
#[tokio::test]
async fn watch_is_refused_on_a_non_pane_connection() {
    use butai_protocol::framing::{decode, encode, length_codec};
    use butai_protocol::{AttachTarget, ClientMsg, Encoding, ServerMsg, PROTOCOL_VERSION};
    use futures::{SinkExt, StreamExt};
    use tokio_util::codec::Framed;

    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;
    let (status, _) = http(&socket, "POST", "/v1/workspaces", Some(r#"{"name":"w"}"#)).await;
    assert_eq!(status, 201);

    let stream = UnixStream::connect(&socket).await.unwrap();
    let mut client = Framed::new(stream, length_codec());
    let hello = ClientMsg::Hello {
        proto_version: PROTOCOL_VERSION,
        encoding: Encoding::Json,
        cols: 80,
        rows: 24,
        target: AttachTarget::Control,
        cwd: PathBuf::from("/"),
    };
    client.send(encode(&hello, Encoding::Json).unwrap()).await.unwrap();
    let _ = client.next().await.unwrap().unwrap(); // server hello

    client
        .send(
            encode(&ClientMsg::Watch { pane: butai_protocol::PaneId(1) }, Encoding::Json).unwrap(),
        )
        .await
        .unwrap();
    let frame = client.next().await.unwrap().unwrap();
    match decode::<ServerMsg>(&frame, Encoding::Json).unwrap() {
        ServerMsg::Error(e) => assert!(e.contains("pane connection"), "unhelpful refusal: {e}"),
        other => panic!("expected an error, got {other:?}"),
    }
}

/// A workspace directory that never answers must not freeze the whole daemon.
///
/// Regression for the incident where a workspace on an unreachable filesystem —
/// an unmounted share, a dropped VPN, a hung NFS/SMB server — wedged the core
/// actor. The blocking `read`/`stat`/`git` ran on the single thread that serves
/// every workspace and every attached client, so nothing repainted and no
/// workspace could be closed, not even a healthy one on a local disk. Requests
/// that touch the filesystem now run on a blocking thread.
///
/// A FIFO with no writer stands in for the dead mount: reading it blocks in the
/// kernel exactly as the mount does, but deterministically and locally.
// Multi-threaded like the real daemon, so the core loop, client I/O and the
// blocking pool sit on separate workers — the setting the fix is about.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hung_workspace_directory_does_not_freeze_the_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    let stuck = tmp.path().join("stuck");
    let healthy = tmp.path().join("healthy");
    std::fs::create_dir(&stuck).unwrap();
    std::fs::create_dir(&healthy).unwrap();
    let fifo = stuck.join("hang");
    assert!(
        std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success(),
        "mkfifo failed"
    );

    for (name, dir) in [("stuck", &stuck), ("healthy", &healthy)] {
        let body = format!(r#"{{"name":"{name}","path":"{}"}}"#, dir.display());
        let (status, body) = http(&socket, "POST", "/v1/workspaces", Some(&body)).await;
        assert_eq!(status, 201, "creating {name}: {body}");
    }

    // One client reads a file that will never return.
    let hung = socket.clone();
    tokio::spawn(async move {
        let _ = http(&hung, "GET", "/v1/workspaces/1/file?path=hang", None).await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The daemon still answers everything else...
    let (status, body) =
        tokio::time::timeout(Duration::from_secs(5), http(&socket, "GET", "/v1/workspaces", None))
            .await
            .expect("workspace listing froze behind the hung read");
    assert_eq!(status, 200, "body: {body}");
    assert!(body.contains("\"name\":\"healthy\""), "body: {body}");

    // ...including detail for the stuck workspace itself, which reads no files.
    let (status, body) = tokio::time::timeout(
        Duration::from_secs(5),
        http(&socket, "GET", "/v1/workspaces/1", None),
    )
    .await
    .expect("workspace detail froze behind the hung read");
    assert_eq!(status, 200, "body: {body}");

    // ...and the healthy workspace can still be closed, which is what the bug
    // took away.
    let (status, body) = tokio::time::timeout(
        Duration::from_secs(5),
        http(&socket, "DELETE", "/v1/workspaces/2", None),
    )
    .await
    .expect("closing a healthy workspace froze behind the hung read");
    assert_eq!(status, 200, "body: {body}");

    // Let the parked read finish: opening the FIFO for writing and closing it
    // gives the reader EOF. Otherwise the runtime would wait for that blocking
    // task at teardown and hang this test instead of the daemon.
    drop(std::fs::OpenOptions::new().write(true).open(&fifo).unwrap());
}

/// Spawn a shell agent in a fresh workspace and return its pane id.
async fn shell_agent_pane(socket: &PathBuf, name: &str) -> u64 {
    let (status, body) =
        http(socket, "POST", "/v1/workspaces", Some(&format!(r#"{{"name":"{name}"}}"#))).await;
    assert_eq!(status, 201, "body: {body}");
    let (status, body) =
        http(socket, "POST", "/v1/workspaces/1/agents", Some(r#"{"type":"sh"}"#)).await;
    assert!(status == 200 || status == 201, "body: {body}");
    first_agent_pane(socket).await
}

/// A `butai` typed after `ssh` announces where it is, and the daemon *reports*
/// that rather than acting on it.
///
/// The daemon is the only party that can detect this — it parses every byte a
/// pane writes — but connecting a second machine is a client decision: whose
/// tab bar those projects land in is a property of the client, and one daemon
/// dialling another to answer it is the relay this refactor removes.
///
/// There is nothing to turn off to make this unambiguous any more: the daemon
/// does not dial at all, so "it reported" is the only thing that can happen
/// here. `remote_auto_attach` is the client's, and gates the client's dial.
#[tokio::test]
async fn a_machine_announcing_itself_is_reported_to_clients() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("butai.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut config = Config::with_defaults();
    config.general.default_shell = Some("/bin/sh".into());
    config.agents.clear();
    config.agents.push(butai_server::config::AgentDef {
        name: "sh".into(),
        command: "/bin/sh".into(),
        args: Vec::new(),
        resume_args: Vec::new(),
        env: Default::default(),
        waiting_pattern: None,
        busy_pattern: None,
    });
    tokio::spawn(butai_server::daemon::serve(listener, config, None));
    let pane = shell_agent_pane(&socket, "ssh-host").await;

    let sock = socket.clone();
    let events = tokio::spawn(async move { read_events_for(&sock, Duration::from_secs(3)).await });
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Exactly what a far-side `butai` writes to its stdout, which is this pane's
    // PTY: `ESC _ butai;here;<hint>;<socket> ESC \`.
    let apc = "\\033_butai;here;user@far;/run/user/1000/butai/butai.sock\\033\\\\";
    let line = format!("printf '{apc}'\n");
    let body = serde_json::json!({ "paste": line }).to_string();
    let (status, reply) =
        http(&socket, "POST", &format!("/v1/workspaces/1/panes/{pane}/input"), Some(&body)).await;
    assert_eq!(status, 200, "body: {reply}");

    let stream = events.await.unwrap();
    let record = stream
        .lines()
        .find(|l| l.contains("remote_announce"))
        .unwrap_or_else(|| panic!("no announcement in the stream:\n{stream}"));
    let data = record.trim_start_matches("data:").trim();
    let event: serde_json::Value = serde_json::from_str(data).unwrap_or_else(|e| {
        panic!("announcement is not JSON ({e}): {data}");
    });
    let a = &event["data"];
    assert_eq!(a["hint"], "user@far", "{a}");
    assert_eq!(a["socket"], "/run/user/1000/butai/butai.sock", "{a}");
    assert_eq!(a["pane"], pane, "the announcement should name the pane it came from: {a}");
    // Nothing recovered the ssh arguments here — the pane's foreground process
    // is a shell, not an ssh — so the client has only the hint to go on, and
    // the field is present and empty rather than missing.
    assert_eq!(a["ssh_target"], "", "{a}");
    assert!(a["ssh_args"].as_array().is_some_and(|v| v.is_empty()), "{a}");
}

/// Search is the one thing 4b needed that had no route.
///
/// It stays server-side because the *files* are: a workspace on another machine
/// is reachable only through its own daemon, so "search this project" cannot be
/// client work however much of the rest of the interface is. Which means it has
/// to be surface every client gets, not a TUI side channel — this is the test
/// that it is one.
#[tokio::test]
async fn search_finds_a_file_by_name_and_a_line_by_content() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/needle_named.rs"), "nothing here\n").unwrap();
    std::fs::write(dir.join("src/other.rs"), "fn a() {}\nlet found = 1;\n").unwrap();

    let body = format!(r#"{{"name":"p","path":"{}"}}"#, dir.display());
    let (status, body) = http(&socket, "POST", "/v1/workspaces", Some(&body)).await;
    assert_eq!(status, 201, "body: {body}");

    // A filename match: no line, no preview.
    let (status, body) = http(&socket, "GET", "/v1/workspaces/1/search?q=needle", None).await;
    assert_eq!(status, 200, "body: {body}");
    assert!(body.contains("needle_named.rs"), "{body}");

    // A content match: the line it is on, and the line itself.
    let (status, body) = http(&socket, "GET", "/v1/workspaces/1/search?q=found", None).await;
    assert_eq!(status, 200, "body: {body}");
    let dto: serde_json::Value = serde_json::from_str(&body).unwrap();
    let hit = dto["hits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["path"].as_str().is_some_and(|p| p.ends_with("other.rs")))
        .unwrap_or_else(|| panic!("no content hit: {body}"));
    assert_eq!(hit["line"], 2, "{hit}");
    assert!(hit["preview"].as_str().is_some_and(|p| p.contains("found")), "{hit}");

    // A query is required, and a bad workspace is a 404 rather than a panic.
    let (status, _) = http(&socket, "GET", "/v1/workspaces/1/search", None).await;
    assert_eq!(status, 400, "a search with no query should be refused");
    let (status, _) = http(&socket, "GET", "/v1/workspaces/9/search?q=x", None).await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn pane_output_returns_plain_text() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon_with_shell_agent(&tmp).await;
    let pane = shell_agent_pane(&socket, "read").await;

    let (status, body) = http(
        &socket,
        "POST",
        &format!("/v1/workspaces/1/panes/{pane}/input"),
        Some(r#"{"paste":"echo hello-from-the-pane\n"}"#),
    )
    .await;
    assert_eq!(status, 200, "body: {body}");

    let path = format!("/v1/workspaces/1/panes/{pane}/output");
    let body =
        poll_until(&socket, &path, "the echoed line", |b| b.contains("hello-from-the-pane")).await;

    // Plain text means no escape sequences survive into the rows.
    assert!(!body.contains("\\u001b"), "text format must strip escapes: {body}");
    assert!(body.contains("\"alt_screen\":false"), "body: {body}");
    assert!(body.contains("\"source\":\"scrollback\""), "body: {body}");
    assert!(body.contains("\"format\":\"text\""), "body: {body}");
    // Trailing blank grid rows are padding, not output: the last line has text.
    let lines = body.split("\"lines\":[").nth(1).expect("lines array");
    let lines = lines.split(']').next().unwrap();
    assert!(!lines.trim_end().ends_with(r#""""#), "trailing blanks not trimmed: {lines}");
}

#[tokio::test]
async fn pane_output_can_return_ansi() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon_with_shell_agent(&tmp).await;
    let pane = shell_agent_pane(&socket, "ansi").await;

    let (status, _) = http(
        &socket,
        "POST",
        &format!("/v1/workspaces/1/panes/{pane}/input"),
        Some(r#"{"paste":"printf '\\033[31mREDTEXT\\033[0m\\n'\n"}"#),
    )
    .await;
    assert_eq!(status, 200);

    // Wait for a real escape byte, not just the text: the shell echoes the
    // typed command line first, and there the `\033` is four literal chars.
    let path = format!("/v1/workspaces/1/panes/{pane}/output?format=ansi");
    let body = poll_until(&socket, &path, "colored output", |b| b.contains("\\u001b[31m")).await;
    assert!(body.contains("REDTEXT"), "body: {body}");

    // ...and the same read in text format must not.
    let text_path = format!("/v1/workspaces/1/panes/{pane}/output?format=text");
    let (status, text) = http(&socket, "GET", &text_path, None).await;
    assert_eq!(status, 200);
    assert!(text.contains("REDTEXT"), "body: {text}");
    assert!(!text.contains("\\u001b"), "text format must strip escapes: {text}");
}

#[tokio::test]
async fn pane_output_does_not_clear_a_bell() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon_with_shell_agent(&tmp).await;
    let pane = shell_agent_pane(&socket, "bellread").await;

    let (status, _) = http(
        &socket,
        "POST",
        &format!("/v1/workspaces/1/panes/{pane}/input"),
        Some(r#"{"paste":"printf '\\a'\n"}"#),
    )
    .await;
    assert_eq!(status, 200);
    poll_until(&socket, "/v1/workspaces/1/agents", "waiting after the bell", |b| {
        b.contains("\"state\":\"waiting\"")
    })
    .await;

    // Read the pane several times. Unlike `ack` or a framed attach, this is a
    // query: it must leave the bell — and so the agent's `waiting` — alone.
    for _ in 0..3 {
        let (status, _) =
            http(&socket, "GET", &format!("/v1/workspaces/1/panes/{pane}/output"), None).await;
        assert_eq!(status, 200);
    }
    let (status, agents) = http(&socket, "GET", "/v1/workspaces/1/agents", None).await;
    assert_eq!(status, 200);
    assert!(
        agents.contains("\"state\":\"waiting\""),
        "reading a pane must not acknowledge its bell: {agents}"
    );
}

#[tokio::test]
async fn pane_output_does_not_resize_the_pane() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon_with_shell_agent(&tmp).await;
    let pane = shell_agent_pane(&socket, "geom").await;

    let path = format!("/v1/workspaces/1/panes/{pane}/output");
    let (status, before) = http(&socket, "GET", &path, None).await;
    assert_eq!(status, 200, "body: {before}");
    let geom = |b: &str| {
        let cols = b.split("\"cols\":").nth(1).unwrap().split(',').next().unwrap().to_string();
        let rows = b.split("\"rows\":").nth(1).unwrap().split(',').next().unwrap().to_string();
        (cols, rows)
    };
    let first = geom(&before);

    // A framed attach resizes the pane to the reader's dimensions; this must
    // not, however many times or at whatever length it is called.
    for q in ["?lines=1", "?lines=5000", "?source=screen", "?source=footer"] {
        let (status, body) = http(&socket, "GET", &format!("{path}{q}"), None).await;
        assert_eq!(status, 200, "GET {q}: {body}");
        assert_eq!(geom(&body), first, "read with {q} changed the pane geometry");
    }
}

#[tokio::test]
async fn pane_output_footer_is_the_detection_band() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon_with_shell_agent(&tmp).await;
    let pane = shell_agent_pane(&socket, "footer").await;

    let path = format!("/v1/workspaces/1/panes/{pane}/output?source=footer");
    let (status, body) = http(&socket, "GET", &path, None).await;
    assert_eq!(status, 200, "body: {body}");
    assert!(body.contains("\"source\":\"footer\""), "body: {body}");
    let lines = body.split("\"lines\":[").nth(1).unwrap().split(']').next().unwrap();
    let count = if lines.trim().is_empty() { 0 } else { lines.matches("\",\"").count() + 1 };
    assert!(count <= 8, "the footer band is 8 rows, got {count}: {lines}");
}

#[tokio::test]
async fn pane_output_rejects_nonsense_and_missing_panes() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon_with_shell_agent(&tmp).await;
    let pane = shell_agent_pane(&socket, "bad").await;

    let base = format!("/v1/workspaces/1/panes/{pane}/output");
    for (q, want) in
        [("?source=sideways", "source"), ("?format=morse", "format"), ("?lines=lots", "lines")]
    {
        let (status, body) = http(&socket, "GET", &format!("{base}{q}"), None).await;
        assert_eq!(status, 400, "GET {q} should be a bad request: {body}");
        assert!(body.contains(want), "error should name {want}: {body}");
    }

    let (status, body) = http(&socket, "GET", "/v1/workspaces/1/panes/9999/output", None).await;
    assert_eq!(status, 404, "body: {body}");
}

#[tokio::test]
async fn a_background_agent_does_not_take_the_stage() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon_with_shell_agent(&tmp).await;
    let first = shell_agent_pane(&socket, "stage").await;

    // `stage` may be the object's last field, so stop at the first non-digit
    // rather than at a comma.
    let stage_of = |b: &str| -> String {
        b.split("\"stage\":").nth(1).unwrap().chars().take_while(|c| c.is_ascii_digit()).collect()
    };
    let (status, body) = http(&socket, "GET", "/v1/workspaces/1", None).await;
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(stage_of(&body), first.to_string(), "the first agent takes the stage");

    // A helper spawned in the background must not yank the view: whoever is
    // watching the first agent keeps watching it.
    let (status, body) = http(
        &socket,
        "POST",
        "/v1/workspaces/1/agents",
        Some(r#"{"type":"sh","background":true}"#),
    )
    .await;
    assert!(status == 200 || status == 201, "body: {body}");

    let (status, body) = http(&socket, "GET", "/v1/workspaces/1", None).await;
    assert_eq!(status, 200);
    assert_eq!(stage_of(&body), first.to_string(), "background spawn stole the stage: {body}");
    assert!(body.matches("\"pane\":").count() >= 2, "both agents should exist: {body}");

    // ...and the default is unchanged: a plain spawn still takes the stage.
    let (status, body) =
        http(&socket, "POST", "/v1/workspaces/1/agents", Some(r#"{"type":"sh"}"#)).await;
    assert!(status == 200 || status == 201, "body: {body}");
    let (status, body) = http(&socket, "GET", "/v1/workspaces/1", None).await;
    assert_eq!(status, 200);
    assert_ne!(stage_of(&body), first.to_string(), "a normal spawn must still stage: {body}");
}

/// Every pane carries its own identity, so a program running inside one can
/// address butai without being told where it is. The socket assertion is the
/// load-bearing one: `paths::socket_path()` re-reads the *daemon's* own
/// `$BUTAI_SOCKET`, which is unset here, so a pane that was handed that would
/// get the default path and shell back to the wrong daemon entirely.
#[tokio::test]
async fn panes_carry_their_own_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon_with_shell_agent(&tmp).await;
    let work = tmp.path().join("work");
    std::fs::create_dir(&work).unwrap();

    let body = format!(r#"{{"name":"ident","path":"{}"}}"#, work.display());
    let (status, body) = http(&socket, "POST", "/v1/workspaces", Some(&body)).await;
    assert_eq!(status, 201, "body: {body}");

    // A *process* pane, not an agent: identity has to reach both, and the
    // process path is the one that used to pass an empty environment.
    let out = work.join("id.txt");
    let cmd = format!(
        r#"{{"name":"ident","command":"sh -c 'printenv BUTAI_PANE > {0}; printenv BUTAI_WORKSPACE >> {0}; printenv BUTAI_SOCKET >> {0}; sleep 30'"}}"#,
        out.display()
    );
    let (status, body) = http(&socket, "POST", "/v1/workspaces/1/processes", Some(&cmd)).await;
    assert!(status == 200 || status == 201, "body: {body}");

    let mut text = String::new();
    for _ in 0..300 {
        if let Ok(s) = std::fs::read_to_string(&out) {
            if s.lines().count() >= 3 {
                text = s;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.len() >= 3, "pane never reported its identity, got {text:?}");

    assert!(lines[0].parse::<u64>().is_ok(), "BUTAI_PANE should be a pane id: {:?}", lines[0]);
    assert_eq!(lines[1], "1", "BUTAI_WORKSPACE should name this workspace");
    assert_eq!(
        lines[2],
        socket.to_string_lossy(),
        "BUTAI_SOCKET must be the socket the daemon actually bound"
    );

    // And the id it reports is really its own row in the pane list.
    let (status, procs) = http(&socket, "GET", "/v1/workspaces/1/processes", None).await;
    assert_eq!(status, 200);
    assert!(
        procs.contains(&format!("\"pane\":{}", lines[0])),
        "pane {} is not in the process list: {procs}",
        lines[0]
    );
}

/// Collect `data:` records from the SSE stream at `GET /v1/events` until `want`
/// returns true, or `budget` elapses. Returns every record seen.
///
/// Not built on [`http`]: that reads to EOF, and this response never ends.
async fn sse_collect(
    socket: &PathBuf,
    budget: Duration,
    mut want: impl FnMut(&[String]) -> bool,
) -> Vec<String> {
    let mut stream = UnixStream::connect(socket).await.unwrap();
    let req = "GET /v1/events HTTP/1.1\r\nHost: butai\r\nAccept: text/event-stream\r\n\r\n";
    stream.write_all(req.as_bytes()).await.unwrap();

    let mut raw = Vec::new();
    let mut records: Vec<String> = Vec::new();
    let deadline = tokio::time::Instant::now() + budget;
    let mut buf = [0u8; 8192];
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(250), stream.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => raw.extend_from_slice(&buf[..n]),
            Ok(Err(e)) => panic!("sse read failed: {e}"),
            // A quiet stretch on the stream is normal; the deadline decides.
            Err(_) => continue,
        }
        let text = String::from_utf8_lossy(&raw).into_owned();
        records =
            text.lines().filter_map(|l| l.strip_prefix("data: ")).map(str::to_string).collect();
        if want(&records) {
            break;
        }
    }
    records
}

/// The rails a client draws have to arrive as events, not only as replies to a
/// poll. `workspaces` carries counts — enough to badge a tab, not enough to draw
/// AGENTS / PROCESSES / CHANGES — so a client rendering those itself was left
/// polling `/v1/workspaces/{id}` on a timer and running a second behind the pane
/// it draws beside.
#[tokio::test]
async fn workspace_detail_is_pushed_on_the_events_stream() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon_with_shell_agent(&tmp).await;
    let (status, body) = http(&socket, "POST", "/v1/workspaces", Some(r#"{"name":"pushy"}"#)).await;
    assert_eq!(status, 201, "body: {body}");

    // Subscribe first, then cause a rail change — a spawned agent is a new
    // AGENTS row, which is precisely what a poller would have been late for.
    let sock = socket.clone();
    let collector = tokio::spawn(async move {
        sse_collect(&sock, Duration::from_secs(20), |recs| {
            recs.iter().any(|r| r.contains(r#""workspace_detail""#) && r.contains(r#""agents""#))
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (status, body) =
        http(&socket, "POST", "/v1/workspaces/1/agents", Some(r#"{"type":"sh"}"#)).await;
    assert!(status == 200 || status == 201, "spawn agent failed: {status} {body}");

    let records = collector.await.unwrap();
    let details: Vec<&String> =
        records.iter().filter(|r| r.contains(r#""workspace_detail""#)).collect();
    assert!(!details.is_empty(), "no workspace_detail was pushed; events seen: {records:?}");

    // It carries the rails themselves, which is the whole difference from the
    // `workspaces` event.
    let last = details.last().unwrap();
    assert!(last.contains(r#""agents""#), "detail has no agents rail: {last}");
    assert!(last.contains(r#""processes""#), "detail has no processes rail: {last}");
    assert!(last.contains(r#""name":"pushy""#), "detail names the wrong workspace: {last}");
}

/// The push is diffed against the last one sent. `dirty` is set by pane output,
/// so `broadcast_ws_details` runs on nearly every frame while the rails it
/// describes change orders of magnitude less often; undiffed, a subscriber on a
/// slow link would get a full snapshot per workspace per frame.
///
/// Asserted as the contract rather than as a rate, so a loaded machine cannot
/// flake it: two pushes in a row for one workspace are never identical.
#[tokio::test]
async fn an_unchanged_workspace_is_not_pushed_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon_with_shell_agent(&tmp).await;
    let pane = shell_agent_pane(&socket, "quiet").await;

    // Keep a shell talking for the whole window, so frames really are being
    // produced and the diff is under load rather than idle.
    let sock = socket.clone();
    let noise = tokio::spawn(async move {
        for _ in 0..40 {
            let _ = http(
                &sock,
                "POST",
                &format!("/v1/workspaces/1/panes/{pane}/input"),
                Some(r#"{"paste":"echo tick\n"}"#),
            )
            .await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });

    let records = sse_collect(&socket, Duration::from_secs(4), |_| false).await;
    noise.abort();

    let details: Vec<&String> =
        records.iter().filter(|r| r.contains(r#""workspace_detail""#)).collect();
    // Without this the pairwise check below passes vacuously on a stream that
    // pushed nothing at all, which is the failure it is least able to notice.
    assert!(!details.is_empty(), "no detail was pushed under load; events seen: {records:?}");
    for pair in details.windows(2) {
        assert_ne!(
            pair[0], pair[1],
            "the same detail was pushed twice in a row — the diff is not working"
        );
    }
}

/// A repo at `tmp/<name>` with a real merge, a tag, a second branch and a
/// stash — the shapes a commit graph is made of, and the ones a linear fixture
/// cannot produce. Returns the repo directory.
fn merged_repo(tmp: &tempfile::TempDir, name: &str) -> PathBuf {
    let dir = tmp.path().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let repo = git2::Repository::init(&dir).unwrap();
    let mut cfg = repo.config().unwrap();
    cfg.set_str("user.name", "Test").unwrap();
    cfg.set_str("user.email", "test@example.com").unwrap();

    let commit = |repo: &git2::Repository, file: &str, msg: &str, parents: &[&git2::Commit]| {
        std::fs::write(dir.join(file), format!("{msg}\n")).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(file)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        let oid = repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, parents).unwrap();
        repo.find_commit(oid).unwrap().id()
    };

    let mut repo = repo;
    let root = commit(&repo, "a.txt", "root", &[]);
    repo.tag_lightweight("v0.1", &repo.find_object(root, None).unwrap(), false).unwrap();

    // A side branch off the root, then main moves on, then they merge — so the
    // tip has two parents and the walk has two lanes to reconcile.
    let side = {
        let root_c = repo.find_commit(root).unwrap();
        repo.branch("side", &root_c, false).unwrap();
        repo.set_head("refs/heads/side").unwrap();
        commit(&repo, "b.txt", "side work", &[&root_c])
    };
    let main_tip = {
        repo.set_head("refs/heads/main").unwrap();
        repo.reset(&repo.find_object(root, None).unwrap(), git2::ResetType::Hard, None).unwrap();
        let root_c = repo.find_commit(root).unwrap();
        commit(&repo, "a.txt", "main moves on", &[&root_c])
    };
    {
        // A real merge tree: main's `a.txt` *and* the side branch's `b.txt`.
        // Committing two parents over a tree that never took the side's file
        // makes a merge whose first-parent diff is empty, which is a fixture
        // that cannot fail the thing it is here to check.
        std::fs::write(dir.join("b.txt"), "side work\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("b.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        let (main_c, side_c) =
            (repo.find_commit(main_tip).unwrap(), repo.find_commit(side).unwrap());
        repo.commit(Some("HEAD"), &sig, &sig, "merge side", &tree, &[&main_c, &side_c]).unwrap();
    }

    // A stash: two synthetic commits under `refs/stash` that must NOT show up
    // as history.
    std::fs::write(dir.join("a.txt"), "dirty\n").unwrap();
    let sig = repo.signature().unwrap();
    repo.stash_save(&sig, "wip", None).unwrap();
    dir
}

/// The four fields a commit graph is drawn from, over the wire.
///
/// `parents` and `refs` are the only things in the GIT page a client cannot
/// derive for itself — the relation lives in the object database, not in the
/// page of commits it was handed — so this is the test that says the daemon
/// really sends them.
#[tokio::test]
async fn the_log_carries_parents_refs_and_a_topological_order() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;
    let repo = merged_repo(&tmp, "graph");
    open_repo_workspace(&socket, &repo, "\"branch\"").await;

    let (status, body) =
        http(&socket, "GET", "/v1/workspaces/1/git/log?all=1&limit=20", None).await;
    assert_eq!(status, 200, "body: {body}");
    let log: serde_json::Value = serde_json::from_str(&body).unwrap();
    let commits = log["commits"].as_array().unwrap();

    let summary_of = |c: &serde_json::Value| c["summary"].as_str().unwrap().to_string();
    let merge = commits.iter().find(|c| summary_of(c) == "merge side").expect("merge commit");
    assert_eq!(
        merge["parents"].as_array().unwrap().len(),
        2,
        "a merge with one parent is a merge nobody can draw: {merge}"
    );
    let root = commits.iter().find(|c| summary_of(c) == "root").expect("root commit");
    assert!(root["parents"].as_array().unwrap().is_empty(), "the root grew a parent");

    // Decoration, told apart by kind rather than guessed from the name.
    let refs_of = |c: &serde_json::Value| -> Vec<(String, String)> {
        c["refs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["name"].as_str().unwrap().into(), r["kind"].as_str().unwrap().into()))
            .collect()
    };
    assert!(
        refs_of(root).contains(&("v0.1".into(), "tag".into())),
        "the tag is not on the root: {:?}",
        refs_of(root)
    );
    assert!(
        refs_of(merge).iter().any(|(n, k)| n == "main" && k == "branch"),
        "the branch tip is not decorated: {:?}",
        refs_of(merge)
    );

    // A stash is two synthetic commits. Showing them as history is the bug
    // `--all` would have shipped.
    for c in commits {
        let s = summary_of(c);
        assert!(!s.starts_with("WIP on") && !s.starts_with("index on"), "a stash leaked in: {s}");
    }

    // Topological order: a parent never precedes its child, which is the whole
    // precondition for assigning lanes in one pass down the page.
    let mut seen: Vec<&str> = Vec::new();
    for c in commits {
        for p in c["parents"].as_array().unwrap() {
            assert!(
                !seen.contains(&p.as_str().unwrap()),
                "parent {} was listed before its child {}",
                p,
                c["id"]
            );
        }
        seen.push(c["id"].as_str().unwrap());
    }
}

/// `?all=` and `?rev=` name different walks. Silently preferring one turns a
/// client's bug into a daemon that quietly answers a question nobody asked.
#[tokio::test]
async fn the_log_refuses_all_and_rev_together() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;
    let repo = merged_repo(&tmp, "graph");
    open_repo_workspace(&socket, &repo, "\"branch\"").await;

    let (status, body) =
        http(&socket, "GET", "/v1/workspaces/1/git/log?all=1&rev=main", None).await;
    assert_eq!(status, 400, "body: {body}");
}

/// The old name list and the new entry list describe the same branches. They
/// are read by different clients — the picker takes names, the GIT page takes
/// entries — and a daemon where they disagree offers two different repos.
#[tokio::test]
async fn branches_carry_their_tips_and_agree_with_the_name_list() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;
    let repo = merged_repo(&tmp, "graph");
    open_repo_workspace(&socket, &repo, "\"branch\"").await;

    let (status, body) = http(&socket, "GET", "/v1/workspaces/1/branches", None).await;
    assert_eq!(status, 200, "body: {body}");
    let dto: serde_json::Value = serde_json::from_str(&body).unwrap();

    let names: Vec<&str> =
        dto["branches"].as_array().unwrap().iter().map(|n| n.as_str().unwrap()).collect();
    let locals: Vec<&str> = dto["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| !e["remote"].as_bool().unwrap())
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, locals, "the two branch lists disagree");
    assert!(names.contains(&"side"), "side branch missing: {names:?}");

    // Without an upstream there is nothing to be ahead of, and inventing a
    // number here is what would put a false `↓2` on the git space's badge.
    for e in dto["entries"].as_array().unwrap() {
        assert!(e["upstream"].is_null(), "invented an upstream: {e}");
        assert_eq!(e["ahead"].as_u64(), Some(0));
        assert_eq!(e["behind"].as_u64(), Some(0));
        assert_eq!(e["tip"].as_str().unwrap().len(), 40, "tip is not a full oid: {e}");
    }
}

/// `git show` on a merge diffs it against *every* parent, and a clean merge
/// differs from none of them — so the endpoint answered a header and no patch,
/// and the GIT page's body came up empty on exactly the commits people go
/// looking for. First-parent is the reading a person means by "what did this
/// merge bring in".
#[tokio::test]
async fn showing_a_merge_answers_with_what_it_brought_in() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;
    let repo = merged_repo(&tmp, "graph");
    open_repo_workspace(&socket, &repo, "\"branch\"").await;

    let (status, body) =
        http(&socket, "GET", "/v1/workspaces/1/git/log?all=1&limit=20", None).await;
    assert_eq!(status, 200, "body: {body}");
    let log: serde_json::Value = serde_json::from_str(&body).unwrap();
    let merge = log["commits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["parents"].as_array().unwrap().len() == 2)
        .expect("a merge commit");
    let id = merge["id"].as_str().unwrap();

    let (status, body) =
        http(&socket, "GET", &format!("/v1/workspaces/1/show?id={id}"), None).await;
    assert_eq!(status, 200, "body: {body}");
    let patch = serde_json::from_str::<serde_json::Value>(&body).unwrap()["patch"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        patch.contains("diff --git") && patch.contains("+side work"),
        "the merge's patch is missing the side branch's work: {patch}"
    );
}

/// `Accept-Encoding: gzip` is honoured, and its absence changes nothing.
///
/// The second half is the load-bearing half. Compression here is negotiated and
/// never volunteered, which is the only reason every shipped client and the rest
/// of this file — none of which send the header — kept working unchanged.
#[tokio::test]
async fn json_replies_compress_only_when_asked() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;
    // Enough workspaces to carry `/v1/workspaces` past the 1 KiB threshold.
    // Deliberately not `/v1/system`, whose size is a fact about the host: on a
    // machine with one interface and no GPU it is a couple of hundred bytes, and
    // a test that only compresses on the developer's laptop is not a test.
    for i in 0..10 {
        let (status, _) =
            http(&socket, "POST", "/v1/workspaces", Some(&format!(r#"{{"name":"gz{i}"}}"#))).await;
        assert_eq!(status, 201);
    }

    // A client that says nothing gets exactly what earlier daemons sent.
    let (head, plain) = http_raw(&socket, "/v1/workspaces", "").await;
    assert!(!head.contains("content-encoding"), "compressed unasked: {head}");
    assert!(plain.len() > 1024, "test needs a body over the threshold, got {}", plain.len());
    serde_json::from_slice::<serde_json::Value>(&plain).expect("plain body is json");

    // Asking gets it gzipped, and it inflates back to the very same bytes.
    let (head, gz) = http_raw(&socket, "/v1/workspaces", "Accept-Encoding: gzip\r\n").await;
    assert!(head.contains("content-encoding: gzip"), "not compressed: {head}");
    assert!(head.contains("vary: accept-encoding"), "no vary: {head}");
    assert!(gz.len() < plain.len(), "gzip made it bigger: {} -> {}", plain.len(), gz.len());
    assert_eq!(gunzip(&gz).as_bytes(), &plain[..], "inflated to something else");

    // `q=0` names gzip in order to refuse it — the one parse slip that would
    // send a client bytes it cannot read.
    let (head, _) = http_raw(&socket, "/v1/workspaces", "Accept-Encoding: gzip;q=0\r\n").await;
    assert!(!head.contains("content-encoding"), "q=0 was treated as an offer: {head}");

    // A named encoding we do not speak is not an invitation to pick another.
    let (head, _) = http_raw(&socket, "/v1/workspaces", "Accept-Encoding: br\r\n").await;
    assert!(!head.contains("content-encoding"), "answered br with something else: {head}");

    // Under the threshold: no encoding, but still `vary`, so nothing between
    // here and a client can cache one answer as the other.
    let (head, small) = http_raw(&socket, "/v1/agents", "Accept-Encoding: gzip\r\n").await;
    assert!(small.len() < 1024, "expected a short body, got {}", small.len());
    assert!(!head.contains("content-encoding"), "compressed a tiny body: {head}");
    assert!(head.contains("vary: accept-encoding"), "no vary on the short reply: {head}");
}

/// The event stream compresses too, and stays *a stream* while it does.
///
/// Flushing after every record is the whole trick: without it deflate holds a
/// record until its window fills, which on this stream is indistinguishable from
/// the daemon having nothing to say. Inflating a prefix — the connection is still
/// open, so there is no gzip trailer — is what proves the flush happened.
#[tokio::test]
async fn the_event_stream_compresses_and_still_streams() {
    use std::io::Read;

    let tmp = tempfile::tempdir().unwrap();
    let socket = start_daemon(&tmp).await;

    let mut stream = UnixStream::connect(&socket).await.unwrap();
    stream
        .write_all(b"GET /v1/events HTTP/1.1\r\nHost: butai\r\nAccept-Encoding: gzip\r\n\r\n")
        .await
        .unwrap();

    // Read a bounded prefix: this response never ends, so reading to EOF hangs.
    let mut buf = vec![0u8; 64 * 1024];
    let mut got = Vec::new();
    while got.len() < 200 {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf))
            .await
            .expect("event stream said nothing")
            .unwrap();
        assert_ne!(n, 0, "stream closed");
        got.extend_from_slice(&buf[..n]);
    }
    let split = got.windows(4).position(|w| w == b"\r\n\r\n").expect("headers end");
    let head = String::from_utf8_lossy(&got[..split]).to_ascii_lowercase();
    assert!(head.contains("content-encoding: gzip"), "stream not compressed: {head}");
    assert!(head.contains("text/event-stream"), "not an event stream: {head}");

    // De-chunk, then inflate as far as the bytes go. `GzDecoder` on a truncated
    // member yields what was flushed and then reports the truncation, so a
    // partial read is a pass as long as a whole record came out of it.
    let mut body = &got[split + 4..];
    let mut deflated = Vec::new();
    while let Some(eol) = body.windows(2).position(|w| w == b"\r\n") {
        let Ok(n) = usize::from_str_radix(&String::from_utf8_lossy(&body[..eol]), 16) else {
            break;
        };
        if n == 0 || body.len() < eol + 2 + n {
            break;
        }
        deflated.extend_from_slice(&body[eol + 2..eol + 2 + n]);
        body = &body[eol + 2 + n + 2..];
    }
    let mut text = String::new();
    let _ = flate2::read::GzDecoder::new(&deflated[..]).read_to_string(&mut text);
    assert!(text.contains("data: "), "no record inflated out of the prefix: {text:?}");
    assert!(text.contains("\"event\""), "record is not an ApiEvent: {text:?}");
}
