//! Git worktrees — the feature that fits butai better than it fits git's own UI.
//!
//! A worktree is a second checkout of the same repository on another branch.
//! butai's model is already **one workspace per directory**, so a worktree *is*
//! a workspace: opening one gives it its own agents, its own processes, its own
//! changes rail, and its own branch, with no stashing and no switching. That is
//! the thing an agent workbench wants and the thing `git worktree` is clumsy at
//! from a terminal — you have to remember the paths.
//!
//! Nothing here touches a repository. Listing is [`parse_list`] over
//! `git worktree list --porcelain`, and the writes are argv built by
//! [`crate::git_op::argv`], so both halves are unit-tested as text.

use std::path::PathBuf;

/// One checkout of the repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// The branch checked out there, short form. `None` when detached.
    pub branch: Option<String>,
    pub head: String,
    /// The worktree the daemon is looking at. Never removable.
    pub is_main: bool,
    pub detached: bool,
    /// `git worktree lock` — removing it needs `--force`.
    pub locked: bool,
    /// The directory is gone from disk; `prune` is what clears it.
    pub prunable: bool,
}

/// Parse `git worktree list --porcelain`.
///
/// The format is one blank-line-separated block per worktree, each starting
/// with `worktree <path>`. Attributes are bare words (`bare`, `detached`) or
/// `key value` pairs. The first block is always the main worktree.
pub fn parse_list(text: &str) -> Vec<Worktree> {
    let mut out: Vec<Worktree> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if let Some(path) = line.strip_prefix("worktree ") {
            out.push(Worktree {
                path: PathBuf::from(path),
                branch: None,
                head: String::new(),
                // Position, not a flag: git lists the main worktree first and
                // marks it in no other way.
                is_main: out.is_empty(),
                detached: false,
                locked: false,
                prunable: false,
            });
            continue;
        }
        let Some(w) = out.last_mut() else { continue };
        if let Some(head) = line.strip_prefix("HEAD ") {
            w.head = head.to_string();
        } else if let Some(branch) = line.strip_prefix("branch ") {
            // `refs/heads/feat/x` -> `feat/x`, leaving any other ref spelled out.
            w.branch = Some(branch.strip_prefix("refs/heads/").unwrap_or(branch).to_string());
        } else if line == "detached" {
            w.detached = true;
        } else if line == "locked" || line.starts_with("locked ") {
            w.locked = true;
        } else if line == "prunable" || line.starts_with("prunable ") {
            w.prunable = true;
        }
    }
    out
}

/// Validate a worktree path before it becomes an argument.
///
/// A path is not a ref, so [`crate::git_op::valid_ref_name`] does not apply —
/// but the option guard does, and for the same reason: `git worktree add
/// --foo` with a user-supplied "path" is an argument-injection hole, and
/// `git worktree remove` takes a path that git will happily interpret as a
/// flag. Absolute paths only, because a relative one resolves against the
/// daemon's cwd rather than the user's.
pub fn valid_path(s: &str) -> Result<&str, String> {
    if s.starts_with('-') {
        return Err(format!("worktree path may not start with '-': {s:?}"));
    }
    if s.is_empty() {
        return Err("empty worktree path".into());
    }
    if s.len() > 4096 {
        return Err("worktree path is too long".into());
    }
    if s.chars().any(|c| c.is_ascii_control()) {
        return Err(format!("worktree path contains a control character: {s:?}"));
    }
    if !s.starts_with('/') {
        return Err(format!("worktree path must be absolute: {s:?}"));
    }
    Ok(s)
}

