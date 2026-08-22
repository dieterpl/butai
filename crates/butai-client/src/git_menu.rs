//! The `g` command menu on the CHANGES rail.
//!
//! The rail has room for about seven verbs and already had them, so the rest of
//! git lives behind one key. The menu is a *table*, not a widget: the same
//! [`ITEMS`] list drives what is drawn, which mnemonic matches, and what a click
//! activates, so those three can never disagree — the discipline
//! `render::CHANGES_HINTS` already uses for the hint rows.
//!
//! It is drawn as a **stack of flat lists**, not a tree. Choosing a group
//! replaces the list with that group's rows plus a `..` row, exactly as the
//! workspace folder picker already does for directories. So
//! `render::draw_list_overlay` needs no nesting support and its three existing
//! callers pay nothing for this one.
//!
//! Rows are added here only once the operation behind them works. A menu that
//! lists something it cannot do is worse than a shorter menu.

/// A group of related operations — one row on the menu's top level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuGroup {
    Branch,
    Remote,
    Stash,
    Integrate,
    Fixup,
    Worktree,
    Tag,
}

impl MenuGroup {
    pub const ALL: [MenuGroup; 7] = [
        MenuGroup::Branch,
        MenuGroup::Remote,
        MenuGroup::Stash,
        MenuGroup::Integrate,
        MenuGroup::Fixup,
        MenuGroup::Worktree,
        MenuGroup::Tag,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MenuGroup::Branch => "Branch",
            MenuGroup::Remote => "Remote",
            MenuGroup::Stash => "Stash",
            MenuGroup::Integrate => "Integrate",
            MenuGroup::Fixup => "Fixup",
            MenuGroup::Worktree => "Worktree",
            MenuGroup::Tag => "Tag",
        }
    }

    /// The letter that jumps straight here.
    pub fn key(self) -> char {
        match self {
            MenuGroup::Branch => 'b',
            MenuGroup::Remote => 'r',
            MenuGroup::Stash => 'z',
            MenuGroup::Integrate => 'i',
            MenuGroup::Fixup => 'f',
            MenuGroup::Worktree => 'w',
            MenuGroup::Tag => 't',
        }
    }
}

/// One operation the menu can start.
///
/// The single vocabulary the menu targets, so adding a row is a compile error
/// until something handles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitAction {
    /// Switch to an existing branch — opens the branch picker.
    Checkout,
    /// Create and switch to a new branch — opens a text prompt.
    NewBranch,
    /// Delete a branch — opens the branch picker, then confirms.
    DeleteBranch,
    /// Choose a stash to restore, rather than always the latest.
    StashList,
    /// Choose a stash to throw away.
    StashDrop,
    /// Create a tag on HEAD — opens a text prompt.
    TagCreate,
    /// Choose a tag to delete.
    TagDelete,
    /// Choose a remote to remove.
    RemoteRemove,
    /// List the repository's worktrees; choosing one opens it as a workspace.
    WorktreeList,
    /// Create a worktree on a new branch — opens a text prompt for the name.
    WorktreeAdd,
    /// Remove a worktree — opens the worktree picker, then confirms.
    WorktreeRemove,
    /// Forget worktrees whose directories are gone.
    WorktreePrune,
    Fetch,
    Pull,
    PullRebase,
    Push,
    PushUpstream,
    PushForce,
    StashPush,
    StashPop,
    Merge,
    Rebase,
    SequenceContinue,
    SequenceAbort,
    SequenceSkip,
    Amend,
    ResetSoft,
    ResetHard,
}

/// A menu row: its group, mnemonic, label, and what it does.
pub struct MenuItem {
    pub group: MenuGroup,
    pub key: char,
    pub label: &'static str,
    pub action: GitAction,
}

const fn item(group: MenuGroup, key: char, label: &'static str, action: GitAction) -> MenuItem {
    MenuItem { group, key, label, action }
}

