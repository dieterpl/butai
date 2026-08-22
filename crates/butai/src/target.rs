//! The target grammar: how a command names a workspace or a pane.
//!
//! ```text
//! TARGET := [SCOPE ":"] LEAF
//! SCOPE  := <workspace id> | <workspace name>
//! LEAF   := <pane id> | <agent alias> | "stage"
//! ```
//!
//! So `4`, `1:4`, `api:4`, `reviewer`, `api:reviewer` and `1:stage` are all
//! targets. butai allocates pane ids from a single daemon-wide counter, so a bare
//! pane id is already unambiguous and the scope half is optional — unlike a
//! multiplexer that numbers panes per window.
//!
//! When a scope *is* given alongside a numeric leaf it is an assertion, not a
//! lookup: the daemon answers 404 if that pane does not belong to that
//! workspace. A script that cached an id across a workspace teardown gets an
//! error rather than acting on whatever pane inherited the number.
//!
//! Only the syntactic split happens here. Turning a name into ids needs live
//! daemon state, so that is `GET /v1/resolve?target=` — which also gives every
//! GUI client the same lookup for free.

use anyhow::{bail, Result};

/// A target as written on the command line, split but not resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The part before `:`, when present.
    pub scope: Option<String>,
    pub leaf: Leaf,
}

/// The addressed thing itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leaf {
    /// A numeric pane id.
    Pane(u64),
    /// An agent alias.
    Name(String),
    /// Whatever is on the workspace's stage right now.
    Stage,
}

impl Target {
    /// Parse a target string.
    ///
    /// Splits on the *first* `:` so an alias may contain one; a scope may not,
    /// which costs nothing because a scope is a workspace id or name.
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.is_empty() {
            bail!("empty target");
        }
        let (scope, leaf) = match s.split_once(':') {
            Some((scope, leaf)) => {
                if scope.trim().is_empty() {
                    bail!("target {s:?} has an empty workspace before the ':'");
                }
                if leaf.trim().is_empty() {
                    bail!("target {s:?} has nothing after the ':'");
                }
                (Some(scope.trim().to_string()), leaf.trim())
            }
            None => (None, s),
        };
        Ok(Self { scope, leaf: Leaf::parse(leaf) })
    }

    /// True when this needs no daemon round-trip to resolve: a bare pane id
    /// carries everything the routes need.
    pub fn is_direct(&self) -> bool {
        self.scope.is_none() && matches!(self.leaf, Leaf::Pane(_))
    }

    /// Re-render in canonical form, for sending to `GET /v1/resolve`.
    pub fn to_query(&self) -> String {
        match &self.scope {
            Some(scope) => format!("{scope}:{}", self.leaf),
            None => self.leaf.to_string(),
        }
    }
}

impl Leaf {
    fn parse(s: &str) -> Self {
        if s == "stage" {
            return Leaf::Stage;
        }
        // A name that happens to be all digits is a pane id. Aliases are
        // rejected at creation if they parse as a number, so this is not a
        // collision the user can walk into.
        match s.parse::<u64>() {
            Ok(n) => Leaf::Pane(n),
            Err(_) => Leaf::Name(s.to_string()),
        }
    }
}

impl std::fmt::Display for Leaf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Leaf::Pane(n) => write!(f, "{n}"),
            Leaf::Name(s) => write!(f, "{s}"),
            Leaf::Stage => write!(f, "stage"),
        }
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_query())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Target {
        Target::parse(s).expect("parses")
    }

    #[test]
    fn bare_pane_id_needs_no_scope() {
        let target = t("4");
        assert_eq!(target, Target { scope: None, leaf: Leaf::Pane(4) });
        assert!(target.is_direct());
    }

    #[test]
    fn scope_may_be_an_id_or_a_name() {
        assert_eq!(t("1:4").scope.as_deref(), Some("1"));
        assert_eq!(t("api:4").scope.as_deref(), Some("api"));
        // A scoped pane id still has to be checked against the workspace.
        assert!(!t("1:4").is_direct());
    }

    #[test]
    fn an_alias_is_anything_that_is_not_a_number() {
        assert_eq!(t("reviewer").leaf, Leaf::Name("reviewer".into()));
        assert_eq!(t("api:reviewer").leaf, Leaf::Name("reviewer".into()));
        assert!(!t("reviewer").is_direct());
    }

    #[test]
    fn stage_is_its_own_leaf() {
        assert_eq!(t("stage").leaf, Leaf::Stage);
        assert_eq!(t("1:stage"), Target { scope: Some("1".into()), leaf: Leaf::Stage });
    }

    #[test]
    fn splits_on_the_first_colon_so_an_alias_may_contain_one() {
        let target = t("api:build:web");
        assert_eq!(target.scope.as_deref(), Some("api"));
        assert_eq!(target.leaf, Leaf::Name("build:web".into()));
    }

    #[test]
    fn surrounding_space_is_ignored() {
        assert_eq!(t("  1 : reviewer "), t("1:reviewer"));
    }

    #[test]
    fn round_trips_through_the_query_form() {
        for s in ["4", "1:4", "api:reviewer", "stage", "1:stage"] {
            assert_eq!(t(s).to_query(), s, "{s}");
        }
    }

    #[test]
    fn rejects_empty_halves() {
        for s in ["", "   ", ":", ":4", "1:", "1:   "] {
            assert!(Target::parse(s).is_err(), "{s:?} should not parse");
        }
    }
}
