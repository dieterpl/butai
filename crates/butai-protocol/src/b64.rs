//! Minimal standard-alphabet base64, shared by the wire protocol and OSC 52.
//!
//! Hand-rolled rather than pulled in as a dependency, which is the same call
//! the TUI's OSC 52 encoder made when it was the only user. There are now two —
//! [`Command::PutFile`](crate::Command::PutFile) carries bytes as base64 so a
//! JSON and a MessagePack client send the identical structure — and two callers
//! in different crates is exactly when the copy should stop being local.
//!
//! Decoding accepts unpadded input: `=` is only ever redundant here, every
//! sender is a program rather than a person, and rejecting a stripped tail
//! would fail a paste for no reason a user could act on.

const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encode `data` with the standard alphabet and `=` padding.
pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Decode standard-alphabet base64, ignoring ASCII whitespace and tolerating
/// missing padding. `Err` names the offending byte, because the one thing a
/// client author needs from this is which character the server disliked.
pub fn decode(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits: u8 = 0;
    for &c in s.as_bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return Err(format!("invalid base64 character {:?}", c as char)),
        };
        acc = acc << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    // A trailing group of exactly one 6-bit symbol encodes no whole byte, so it
    // cannot be a truncation of anything valid — that is corruption, not a
    // stripped `=`.
    if bits >= 6 {
        return Err("truncated base64".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for len in 0..32usize {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 % 251) as u8).collect();
            assert_eq!(decode(&encode(&data)).unwrap(), data, "len {len}");
        }
    }

    #[test]
    fn known_vectors() {
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(decode("Zm9vYmFy").unwrap(), b"foobar");
        // Every byte value, so the +/ end of the alphabet is covered too.
        let all: Vec<u8> = (0..=255u8).collect();
        assert_eq!(decode(&encode(&all)).unwrap(), all);
    }

    #[test]
    fn tolerates_missing_padding_and_whitespace() {
        assert_eq!(decode("Zg").unwrap(), b"f");
        assert_eq!(decode("Zm8").unwrap(), b"fo");
        assert_eq!(decode("Zm9v\nYmFy").unwrap(), b"foobar");
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode("Zm9v!").is_err());
        // One leftover symbol is six bits that decode to nothing.
        assert!(decode("Zm9vY").is_err());
    }
}
