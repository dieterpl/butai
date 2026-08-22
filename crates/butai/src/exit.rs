//! Exit codes.
//!
//! `butai` is meant to be shelled out to — by a script, by a plugin, and above
//! all by an agent running inside one of its own panes. For that caller the exit
//! code *is* the API: under `--quiet` nothing is printed at all, so
//! `butai agent wait 7 -q && ./deploy.sh` has to be able to tell "it finished"
//! from "it timed out" from "there is no pane 7".
//!
//! | code | meaning |
//! |------|---------|
//! | 0    | success; for `wait`, the target reached the state |
//! | 1    | generic failure — daemon unreachable, 5xx, unexpected reply |
//! | 2    | not found — no such workspace, pane, or agent |
//! | 3    | `wait` timed out; the target is still running |
//! | 4    | the target exited, or `process status` found a failure |
//! | 64   | usage — bad flag, bad target, self-target (`EX_USAGE`) |
//!
//! Codes 2 and 64 come straight from the daemon's own 404/400, which is why
//! [`butai_client::api::ApiError`] keeps the status rather than flattening to a string.

use butai_client::api::ApiError;

pub const OK: u8 = 0;
pub const FAILED: u8 = 1;
pub const NOT_FOUND: u8 = 2;
pub const TIMED_OUT: u8 = 3;
pub const EXITED: u8 = 4;
/// `EX_USAGE` from `sysexits.h`, which clap and the BSD tools both use.
pub const USAGE: u8 = 64;

/// A command used wrongly — a target that does not parse, an unknown state
/// name, or a pane the caller may not address.
///
/// Distinct from a plain `anyhow!` so [`code_for`] can return `EX_USAGE`
/// without matching on message text.
#[derive(Debug)]
pub struct Usage(pub String);

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Usage {}

/// Shorthand for bailing with a usage error.
pub fn usage<T>(msg: impl Into<String>) -> anyhow::Result<T> {
    Err(Usage(msg.into()).into())
}

/// A target that does not exist — an unknown pane, workspace, or agent name.
///
/// The daemon reports this as a 404 for things it is asked about directly; this
/// is the same answer for the resolution the CLI does client-side, so
/// `butai pane read 9999` and `butai pane read 1:9999` agree on the exit code.
#[derive(Debug)]
pub struct NotFound(pub String);

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotFound {}

/// Shorthand for bailing with a not-found error.
pub fn not_found<T>(msg: impl Into<String>) -> anyhow::Result<T> {
    Err(NotFound(msg.into()).into())
}

/// Pick the exit code for a failed command.
///
/// Walks the `anyhow` cause chain, so a context-wrapped daemon error still
/// reports the status the daemon gave it.
pub fn code_for(err: &anyhow::Error) -> u8 {
    for cause in err.chain() {
        if cause.downcast_ref::<Usage>().is_some() {
            return USAGE;
        }
        if cause.downcast_ref::<NotFound>().is_some() {
            return NOT_FOUND;
        }
        if let Some(api) = cause.downcast_ref::<ApiError>() {
            return match api.status.as_u16() {
                404 => NOT_FOUND,
                400 => USAGE,
                _ => FAILED,
            };
        }
    }
    FAILED
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use hyper::StatusCode;

    fn api_err(status: StatusCode) -> anyhow::Error {
        ApiError { status, message: "boom".into() }.into()
    }

    #[test]
    fn a_plain_error_is_a_generic_failure() {
        assert_eq!(code_for(&anyhow::anyhow!("nope")), FAILED);
    }

    #[test]
    fn daemon_statuses_map_to_their_own_codes() {
        assert_eq!(code_for(&api_err(StatusCode::NOT_FOUND)), NOT_FOUND);
        assert_eq!(code_for(&api_err(StatusCode::BAD_REQUEST)), USAGE);
        assert_eq!(code_for(&api_err(StatusCode::INTERNAL_SERVER_ERROR)), FAILED);
    }

    #[test]
    fn the_status_survives_being_wrapped_in_context() {
        let err =
            Err::<(), _>(api_err(StatusCode::NOT_FOUND)).context("reading a pane").unwrap_err();
        assert_eq!(code_for(&err), NOT_FOUND, "context must not hide the status");
    }

    #[test]
    fn a_missing_target_matches_the_daemons_own_404() {
        let err: anyhow::Error = NotFound("no pane 9999".into()).into();
        assert_eq!(code_for(&err), NOT_FOUND);
        assert_eq!(code_for(&api_err(StatusCode::NOT_FOUND)), NOT_FOUND, "same code either way");
    }

    #[test]
    fn a_usage_error_is_ex_usage() {
        let err: anyhow::Error = Usage("bad target".into()).into();
        assert_eq!(code_for(&err), USAGE);
        assert_eq!(code_for(&Err::<(), _>(err).context("resolving").unwrap_err()), USAGE);
    }
}
