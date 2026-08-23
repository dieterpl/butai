//! The daemon's half of `~/.butai/config.toml`.
//!
//! One file, two readers. This one takes the shell, the scrollback and restore
//! budgets, `[api]`, `[[agents]]` and `[update] allow_remote`; the client's
//! `config::Config` takes `[keys]`, `[theme]`, `[ui]`, `[[remote]]`, the prefix
//! and the rest of `[update]`. Neither struct declares the other's tables and
//! serde ignores what it does not know, so each side parses the whole file and
//! sees only its own part — `[update]` is the one table they share, and they
//! share it a key at a time.
//!
//! Also `.butai.toml`, the per-project file, which is entirely the daemon's:
//! what to name a workspace, which processes to bring up, which agents to
//! autostart.
//!
//! Everything is optional; unknown keys are ignored rather than rejected, so
//! configs survive version skew in both directions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub api: Api,
    pub update: Update,
    pub agents: Vec<AgentDef>,
    pub budgets: Vec<BudgetDef>,
}

/// A ceiling you declare for one accounting window on the USAGE page.
///
/// Separate from [`AgentDef`] rather than a field on it, because the two are
/// different concerns: that one says how to *launch* a CLI, this one says what
/// you are paying for. They are also independent in practice — a budget is
/// worth declaring for a stock agent nobody has customised, and customising a
/// launcher says nothing about a plan.
///
/// It exists because no CLI publishes its limits: the daemon can count what a
/// window has consumed but has nothing to divide it by, so a proportion can
/// only ever come from a number the user states. Windows are matched by the
/// label the daemon writes (`last 5h`, `last 7d`, `last 7d · opus`); a `window`
/// that matches nothing is ignored, which is the right failure for a number
/// nothing can validate.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct BudgetDef {
    /// The `[[agents]]` name this applies to.
    pub agent: String,
    /// The window label to put a ceiling under.
    pub window: String,
    pub tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct General {
    /// Defaults to `$SHELL`, then `/bin/sh`.
    pub default_shell: Option<String>,
    pub exit_when_empty: bool,
    /// Scrollback lines kept per terminal pane.
    pub scrollback: usize,
    /// Bytes of raw PTY output kept per pane for restart restore, replayed into
    /// the fresh pane when the daemon comes back. `0` disables restore and
    /// stops the capture entirely.
    ///
    /// Counted in bytes rather than lines because that is what bounds the cost:
    /// this is the untouched output stream, so a pane redrawing a full-screen
    /// TUI spends far more per line than a pane printing log text, and a line
    /// budget would size the two wildly differently. 256 KiB covers a few
    /// screens of a redraw-heavy agent and well over a thousand lines of plain
    /// output.
    pub restore_bytes: usize,
}

impl Default for General {
    fn default() -> Self {
        Self {
            default_shell: None,
            exit_when_empty: true,
            scrollback: 5000,
            restore_bytes: 256 * 1024,
        }
    }
}