/// Every operation, grouped. Keep each group under twelve rows: the shared list
/// overlay clamps to sixteen lines, three of which are chrome and one the `..`.
pub const ITEMS: &[MenuItem] = &[
    item(MenuGroup::Branch, 'c', "checkout…", GitAction::Checkout),
    item(MenuGroup::Branch, 'n', "new branch…", GitAction::NewBranch),
    item(MenuGroup::Branch, 'd', "delete branch…", GitAction::DeleteBranch),
    item(MenuGroup::Remote, 'f', "fetch --prune", GitAction::Fetch),
    item(MenuGroup::Remote, 'l', "pull", GitAction::Pull),
    item(MenuGroup::Remote, 'r', "pull --rebase", GitAction::PullRebase),
    item(MenuGroup::Remote, 'p', "push", GitAction::Push),
    item(MenuGroup::Remote, 'u', "push --set-upstream", GitAction::PushUpstream),
    item(MenuGroup::Remote, 'F', "push --force-with-lease", GitAction::PushForce),
    item(MenuGroup::Stash, 'z', "stash everything", GitAction::StashPush),
    item(MenuGroup::Stash, 'p', "pop the latest", GitAction::StashPop),
    item(MenuGroup::Stash, 'l', "pop a specific one…", GitAction::StashList),
    item(MenuGroup::Stash, 'x', "drop one…", GitAction::StashDrop),
    item(MenuGroup::Remote, 'x', "remove a remote…", GitAction::RemoteRemove),
    item(MenuGroup::Tag, 'n', "new tag…", GitAction::TagCreate),
    item(MenuGroup::Tag, 'x', "delete a tag…", GitAction::TagDelete),
    // Continue/abort/skip lead the group: mid-sequence they are the only rows
    // that show, and they are what you came for.
    item(MenuGroup::Integrate, 'c', "continue", GitAction::SequenceContinue),
    item(MenuGroup::Integrate, 'a', "abort", GitAction::SequenceAbort),
    item(MenuGroup::Integrate, 's', "skip this commit", GitAction::SequenceSkip),
    item(MenuGroup::Integrate, 'm', "merge…", GitAction::Merge),
    item(MenuGroup::Integrate, 'r', "rebase onto…", GitAction::Rebase),
    item(MenuGroup::Fixup, 'a', "amend last commit", GitAction::Amend),
    item(MenuGroup::Fixup, 's', "reset --soft HEAD~1", GitAction::ResetSoft),
    item(MenuGroup::Fixup, 'h', "reset --hard", GitAction::ResetHard),
    // A worktree is a second checkout on another branch, and butai's model is
    // already one workspace per directory — so `l` opens one *as a workspace*,
    // with its own agents, processes and rail. That is the row this group
    // exists for; the rest is upkeep.
    item(MenuGroup::Worktree, 'l', "open worktree…", GitAction::WorktreeList),
    item(MenuGroup::Worktree, 'n', "new worktree…", GitAction::WorktreeAdd),
    item(MenuGroup::Worktree, 'x', "remove worktree…", GitAction::WorktreeRemove),
    item(MenuGroup::Worktree, 'p', "prune gone worktrees", GitAction::WorktreePrune),
];

/// What the repository is doing, so the menu can hide what would not work.
pub struct MenuContext {
    /// A merge, rebase, cherry-pick or revert is in progress.
    pub in_sequence: bool,
}

impl GitAction {
    /// Whether this action is worth offering right now.
    ///
    /// Hiding rather than disabling: a menu of mostly-dead rows is harder to
    /// read than a short one.
    pub fn available(self, cx: &MenuContext) -> bool {
        match self {
            // The way out of a stuck repository, and the only thing offered
            // while one is stuck.
            GitAction::SequenceContinue | GitAction::SequenceAbort | GitAction::SequenceSkip => {
                cx.in_sequence
            }
            // Nothing else can be started mid-sequence: git refuses most of it,
            // and the rest would tangle the sequence further.
            _ => !cx.in_sequence,
        }
    }

