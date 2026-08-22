//! HTTP client for the daemon's REST face.
//!
//! The daemon serves two protocols on one Unix socket, told apart by the first
//! byte (see `butai-server`'s `client_conn`): a length-prefixed framed protocol
//! for streaming a pane, and HTTP/1.1 for everything structured.
//!
//! This used to live in the `butai` binary, on the reasoning that `butai-client`
//! spoke "only the public `butai-protocol` API" and should carry no HTTP
//! dependency. That reasoning is what the client/daemon split overturned: the
//! TUI now draws its tab bar, rails and pages from the same `/v1/*` DTOs the
//! web and native clients use, so REST is not a CLI convenience bolted onto the
//! side — it is how a client learns anything that is not a pane's cells. The
//! binary still uses it, through this module.
//!
//! One connection per request. A CLI makes a handful of calls and exits, and the
//! TUI's calls are user-paced, so pooling would buy nothing; a fresh connection
//! also keeps each call independent of whatever state a previous one left
//! behind. The one long-lived connection is the event stream, which has its own
//! path in [`crate::daemon`].

use std::path::PathBuf;

use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde::de::DeserializeOwned;

/// A handle to one daemon socket.
///
/// `Clone` so a caller that needs an owned handle for a task — the GIT page's
/// reads run off the loop — can take the one the daemon already has rather than
/// rebuild it from a bare path. Rebuilding is how the spawn policy below came to
/// diverge in the first place.
#[derive(Clone)]
pub struct Api {
    socket: PathBuf,
    /// Whether a socket that is not answering may be answered by starting a
    /// daemon on it. See [`Api::remote`].
    may_spawn: bool,
}

/// A non-2xx response, carrying the status and whatever the daemon said.
///
/// Kept as a distinct type so a caller can branch on the status — `agent wait`
/// needs to tell a 404 (bad pane) from a 400 (bad `--until`) to pick its exit
/// code — instead of string-matching a flattened error.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A 4xx message explains itself ("no workspace 7"); a 5xx is a daemon
        // bug, and the status is the part worth reporting in an issue.
        if self.status.is_server_error() {
            write!(f, "{} ({})", self.message, self.status)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl std::error::Error for ApiError {}

impl Api {
    /// A handle to the daemon on *this* machine, started if it is not running.
    ///
    /// Auto-spawn is what makes a bare `butai` work from a cold boot, so it is
    /// the right default for a local socket and the wrong one for any other —
    /// see [`Api::remote`].
    pub fn new(socket: PathBuf) -> Self {
        Self { socket, may_spawn: true }
    }

    /// A handle to a daemon reached through an `ssh -L` forward.
    ///
    /// **The difference is auto-spawn, and it is not a nicety.**
    /// [`crate::conn::spawn_daemon`] starts the daemon with
    /// `BUTAI_SOCKET=<this path>`, so asking it to fill a silent *forwarded*
    /// socket starts a local daemon wearing the far machine's name: the tab
    /// answers again, with none of that machine's workspaces, and it leaves a
    /// lock file beside the forward that stops the real one being restored.
    /// A forwarded socket that has gone quiet means the tunnel is down or the
    /// far daemon is gone, and a daemon here is the answer to neither.
    ///
    /// [`crate::conn::connect_existing`] carried this rule for the event stream
    /// and the pane connection from the start; this is the third path, which
    /// had been calling `connect_or_spawn` and quietly doing the wrong thing.
    pub fn remote(socket: PathBuf) -> Self {
        Self { socket, may_spawn: false }
    }

    /// The socket this handle talks to.
    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    /// Send one request and return the raw response body.
    ///
    /// Raw bytes rather than a parsed value because `--json` re-emits the
    /// daemon's body verbatim: the CLI's JSON output is the REST API's JSON
    /// output, so the two can never drift.
    pub async fn call(&self, method: Method, path: &str, body: Option<Vec<u8>>) -> Result<Bytes> {
        self.send(method, path, body, "application/json").await
    }

    /// Send one request with an explicit content type.
    ///
    /// Not every body is JSON: `upload` takes a file's bytes as the body, and
    /// labelling those `application/json` would be a lie a proxy could act on.
    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        content_type: &str,
    ) -> Result<Bytes> {
        let stream = if self.may_spawn {
            crate::conn::connect_or_spawn(&self.socket).await?
        } else {
            crate::conn::connect_existing(&self.socket).await?
        };
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .context("HTTP handshake with the daemon")?;
        // The connection task drives the socket; it ends when the response is
        // done. Errors surface on `sender`, so the join handle is dropped.
        tokio::spawn(conn);

        let has_body = body.is_some();
        let mut req = Request::builder().method(method).uri(path);
        if has_body {
            req = req.header("content-type", content_type);
        }
        // hyper/1.1 requires a Host even over a Unix socket, where it means
        // nothing. The daemon's router only looks at method and path.
        let req = req
            .header("host", "butai")
            .body(Full::new(Bytes::from(body.unwrap_or_default())))
            .context("build request")?;

        let res = sender
            .send_request(req)
            .await
            .with_context(|| format!("no response from the daemon on {}", self.socket.display()))?;
        let status = res.status();
        let bytes = res.into_body().collect().await.context("read response body")?.to_bytes();

        if status.is_success() {
            return Ok(bytes);
        }
        // Error bodies are `{"error": "..."}` (see `reply_to_response`). Fall
        // back to the raw body when a proxy or a panic produced something else.
        let message = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
            .unwrap_or_else(|| {
                let raw = String::from_utf8_lossy(&bytes).trim().to_string();
                if raw.is_empty() {
                    format!("daemon returned {status}")
                } else {
                    raw
                }
            });
        Err(ApiError { status, message }.into())
    }

    pub async fn get(&self, path: &str) -> Result<Bytes> {
        self.call(Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: &serde_json::Value) -> Result<Bytes> {
        self.call(Method::POST, path, Some(serde_json::to_vec(body)?)).await
    }

    pub async fn delete(&self, path: &str) -> Result<Bytes> {
        self.call(Method::DELETE, path, None).await
    }

    /// POST a raw body — a file's bytes rather than a JSON document. This is
    /// what `upload` wants.
    pub async fn post_bytes(&self, path: &str, body: Vec<u8>) -> Result<Bytes> {
        self.send(Method::POST, path, Some(body), "application/octet-stream").await
    }

    /// GET and deserialize. Use [`Api::get`] instead when the bytes are about to
    /// be printed verbatim under `--json`.
    pub async fn get_as<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let bytes = self.get(path).await?;
        parse(&bytes)
    }
}