/// The daemon's share of `[update]`. The client reads `check` and
/// `declined_version` out of the same table and ignores this key, as this
/// struct ignores those — the "one file, two readers" split at the top of this
/// module.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Update {
    /// Let a client attached to this daemon make it update *itself* —
    /// `POST /v1/update`, and `butai update --daemon` on top of it.
    ///
    /// Off by default, and the default is the interesting half. The socket's
    /// only access control is the `0700` on its directory, and over a forward
    /// or `butai proxy` the far end is whoever holds the ssh session; "can
    /// reach the daemon" is a much weaker claim than "may replace the program
    /// this machine runs". Turning it on is the machine's owner saying those
    /// are the same set here.
    ///
    /// It is not a promise that the update is quiet: when this fires, clients
    /// are detached and every workspace is killed and restored, exactly as
    /// `kill-server` already does.
    pub allow_remote: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Api {
    /// 0 = disabled. Nonzero enables a localhost WebSocket listener with
    /// token auth (dev convenience only; SSH is the remote-access path).
    pub websocket_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgentDef {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Args used *instead of* [`Self::args`] when this agent is respawned by
    /// restart restore, so the CLI reopens its previous conversation rather
    /// than starting an empty one. Empty means "no resume support": the agent
    /// comes back with its scrollback replayed but a fresh session.
    ///
    /// A full replacement rather than a suffix because resume is not uniformly
    /// a flag — `claude` takes `--continue` alongside its other args, but
    /// `codex` resumes through a subcommand that has to come first, and
    /// appending could not express that. The cost is repeating the launch flags
    /// in both lists, which is why the built-ins below spell them out.
    #[serde(default)]
    pub resume_args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Regex that means "blocked on you" when it matches this agent's footer.
    ///
    /// The built-in prompt markers are generic on purpose — they have to work
    /// for a CLI nobody has seen — so they misfire on an agent that spells its
    /// confirmations unusually, in either direction. Setting this *replaces*
    /// them for this agent rather than adding to it, which is the only shape
    /// that can fix a false positive as well as a false negative.
    ///
    /// Matched case-insensitively against the footer band, one line at a time.
    #[serde(default)]
    pub waiting_pattern: Option<String>,
    /// Regex that means "still working", replacing the built-in busy markers
    /// for this agent. Same rationale as [`Self::waiting_pattern`].
    ///
    /// Anchor it to a key the status line offers (`esc to interrupt`) rather
    /// than a bare verb: the footer band scrolls prose too, and a match on
    /// "interrupt" alone pins the pane to busy for as long as the sentence is
    /// on screen — no spinner ever stopping, no finished notification.
    #[serde(default)]
    pub busy_pattern: Option<String>,
}

impl Config {
    /// `~/.butai/config.toml`. The same file the client reads its own half of.
    pub fn path() -> PathBuf {
        butai_protocol::paths::config_path()
    }

    /// Load from the default path; missing file yields defaults. Returns
    /// human-readable warnings (parse fallback, bad keybindings, ...).
    pub fn load() -> (Self, Vec<String>) {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut cfg = match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    warnings.push(format!("{}: {e}; using defaults", path.display()));
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        };
        cfg.fill_builtins();
        (cfg, warnings)
    }

    pub fn with_defaults() -> Self {
        let mut cfg = Config::default();
        cfg.fill_builtins();
        cfg
    }

    fn fill_builtins(&mut self) {
        if self.agents.is_empty() {
            // Agents run unattended in rail panes, so the built-in launchers
            // use each CLI's auto-approve flag; override via [[agents]].
            // Third column is `resume_args` (see [`AgentDef::resume_args`]).
            //
            // `{session_id}` is the conversation butai names for this pane. It
            // is what makes restore correct with more than one agent open: the
            // obvious flags (`claude --continue`, `gemini --resume latest`) all
            // mean "the most recent conversation *in this directory*", so two
            // agents in one workspace would reopen the same transcript and
            // interleave into it. Naming it removes the ambiguity.
            //
            // Note the id is set with one flag and reopened with another. Both
            // CLIs refuse to re-declare an id that already exists — gemini
            // exits outright — so `--session-id` must appear only in `args` and
            // `--resume` only in `resume_args`.
            //
            // Filled in only where verified against the installed CLI:
            // `claude` v2.1 and `gemini` v0.53.1, both 2026-08-03. The rest are
            // left empty deliberately — a wrong flag makes the CLI exit on
            // launch. Fill them in via `[[agents]]` once checked against the
            // CLI you actually run.
            let builtins: [(&str, &[&str], &[&str]); 5] = [
                (
                    "claude",
                    &["--dangerously-skip-permissions", "--session-id", "{session_id}"],
                    &["--dangerously-skip-permissions", "--resume", "{session_id}"],
                ),
                // Codex has no way to be told an id at launch; it assigns its
                // own, and `codex resume` takes it afterwards. Reopening the
                // right one therefore means learning the id from codex first,
                // which butai does not do yet.
                ("codex", &["--dangerously-bypass-approvals-and-sandbox"], &[]),
                (
                    "gemini",
                    &["--yolo", "--session-id", "{session_id}"],
                    &["--yolo", "--resume", "{session_id}"],
                ),
                // aider has no session concept at all: its history is per
                // directory, so there is nothing per-pane to name.
                ("aider", &["--yes-always"], &[]),
                // Antigravity, Google's agent CLI and the announced successor
                // to `gemini`. Its binary is `agy`, so that is the agent's name
                // here, the same way the others are named after theirs.
                //
                // Codex-shaped for sessions: `agy --conversation <id>` reopens
                // one, but nothing names an id at *launch*, so butai has no id
                // to hand back and `resume_args` stays empty. `--continue` is
                // deliberately not used — it means "the most recent
                // conversation in this directory", which is exactly the
                // ambiguity `{session_id}` exists to remove.
                //
                // Flag verified against agy 1.1.12 on 2026-08-11.
                ("agy", &["--dangerously-skip-permissions"], &[]),
            ];
            for (name, args, resume_args) in builtins {
                self.agents.push(AgentDef {
                    name: name.into(),
                    command: name.into(),
                    args: args.iter().map(|s| s.to_string()).collect(),
                    resume_args: resume_args.iter().map(|s| s.to_string()).collect(),
                    env: HashMap::new(),
                    // The built-ins are exactly the agents the generic tables
                    // are tuned against, so they carry no overrides.
                    waiting_pattern: None,
                    busy_pattern: None,
                });
            }
        }
    }

    pub fn shell(&self) -> String {
        self.general
            .default_shell
            .clone()
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/sh".into())
    }

    pub fn agent(&self, name: &str) -> Option<&AgentDef> {
        self.agents.iter().find(|a| a.name == name)
    }
}

