//! Per-connection plumbing: framed socket <-> ServerCore event channel.
//!
//! The first frame in each direction is always JSON (`Hello`); afterwards
//! both sides use the encoding the client's Hello negotiated.

use butai_protocol::framing::{decode, encode, length_codec, MAX_CONSECUTIVE_BAD_FRAMES};
use butai_protocol::{ClientMsg, Encoding, ServerMsg};
use futures::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::codec::Framed;
use tracing::{debug, warn};

use crate::core::{ClientId, Event};

pub async fn handle_connection(stream: UnixStream, id: ClientId, events: UnboundedSender<Event>) {
    // Same socket, two protocols: a framed hello always starts with the top
    // byte of a 4-byte big-endian length prefix (`0x00`), while an HTTP
    // request starts with an ASCII method letter. Peek one byte without
    // consuming it (MSG_PEEK — UnixStream has no async peek) to route.
    let first = loop {
        if stream.readable().await.is_err() {
            return;
        }
        let mut probe = [0u8; 1];
        match rustix::net::recv(&stream, &mut probe, rustix::net::RecvFlags::PEEK) {
            Ok(0) => return, // closed before sending anything
            Ok(_) => break probe[0],
            Err(rustix::io::Errno::WOULDBLOCK) | Err(rustix::io::Errno::INTR) => continue,
            Err(e) => {
                debug!("client {id}: peek failed: {e}");
                return;
            }
        }
    };
    if first != 0 {
        crate::http_conn::handle(stream, events).await;
        return;
    }

    let mut framed = Framed::new(stream, length_codec());

    // First frame must be a JSON Hello carrying the negotiated encoding.
    let hello: ClientMsg = match framed.next().await {
        Some(Ok(bytes)) => match decode(&bytes, Encoding::Json) {
            Ok(msg) => msg,
            Err(e) => {
                warn!("client {id}: bad hello: {e}");
                return;
            }
        },
        _ => return,
    };
    let encoding = match &hello {
        ClientMsg::Hello { encoding, .. } => *encoding,
        _ => {
            warn!("client {id}: first message was not hello");
            return;
        }
    };

    let (tx, mut rx) = unbounded_channel::<ServerMsg>();
    if events.send(Event::ClientConnected(id, tx)).is_err() {
        return;
    }
    if events.send(Event::Client(id, hello)).is_err() {
        return;
    }

    let (mut sink, mut source) = framed.split();

    // Outbound: core -> socket. The server Hello acknowledges the encoding
    // switch and is itself JSON; everything after uses `encoding`.
    let writer = tokio::spawn(async move {
        let mut first = true;
        while let Some(msg) = rx.recv().await {
            let enc = if first { Encoding::Json } else { encoding };
            first = false;
            let last = matches!(msg, ServerMsg::Detached { .. });
            match encode(&msg, enc) {
                Ok(bytes) => {
                    if sink.send(bytes).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    warn!("encode failed: {e}");
                    break;
                }
            }
            if last {
                break;
            }
        }
        let _ = sink.close().await;
    });

    // Inbound: socket -> core.
    //
    // An undecodable frame is *skipped*, not fatal. The versioning rule in
    // `docs/protocol.md` is that additive changes — a new command — do not bump
    // `proto_version`, and that only works if the side which has never heard of
    // the new message ignores it. Dropping the connection instead turns "this
    // daemon is one release behind" into a reconnect loop: the client re-dials,
    // sends the same unknown message at the next stage change, and is dropped
    // again. A real session was caught doing exactly that 25 times, and it
    // presented as the stage blanking rather than as anything version-shaped.
    //
    // Framing errors stay fatal, and are a different thing: they come from
    // `source.next()` above, and a bad length prefix means the next frame
    // boundary is no longer known, so there is nothing to resynchronise to.
    let mut consecutive_bad = 0u32;
    while let Some(frame) = source.next().await {
        let bytes = match frame {
            Ok(b) => b,
            Err(e) => {
                debug!("client {id}: read error: {e}");
                break;
            }
        };
        let msg: ClientMsg = match decode(&bytes, encoding) {
            Ok(m) => {
                consecutive_bad = 0;
                m
            }
            Err(e) => {
                consecutive_bad += 1;
                warn!("client {id}: skipping undecodable frame ({consecutive_bad}): {e}");
                // A cap, so a stream that is genuinely desynchronised rather
                // than merely newer than us still ends instead of spinning.
                if consecutive_bad >= MAX_CONSECUTIVE_BAD_FRAMES {
                    warn!("client {id}: {consecutive_bad} undecodable frames in a row, dropping");
                    break;
                }
                continue;
            }
        };
        if events.send(Event::Client(id, msg)).is_err() {
            break;
        }
    }
    let _ = events.send(Event::ClientGone(id));
    writer.abort();
    debug!("client {id} disconnected");
}
