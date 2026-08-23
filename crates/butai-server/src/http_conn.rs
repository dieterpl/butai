//! HTTP/REST facade served on the *same* Unix socket as the framed client
//! protocol (the "Docker-style" control API). A connection is routed here
//! when its first byte is an ASCII HTTP method rather than a framed hello's
//! `0x00` length prefix (see `client_conn::handle_connection`).
//!
//! The handler owns no state: it translates HTTP into `Event::Api` /
//! `Event::ApiSubscribe`, round-tripping through the single-owner core actor
//! via a oneshot (queries/actions) or an mpsc stream (`GET /v1/events`).

use std::convert::Infallible;
use std::io::Write;

use butai_protocol::api::{
    ApiEvent, ApiReply, ApiRequest, GitOp, OutputFormat, OutputSource, TreeFilter,
};
use butai_protocol::{InputEvent, PaneId, SessionId};
use bytes::Bytes;
use flate2::write::GzEncoder;
use flate2::Compression;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_TYPE, VARY};
use hyper::service::service_fn;
use hyper::{HeaderMap, Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::sync::oneshot;
use tracing::warn;

use crate::core::Event;

type ResBody = BoxBody<Bytes, std::io::Error>;

/// Rows a pane read returns when `?lines=` is not given. Enough to cover an
/// agent's last turn without making the caller page, and small enough that a
/// polling reader stays cheap.
const DEFAULT_OUTPUT_LINES: usize = 200;

/// Smallest reply worth compressing.
///
/// Below this the saving does not repay the work: a 215-byte workspace list only
/// reaches 167, and gzip's own header and trailer are ~20 of the bytes it saved.
/// The payload this exists for is `/v1/system`, which is ~16 KB and compresses
/// better than 4:1 — and, being ~98% of the event stream, is most of what a live
/// client ever reads.
const GZIP_MIN_BYTES: usize = 1024;

/// Deflate level for both paths.
///
/// Level 1, not the default 6. On this API's JSON the ratio is within a few
/// percent of level 6 for a fraction of the CPU, and the daemon compresses on
/// its own event-loop thread every sampler tick — spending 400us there to save
/// 200 bytes over a Unix socket would be a poor trade for the one deployment
/// (local) where compression buys nothing anyway.
const GZIP_LEVEL: Compression = Compression::new(1);

/// Serve HTTP/1.1 over one already-accepted Unix socket connection.
pub async fn handle(stream: UnixStream, events: UnboundedSender<Event>) {
    let io = TokioIo::new(stream);
    let service = service_fn(move |req: Request<Incoming>| {
        let events = events.clone();
        let gzip = accepts_gzip(req.headers());
        async move { Ok::<_, Infallible>(compress(route(req, events, gzip).await, gzip).await) }
    });
    if let Err(e) = hyper::server::conn::http1::Builder::new().serve_connection(io, service).await {
        warn!("http connection error: {e}");
    }
}

/// Whether this request said it would take gzip.
///
/// Read literally, because the two ways to get this wrong both send a client
/// bytes it cannot read: `gzip;q=0` is a refusal spelled like an offer, and `*`
/// is an offer that never says the word. Anything unparseable is treated as no.
fn accepts_gzip(headers: &HeaderMap) -> bool {
    let Some(v) = headers.get(ACCEPT_ENCODING).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    v.split(',').any(|part| {
        let mut bits = part.split(';').map(str::trim);
        let token = bits.next().unwrap_or("");
        if !token.eq_ignore_ascii_case("gzip") && token != "*" {
            return false;
        }
        // A `q` of zero is the client naming this encoding to rule it out.
        !bits.any(|p| {
            p.strip_prefix("q=").is_some_and(|q| q.trim().parse::<f32>().is_ok_and(|q| q <= 0.0))
        })
    })
}

/// Gzip a finished reply, when the client asked and the body is worth it.
///
/// Only `application/json`: everything else this serves is a download, which is
/// arbitrary bytes and as likely as not already compressed — spending CPU to
/// make a PNG marginally larger is not a saving. The event stream compresses
/// itself, incrementally, and is skipped here by its own `content-encoding`.
async fn compress(resp: Response<ResBody>, want: bool) -> Response<ResBody> {
    let is_json = resp
        .headers()
        .get(CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"application/json"));
    if !want || !is_json || resp.headers().contains_key(CONTENT_ENCODING) {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    // Finite by construction: every body reaching here is a `Full`, because the
    // one streaming route returned before this and carries `content-encoding`.
    let Ok(collected) = body.collect().await else {
        return Response::from_parts(parts, full(Bytes::new()));
    };
    let bytes = collected.to_bytes();
    // `Vary` whether or not this particular body was big enough, so a cache
    // never serves one client's answer to another with a different header.
    parts.headers.insert(VARY, ACCEPT_ENCODING.as_str().parse().unwrap());
    if bytes.len() < GZIP_MIN_BYTES {
        return Response::from_parts(parts, full(bytes));
    }
    let mut enc = GzEncoder::new(Vec::with_capacity(bytes.len() / 3), GZIP_LEVEL);
    if enc.write_all(&bytes).is_err() {
        return Response::from_parts(parts, full(bytes));
    }
    let Ok(gz) = enc.finish() else { return Response::from_parts(parts, full(bytes)) };
    // hyper recomputes `content-length` from the new body's size hint, but the
    // old one is still in `parts` and would describe the wrong number of bytes.
    parts.headers.remove(hyper::header::CONTENT_LENGTH);
    parts.headers.insert(CONTENT_ENCODING, "gzip".parse().unwrap());
    Response::from_parts(parts, full(Bytes::from(gz)))
}

async fn route(
    req: Request<Incoming>,
    events: UnboundedSender<Event>,
    gzip: bool,
) -> Response<ResBody> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    // Streaming endpoint returns its own long-lived body, not an ApiReply.
    if method == Method::GET && segs.as_slice() == ["v1", "events"] {
        return events_stream(events, gzip);
    }

    let body = match req.into_body().collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => Bytes::new(),
    };

    let reply = match (&method, segs.as_slice()) {
        // -- queries --
        (&Method::GET, ["v1", "workspaces"]) => call(&events, ApiRequest::ListWorkspaces).await,
        (&Method::GET, ["v1", "system"]) => call(&events, ApiRequest::System).await,
        (&Method::GET, ["v1", "agents"]) => call(&events, ApiRequest::AgentTypes).await,
        (&Method::GET, ["v1", "usage"]) => call(&events, ApiRequest::Usage).await,
        // -- the daemon itself --
        (&Method::POST, ["v1", "update"]) => call(&events, ApiRequest::Update).await,
        (&Method::GET, ["v1", "fs"]) => {
            call(&events, ApiRequest::BrowseFs { path: query_get(&query, "path") }).await
        }
        (&Method::POST, ["v1", "fs", "mkdir"]) => {
            let b: MkDirBody = match parse(&body) {
                Ok(b) => b,
                Err(e) => return reply_to_response(ApiReply::BadRequest(e)),
            };
            call(&events, ApiRequest::MakeDir { path: b.path, name: b.name }).await
        }
        (&Method::GET, ["v1", "notifications"]) => {
            let since = query_get(&query, "since").and_then(|s| s.parse().ok()).unwrap_or(0);
            call(&events, ApiRequest::Notifications { since }).await
        }
        (&Method::GET, ["v1", "workspaces", id, "branches"]) => {
            with_ws(id, ApiRequest::Branches, &events).await
        }
        (&Method::GET, ["v1", "workspaces", id, "download"]) => {
            match (sid(id), query_get(&query, "path")) {
                (Some(ws), Some(path)) => call(&events, ApiRequest::Download { ws, path }).await,
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, None) => ApiReply::BadRequest("missing ?path=".into()),
            }
        }
        (&Method::GET, ["v1", "workspaces", id]) => {
            with_ws(id, ApiRequest::Workspace, &events).await
        }
        (&Method::GET, ["v1", "workspaces", id, "agents"]) => {
            with_ws(id, ApiRequest::Agents, &events).await
        }
        (&Method::GET, ["v1", "workspaces", id, "processes"]) => {
            with_ws(id, ApiRequest::Processes, &events).await
        }
        (&Method::GET, ["v1", "workspaces", id, "changes"]) => {
            with_ws(id, ApiRequest::Changes, &events).await
        }
        (&Method::GET, ["v1", "workspaces", id, "tree"]) => {
            let filter = TreeFilter::parse(&query_get(&query, "filter").unwrap_or_default());
            match (sid(id), filter) {
                (Some(ws), Some(filter)) => {
                    let path = query_get(&query, "path").unwrap_or_default();
                    call(&events, ApiRequest::Tree { ws, path, filter }).await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                // Named rather than ignored: a typo that quietly answered the
                // unfiltered listing would look like the filter doing nothing.
                (_, None) => ApiReply::BadRequest("?filter= must be `all` or `docs`".into()),
            }
        }
        (&Method::GET, ["v1", "workspaces", id, "file"]) => {
            match (sid(id), query_get(&query, "path")) {
                (Some(ws), Some(path)) => call(&events, ApiRequest::File { ws, path }).await,
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, None) => ApiReply::BadRequest("missing ?path=".into()),
            }
        }
        (&Method::GET, ["v1", "workspaces", id, "show"]) => {
            match (sid(id), query_get(&query, "id").or_else(|| query_get(&query, "rev"))) {
                (Some(ws), Some(rev)) => call(&events, ApiRequest::Show { ws, id: rev }).await,
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, None) => ApiReply::BadRequest("missing ?id=".into()),
            }
        }
        // Fuzzy filename search plus a content grep, rooted at the workspace.
        // Server-side because the files are: a workspace on another machine is
        // reachable only through its own daemon.
        (&Method::GET, ["v1", "workspaces", id, "search"]) => {
            match (sid(id), query_get(&query, "q")) {
                (Some(ws), Some(q)) => call(&events, ApiRequest::Search { ws, query: q }).await,
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, None) => ApiReply::BadRequest("missing ?q=".into()),
            }
        }
        (&Method::GET, ["v1", "workspaces", id, "diff"]) => {
            match (sid(id), query_get(&query, "path")) {
                (Some(ws), Some(path)) => {
                    let staged = matches!(query_get(&query, "kind").as_deref(), Some("staged"))
                        || matches!(query_get(&query, "staged").as_deref(), Some("true" | "1"));
                    call(&events, ApiRequest::Diff { ws, path, staged }).await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, None) => ApiReply::BadRequest("missing ?path=".into()),
            }
        }
        // Read a pane's output as text. A query, not an attach: unlike
        // `AttachTarget::Pane` this neither resizes the pane to the reader's
        // dimensions nor acknowledges its bell, so a script can poll a sibling
        // without perturbing it or the agent-state machine watching it.
        (&Method::GET, ["v1", "workspaces", id, "panes", pane, "output"]) => {
            match (sid(id), pid(pane)) {
                (Some(ws), Some(pane)) => {
                    let source = match query_get(&query, "source").as_deref() {
                        None | Some("scrollback") => Some(OutputSource::Scrollback),
                        Some("screen") => Some(OutputSource::Screen),
                        Some("footer") => Some(OutputSource::Footer),
                        Some(_) => None,
                    };
                    let format = match query_get(&query, "format").as_deref() {
                        None | Some("text") => Some(OutputFormat::Text),
                        Some("ansi") => Some(OutputFormat::Ansi),
                        Some(_) => None,
                    };
                    let lines = match query_get(&query, "lines") {
                        None => Some(DEFAULT_OUTPUT_LINES),
                        Some(s) => s.parse::<usize>().ok(),
                    };
                    match (source, format, lines) {
                        (Some(source), Some(format), Some(lines)) => {
                            let req = ApiRequest::PaneOutput { ws, pane, lines, source, format };
                            call(&events, req).await
                        }
                        (None, _, _) => ApiReply::BadRequest(
                            "?source= must be scrollback, screen or footer".into(),
                        ),
                        (_, None, _) => {
                            ApiReply::BadRequest("?format= must be text or ansi".into())
                        }
                        (_, _, None) => {
                            ApiReply::BadRequest("?lines= must be a whole number".into())
                        }
                    }
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, None) => ApiReply::BadRequest(format!("bad pane id {pane:?}")),
            }
        }

        // -- actions --
        (&Method::POST, ["v1", "workspaces"]) => {
            let b: NewWsBody = match parse_or_default(&body) {
                Ok(b) => b,
                Err(e) => return reply_to_response(ApiReply::BadRequest(e)),
            };
            call(&events, ApiRequest::NewWorkspace { name: b.name, layout: b.layout, path: b.path })
                .await
        }
        (&Method::DELETE, ["v1", "workspaces", id]) => {
            with_ws(id, ApiRequest::KillWorkspace, &events).await
        }
        (&Method::POST, ["v1", "workspaces", id, "agents"]) => {
            match (sid(id), parse::<SpawnAgentBody>(&body)) {
                (Some(ws), Ok(b)) => {
                    let req = ApiRequest::SpawnAgent { ws, name: b.kind, background: b.background };
                    call(&events, req).await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, Err(e)) => ApiReply::BadRequest(e),
            }
        }
        (&Method::POST, ["v1", "workspaces", id, "processes"]) => {
            match (sid(id), parse::<NewProcBody>(&body)) {
                (Some(ws), Ok(b)) => {
                    call(&events, ApiRequest::NewProcess { ws, name: b.name, command: b.command })
                        .await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, Err(e)) => ApiReply::BadRequest(e),
            }
        }
        (&Method::POST, ["v1", "workspaces", id, "processes", pane, "restart"]) => {
            match (sid(id), pid(pane)) {
                (Some(ws), Some(pane)) => {
                    call(&events, ApiRequest::RestartProcess { ws, pane }).await
                }
                _ => ApiReply::BadRequest("bad workspace or pane id".into()),
            }
        }
        // Kill any pane — agent, process, editor, tree. `KillPane` was always
        // pane-generic (it validates against the workspace's whole pane set), but
        // the only route to it sat under `processes`, which read as a restriction
        // and left every GUI without a way to kill an agent. This is the honest
        // name; the `processes` spelling below stays as an alias so a client that
        // ships against an older daemon keeps working.
        (&Method::DELETE, ["v1", "workspaces", id, "panes", pane])
        | (&Method::DELETE, ["v1", "workspaces", id, "processes", pane]) => {
            match (sid(id), pid(pane)) {
                (Some(ws), Some(pane)) => call(&events, ApiRequest::KillPane { ws, pane }).await,
                _ => ApiReply::BadRequest("bad workspace or pane id".into()),
            }
        }
        // Inject a keystroke/paste into a pane (Accept = Enter, Stop = Esc) from a
        // list UI, without a streaming attach. Body is a JSON `InputEvent`, e.g.
        // `{"key":{"code":"enter"}}` or `{"key":{"code":"esc"}}`.
        (&Method::POST, ["v1", "workspaces", id, "panes", pane, "input"]) => {
            match (sid(id), pid(pane), parse::<InputEvent>(&body)) {
                (Some(ws), Some(pane), Ok(input)) => {
                    call(&events, ApiRequest::PaneInput { ws, pane, input }).await
                }
                (None, _, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, None, _) => ApiReply::BadRequest(format!("bad pane id {pane:?}")),
                (_, _, Err(e)) => ApiReply::BadRequest(e),
            }
        }
        // Dismiss a pane's pending bell without opening it — the list-UI analog of
        // the TUI staging the pane. Without this, an agent that rang the bell
        // reports `waiting` forever to every non-TUI client.
        (&Method::POST, ["v1", "workspaces", id, "panes", pane, "ack"]) => {
            match (sid(id), pid(pane)) {
                (Some(ws), Some(pane)) => call(&events, ApiRequest::AckPane { ws, pane }).await,
                _ => ApiReply::BadRequest("bad workspace or pane id".into()),
            }
        }
        (&Method::POST, ["v1", "workspaces", id, "changes", "stage"]) => {
            match (sid(id), parse::<PathBody>(&body)) {
                (Some(ws), Ok(b)) => {
                    call(&events, ApiRequest::StageFile { ws, path: b.path }).await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, Err(e)) => ApiReply::BadRequest(e),
            }
        }
        (&Method::POST, ["v1", "workspaces", id, "changes", "unstage"]) => {
            match (sid(id), parse::<PathBody>(&body)) {
                (Some(ws), Ok(b)) => {
                    call(&events, ApiRequest::UnstageFile { ws, path: b.path }).await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, Err(e)) => ApiReply::BadRequest(e),
            }
        }
        (&Method::POST, ["v1", "workspaces", id, "changes", "discard"]) => {
            match (sid(id), parse::<PathBody>(&body)) {
                (Some(ws), Ok(b)) => {
                    call(&events, ApiRequest::DiscardFile { ws, path: b.path }).await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, Err(e)) => ApiReply::BadRequest(e),
            }
        }
        (&Method::POST, ["v1", "workspaces", id, "changes", "commit"]) => {
            match (sid(id), parse::<CommitBody>(&body)) {
                (Some(ws), Ok(b)) => {
                    call(&events, ApiRequest::Commit { ws, message: b.message }).await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, Err(e)) => ApiReply::BadRequest(e),
            }
        }
        (&Method::POST, ["v1", "workspaces", id, "changes", "commit-all"]) => {
            match (sid(id), parse::<CommitBody>(&body)) {
                (Some(ws), Ok(b)) => {
                    call(&events, ApiRequest::CommitAll { ws, message: b.message }).await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, Err(e)) => ApiReply::BadRequest(e),
            }
        }
        (&Method::POST, ["v1", "workspaces", id, "checkout"]) => {
            match (sid(id), parse::<CheckoutBody>(&body)) {
                (Some(ws), Ok(b)) => {
                    call(&events, ApiRequest::Checkout { ws, branch: b.branch, create: b.create })
                        .await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, Err(e)) => ApiReply::BadRequest(e),
            }
        }
        // Upload: the raw request body is the file's bytes; `?path=` is the
        // destination relative to the workspace cwd (directory + filename).
        (&Method::POST, ["v1", "workspaces", id, "upload"]) => {
            match (sid(id), query_get(&query, "path")) {
                (Some(ws), Some(path)) => {
                    call(&events, ApiRequest::Upload { ws, path, data: body.to_vec() }).await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, None) => ApiReply::BadRequest("missing ?path=".into()),
            }
        }
        // Delete: `?path=` rather than a JSON body, so it reads as the inverse
        // of the two routes either side of it and matches the other DELETEs
        // that name their target (`git/branch`, `git/worktree`, `git/remote`).
        (&Method::DELETE, ["v1", "workspaces", id, "file"]) => {
            match (sid(id), query_get(&query, "path")) {
                (Some(ws), Some(path)) => call(&events, ApiRequest::DeleteFile { ws, path }).await,
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, None) => ApiReply::BadRequest("missing ?path=".into()),
            }
        }

        // -- git operations ---------------------------------------------------
        // Everything that writes the repository beyond the index. Each route's
        // body parses into a `GitOp`; the core validates, takes the repository's
        // write lock and answers 200 (finished), 202 (still running), 400
        // (refused before running), 404 (no repo) or 409 (another op holds the
        // lock). See `git_op.rs`.
        (&Method::POST, ["v1", "workspaces", id, "git", "fetch"]) => {
            git_run(&events, id, &body, |b: FetchBody| GitOp::Fetch {
                remote: b.remote,
                all: b.all,
                prune: b.prune,
            })
            .await
        }
        (&Method::POST, ["v1", "workspaces", id, "git", "pull"]) => {
            git_run(&events, id, &body, |b: PullBody| GitOp::Pull {
                remote: b.remote,
                branch: b.branch,
                rebase: b.rebase,
                ff_only: b.ff_only,
            })
            .await
        }
        (&Method::POST, ["v1", "workspaces", id, "git", "push"]) => {
            git_run(&events, id, &body, |b: PushBody| GitOp::Push {
                remote: b.remote,
                branch: b.branch,
                set_upstream: b.set_upstream,
                force_with_lease: b.force_with_lease,
            })
            .await
        }
        (&Method::POST, ["v1", "workspaces", id, "git", "stash"]) => {
            git_run(&events, id, &body, |b: StashBody| GitOp::Stash {
                message: b.message,
                include_untracked: b.include_untracked,
            })
            .await
        }
        (&Method::POST, ["v1", "workspaces", id, "git", "stash", "apply"]) => {
            git_run(&events, id, &body, |b: StashRefBody| GitOp::StashApply {
                index: b.index,
                pop: b.pop,
            })
            .await
        }
        (&Method::DELETE, ["v1", "workspaces", id, "git", "stash"]) => match sid(id) {
            Some(ws) => {
                let index =
                    query_get(&query, "index").and_then(|v| v.parse().ok()).unwrap_or(0usize);
                call(&events, ApiRequest::GitRun { ws, op: GitOp::StashDrop { index } }).await
            }
            None => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
        },
        (&Method::POST, ["v1", "workspaces", id, "git", "amend"]) => {
            git_run(&events, id, &body, |b: MessageBody| GitOp::Amend { message: b.message }).await
        }
        (&Method::POST, ["v1", "workspaces", id, "git", "reset"]) => {
            git_run(&events, id, &body, |b: ResetBody| GitOp::Reset { rev: b.rev, mode: b.mode })
                .await
        }
        (&Method::POST, ["v1", "workspaces", id, "git", "revert"]) => {
            git_run(&events, id, &body, |b: RevBody| GitOp::Revert { rev: b.rev }).await
        }
        (&Method::POST, ["v1", "workspaces", id, "git", "cherry-pick"]) => {
            git_run(&events, id, &body, |b: RevBody| GitOp::CherryPick { rev: b.rev }).await
        }
        (&Method::POST, ["v1", "workspaces", id, "git", "merge"]) => {
            git_run(&events, id, &body, |b: MergeBody| GitOp::Merge {
                branch: b.branch,
                no_ff: b.no_ff,
            })
            .await
        }
        (&Method::POST, ["v1", "workspaces", id, "git", "rebase"]) => {
            git_run(&events, id, &body, |b: RebaseBody| GitOp::Rebase { onto: b.onto }).await
        }
        (&Method::POST, ["v1", "workspaces", id, "git", "sequence"]) => {
            git_run(&events, id, &body, |b: SequenceBody| GitOp::Sequence { action: b.action })
                .await
        }
        (&Method::POST, ["v1", "workspaces", id, "git", "tag"]) => {
            git_run(&events, id, &body, |b: TagBody| GitOp::Tag {
                name: b.name,
                rev: b.rev,
                message: b.message,
            })
            .await
        }
        (&Method::DELETE, ["v1", "workspaces", id, "git", "tag"]) => {
            match (sid(id), query_get(&query, "name")) {
                (Some(ws), Some(name)) => {
                    call(&events, ApiRequest::GitRun { ws, op: GitOp::TagDelete { name } }).await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, None) => ApiReply::BadRequest("missing ?name=".into()),
            }
        }
        (&Method::GET, ["v1", "workspaces", id, "git", "log"]) => match sid(id) {
            Some(ws) => {
                let n = |k: &str| query_get(&query, k).and_then(|v| v.parse::<usize>().ok());
                call(
                    &events,
                    ApiRequest::GitLog {
                        ws,
                        limit: n("limit").unwrap_or(50).clamp(1, 500),
                        skip: n("skip").unwrap_or(0),
                        rev: query_get(&query, "rev"),
                        path: query_get(&query, "path"),
                        all: query_get(&query, "all").is_some_and(|v| v != "0"),
                    },
                )
                .await
            }
            None => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
        },
        (&Method::GET, ["v1", "workspaces", id, "git", "stashes"]) => {
            with_ws(id, ApiRequest::GitStashes, &events).await
        }
        (&Method::GET, ["v1", "workspaces", id, "git", "remotes"]) => {
            with_ws(id, ApiRequest::GitRemotes, &events).await
        }
        (&Method::GET, ["v1", "workspaces", id, "git", "tags"]) => {
            with_ws(id, ApiRequest::GitTags, &events).await
        }
        (&Method::GET, ["v1", "workspaces", id, "git", "conflict"]) => {
            match (sid(id), query_get(&query, "path")) {
                (Some(ws), Some(path)) => call(&events, ApiRequest::GitConflict { ws, path }).await,
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, None) => ApiReply::BadRequest("missing ?path=".into()),
            }
        }
        (&Method::GET, ["v1", "workspaces", id, "git", "op"]) => {
            with_ws(id, ApiRequest::GitOpStatus, &events).await
        }
        // Remote management. The URL is validated against an allowlist of
        // transports before it reaches git — see `git_op::valid_remote_url`.
        (&Method::POST, ["v1", "workspaces", id, "git", "remote"]) => {
            match (sid(id), parse::<RemoteBody>(&body)) {
                (Some(ws), Ok(b)) => {
                    let op = GitOp::RemoteAdd { name: b.name, url: b.url };
                    call(&events, ApiRequest::GitRun { ws, op }).await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, Err(e)) => ApiReply::BadRequest(e),
            }
        }
        (&Method::DELETE, ["v1", "workspaces", id, "git", "remote"]) => {
            match (sid(id), query_get(&query, "name")) {
                (Some(ws), Some(name)) => {
                    call(&events, ApiRequest::GitRun { ws, op: GitOp::RemoteRemove { name } }).await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, None) => ApiReply::BadRequest("missing ?name=".into()),
            }
        }
        // Worktrees: a second checkout of the same repository. `GET` also says
        // which butai workspace is already open on each one, so a client can
        // offer "go there" rather than "open it again".
        (&Method::GET, ["v1", "workspaces", id, "git", "worktrees"]) => {
            with_ws(id, ApiRequest::GitWorktrees, &events).await
        }
        (&Method::POST, ["v1", "workspaces", id, "git", "worktree"]) => {
            match (sid(id), parse::<WorktreeBody>(&body)) {
                (Some(ws), Ok(b)) => {
                    let op = GitOp::WorktreeAdd {
                        path: b.path,
                        branch: b.branch,
                        new_branch: b.new_branch,
                    };
                    call(&events, ApiRequest::GitRun { ws, op }).await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, Err(e)) => ApiReply::BadRequest(e),
            }
        }
        (&Method::DELETE, ["v1", "workspaces", id, "git", "worktree"]) => {
            match (sid(id), query_get(&query, "path")) {
                (Some(ws), Some(path)) => {
                    let force = matches!(query_get(&query, "force").as_deref(), Some("true" | "1"));
                    call(
                        &events,
                        ApiRequest::GitRun { ws, op: GitOp::WorktreeRemove { path, force } },
                    )
                    .await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, None) => ApiReply::BadRequest("missing ?path=".into()),
            }
        }
        (&Method::POST, ["v1", "workspaces", id, "git", "worktree", "prune"]) => match sid(id) {
            Some(ws) => call(&events, ApiRequest::GitRun { ws, op: GitOp::WorktreePrune }).await,
            None => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
        },
        (&Method::DELETE, ["v1", "workspaces", id, "git", "op"]) => {
            with_ws(id, ApiRequest::GitOpCancel, &events).await
        }

        // Index-only git actions: libgit2, synchronous, no runner.
        (&Method::POST, ["v1", "workspaces", id, "git", "resolve"]) => {
            match (sid(id), parse::<ResolveBody>(&body)) {
                (Some(ws), Ok(b)) => {
                    call(&events, ApiRequest::GitResolve { ws, path: b.path, take: b.take }).await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, Err(e)) => ApiReply::BadRequest(e),
            }
        }
        // Partial staging: the caller sends back a patch containing only the
        // hunks or lines it chose. `target` says which copy of the file it
        // lands on and `reverse` which way round, so stage/unstage/discard are
        // one route rather than three.
        (&Method::POST, ["v1", "workspaces", id, "git", "apply"]) => {
            match (sid(id), parse::<ApplyBody>(&body)) {
                (Some(ws), Ok(b)) => {
                    call(
                        &events,
                        ApiRequest::GitApply {
                            ws,
                            patch: b.patch,
                            target: b.target,
                            reverse: b.reverse,
                        },
                    )
                    .await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, Err(e)) => ApiReply::BadRequest(e),
            }
        }
        (&Method::POST, ["v1", "workspaces", id, "git", "branch"]) => {
            match (sid(id), parse::<BranchBody>(&body)) {
                (Some(ws), Ok(b)) => {
                    call(&events, ApiRequest::GitBranchCreate { ws, name: b.name, from: b.from })
                        .await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, Err(e)) => ApiReply::BadRequest(e),
            }
        }
        (&Method::DELETE, ["v1", "workspaces", id, "git", "branch"]) => {
            match (sid(id), query_get(&query, "name")) {
                (Some(ws), Some(name)) => {
                    let force = matches!(query_get(&query, "force").as_deref(), Some("true" | "1"));
                    call(&events, ApiRequest::GitBranchDelete { ws, name, force }).await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, None) => ApiReply::BadRequest("missing ?name=".into()),
            }
        }
        (&Method::POST, ["v1", "workspaces", id, "git", "branch", "rename"]) => {
            match (sid(id), parse::<RenameBody>(&body)) {
                (Some(ws), Ok(b)) => {
                    call(&events, ApiRequest::GitBranchRename { ws, from: b.from, to: b.to }).await
                }
                (None, _) => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
                (_, Err(e)) => ApiReply::BadRequest(e),
            }
        }

        _ => ApiReply::NotFound(format!("no route: {method} /{}", segs.join("/"))),
    };
    reply_to_response(reply)
}

