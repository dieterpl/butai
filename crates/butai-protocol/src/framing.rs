//! Wire framing: 4-byte big-endian length prefix, then one encoded message.
//!
//! The `Hello` exchange is always JSON in both directions; if the client's
//! `Hello` negotiated [`Encoding::Msgpack`], every frame after each side's
//! `Hello` switches to MessagePack (named-field encoding). Third-party
//! clients can ignore msgpack entirely — JSON is the baseline.

use bytes::{Bytes, BytesMut};
use serde::{de::DeserializeOwned, Serialize};
use tokio_util::codec::LengthDelimitedCodec;

use crate::Encoding;

/// Frames above this size are rejected (a full 4K-screen frame update is far
/// below this; anything bigger indicates a corrupt stream).
pub const MAX_FRAME_LEN: usize = 32 * 1024 * 1024;

/// How many undecodable frames in a row before a connection is dropped.
///
/// A frame that will not decode is skipped rather than killing the connection:
/// the versioning rule is that additive changes do not bump `proto_version`, so
/// meeting a message the other side invented after your build is *expected*, and
/// it must cost one ignored frame rather than the session. Both directions apply
/// this, which is why it lives here rather than twice.
///
/// The cap is what keeps "tolerant" from meaning "will not admit the stream has
/// stopped making sense": far above a peer a few releases ahead, far below
/// forever. A decode error is per-frame and leaves the framing intact — a bad
/// *length prefix* is a different failure and is always fatal, because the next
/// frame boundary is then unknown.
pub const MAX_CONSECUTIVE_BAD_FRAMES: u32 = 16;

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("msgpack encode: {0}")]
    MsgpackEncode(#[from] rmp_serde::encode::Error),
    #[error("msgpack decode: {0}")]
    MsgpackDecode(#[from] rmp_serde::decode::Error),
}

/// The `tokio_util` codec both sides wrap their socket in.
pub fn length_codec() -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder()
        .max_frame_length(MAX_FRAME_LEN)
        .length_field_length(4)
        .big_endian()
        .new_codec()
}

pub fn encode<T: Serialize>(msg: &T, enc: Encoding) -> Result<Bytes, CodecError> {
    Ok(match enc {
        Encoding::Json => Bytes::from(serde_json::to_vec(msg)?),
        Encoding::Msgpack => Bytes::from(rmp_serde::to_vec_named(msg)?),
    })
}

pub fn decode<T: DeserializeOwned>(buf: &BytesMut, enc: Encoding) -> Result<T, CodecError> {
    Ok(match enc {
        Encoding::Json => serde_json::from_slice(buf)?,
        Encoding::Msgpack => rmp_serde::from_slice(buf)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientMsg, InputEvent, KeyEvent};

    #[test]
    fn encode_decode_both_encodings() {
        let msg = ClientMsg::Input(InputEvent::Key(KeyEvent::char('x')));
        for enc in [Encoding::Json, Encoding::Msgpack] {
            let bytes = encode(&msg, enc).unwrap();
            let back: ClientMsg = decode(&BytesMut::from(&bytes[..]), enc).unwrap();
            assert_eq!(back, msg);
        }
    }
}