/// Per-project workspace file (`.butai.toml` at the workspace root):
/// managed processes and agents to start when the workspace opens.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct WorkspaceFile {
    /// Workspace name override (used when none is given on the CLI).
    pub name: Option<String>,
    pub processes: Vec<ProcDef>,
    pub agents: WorkspaceAgents,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProcDef {
    pub name: String,
    pub cmd: String,
    /// Substring of output that flips the row's status to "ok".
    #[serde(default)]
    pub ready: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct WorkspaceAgents {
    pub autostart: Vec<String>,
}

impl WorkspaceFile {
    pub fn load(cwd: &Path) -> (Self, Vec<String>) {
        let path = cwd.join(".butai.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<WorkspaceFile>(&text) {
                Ok(file) => (file, vec![]),
                Err(e) => (WorkspaceFile::default(), vec![format!("{}: {e}", path.display())]),
            },
            Err(_) => (WorkspaceFile::default(), vec![]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_have_builtins() {
        let cfg = Config::with_defaults();
        assert!(cfg.agent("claude").is_some());
        assert!(cfg.agent("agy").is_some(), "antigravity is a built-in");
        assert_eq!(cfg.general.scrollback, 5000);
    }

    /// Two promises the built-in table makes, and the second is the one that
    /// bites: a resume flag that does *not* name the conversation means "the
    /// most recent one in this directory", so two agents in a workspace reopen
    /// the same transcript and interleave into it. Leaving `resume_args` empty
    /// is the correct answer for a CLI that cannot be told an id — not
    /// `--continue`, which looks like it works until there are two.
    #[test]
    fn every_builtin_auto_approves_and_names_the_conversation_it_resumes() {
        for agent in &Config::with_defaults().agents {
            assert!(
                !agent.args.is_empty(),
                "{} launches without its auto-approve flag",
                agent.name
            );
            if !agent.resume_args.is_empty() {
                assert!(
                    agent.resume_args.iter().any(|a| a.contains("{session_id}")),
                    "{} resumes 'the most recent conversation' rather than its own",
                    agent.name
                );
            }
        }
    }

    #[test]
    fn parses_full_config() {
        let text = r##"
            [general]
            default_shell = "fish"

            # The client's tables. This side declares none of them, so they must
            # parse away silently rather than failing the whole file.
            [keys]
            "s" = "split vertical"

            [[agents]]
            name = "claude"
            command = "claude"
            args = ["--continue"]

            [theme]
            border_focused = "#ff0000"
        "##;
        let mut cfg: Config = toml::from_str(text).unwrap();
        cfg.fill_builtins();
        assert_eq!(cfg.general.default_shell.as_deref(), Some("fish"));
        assert_eq!(cfg.agents.len(), 1, "explicit agents replace builtins");
        assert_eq!(cfg.agent("claude").unwrap().args, vec!["--continue"]);
    }

    /// A `.butai.toml` from before rail widths went global still loads. It
    /// carries a `[ui]` table that nothing reads any more, and an unknown table
    /// has to be *ignored* rather than rejected — there is no
    /// `deny_unknown_fields` here precisely so old project files keep working.
    #[test]
    fn an_old_workspace_file_with_a_ui_table_still_loads() {
        let text = "name = \"demo\"\n\n[ui]\nleft_rail = 36\nright_rail = 50\n";
        let file: WorkspaceFile = toml::from_str(text).expect("[ui] must not fail the parse");
        assert_eq!(file.name.as_deref(), Some("demo"));
    }
}