/// Parse a git operation's body and dispatch it. Every `POST .../git/*` route
/// is this shape: an id, a body, and a closure naming which [`GitOp`] it is.
async fn git_run<B, F>(
    events: &UnboundedSender<Event>,
    id: &str,
    body: &Bytes,
    build: F,
) -> ApiReply
where
    B: serde::de::DeserializeOwned + Default,
    F: FnOnce(B) -> GitOp,
{
    let Some(ws) = sid(id) else {
        return ApiReply::BadRequest(format!("bad workspace id {id:?}"));
    };
    // Every field of every git body is optional, so an empty body is the
    // ordinary "just do the default thing" call, not an error.
    match parse_or_default::<B>(body) {
        Ok(b) => call(events, ApiRequest::GitRun { ws, op: build(b) }).await,
        Err(e) => ApiReply::BadRequest(e),
    }
}

/// Parse a workspace id then build+dispatch a request needing only that id.
async fn with_ws(
    id: &str,
    build: impl FnOnce(SessionId) -> ApiRequest,
    events: &UnboundedSender<Event>,
) -> ApiReply {
    match sid(id) {
        Some(ws) => call(events, build(ws)).await,
        None => ApiReply::BadRequest(format!("bad workspace id {id:?}")),
    }
}

/// Round-trip one request through the core actor and await its reply.
async fn call(events: &UnboundedSender<Event>, req: ApiRequest) -> ApiReply {
    let (tx, rx) = oneshot::channel();
    if events.send(Event::Api(req, tx)).is_err() {
        return ApiReply::Error("core unavailable".into());
    }
    rx.await.unwrap_or_else(|_| ApiReply::Error("core dropped the reply".into()))
}

