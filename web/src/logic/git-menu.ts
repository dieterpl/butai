// The `g` command menu.
//
// The port of `crates/butai-client/src/git_menu.rs`. The GIT page's footers
// carry the handful of verbs that are *about the row you are on*; git has about
// thirty operations, and the rest live behind one key. `TODO/git-page.md` states
// the division and why it must hold: "the menu is the command set, the page is
// the view, and a page that duplicated the menu's coverage in footers would be
// the third place to keep in sync."
//
// It is a **table, not a widget**: the same `ITEMS` list decides what is drawn,
// which mnemonic matches, and what a click activates, so those three can never
// disagree. Nothing here touches the DOM or the network — `check.py` runs the
// whole file under node.
//
// It is drawn as a **stack of flat lists**, not a tree. Choosing a group
// replaces the list with that group's rows plus a `..` row, exactly as the
// folder picker already does for directories.
//
// Rows are added here only once the operation behind them works. A menu that
// lists something it cannot do is worse than a shorter menu.

/// Everything the menu can start. One flat vocabulary, because the app
/// dispatches on it and a name that resolves nowhere is what `check.py`'s
/// `gitmenu/runnable` reports.
export const GitAction = Object.freeze(ids([
  "Checkout", "NewBranch", "DeleteBranch", "RenameBranch",
  "Fetch", "Pull", "PullRebase", "Push", "PushUpstream", "PushForce",
  "RemoteAdd", "RemoteRemove",
  "StashPush", "StashPop", "StashList", "StashDrop",
  "SequenceContinue", "SequenceAbort", "SequenceSkip", "Merge", "Rebase",
  "Amend", "ResetSoft", "ResetHard",
  "WorktreeList", "WorktreeAdd", "WorktreeRemove", "WorktreePrune",
  "TagCreate", "TagDelete",
] as const));

/// One name out of [`GitAction`]. The union rather than `string`, so a `case`
/// on an action that does not exist is a compile error rather than a branch
/// that never runs — the type-level half of what `gitmenu/runnable` checks.
export type GitActionId = (typeof GitAction)[keyof typeof GitAction];

// `T extends string` is what makes the argument infer as its literals rather
// than widening to `string[]`, which is the whole reason the ids are worth
// having as a type at all — with the `as const` above, which is what stops
// `Object.freeze`'s own index-signature overload from widening them back.
function ids<T extends string>(names: readonly T[]): { readonly [K in T]: K } {
  const out = {} as { [K in T]: K };
  for (const n of names) out[n] = n;
  return out;
}

/// A group of related operations — one row on the menu's top level. `key` is
/// the letter that jumps straight into it.
export const GROUPS = Object.freeze([
  { id: "Branch", label: "Branch", key: "b" },
  { id: "Remote", label: "Remote", key: "r" },
  { id: "Stash", label: "Stash", key: "z" },
  { id: "Integrate", label: "Integrate", key: "i" },
  { id: "Fixup", label: "Fixup", key: "f" },
  { id: "Worktree", label: "Worktree", key: "w" },
  { id: "Tag", label: "Tag", key: "t" },
] as const);

/// One row of [`GROUPS`].
export type GitMenuGroup = (typeof GROUPS)[number];

/// One group's id. `ITEMS` is written in these, so a row filed under a group
/// that does not exist does not compile.
export type GroupId = GitMenuGroup["id"];

/// One row of the menu.
export type GitMenuItem = Readonly<{
  group: GroupId;
  key: string;
  label: string;
  action: GitActionId;
}>;

const it = (group: GroupId, key: string, label: string, action: GitActionId): GitMenuItem =>
  Object.freeze({ group, key, label, action });

