//! One daemon, as a client sees it: its REST face, its event stream, and the
//! state those two produce.
//!
//! This is the object the TUI holds — one per daemon, so several machines can
//! share a tab bar without any daemon relaying another. Everything a client
//! draws that is not a pane's cells comes from here.
//!
//! **Why the event stream and not polling.** The shipping clients poll
//! `/v1/workspaces/{id}` on a 1–2s timer, which is fine for a phone glancing at
//! a rail and wrong for a rail drawn beside the pane it describes: an agent that
//! starts working should light up with the output that proves it, not a second
//! later. `GET /v1/events` already pushes `workspace_detail` on the frame clock,
//! diffed against the last one sent, so subscribing costs an idle daemon
//! nothing.
//!
//! Unknown event tags are skipped rather than fatal — `docs/building-a-client.md`
//! requires that of every client, and it is what lets the daemon add an event
//! without a coordinated release.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use butai_protocol::api::{
    ApiEvent, GitOpDto, NotificationDto, SysDto, WorkspaceDetail, WorkspaceSummary,
};
use butai_protocol::SessionId;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::api::Api;

/// Most recent notifications kept for the attention notice. The daemon's own
/// ring is the source of truth; this is only what the footer might show.
const NOTIFICATION_CAP: usize = 64;

/// Refuse to buffer more than this between SSE record separators. A daemon
/// never emits a record anywhere near it, so exceeding it means the stream is
/// not what we think it is and reconnecting beats growing forever.
const MAX_SSE_RECORD: usize = 8 * 1024 * 1024;

const BACKOFF_MIN: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(10);

/// What the event-stream task reports upward.
///
/// Connection transitions are events in their own right rather than a flag the
/// TUI polls, because a tab whose daemon has gone away has to say so — the rails
/// it is showing are now a photograph.
#[derive(Debug)]
pub enum DaemonEvent {
    /// The event stream is open. Sent on first connect and after every recovery.
    Connected,
    /// The stream dropped; the task is already backing off to retry.
    Lost(String),
    /// One event from `GET /v1/events`.
    Api(Box<ApiEvent>),
}

/// Everything a client knows about one daemon without asking again.
///
/// Fed by [`State::apply`] from the event stream. Every field is last-value-wins
/// because that is what the daemon sends: `workspaces` and `workspace_detail`
/// are full snapshots, not deltas.
#[derive(Debug, Default)]
pub struct State {
    /// Every workspace on this daemon, with its attention counts — the tab bar.
    pub tabs: Vec<WorkspaceSummary>,
    /// Full rail contents per workspace, for the ones we have heard about.
    pub detail: HashMap<SessionId, WorkspaceDetail>,
    /// The SYSTEM gauges.
    pub system: SysDto,
    /// Recent agent transitions, newest last.
    pub notifications: VecDeque<NotificationDto>,
    /// The running (or last finished) git operation.
    pub git_op: Option<GitOpDto>,
    /// False while the event stream is down.
    pub connected: bool,
}

impl State {
    pub fn apply(&mut self, event: &ApiEvent) {
        match event {
            ApiEvent::System(sys) => self.system = sys.clone(),
            ApiEvent::Workspaces(list) => {
                self.tabs = list.clone();
                // A workspace that is gone from the list is gone: keeping its
                // detail would let a stale rail outlive the tab that showed it.
                self.detail.retain(|id, _| list.iter().any(|w| w.id == *id));
            }
            ApiEvent::WorkspaceDetail(detail) => {
                self.detail.insert(detail.id, detail.clone());
            }
            ApiEvent::Notification(n) => {
                self.notifications.push_back(n.clone());
                while self.notifications.len() > NOTIFICATION_CAP {
                    self.notifications.pop_front();
                }
            }
            ApiEvent::GitOp(op) => self.git_op = Some(op.clone()),
            // Not state: it is a one-off instruction to go and connect a
            // machine, which the workbench loop acts on. Recording it here
            // would leave a stale "there is a host over there" in the cache
            // long after it was dealt with.
            ApiEvent::RemoteAnnounce(_) => {}
        }
    }

    /// The workspace detail behind a tab, if we have it yet.
    pub fn workspace(&self, id: SessionId) -> Option<&WorkspaceDetail> {
        self.detail.get(&id)
    }
}