fn events_stream(events: UnboundedSender<Event>, gzip: bool) -> Response<ResBody> {
    let (tx, rx) = unbounded_channel::<ApiEvent>();
    if events.send(Event::ApiSubscribe(tx)).is_err() {
        return reply_to_response(ApiReply::Error("core unavailable".into()));
    }
    let b = Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header(VARY, ACCEPT_ENCODING.as_str());

    // Server-Sent Events: one `data: <json>\n\n` record per pushed event.
    let record =
        |ev: &ApiEvent| format!("data: {}\n\n", serde_json::to_string(ev).unwrap_or_default());

    if !gzip {
        let stream = futures::stream::unfold(rx, move |mut rx| async move {
            let ev = rx.recv().await?;
            let frame = Frame::data(Bytes::from(record(&ev)));
            Some((Ok::<_, std::io::Error>(frame), rx))
        });
        return b.body(StreamBody::new(stream).boxed()).unwrap();
    }

    // The same records through one long-lived gzip stream. This is where the
    // saving on this API actually is: `system` is ~98% of the bytes here and
    // compresses better than 4:1, and the stream is what a live client holds
    // open — over `ssh host butai proxy` it is the whole cost of staying current.
    let stream = futures::stream::unfold((rx, GzEncoder::new(Vec::new(), GZIP_LEVEL)), {
        move |(mut rx, mut enc)| async move {
            loop {
                let ev = rx.recv().await?;
                // Flush after every record. Deflate would otherwise hold it back
                // until its window filled, which on a stream whose whole purpose
                // is "you hear about it when it happens" is indistinguishable
                // from the daemon having nothing to say.
                if enc.write_all(record(&ev).as_bytes()).is_err() || enc.flush().is_err() {
                    return None;
                }
                let chunk = std::mem::take(enc.get_mut());
                // A sync flush always emits something, but never hand hyper an
                // empty data frame: in chunked encoding a zero-length chunk is
                // what *ends* the body.
                if !chunk.is_empty() {
                    return Some((
                        Ok::<_, std::io::Error>(Frame::data(Bytes::from(chunk))),
                        (rx, enc),
                    ));
                }
            }
        }
    });
    b.header(CONTENT_ENCODING, "gzip").body(StreamBody::new(stream).boxed()).unwrap()
}