    /// Whether this is destructive enough to confirm first.
    pub fn needs_confirm(self) -> bool {
        // The ones that can destroy work you cannot get back: a force push
        // rewrites what others may have pulled (`--force-with-lease` refuses
        // when the remote moved since you fetched, but not when you have seen
        // the commits it is about to drop), `reset --hard` throws away the
        // worktree, and aborting a sequence discards whatever was resolved.
        //
        // Branch and worktree deletion confirm too, but *after* the picker —
        // the row has to be chosen before there is anything to name in the
        // question, so those arm their confirmation there rather than here.
        matches!(self, GitAction::PushForce | GitAction::ResetHard | GitAction::SequenceAbort)
    }
}

/// The rows of one group, filtered to what applies.
pub fn items_for(group: MenuGroup, cx: &MenuContext) -> Vec<&'static MenuItem> {
    ITEMS.iter().filter(|i| i.group == group && i.action.available(cx)).collect()
}

/// The groups worth showing — one with no available rows is not offered.
pub fn groups_for(cx: &MenuContext) -> Vec<MenuGroup> {
    MenuGroup::ALL.into_iter().filter(|g| !items_for(*g, cx).is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mnemonic matching two rows of one group leaves one unreachable from
    /// the keyboard.
    #[test]
    fn mnemonics_are_unique_within_each_group() {
        for group in MenuGroup::ALL {
            let mut keys: Vec<char> =
                ITEMS.iter().filter(|i| i.group == group).map(|i| i.key).collect();
            let before = keys.len();
            keys.sort_unstable();
            keys.dedup();
            assert_eq!(keys.len(), before, "duplicate mnemonic in {}", group.label());
        }
        let mut group_keys: Vec<char> = MenuGroup::ALL.iter().map(|g| g.key()).collect();
        let before = group_keys.len();
        group_keys.sort_unstable();
        group_keys.dedup();
        assert_eq!(group_keys.len(), before, "duplicate group mnemonic");
    }

    /// The shared overlay clamps to 16 rows, three of which are chrome and one
    /// the `..` back row.
    #[test]
    fn no_group_overflows_the_overlay() {
        for group in MenuGroup::ALL {
            let n = ITEMS.iter().filter(|i| i.group == group).count();
            assert!(n <= 12, "{} has {n} rows, too many to draw", group.label());
        }
    }

    /// Mid-sequence the menu offers nothing that would tangle it further, and
    /// says so by being empty rather than by listing rows that would fail.
    /// A stuck repository must offer the way out and nothing that would make
    /// it worse — the menu's one piece of real logic.
    #[test]
    fn only_the_way_out_is_offered_mid_sequence() {
        let quiet = MenuContext { in_sequence: false };
        assert_eq!(groups_for(&quiet), MenuGroup::ALL.to_vec());
        let integrate = items_for(MenuGroup::Integrate, &quiet);
        assert!(integrate.iter().any(|i| i.action == GitAction::Merge));
        assert!(!integrate.iter().any(|i| i.action == GitAction::SequenceContinue));

        let stuck = MenuContext { in_sequence: true };
        assert_eq!(groups_for(&stuck), vec![MenuGroup::Integrate]);
        let integrate = items_for(MenuGroup::Integrate, &stuck);
        assert!(integrate.iter().any(|i| i.action == GitAction::SequenceContinue));
        assert!(integrate.iter().any(|i| i.action == GitAction::SequenceAbort));
        assert!(!integrate.iter().any(|i| i.action == GitAction::Merge), "offered a new merge");
    }

    #[test]
    fn only_the_irreversible_actions_are_confirmed() {
        for a in [GitAction::PushForce, GitAction::ResetHard, GitAction::SequenceAbort] {
            assert!(a.needs_confirm(), "{a:?} should ask first");
        }
        for a in [
            GitAction::Fetch,
            GitAction::Pull,
            GitAction::Push,
            GitAction::Checkout,
            GitAction::StashPush,
            GitAction::Amend,
            GitAction::SequenceContinue,
        ] {
            assert!(!a.needs_confirm(), "{a:?} should not ask");
        }
    }
}