/// A short label for a worktree row: the branch, or a detached head.
pub fn label(w: &Worktree) -> String {
    let name = match (&w.branch, w.detached) {
        (Some(b), _) => b.clone(),
        (None, true) => format!("detached at {:.7}", w.head),
        (None, false) => "(no branch)".to_string(),
    };
    let mut flags = Vec::new();
    if w.is_main {
        flags.push("current");
    }
    if w.locked {
        flags.push("locked");
    }
    if w.prunable {
        flags.push("gone");
    }
    let suffix = if flags.is_empty() { String::new() } else { format!(" [{}]", flags.join(" ")) };
    format!("{name}{suffix}  {}", w.path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real output, including the awkward parts: a detached worktree, a locked
    /// one, and one whose directory has been deleted.
    const PORCELAIN: &str = "\
worktree /home/paul/proj
HEAD 43c93230f1cd0f4f3f61b6e6b1a1a6ce4e35b0a1
branch refs/heads/main

worktree /home/paul/proj-feature
HEAD 0b212dd0f1cd0f4f3f61b6e6b1a1a6ce4e35b0a2
branch refs/heads/feat/detection-patterns

worktree /home/paul/proj-detached
HEAD e70c3e40f1cd0f4f3f61b6e6b1a1a6ce4e35b0a3
detached

worktree /home/paul/proj-locked
HEAD 67bff3600f1cd0f4f3f61b6e6b1a1a6ce4e35b04
branch refs/heads/federation
locked being tested

worktree /home/paul/proj-gone
HEAD a0604c900f1cd0f4f3f61b6e6b1a1a6ce4e35b05
branch refs/heads/worktree-default-agent
prunable gitdir file points to non-existent location
";

    #[test]
    fn parses_every_shape_of_worktree() {
        let w = parse_list(PORCELAIN);
        assert_eq!(w.len(), 5);

        assert_eq!(w[0].path, PathBuf::from("/home/paul/proj"));
        assert_eq!(w[0].branch.as_deref(), Some("main"));
        assert!(w[0].is_main, "the first block is the main worktree");
        assert!(!w[0].detached && !w[0].locked && !w[0].prunable);

        // A branch with a slash keeps it; only `refs/heads/` comes off.
        assert_eq!(w[1].branch.as_deref(), Some("feat/detection-patterns"));
        assert!(!w[1].is_main);

        assert!(w[2].detached);
        assert_eq!(w[2].branch, None);

        // `locked` and `prunable` both carry a reason, and both forms parse.
        assert!(w[3].locked, "locked with a reason was missed");
        assert!(w[4].prunable, "prunable with a reason was missed");
    }

    #[test]
    fn an_empty_or_junk_listing_yields_nothing_rather_than_panicking() {
        assert!(parse_list("").is_empty());
        assert!(parse_list("nonsense\nbranch refs/heads/x\n").is_empty());
    }

    /// The main worktree is where the daemon is looking; removing it is not a
    /// thing git allows and not a thing a client should be able to ask for.
    #[test]
    fn the_main_worktree_is_identified_so_it_can_be_protected() {
        let w = parse_list(PORCELAIN);
        assert_eq!(w.iter().filter(|w| w.is_main).count(), 1);
        assert!(w[0].is_main);
    }

    #[test]
    fn a_path_that_git_would_read_as_an_option_is_refused() {
        for hostile in [
            "--force",
            "-f",
            "--git-dir=/etc",
            "",
            "relative/path",
            "../escape",
            "/tmp/with\nnewline",
        ] {
            assert!(valid_path(hostile).is_err(), "{hostile:?} was accepted");
        }
        for ok in ["/home/paul/wt", "/tmp/a b/c", "/srv/repos/proj-feature"] {
            assert!(valid_path(ok).is_ok(), "{ok:?} was refused");
        }
    }

    #[test]
    fn labels_say_what_the_row_is() {
        let w = parse_list(PORCELAIN);
        assert!(label(&w[0]).starts_with("main [current]"), "{}", label(&w[0]));
        assert!(label(&w[2]).starts_with("detached at e70c3e4"), "{}", label(&w[2]));
        assert!(label(&w[3]).contains("[locked]"), "{}", label(&w[3]));
        assert!(label(&w[4]).contains("[gone]"), "{}", label(&w[4]));
        assert!(label(&w[1]).contains("/home/paul/proj-feature"));
    }
}