/// Fetch and percent-decode one query-string parameter.
fn query_get(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == key).then(|| percent_decode(v))
    })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let hex = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 3 <= b.len() => match (hex(b[i + 1]), hex(b[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h * 16 + l);
                    i += 3;
                }
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn reply_to_response(reply: ApiReply) -> Response<ResBody> {
    match reply {
        ApiReply::Workspaces(v) => json(StatusCode::OK, &v),
        ApiReply::Workspace(v) => json(StatusCode::OK, &v),
        ApiReply::Agents(v) => json(StatusCode::OK, &v),
        ApiReply::Processes(v) => json(StatusCode::OK, &v),
        ApiReply::Changes(v) => json(StatusCode::OK, &v),
        ApiReply::Tree(v) => json(StatusCode::OK, &v),
        ApiReply::File(v) => json(StatusCode::OK, &v),
        ApiReply::Diff(v) => json(StatusCode::OK, &v),
        ApiReply::Search(v) => json(StatusCode::OK, &v),
        ApiReply::System(v) => json(StatusCode::OK, &v),
        ApiReply::AgentTypes(v) => json(StatusCode::OK, &v),
        ApiReply::Usage(v) => json(StatusCode::OK, &v),
        ApiReply::Branches(v) => json(StatusCode::OK, &v),
        ApiReply::Browse(v) => json(StatusCode::OK, &v),
        ApiReply::Notifications(v) => json(StatusCode::OK, &v),
        ApiReply::PaneOutput(v) => json(StatusCode::OK, &v),
        ApiReply::Bytes { data, content_type, download_name } => {
            let mut b =
                Response::builder().status(StatusCode::OK).header("content-type", content_type);
            if let Some(name) = download_name {
                // Quote the filename and strip characters that could break out of
                // the header (the name is a basename, but be defensive).
                let safe: String =
                    name.chars().filter(|c| !matches!(c, '"' | '\r' | '\n')).collect();
                b = b.header("content-disposition", format!("attachment; filename=\"{safe}\""));
            }
            b.body(full(Bytes::from(data))).unwrap()
        }
        ApiReply::Log(v) => json(StatusCode::OK, &v),
        ApiReply::Stashes(v) => json(StatusCode::OK, &v),
        ApiReply::Remotes(v) => json(StatusCode::OK, &v),
        ApiReply::Tags(v) => json(StatusCode::OK, &v),
        ApiReply::Worktrees(v) => json(StatusCode::OK, &v),
        ApiReply::Conflict(v) => json(StatusCode::OK, &v),
        ApiReply::GitOp(v) => json(StatusCode::OK, &v),
        ApiReply::Accepted(v) => json(StatusCode::ACCEPTED, &v),
        // 202 when it is going to happen: a daemon that answers this is about
        // to stop and come back on the new binary, so the work is accepted and
        // not yet done. Nothing to do is a plain 200.
        ApiReply::Update(v) => {
            let code = if v.updating { StatusCode::ACCEPTED } else { StatusCode::OK };
            json(code, &v)
        }
        ApiReply::Ok => json(StatusCode::OK, &serde_json::json!({ "ok": true })),
        ApiReply::Created(id) => json(StatusCode::CREATED, &serde_json::json!({ "id": id })),
        ApiReply::NotFound(m) => json(StatusCode::NOT_FOUND, &err_obj(&m)),
        ApiReply::BadRequest(m) => json(StatusCode::BAD_REQUEST, &err_obj(&m)),
        ApiReply::Busy(m) => json(StatusCode::CONFLICT, &err_obj(&m)),
        ApiReply::Error(m) => json(StatusCode::INTERNAL_SERVER_ERROR, &err_obj(&m)),
    }
}

fn json<T: Serialize>(status: StatusCode, body: &T) -> Response<ResBody> {
    let bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(full(Bytes::from(bytes)))
        .unwrap()
}

fn err_obj(msg: &str) -> serde_json::Value {
    serde_json::json!({ "error": msg })
}

fn full(b: Bytes) -> ResBody {
    Full::new(b).map_err(|never| match never {}).boxed()
}

fn sid(s: &str) -> Option<SessionId> {
    s.parse::<u64>().ok().map(SessionId)
}

fn pid(s: &str) -> Option<PaneId> {
    s.parse::<u64>().ok().map(PaneId)
}

fn parse<T: for<'de> Deserialize<'de>>(body: &Bytes) -> Result<T, String> {
    serde_json::from_slice(body).map_err(|e| format!("bad json body: {e}"))
}

/// Like [`parse`] but an empty body yields `T::default()` (for optional-field
/// POSTs such as creating a workspace with no name).
fn parse_or_default<T: Default + for<'de> Deserialize<'de>>(body: &Bytes) -> Result<T, String> {
    if body.is_empty() {
        Ok(T::default())
    } else {
        parse(body)
    }
}

#[derive(Deserialize, Default)]
struct NewWsBody {
    name: Option<String>,
    layout: Option<String>,
    /// Absolute (or daemon-cwd-relative) directory to open the workspace in.
    path: Option<String>,
}

#[derive(Deserialize)]
struct CheckoutBody {
    branch: String,
    #[serde(default)]
    create: bool,
}

// Git operation bodies. Every field is optional and `Default` so a bare
// `POST .../git/fetch` with no body means "the obvious thing" — which is what
// a person typing `git fetch` gets.
#[derive(Deserialize, Default)]
struct StashBody {
    message: Option<String>,
    #[serde(default)]
    include_untracked: bool,
}

#[derive(Deserialize, Default)]
struct StashRefBody {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    pop: bool,
}

#[derive(Deserialize, Default)]
struct MessageBody {
    message: Option<String>,
}

#[derive(Deserialize, Default)]
struct ResetBody {
    rev: Option<String>,
    #[serde(default)]
    mode: butai_protocol::api::ResetMode,
}

/// A required revision. `Default` only so the shared `git_run` helper can build
/// one from an empty body; an empty rev is then refused with a 400 by the
/// validator, which is a better message than "missing field `rev`".
#[derive(Deserialize, Default)]
struct RevBody {
    #[serde(default)]
    rev: String,
}

#[derive(Deserialize, Default)]
struct MergeBody {
    #[serde(default)]
    branch: String,
    #[serde(default)]
    no_ff: bool,
}

#[derive(Deserialize, Default)]
struct RebaseBody {
    #[serde(default)]
    onto: String,
}

#[derive(Deserialize)]
struct SequenceBody {
    action: butai_protocol::api::SequenceAction,
}

impl Default for SequenceBody {
    fn default() -> Self {
        Self { action: butai_protocol::api::SequenceAction::Continue }
    }
}

#[derive(Deserialize, Default)]
struct TagBody {
    #[serde(default)]
    name: String,
    rev: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct ResolveBody {
    path: String,
    take: butai_protocol::api::ResolveSide,
}

#[derive(Deserialize)]
struct RemoteBody {
    name: String,
    url: String,
}

#[derive(Deserialize)]
struct WorktreeBody {
    /// Absolute path for the new checkout.
    path: String,
    /// The branch to check out there, or to create when `new_branch` is set.
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    new_branch: bool,
}

#[derive(Deserialize)]
struct ApplyBody {
    /// Unified diff text. Whatever a client got from `GET .../diff`, minus the
    /// parts it did not want.
    patch: String,
    #[serde(default)]
    target: butai_protocol::api::ApplyTarget,
    #[serde(default)]
    reverse: bool,
}

#[derive(Deserialize)]
struct BranchBody {
    name: String,
    #[serde(default)]
    from: Option<String>,
}

#[derive(Deserialize)]
struct RenameBody {
    /// The branch to rename; the current one when omitted.
    #[serde(default)]
    from: Option<String>,
    to: String,
}

#[derive(Deserialize, Default)]
struct FetchBody {
    remote: Option<String>,
    #[serde(default)]
    all: bool,
    #[serde(default)]
    prune: bool,
}

#[derive(Deserialize, Default)]
struct PullBody {
    remote: Option<String>,
    branch: Option<String>,
    #[serde(default)]
    rebase: bool,
    #[serde(default)]
    ff_only: bool,
}

#[derive(Deserialize, Default)]
struct PushBody {
    remote: Option<String>,
    branch: Option<String>,
    #[serde(default)]
    set_upstream: bool,
    #[serde(default)]
    force_with_lease: bool,
}

#[derive(Deserialize)]
struct SpawnAgentBody {
    #[serde(rename = "type", alias = "name", alias = "agent")]
    kind: String,
    /// Do not take the stage. Defaults to false so existing clients are
    /// unaffected.
    #[serde(default)]
    background: bool,
}

#[derive(Deserialize)]
struct NewProcBody {
    name: String,
    /// Omitted (or empty) means the workspace's default shell — which is what
    /// the `[+ term]` button asks for, and the only sensible reading of "start
    /// me a process" with nothing to run.
    #[serde(default)]
    command: String,
}

#[derive(Deserialize)]
struct MkDirBody {
    /// Parent directory (absent = daemon default dir), matching the browse route.
    #[serde(default)]
    path: Option<String>,
    name: String,
}

#[derive(Deserialize)]
struct PathBody {
    path: String,
}

#[derive(Deserialize)]
struct CommitBody {
    message: String,
}