/// Deserialize a daemon response, quoting it on failure.
///
/// A parse error here means the client and the daemon disagree about a DTO —
/// most likely a stale binary talking to a newer daemon or vice versa — so the
/// body is worth more than serde's positional complaint alone.
pub fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).with_context(|| {
        let body = String::from_utf8_lossy(bytes);
        let body: String = body.chars().take(200).collect();
        format!("unexpected response from the daemon: {body}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_the_body_when_a_response_does_not_deserialize() {
        let err = parse::<Vec<u32>>(b"<html>not json</html>").unwrap_err();
        assert!(err.to_string().contains("<html>not json</html>"), "{err}");
    }

    #[test]
    fn truncates_a_long_body_in_the_error() {
        let body = vec![b'x'; 5000];
        let err = parse::<Vec<u32>>(&body).unwrap_err();
        assert!(err.to_string().len() < 400, "error should stay readable");
    }

    /// A silent forwarded socket must fail, not be filled.
    ///
    /// The regression this guards is quiet and expensive: answering a dead
    /// tunnel by spawning a daemon here puts a *local* daemon behind the far
    /// machine's tab, so it answers again with none of that machine's
    /// workspaces, and the lock file it leaves beside the forward stops the
    /// real one being restored. The `.lock` assertion is the load-bearing one —
    /// the call errors either way, a moment later and for a different reason.
    #[tokio::test]
    async fn a_remote_handle_never_starts_a_daemon() {
        let dir = std::env::temp_dir().join(format!("butai-remote-api-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let socket = dir.join("far.sock");

        assert!(Api::remote(socket.clone()).get("/v1/workspaces").await.is_err());

        let lock = butai_protocol::paths::lock_path_for(&socket);
        let spawned = lock.exists();
        let bound = socket.exists();
        std::fs::remove_dir_all(&dir).ok();
        assert!(!spawned, "spawned a daemon on a forwarded socket: {}", lock.display());
        assert!(!bound, "something bound the far machine's socket");
    }
}