/// Every operation, grouped. Keep each group under twelve rows: the overlay is
/// a scrolling list here rather than a clamped box, but the terminal's is not,
/// and a menu that reads differently in the two clients is two menus.
///
/// **Two rows the TUI's menu does not have**, and they are deliberate rather
/// than drift: `rename…` and `add a remote…`. Both operations exist in the
/// daemon (`POST .../git/branch/rename`, `POST .../git/remote`) and neither had
/// a caller anywhere — the remote route in particular is the only one in the
/// API that accepts a URL, so it is the one with a security-relevant validator
/// and it had no client exercising it at all. `web/README.md` lists them under
/// what this client does differently.
export const ITEMS: readonly GitMenuItem[] = Object.freeze([
  it("Branch", "c", "checkout…", GitAction.Checkout),
  it("Branch", "n", "new branch…", GitAction.NewBranch),
  it("Branch", "d", "delete branch…", GitAction.DeleteBranch),
  it("Branch", "r", "rename…", GitAction.RenameBranch),
  it("Remote", "f", "fetch --prune", GitAction.Fetch),
  it("Remote", "l", "pull", GitAction.Pull),
  it("Remote", "r", "pull --rebase", GitAction.PullRebase),
  it("Remote", "p", "push", GitAction.Push),
  it("Remote", "u", "push --set-upstream", GitAction.PushUpstream),
  it("Remote", "F", "push --force-with-lease", GitAction.PushForce),
  it("Remote", "a", "add a remote…", GitAction.RemoteAdd),
  it("Remote", "x", "remove a remote…", GitAction.RemoteRemove),
  it("Stash", "z", "stash everything", GitAction.StashPush),
  it("Stash", "p", "pop the latest", GitAction.StashPop),
  it("Stash", "l", "pop a specific one…", GitAction.StashList),
  it("Stash", "x", "drop one…", GitAction.StashDrop),
  it("Tag", "n", "new tag…", GitAction.TagCreate),
  it("Tag", "x", "delete a tag…", GitAction.TagDelete),
  // Continue/abort/skip lead the group: mid-sequence they are the only rows
  // that show, and they are what you came for.
  it("Integrate", "c", "continue", GitAction.SequenceContinue),
  it("Integrate", "a", "abort", GitAction.SequenceAbort),
  it("Integrate", "s", "skip this commit", GitAction.SequenceSkip),
  it("Integrate", "m", "merge…", GitAction.Merge),
  it("Integrate", "r", "rebase onto…", GitAction.Rebase),
  it("Fixup", "a", "amend last commit", GitAction.Amend),
  it("Fixup", "s", "reset --soft HEAD~1", GitAction.ResetSoft),
  it("Fixup", "h", "reset --hard", GitAction.ResetHard),
  // A worktree is a second checkout on another branch, and butai's model is
  // already one workspace per directory — so `l` opens one *as a workspace*,
  // with its own agents, processes and rail. That is the row this group exists
  // for; the rest is upkeep.
  it("Worktree", "l", "open worktree…", GitAction.WorktreeList),
  it("Worktree", "n", "new worktree…", GitAction.WorktreeAdd),
  it("Worktree", "x", "remove worktree…", GitAction.WorktreeRemove),
  it("Worktree", "p", "prune gone worktrees", GitAction.WorktreePrune),
]);

/// What the menu is being opened *into* — the repository's own state, as much
/// of it as decides which rows are worth offering.
export type MenuCx = { readonly inSequence?: boolean } | null | undefined;

/// Whether an action is worth offering right now.
///
/// Hiding rather than disabling: a menu of mostly-dead rows is harder to read
/// than a short one. `cx` is `{ inSequence }`.
export function available(action: GitActionId, cx: MenuCx): boolean {
  const stuck = !!(cx && cx.inSequence);
  switch (action) {
    // The way out of a stuck repository, and the only thing offered while one
    // is stuck.
    case GitAction.SequenceContinue:
    case GitAction.SequenceAbort:
    case GitAction.SequenceSkip:
      return stuck;
    // Nothing else can be started mid-sequence: git refuses most of it, and the
    // rest would tangle the sequence further.
    default:
      return !stuck;
  }
}

/// Destructive enough to ask first.
///
/// The ones that can destroy work you cannot get back: a force push rewrites
/// what others may have pulled (`--force-with-lease` refuses when the remote
/// moved since you fetched, but not when you have seen the commits it is about
/// to drop), `reset --hard` throws away the worktree, and aborting a sequence
/// discards whatever was resolved.
///
/// Branch and worktree deletion confirm too, but *after* the picker — the row
/// has to be chosen before there is anything to name in the question, so those
/// arm their confirmation there rather than here.
export function needsConfirm(action: GitActionId): boolean {
  return action === GitAction.PushForce
    || action === GitAction.ResetHard
    || action === GitAction.SequenceAbort;
}

/// The rows of one group, filtered to what applies.
export function itemsFor(group: GroupId, cx: MenuCx): readonly GitMenuItem[] {
  return ITEMS.filter((i) => i.group === group && available(i.action, cx));
}

/// The groups worth showing — one with no available rows is not offered.
export function groupsFor(cx: MenuCx): readonly GitMenuGroup[] {
  return GROUPS.filter((g) => itemsFor(g.id, cx).length > 0);
}