/// A connection to one daemon: its REST face plus a live event stream.
pub struct Daemon {
    pub api: Api,
    pub state: State,
    socket: PathBuf,
    events: UnboundedReceiver<DaemonEvent>,
}

impl Daemon {
    /// Open a connection to the daemon on *this* machine, spawning it if the
    /// socket is not answering (the same auto-spawn a bare `butai` does).
    ///
    /// The event-stream task starts immediately and reconnects on its own, so a
    /// daemon that restarts under a running client comes back without the
    /// client noticing anything beyond a `Lost` / `Connected` pair.
    pub async fn connect(socket: PathBuf) -> Result<Self> {
        Self::open(Api::new(socket.clone()), socket).await
    }

    /// Open a connection to a daemon on another machine, reached through a
    /// forwarded socket.
    ///
    /// Same connection, one rule fewer: a forwarded socket that is silent is
    /// never answered by starting a daemon here. [`Api::remote`] has the whole
    /// argument.
    pub async fn connect_remote(socket: PathBuf) -> Result<Self> {
        Self::open(Api::remote(socket.clone()), socket).await
    }

    async fn open(api: Api, socket: PathBuf) -> Result<Self> {
        // Prove the socket answers before starting the stream task, so a bad
        // path is an error here rather than an endless retry loop.
        api.get("/v1/workspaces").await.context("daemon did not answer /v1/workspaces")?;
        let events = spawn_event_stream(socket.clone());
        Ok(Self { api, state: State::default(), socket, events })
    }

    /// The socket this daemon is on. Also its identity in a multi-daemon client.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Take the next event, folding it into [`Daemon::state`] first.
    ///
    /// Returns `None` only when the stream task is gone, which cannot happen
    /// while this `Daemon` is alive — the task holds the sender.
    pub async fn next_event(&mut self) -> Option<DaemonEvent> {
        let event = self.events.recv().await?;
        match &event {
            DaemonEvent::Connected => self.state.connected = true,
            DaemonEvent::Lost(_) => self.state.connected = false,
            DaemonEvent::Api(e) => self.state.apply(e),
        }
        Some(event)
    }

    /// Fetch the current state without waiting for a push.
    ///
    /// The event stream only sends what *changed*, so a client that has just
    /// subscribed knows nothing until something moves. This is the one-time
    /// catch-up that makes the first frame complete.
    pub async fn prime(&mut self) -> Result<()> {
        let tabs: Vec<WorkspaceSummary> = self.api.get_as("/v1/workspaces").await?;
        for tab in &tabs {
            if let Ok(detail) =
                self.api.get_as::<WorkspaceDetail>(&format!("/v1/workspaces/{}", tab.id)).await
            {
                self.state.detail.insert(detail.id, detail);
            }
        }
        self.state.tabs = tabs;
        if let Ok(sys) = self.api.get_as::<SysDto>("/v1/system").await {
            self.state.system = sys;
        }
        Ok(())
    }
}

