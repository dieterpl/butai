//! The `butai` binary. The command tree lives in [`cli`]; this is only the entry
//! point, so that adding a subcommand never means editing `main`.
//!
//! It returns an [`ExitCode`] rather than a `Result` because butai is meant to be
//! shelled out to — by a script, a plugin, or an agent running inside one of its
//! own panes. The codes in [`exit`] are part of that interface: collapsing every
//! failure to 1 would make `butai agent wait 7 && deploy` (it timed out)
//! indistinguishable from `butai agent wait 77 && deploy` (no such pane).

use std::process::ExitCode;

use clap::Parser;

mod cli;
mod exit;
mod handoff;
mod out;
mod proxy;
mod standalone;
mod target;

fn main() -> ExitCode {
    let cli = match cli::Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            // clap writes `--help` and `--version` to stdout and means them as
            // success. Anything else is a usage error, reported as 64 rather
            // than clap's own 2 so it matches the rest of the CLI.
            let _ = e.print();
            return match e.kind() {
                clap::error::ErrorKind::DisplayHelp
                | clap::error::ErrorKind::DisplayVersion
                | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                    ExitCode::SUCCESS
                }
                _ => ExitCode::from(exit::USAGE),
            };
        }
    };
    match cli::run(cli) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("Error: {e:#}");
            ExitCode::from(exit::code_for(&e))
        }
    }
}
