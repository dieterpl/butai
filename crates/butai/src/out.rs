//! Output rendering: the one module allowed to write to stdout.
//!
//! Two modes. Under `--json` the daemon's own response body is re-emitted
//! **verbatim** — not re-serialized from a parsed struct — so the CLI's JSON is
//! the REST API's JSON by construction and the two cannot drift as DTOs gain
//! fields. Otherwise a human renderer runs.
//!
//! `--quiet` suppresses success output entirely; the exit code is the answer.
//! Errors still go to stderr, because a silent failure is not a quiet one.

use std::io::Write;

use anyhow::Result;

pub struct Out {
    json: bool,
    quiet: bool,
}

impl Out {
    pub fn new(json: bool, quiet: bool) -> Self {
        Self { json, quiet }
    }

    /// Emit a daemon response: the raw body under `--json`, otherwise whatever
    /// `render` writes.
    pub fn emit(
        &self,
        body: &[u8],
        render: impl FnOnce(&mut dyn Write) -> Result<()>,
    ) -> Result<()> {
        if self.quiet {
            return Ok(());
        }
        let stdout = std::io::stdout();
        let mut w = stdout.lock();
        if self.json {
            w.write_all(body)?;
            // Daemon bodies carry no trailing newline; a shell prompt wants one.
            if !body.ends_with(b"\n") {
                w.write_all(b"\n")?;
            }
        } else {
            render(&mut w)?;
        }
        w.flush()?;
        Ok(())
    }

    /// Emit a value the CLI itself produced, where there is no daemon body to
    /// pass through — `butai ls` answers on the framed protocol, not over HTTP.
    pub fn emit_owned<T: serde::Serialize>(
        &self,
        value: &T,
        render: impl FnOnce(&mut dyn Write) -> Result<()>,
    ) -> Result<()> {
        if self.json {
            let body = serde_json::to_vec(value)?;
            return self.emit(&body, render);
        }
        self.emit(&[], render)
    }
}