/// Subscribe to `GET /v1/events`, reconnecting with backoff, forever.
fn spawn_event_stream(socket: PathBuf) -> UnboundedReceiver<DaemonEvent> {
    let (tx, rx) = unbounded_channel();
    tokio::spawn(async move {
        let mut backoff = BACKOFF_MIN;
        loop {
            match read_events(&socket, &tx).await {
                // A clean end still means the daemon went away; the stream is
                // infinite by design.
                Ok(()) => {
                    if tx.send(DaemonEvent::Lost("event stream closed".into())).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    if tx.send(DaemonEvent::Lost(format!("{e:#}"))).is_err() {
                        return;
                    }
                }
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(BACKOFF_MAX);
            if tx.is_closed() {
                return;
            }
        }
    });
    rx
}

/// One pass over the event stream: connect, announce, then pump until it ends.
async fn read_events(socket: &Path, tx: &UnboundedSender<DaemonEvent>) -> Result<()> {
    let stream = crate::conn::connect_existing(socket).await?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .context("HTTP handshake for the event stream")?;
    tokio::spawn(conn);

    let req = Request::builder()
        .method("GET")
        .uri("/v1/events")
        .header("host", "butai")
        .header("accept", "text/event-stream")
        .body(Full::<bytes::Bytes>::default())
        .context("build event-stream request")?;
    let res = sender.send_request(req).await.context("open /v1/events")?;
    if res.status() != StatusCode::OK {
        anyhow::bail!("daemon answered /v1/events with {}", res.status());
    }
    if tx.send(DaemonEvent::Connected).is_err() {
        return Ok(());
    }

    let mut body = res.into_body();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.context("read from the event stream")?;
        let Some(chunk) = frame.data_ref() else { continue };
        buf.extend_from_slice(chunk);
        if buf.len() > MAX_SSE_RECORD {
            anyhow::bail!("event stream sent {} bytes without a record break", buf.len());
        }
        while let Some(end) = find_record_end(&buf) {
            let record = buf.drain(..end.0).collect::<Vec<u8>>();
            buf.drain(..end.1);
            if let Some(event) = parse_record(&record) {
                if tx.send(DaemonEvent::Api(Box::new(event))).is_err() {
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

/// Offset of the next record separator and its length (`\n\n` or `\r\n\r\n`).
fn find_record_end(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some((i, 2));
        }
        if i + 3 < buf.len() && &buf[i..i + 4] == b"\r\n\r\n" {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}

/// One SSE record into an [`ApiEvent`], or `None` to skip it.
///
/// Skipping covers three cases that all mean the same thing to a client: a
/// comment/keepalive line, a field we do not use, and — the one that matters —
/// an `event` tag this build does not know. Erroring on the last would make
/// adding an event a breaking change.
fn parse_record(record: &[u8]) -> Option<ApiEvent> {
    let text = std::str::from_utf8(record).ok()?;
    let mut data = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if data.is_empty() {
        return None;
    }
    serde_json::from_str::<ApiEvent>(&data).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sys_event(pct: f32) -> String {
        let sys = SysDto { cpu_pct: pct, ..SysDto::default() };
        let data = serde_json::to_string(&ApiEvent::System(sys)).unwrap();
        format!("data: {data}")
    }

    #[test]
    fn a_record_is_parsed_into_an_event() {
        let event = parse_record(sys_event(12.5).as_bytes()).expect("parses");
        match event {
            ApiEvent::System(sys) => assert_eq!(sys.cpu_pct, 12.5),
            other => panic!("wrong event: {other:?}"),
        }
    }

    #[test]
    fn an_unknown_event_tag_is_skipped_not_fatal() {
        // The daemon is allowed to add events; a client that has not been
        // rebuilt has to ignore them rather than drop the stream.
        let record = br#"data: {"event":"something_new","data":{"whatever":1}}"#;
        assert!(parse_record(record).is_none());
    }

    #[test]
    fn a_comment_only_record_is_skipped() {
        assert!(parse_record(b": keepalive").is_none());
    }

    #[test]
    fn multi_line_data_is_joined() {
        // SSE allows a payload split across `data:` lines; the daemon does not
        // do this today, but the spec does and a reader is cheap.
        let record = b"data: {\"event\":\"workspaces\",\ndata: \"data\":[]}";
        assert!(matches!(parse_record(record), Some(ApiEvent::Workspaces(v)) if v.is_empty()));
    }

    #[test]
    fn records_split_across_chunks_are_found_in_order() {
        let mut buf = Vec::new();
        buf.extend_from_slice(sys_event(1.0).as_bytes());
        buf.extend_from_slice(b"\n\n");
        buf.extend_from_slice(sys_event(2.0).as_bytes());
        buf.extend_from_slice(b"\n\n");
        let mut seen = Vec::new();
        while let Some((end, sep)) = find_record_end(&buf) {
            let record: Vec<u8> = buf.drain(..end).collect();
            buf.drain(..sep);
            if let Some(ApiEvent::System(sys)) = parse_record(&record) {
                seen.push(sys.cpu_pct);
            }
        }
        assert_eq!(seen, vec![1.0, 2.0]);
        assert!(buf.is_empty());
    }

    #[test]
    fn a_vanished_workspace_takes_its_detail_with_it() {
        let mut state = State::default();
        let detail = WorkspaceDetail {
            id: SessionId(1),
            name: "a".into(),
            cwd: "/tmp".into(),
            agents: vec![],
            processes: vec![],
            changes: None,
            stage: None,
        };
        state.apply(&ApiEvent::WorkspaceDetail(detail));
        assert!(state.workspace(SessionId(1)).is_some());
        state.apply(&ApiEvent::Workspaces(vec![]));
        assert!(state.workspace(SessionId(1)).is_none(), "stale rail outlived its tab");
    }
}
