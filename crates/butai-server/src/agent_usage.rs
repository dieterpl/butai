//! Which agents you start, kept so the picker can put them in order.
//!
//! A count and a last-started time per agent name, in `agent_usage.json` beside
//! the session store. It is state the daemon writes, not configuration: the
//! file `config.toml` is read by two programs and edited by hand, and a daemon
//! writing into it would fight an open editor.
//!
//! Only starts a person makes are recorded — see [`ServerCore`]'s two call
//! sites. Autostart, restart restore and a helper another agent spawns in the
//! background all go around it, so the order reflects what was picked.
//!
//! Keyed by name, so renaming an agent starts it from zero, and a name that has
//! left `config.toml` is kept but never listed.
//!
//! [`ServerCore`]: crate::core::ServerCore

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::AgentOrder;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Use {
    /// Starts you made.
    pub count: u64,
    /// When the latest one was, in seconds since the epoch.
    pub last: u64,
}

#[derive(Debug, Default)]
pub struct AgentUsage {
    uses: BTreeMap<String, Use>,
    /// Where to write. `None` for a daemon with no session store, which keeps
    /// the counts for as long as it runs and leaves nothing on disk.
    path: Option<PathBuf>,
}

impl AgentUsage {
    /// Read the file at `path`. Missing or unreadable is empty, and the next
    /// start writes it afresh: a history is worth less than a daemon that
    /// refuses to come up over one.
    pub fn load(path: PathBuf) -> Self {
        let uses = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self { uses, path: Some(path) }
    }

    pub fn get(&self, name: &str) -> Use {
        self.uses.get(name).copied().unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.uses.is_empty()
    }

    /// Note a start at `now` (seconds since the epoch) and write the file.
    pub fn record(&mut self, name: &str, now: u64) {
        let u = self.uses.entry(name.to_string()).or_default();
        u.count += 1;
        u.last = now;
        self.save();
    }

    /// `names` in the order the picker lists them.
    ///
    /// Stable: agents that tie, which is all of them before the first start,
    /// keep the order of the launchers.
    pub fn ordered(&self, names: &[String], order: AgentOrder) -> Vec<String> {
        let mut out: Vec<String> = names.to_vec();
        match order {
            AgentOrder::Config => {}
            AgentOrder::Name => out.sort_by_key(|n| n.to_lowercase()),
            AgentOrder::Frequent => {
                out.sort_by_key(|n| std::cmp::Reverse((self.get(n).count, self.get(n).last)))
            }
            AgentOrder::Recent => out.sort_by_key(|n| std::cmp::Reverse(self.get(n).last)),
        }
        out
    }

    /// Write via a sibling temp file and a rename, so a crash mid-write leaves
    /// the old history rather than half of a new one. Failure is logged and
    /// dropped: losing a count must never fail the spawn it was counting.
    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Err(e) = write_atomic(path, &self.uses) {
            tracing::warn!("agent usage: {}: {e:#}", path.display());
        }
    }
}

fn write_atomic(path: &Path, uses: &BTreeMap<String, Use>) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(uses)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn history() -> AgentUsage {
        let mut u = AgentUsage::default();
        for _ in 0..3 {
            u.record("codex", 100);
        }
        u.record("claude", 300);
        u.record("gemini", 200);
        u
    }

    #[test]
    fn frequent_puts_the_most_started_first_and_ties_keep_config_order() {
        let list = names(&["claude", "codex", "gemini", "aider", "agy"]);
        assert_eq!(
            history().ordered(&list, AgentOrder::Frequent),
            names(&["codex", "claude", "gemini", "aider", "agy"]),
            "claude and gemini tie on one start, the later one first; the unused keep their order"
        );
    }

    #[test]
    fn recent_orders_by_the_last_start() {
        let list = names(&["claude", "codex", "gemini", "aider"]);
        assert_eq!(
            history().ordered(&list, AgentOrder::Recent),
            names(&["claude", "gemini", "codex", "aider"])
        );
    }

    #[test]
    fn name_and_config_ignore_the_history() {
        let list = names(&["gemini", "Claude", "aider"]);
        let u = history();
        assert_eq!(u.ordered(&list, AgentOrder::Config), list);
        assert_eq!(u.ordered(&list, AgentOrder::Name), names(&["aider", "Claude", "gemini"]));
    }

    #[test]
    fn with_no_history_every_order_but_name_is_the_config_order() {
        let list = names(&["claude", "codex", "gemini"]);
        let u = AgentUsage::default();
        for order in [AgentOrder::Config, AgentOrder::Frequent, AgentOrder::Recent] {
            assert_eq!(u.ordered(&list, order), list, "{order:?}");
        }
    }

    #[test]
    fn the_history_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent_usage.json");
        let mut u = AgentUsage::load(path.clone());
        assert!(u.is_empty());
        u.record("claude", 10);
        u.record("claude", 20);

        let back = AgentUsage::load(path);
        assert_eq!(back.get("claude"), Use { count: 2, last: 20 });
        assert_eq!(back.get("codex"), Use::default());
    }

    #[test]
    fn an_unreadable_file_is_an_empty_history_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent_usage.json");
        std::fs::write(&path, "{ not json").unwrap();
        let mut u = AgentUsage::load(path.clone());
        assert!(u.is_empty());
        u.record("claude", 5);
        assert_eq!(AgentUsage::load(path).get("claude").count, 1, "and the next start repairs it");
    }

    #[test]
    fn without_a_path_nothing_is_written() {
        let mut u = AgentUsage::default();
        u.record("claude", 1);
        assert_eq!(u.get("claude").count, 1);
    }
}
