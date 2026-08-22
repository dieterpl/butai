//! What this program is called — on this machine and on the far end of an ssh.
//!
//! Reaching another machine means running *its* copy of this program: the host
//! picker asks it `--json whoami`, the proxy runs `proxy`, and the handoff
//! recognises the APC it writes. All three have to name a binary, and the name
//! has changed once already (`bmux` became `butai`) with the final one still
//! undecided.
//!
//! A rename that only lands here leaves every machine you have not upgraded
//! unreachable, and says so in the one way that reads as the user's fault —
//! "not installed", about a machine with a working install under the old name.
//! So the far side is searched for *every* name this program has been called
//! by, current one first, and the parts that must agree about it read
//! [`BINARIES`] rather than spelling one out.

/// Every name this program has shipped under, most recent first.
///
/// Order is the preference order: a machine carrying both an upgraded `butai`
/// and a stale `bmux` is reached through the new one.
///
/// Removing a name from the tail is a decision to stop talking to machines that
/// still have it, which is worth making deliberately rather than as part of the
/// next rename.
pub const BINARIES: &[&str] = &["butai", "bmux"];

/// The current name — the binary this build installs as, and the one every
/// message shown to a user should say.
pub const BINARY: &str = BINARIES[0];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_current_name_leads_and_the_old_one_is_still_answered() {
        assert_eq!(BINARY, "butai");
        assert_eq!(BINARIES.first(), Some(&"butai"), "the current name is tried first");
        assert!(
            BINARIES.contains(&"bmux"),
            "machines that were never upgraded are still reachable"
        );
    }

    /// Each of these becomes a `$HOME/.local/bin/<name>` test and a `command -v`
    /// in a shell fragment, and an APC prefix in the daemon's parser. A name
    /// with a space, a quote or a slash in it would break one of those on the
    /// far side, where nobody can see it.
    #[test]
    fn every_name_is_safe_to_paste_into_a_shell_and_a_prefix() {
        for name in BINARIES {
            assert!(!name.is_empty());
            assert!(
                name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "{name} is not a bare word"
            );
        }
    }
}
