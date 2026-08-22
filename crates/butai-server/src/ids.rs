//! Identifiers that have to mean something outside this process.
//!
//! Everything butai numbers internally — panes, workspaces, clients — is a
//! monotonic counter, which is right for ids whose whole life is one daemon
//! run. This module is for the other kind: an id butai hands to a *foreign*
//! program and expects to still be able to name after a restart.

/// A random (version 4) UUID, in the usual `8-4-4-4-12` hex form.
///
/// Used for agent conversation ids: butai mints one, passes it to the agent CLI
/// at launch, and persists it so a restart can ask that same CLI to reopen that
/// same conversation. The agents that accept an id validate the *shape* — Claude
/// Code's `--session-id` documents "must be a valid UUID" — so conforming to
/// RFC 4122 matters more here than the quality of the entropy behind it.
///
/// Not a `uuid` crate dependency: this is the only place in the tree that needs
/// one, and it needs one function of it.
pub fn uuid_v4() -> String {
    let mut b = random_bytes();
    // Version 4 in the high nibble of byte 6, RFC 4122 variant in the top two
    // bits of byte 8. Without these a CLI validating the string rejects it.
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex = |r: &[u8]| r.iter().map(|x| format!("{x:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        hex(&b[0..4]),
        hex(&b[4..6]),
        hex(&b[6..8]),
        hex(&b[8..10]),
        hex(&b[10..16])
    )
}

/// 16 random bytes from the kernel.
///
/// butai is Unix-only by construction (its transport is a Unix-domain socket and
/// Windows is a documented non-goal), so `/dev/urandom` is always the right
/// source and never needs a portability shim.
///
/// If it cannot be read — a pathological sandbox, an exhausted fd table — fall
/// back to mixing the clock, the pid and a per-process counter rather than
/// failing the spawn that asked for the id. That fallback is not unguessable,
/// but nothing here is a secret: the id only has to be *distinct* from the
/// other conversations in the same directory.
fn random_bytes() -> [u8; 16] {
    use std::io::Read;
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(&mut b).is_ok() {
            return b;
        }
    }
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seed = nanos
        ^ (u64::from(std::process::id()) << 32)
        ^ COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Two FNV-1a rounds over the seed, one per half, so both halves vary.
    for (half, chunk) in b.chunks_mut(8).enumerate() {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ (half as u64);
        for byte in seed.to_le_bytes() {
            h ^= u64::from(byte);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        chunk.copy_from_slice(&h.to_le_bytes());
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_a_version_4_uuid() {
        let id = uuid_v4();
        assert_eq!(id.len(), 36, "8-4-4-4-12 plus four dashes: {id}");
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), vec![8, 4, 4, 4, 12]);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'), "lowercase hex: {id}");
        // A CLI that validates the string checks these two fields.
        assert_eq!(parts[2].as_bytes()[0], b'4', "version nibble: {id}");
        assert!(
            matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'),
            "RFC 4122 variant: {id}"
        );
    }

    #[test]
    fn ids_are_distinct() {
        // The property that matters: two agents in one directory must not be
        // handed the same conversation.
        let ids: std::collections::HashSet<String> = (0..256).map(|_| uuid_v4()).collect();
        assert_eq!(ids.len(), 256, "every id was unique");
    }
}
