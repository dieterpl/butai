//! What the surface you are looking at can do *right now*, as data.
//!
//! One table drives four things that were previously written out separately and
//! were free to drift apart: the footer text, the click hit-test, the key
//! dispatch, and the `?` help. `render::CHANGES_HINTS` established the
//! discipline over a fixed list of eight verbs; this generalises it to a list
//! that changes with what is selected, which is the whole point — the changes
//! rail used to offer `s stage` while a commit was selected, and offered
//! nothing at all while a *conflict* was.
//!
//! Two rules keep it honest:
//!
//! 1. **A key that is not in some table does not exist.** Dispatch reads
//!    [`Verb::id`], so binding a key without listing it is not possible.
//! 2. **A verb that loses the competition for 38 columns is still listed.**
//!    [`Verb::footer`] only decides whether it fits on screen; `?` shows
//!    everything either way. That is the difference between "not shown here"
//!    and "undiscoverable", which is what `p` (push) and `Enter` were.

/// Every verb any surface can offer. One flat vocabulary rather than one enum
/// per surface: the dispatchers match on it, so an unhandled verb is a compile
/// error at the surface that offered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerbId {
    // Changes rail — file rows.
    Stage,
    Unstage,
    Discard,
    Diff,
    // Changes rail — conflict rows. None of these had a surface before.
    ResolveOurs,
    ResolveTheirs,
    ResolveDone,
    // Changes rail — always available.
    Commit,
    GitMenu,
    Push,
    Refresh,
    Help,
    /// Stage everything, then commit. Kept as a shortcut for the muscle memory
    /// it already has; the commit overlay's own toggle is the discoverable way.
    CommitAll,
    // List navigation, listed in `?` so the rail documents itself.
    Down,
    Up,
    First,
    Last,
    // Diff pane — partial staging.
    NextHunk,
    PrevHunk,
    /// Stage the hunk, or the picked lines. On a staged diff it unstages, which
    /// is the same verb pointed the other way — hence one id, not two.
    StageHunk,
    DiscardHunk,
    /// Enter or leave line-select.
    LineSelect,
    /// Add the line under the cursor to the selection.
    PickLine,
    /// Leave line-select without applying anything.
    Cancel,
    // Left rail — AGENTS and PROCESSES.
    /// Spawn the pinned agent, or ask which when nothing is pinned.
    NewAgent,
    /// Always ask, whatever is pinned.
    PickAgent,
    NewShell,
    Restart,
    /// Stop the pane the cursor is on. One id for both sections: the rails
    /// differ in what they hold, not in what killing a row means.
    Kill,
    /// Open the row's context menu — the one the right button opens.
    ///
    /// Answered by the loop rather than by [`crate::workbench`]'s rail
    /// dispatcher, because building the menu needs the workspace and that
    /// function takes only a row count. It is listed here anyway, because a key
    /// that is in no table does not exist: this is the entry that makes `m`
    /// real, keeps it out of another verb's way, and puts it in `?`.
    Menu,

    // GIT page. Every one of these acts on the row the cursor is on — that is
    // what makes them different from the `g` menu, which asks first.
    /// Point the history at the ref under the cursor.
    Scope,
    /// Switch to the branch under the cursor.
    Checkout,
    DeleteBranch,
    Merge,
    Fetch,
    TagDelete,
    StashPop,
    StashDrop,
    /// Open another checkout of this repository as a workspace.
    OpenWorktree,
    RemoveWorktree,
    /// Put the commit's full id on the clipboard.
    CopySha,
    Revert,
    CherryPick,
    /// Load what the cursor names into the page's body.
    Show,
    /// Leave for the CHANGES rail, which is where staging lives.
    GoToChanges,
}

/// A verb as a surface offers it: the key that runs it, the word the footer
/// draws, and whether it is worth screen space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verb {
    pub key: char,
    /// Drawn after the key. Keep it to one word wherever the meaning survives:
    /// the changes rail is 38 columns by default and users narrow it.
    pub label: &'static str,
    pub id: VerbId,
    /// Drawn in the theme's danger colour. Confirmation is the surface's call —
    /// this only says the verb is worth looking at twice.
    pub danger: bool,
    /// Whether it competes for a footer slot. `false` means "still bound, still
    /// in `?`, just not worth a column here".
    pub footer: bool,
}

const fn verb(key: char, label: &'static str, id: VerbId) -> Verb {
    Verb { key, label, id, danger: false, footer: true }
}

const fn danger(key: char, label: &'static str, id: VerbId) -> Verb {
    Verb { key, label, id, danger: true, footer: true }
}

/// Bound and documented, but never drawn in the footer.
const fn quiet(key: char, label: &'static str, id: VerbId) -> Verb {
    Verb { key, label, id, danger: false, footer: false }
}

/// Separator between two verbs on the same footer row.
pub const SEP: &str = " · ";

/// The most footer rows any surface will give up to verbs. Past this the list
/// itself starts to disappear, which is a worse trade than a verb that only
/// `?` can tell you about.
pub const MAX_ROWS: usize = 3;

/// Where one verb sits once laid out: which footer row, and the columns it
/// occupies from the surface's left edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub row: usize,
    pub start: usize,
    pub end: usize,
    pub key: char,
}

impl Verb {
    /// How the key is written down. Most are themselves; the two that have no
    /// glyph get their names, because a footer reading `"  stage hunk"` says
    /// nothing and one containing a literal newline would shear the row.
    pub fn key_text(&self) -> &'static str {
        match self.key {
            ' ' => "space",
            '\n' => "enter",
            '\t' => "tab",
            _ => "",
        }
    }
}

/// How wide `"<key> <label>"` draws.
fn cell_width(v: &Verb) -> usize {
    let key = match v.key_text() {
        "" => 1,
        named => named.chars().count(),
    };
    key + 1 + v.label.chars().count()
}

/// Pack `verbs` into at most `rows` lines of `width` columns, in order.
///
/// Greedy and stable: a verb goes on the current row if it fits, otherwise it
/// starts the next one. Verbs that run out of rows are dropped from the footer
/// and nowhere else — they keep working and keep appearing in `?`.
pub fn layout(verbs: &[Verb], width: usize, rows: usize) -> Vec<Span> {
    let mut spans = Vec::new();
    let (mut row, mut x) = (0usize, 0usize);
    for v in verbs.iter().filter(|v| v.footer) {
        if row >= rows {
            break;
        }
        let w = cell_width(v);
        let sep = if x == 0 { 0 } else { SEP.chars().count() };
        if x + sep + w > width {
            // Doesn't fit here. Try a fresh row — unless it would not fit on an
            // empty row either, in which case no row will ever take it.
            if w > width {
                continue;
            }
            row += 1;
            x = 0;
            if row >= rows {
                break;
            }
        } else {
            x += sep;
        }
        spans.push(Span { row, start: x, end: x + w, key: v.key });
        x += w;
    }
    spans
}

/// The footer as text, one string per row (`rows` of them, blank where empty).
pub fn lines(verbs: &[Verb], width: usize, rows: usize) -> Vec<String> {
    let spans = layout(verbs, width, rows);
    let mut out = vec![String::new(); rows];
    for span in &spans {
        let Some(v) = verbs.iter().find(|v| v.key == span.key) else { continue };
        let line = &mut out[span.row];
        // `layout` already placed the spans contiguously with one separator
        // between them, so appending in span order reproduces its geometry
        // exactly — which is what `rendered_rows_are_exactly_as_wide_as_the_
        // layout_says` pins down.
        if !line.is_empty() {
            line.push_str(SEP);
        }
        match v.key_text() {
            "" => line.push(v.key),
            named => line.push_str(named),
        }
        line.push(' ');
        line.push_str(v.label);
    }
    out
}

/// How many rows this verb set actually needs, capped at `rows`.
pub fn rows_needed(verbs: &[Verb], width: usize, rows: usize) -> usize {
    layout(verbs, width, rows).iter().map(|s| s.row + 1).max().unwrap_or(0)
}

/// The verb a click at (`row`, `col`) lands on.
///
/// The last verb on a row owns the blank space after it, matching what
/// `render::changes_hint_hit` did — the trailing gap is visually part of the
/// button you just aimed at, and a click that lands one column past a label is
/// a hit, not a miss.
pub fn hit(verbs: &[Verb], width: usize, rows: usize, row: usize, col: usize) -> Option<char> {
    let spans = layout(verbs, width, rows);
    let mut last = None;
    for span in spans.iter().filter(|s| s.row == row) {
        if col < span.end {
            return Some(span.key);
        }
        last = Some(span.key);
    }
    last
}

// ---------------------------------------------------------------------------
// The GIT page
// ---------------------------------------------------------------------------

/// Which kind of row one of the GIT page's two lists has selected.
///
/// A summary of `chrome::RefRow` for the same reason [`ChangesRow`] is one of
/// the rail's `Row`: the drawing's enum carries borrowed payloads this table
/// has no use for, and the two meet here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitRow {
    WorkingTree,
    /// A changed file the index has not been told about.
    ChangeUnstaged,
    /// A changed file that is in the index and will go into the next commit.
    ChangeStaged,
    /// A file with a merge still to settle. It cannot be staged at all — the
    /// three ways out are what the row is for.
    ChangeConflicted,
    /// A local branch that is not checked out here.
    Branch,
    /// The checked-out branch: it cannot be switched to or deleted.
    CurrentBranch,
    /// A local branch checked out in another worktree — git refuses to check it
    /// out twice, so the row says where it went instead of offering a verb that
    /// would fail.
    BranchElsewhere,
    RemoteBranch,
    Remote,
    Tag,
    Stash,
    /// Another checkout of this repository.
    Worktree,
    /// The checkout this page is looking at. There is nowhere to go.
    ThisWorktree,
    Commit,
    /// A heading, or a cursor past the end of a list that shrank.
    None,
}

/// Verbs for the row the cursor is on.
///
/// Deliberately short. Git has about thirty operations and the `g` menu already
/// carries all of them; these are the handful that are *about this row*, which
/// is the only thing a footer can say that a menu cannot. `j`/`k` are
/// navigation here, so no verb may take them — and `g` is the menu, which is
/// why this page has no `g`-for-top the way the diff pane does. The CHANGES
/// rail made the same trade for the same reason.
pub fn git_row_verbs(row: GitRow) -> &'static [Verb] {
    // `enter` opens the whole worktree diff in the body; `S` and `U` are the
    // section-wide forms of the keys the file rows below carry. The rail is
    // still one press away and still owns the commit box, which is why `changes`
    // keeps a slot on the summary row rather than being dropped for the diff.
    const WORKING_TREE: &[Verb] =
        &[verb('\n', "diff all", VerbId::Diff), verb('C', "changes", VerbId::GoToChanges)];
    // Deliberately the CHANGES rail's own letters. A file row means the same
    // thing on both surfaces, and a page that spelled staging differently would
    // be the second product this one exists not to be.
    const CHANGE_UNSTAGED: &[Verb] = &[
        verb('\n', "diff", VerbId::Diff),
        verb('s', "stage", VerbId::Stage),
        danger('x', "discard", VerbId::Discard),
    ];
    const CHANGE_STAGED: &[Verb] =
        &[verb('\n', "diff", VerbId::Diff), verb('u', "unstage", VerbId::Unstage)];
    const CHANGE_CONFLICTED: &[Verb] = &[
        verb('o', "ours", VerbId::ResolveOurs),
        verb('t', "theirs", VerbId::ResolveTheirs),
        verb('a', "resolved", VerbId::ResolveDone),
        quiet('\n', "diff", VerbId::Diff),
    ];
    const BRANCH: &[Verb] = &[
        verb('\n', "scope", VerbId::Scope),
        verb('c', "checkout", VerbId::Checkout),
        verb('m', "merge", VerbId::Merge),
        danger('d', "delete", VerbId::DeleteBranch),
    ];
    // No checkout and no delete: you are standing on it.
    const CURRENT: &[Verb] = &[verb('\n', "scope", VerbId::Scope)];
    // Nor here — the other worktree holds it.
    const ELSEWHERE: &[Verb] =
        &[verb('\n', "scope", VerbId::Scope), verb('m', "merge", VerbId::Merge)];
    // No `c checkout`. The value a row carries is its shorthand, and the
    // daemon's checkout resolves `refs/heads/{name}` — so `origin/main` asks
    // for a local branch of that name and always fails. Checking out a remote
    // branch properly means creating a local one that tracks it, which is a
    // route that does not exist yet; until it does, offering the verb would be
    // advertising a failure, which is the rule `BranchElsewhere` already obeys.
    // `m merge` works on a remote branch as-is — that resolves the ref, not a
    // branch name.
    const REMOTE_BRANCH: &[Verb] =
        &[verb('\n', "scope", VerbId::Scope), verb('m', "merge", VerbId::Merge)];
    const REMOTE: &[Verb] = &[verb('f', "fetch", VerbId::Fetch)];
    const TAG: &[Verb] =
        &[verb('\n', "scope", VerbId::Scope), danger('x', "delete", VerbId::TagDelete)];
    const STASH: &[Verb] = &[
        verb('\n', "show", VerbId::Show),
        verb('p', "pop", VerbId::StashPop),
        danger('x', "drop", VerbId::StashDrop),
    ];
    const WORKTREE: &[Verb] =
        &[verb('\n', "open", VerbId::OpenWorktree), danger('x', "remove", VerbId::RemoveWorktree)];
    const THIS_WORKTREE: &[Verb] = &[];
    const COMMIT: &[Verb] = &[
        verb('\n', "diff", VerbId::Show),
        verb('y', "sha", VerbId::CopySha),
        verb('v', "revert", VerbId::Revert),
        verb('p', "pick", VerbId::CherryPick),
    ];
    const NONE: &[Verb] = &[];
    match row {
        GitRow::WorkingTree => WORKING_TREE,
        GitRow::ChangeUnstaged => CHANGE_UNSTAGED,
        GitRow::ChangeStaged => CHANGE_STAGED,
        GitRow::ChangeConflicted => CHANGE_CONFLICTED,
        GitRow::Branch => BRANCH,
        GitRow::CurrentBranch => CURRENT,
        GitRow::BranchElsewhere => ELSEWHERE,
        GitRow::RemoteBranch => REMOTE_BRANCH,
        GitRow::Remote => REMOTE,
        GitRow::Tag => TAG,
        GitRow::Stash => STASH,
        GitRow::Worktree => WORKTREE,
        GitRow::ThisWorktree => THIS_WORKTREE,
        GitRow::Commit => COMMIT,
        GitRow::None => NONE,
    }
}

/// Verbs that apply wherever the cursor is on this page.
pub fn git_always_verbs() -> &'static [Verb] {
    const ALL: &[Verb] = &[
        verb('g', "git", VerbId::GitMenu),
        verb('r', "refresh", VerbId::Refresh),
        verb('?', "keys", VerbId::Help),
    ];
    ALL
}

/// The footer for one of the page's lists: the row's verbs, then the shared
/// ones. One list so the drawing, the hit test and the key dispatch agree.
pub fn git_footer(row: GitRow) -> Vec<Verb> {
    let mut v = git_row_verbs(row).to_vec();
    v.extend_from_slice(git_always_verbs());
    v
}

/// Everything the GIT page responds to, in `?` order.
pub fn git_help_verbs() -> Vec<Verb> {
    let mut v = vec![
        quiet('j', "down", VerbId::Down),
        quiet('k', "up", VerbId::Up),
        quiet('\t', "next column", VerbId::LineSelect),
    ];
    for row in [
        GitRow::WorkingTree,
        GitRow::ChangeUnstaged,
        GitRow::ChangeStaged,
        GitRow::ChangeConflicted,
        GitRow::Branch,
        GitRow::RemoteBranch,
        GitRow::Remote,
        GitRow::Tag,
        GitRow::Stash,
        GitRow::Worktree,
        GitRow::Commit,
    ] {
        for verb in git_row_verbs(row) {
            if !v.iter().any(|e: &Verb| e.key == verb.key && e.label == verb.label) {
                v.push(*verb);
            }
        }
    }
    v.extend_from_slice(git_always_verbs());
    v
}

// ---------------------------------------------------------------------------
// The changes rail
// ---------------------------------------------------------------------------

/// Which kind of row the changes rail has selected. The rail's `Row` enum is
/// private to its module and carries payloads this does not need, so the two
/// meet at this summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangesRow {
    Conflict,
    Unstaged,
    Staged,
    Commit,
    /// A header, a placeholder, or an empty rail.
    None,
}

/// Verbs for the selected row. `d` is listed once, on the rows where a diff
/// means something; `Enter` runs it too but is not worth a footer slot.
pub fn changes_row_verbs(row: ChangesRow) -> &'static [Verb] {
    // A conflicted file offers the three ways out and nothing that would commit
    // half a merge. `d` still opens the conflict diff, but the three resolutions
    // are what the row is *for*, and they are what fits.
    const CONFLICT: &[Verb] = &[
        verb('o', "ours", VerbId::ResolveOurs),
        verb('t', "theirs", VerbId::ResolveTheirs),
        verb('a', "resolved", VerbId::ResolveDone),
        quiet('d', "diff", VerbId::Diff),
    ];
    const UNSTAGED: &[Verb] = &[
        verb('s', "stage", VerbId::Stage),
        danger('x', "discard", VerbId::Discard),
        verb('d', "diff", VerbId::Diff),
    ];
    const STAGED: &[Verb] =
        &[verb('u', "unstage", VerbId::Unstage), verb('d', "diff", VerbId::Diff)];
    const COMMIT: &[Verb] = &[verb('d', "show", VerbId::Diff)];
    const NONE: &[Verb] = &[];
    match row {
        ChangesRow::Conflict => CONFLICT,
        ChangesRow::Unstaged => UNSTAGED,
        ChangesRow::Staged => STAGED,
        ChangesRow::Commit => COMMIT,
        ChangesRow::None => NONE,
    }
}

/// Verbs that apply whatever is selected. `p push` earns its slot only when
/// there is something to push — it was bound but never drawn before, which is
/// the worst of both.
pub fn changes_always_verbs(ahead: usize) -> Vec<Verb> {
    let mut v = Vec::with_capacity(6);
    if ahead > 0 {
        v.push(verb('p', "push", VerbId::Push));
    }
    v.push(verb('c', "commit", VerbId::Commit));
    v.push(verb('g', "git", VerbId::GitMenu));
    v.push(verb('?', "keys", VerbId::Help));
    // Bound, documented, and not worth a column: the rail rescans itself after
    // every mutation, so `r` is for the case where something changed the tree
    // behind its back.
    v.push(quiet('r', "refresh", VerbId::Refresh));
    v.push(quiet('C', "stage all + commit", VerbId::CommitAll));
    v
}

/// Everything the rail responds to, in `?` order: navigation, then the verbs
/// for each kind of row, then the ones that always apply. Used to generate the
/// help modal, so the help cannot describe a key the rail does not have.
pub fn changes_help_verbs() -> Vec<Verb> {
    // Navigation only; the rail moves to the ends with Home/End, which have no
    // character to list and need none. `g` is the git menu here, so it is
    // deliberately *not* the "go to top" it is in the diff pane.
    let mut v = vec![quiet('j', "down", VerbId::Down), quiet('k', "up", VerbId::Up)];
    for row in [ChangesRow::Unstaged, ChangesRow::Staged, ChangesRow::Conflict, ChangesRow::Commit]
    {
        for verb in changes_row_verbs(row) {
            if !v.iter().any(|e: &Verb| e.key == verb.key && e.label == verb.label) {
                v.push(*verb);
            }
        }
    }
    for verb in changes_always_verbs(1) {
        if !v.iter().any(|e: &Verb| e.key == verb.key) {
            v.push(verb);
        }
    }
    v
}

/// The changes rail's `?` entry, written out from the tables above.
///
/// Grouped by what the verb applies to, because that is the thing the footer
/// cannot show all at once and the thing a user is actually asking `?` about:
/// "why does `s` do nothing here?"
pub fn changes_help_line() -> String {
    let group = |label: &str, row: ChangesRow| {
        let verbs = changes_row_verbs(row)
            .iter()
            .map(|v| format!("{} {}", v.key, v.label))
            .collect::<Vec<_>>()
            .join(" ");
        format!("{label}: {verbs}")
    };
    let always = changes_always_verbs(1)
        .iter()
        .map(|v| format!("{} {}", v.key, v.label))
        .collect::<Vec<_>>()
        .join(" · ");
    format!(
        "verbs follow the selected row — {} · {} · {} · {} · always: {always} · \
         [ commit... ] types the message inline",
        group("unstaged", ChangesRow::Unstaged),
        group("staged", ChangesRow::Staged),
        group("conflict", ChangesRow::Conflict),
        group("commit", ChangesRow::Commit),
    )
}

/// The rail's footer: the selected row's verbs, then the always-available ones.
pub fn changes_footer(row: ChangesRow, ahead: usize) -> Vec<Verb> {
    let mut v = changes_row_verbs(row).to_vec();
    v.extend(changes_always_verbs(ahead));
    v
}

/// How many rows to reserve for the rail's footer at this width.
///
/// The **maximum** over every kind of row, not the current one: a footer that
/// grew and shrank as the selection moved would shift the list under the
/// cursor, which is a worse cost than one occasionally-blank line.
pub fn changes_footer_rows(width: usize) -> usize {
    [ChangesRow::Conflict, ChangesRow::Unstaged, ChangesRow::Staged, ChangesRow::Commit]
        .into_iter()
        .flat_map(|row| [0usize, 1].map(move |ahead| (row, ahead)))
        .map(|(row, ahead)| rows_needed(&changes_footer(row, ahead), width, MAX_ROWS))
        .max()
        .unwrap_or(1)
        .max(1)
}

// ---------------------------------------------------------------------------
// The left rail
// ---------------------------------------------------------------------------

/// The one row each left-rail section gives its verbs.
///
/// [`crate::chrome::Chrome::compute`] carves exactly this off the bottom of a
/// section that can spare it, so the drawing, the hit-test and the packing all
/// have to agree on the same number.
pub const RAIL_FOOTER_ROWS: usize = 1;

/// The keys the two `[+ ...]` buttons on the box borders stand for.
///
/// A button is a second spelling of a verb, not a verb of its own — so it
/// resolves to a key and goes through the same dispatch the footer word does.
/// `no_button_names_a_key_its_section_does_not_have` is what keeps these two
/// honest against the tables below.
pub const SPAWN_AGENT_KEY: char = 'a';
pub const NEW_SHELL_KEY: char = 't';

/// What the AGENTS section can do, given whether an agent is pinned.
///
/// The rows are all the same kind of thing, so — unlike the changes rail —
/// nothing the *cursor* is on changes this. The pin does, and it has to: `a`
/// and `A` are the same verb until one is set, and until then a footer offering
/// both is offering the same thing twice under two names.
///
/// `pick` is what the old hint line called `A`, and it was the wrong word. Every
/// other verb here acts on the list above it, so `pick` read as "pick one of my
/// agents" — it opens a chooser of agent *types* to start a new one, which is
/// what `a` does too. It was reported exactly that way: "click pick, it's still
/// the spawn agent, it just spawns a new agent". `...` is the ordinary way to
/// write "this one asks first", and it puts the two on the same verb where they
/// belong.
///
/// Spelled with three dots rather than `…`: the ellipsis character is
/// East-Asian-ambiguous width and draws two cells wide in some terminals, which
/// would shift the row the same way the pointing glyphs would have.
pub fn agents_verbs(pinned: bool) -> &'static [Verb] {
    // Pinned, `a` starts that agent with nothing in between and `A` is the only
    // route to the others — so both are worth a column, and the border button
    // already reads `[+ claude]` to say which one `a` means.
    const PINNED: &[Verb] = &[
        verb('a', "new", VerbId::NewAgent),
        verb('A', "new...", VerbId::PickAgent),
        danger('x', "kill", VerbId::Kill),
        quiet('m', "menu", VerbId::Menu),
    ];
    // Unpinned, `a` opens the very chooser `A` would, so only one of them is
    // worth drawing. `A` stays bound and stays in `?`; it is simply not a second
    // button for a thing there is already a button for.
    const UNPINNED: &[Verb] = &[
        verb('a', "new...", VerbId::NewAgent),
        quiet('A', "new...", VerbId::PickAgent),
        danger('x', "kill", VerbId::Kill),
        quiet('m', "menu", VerbId::Menu),
    ];
    if pinned {
        PINNED
    } else {
        UNPINNED
    }
}

/// What the PROCESSES section can do.
///
/// `t` rather than `n`: it is the bare spelling of `alt-t`, and `n` is already
/// "open another project" everywhere else in the workbench.
pub fn procs_verbs() -> &'static [Verb] {
    const PROCS: &[Verb] = &[
        verb('t', "new", VerbId::NewShell),
        verb('r', "restart", VerbId::Restart),
        danger('x', "kill", VerbId::Kill),
        quiet('m', "menu", VerbId::Menu),
    ];
    PROCS
}

// ---------------------------------------------------------------------------
// The diff pane
// ---------------------------------------------------------------------------

/// What the diff pane is doing: reading it, or picking lines out of a hunk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DiffMode {
    #[default]
    Read,
    Lines,
}

/// What the diff pane offers, given what it is showing.
///
/// A commit's diff is history: there is nothing to stage, so it offers none of
/// it rather than offering verbs that would fail. An unstaged diff stages; a
/// staged one unstages. The same key does both, because "the thing this pane is
/// for" is one idea, and two keys for it would only ask the user to remember
/// which side of the index they are on — the footer already says.
pub fn diff_verbs(mode: DiffMode, staged: bool, mutable: bool) -> Vec<Verb> {
    let mut v = Vec::with_capacity(8);
    if !mutable {
        // A commit diff: navigation only. `?` still lists the rest, marked as
        // unavailable by simply not being here.
        v.push(quiet('j', "down", VerbId::Down));
        v.push(quiet('k', "up", VerbId::Up));
        v.push(verb(']', "next hunk", VerbId::NextHunk));
        v.push(quiet('[', "prev hunk", VerbId::PrevHunk));
        v.push(verb('?', "keys", VerbId::Help));
        return v;
    }
    match mode {
        DiffMode::Read => {
            v.push(verb(']', "next hunk", VerbId::NextHunk));
            v.push(quiet('[', "prev hunk", VerbId::PrevHunk));
            v.push(Verb {
                key: ' ',
                label: if staged { "unstage hunk" } else { "stage hunk" },
                id: VerbId::StageHunk,
                danger: false,
                footer: true,
            });
            v.push(verb('v', "lines", VerbId::LineSelect));
            if !staged {
                v.push(danger('x', "discard", VerbId::DiscardHunk));
            }
        }
        DiffMode::Lines => {
            v.push(verb('j', "next line", VerbId::Down));
            v.push(quiet('k', "prev line", VerbId::Up));
            v.push(Verb {
                key: ' ',
                label: "pick",
                id: VerbId::PickLine,
                danger: false,
                footer: true,
            });
            v.push(Verb {
                key: '\n',
                label: if staged { "unstage picked" } else { "stage picked" },
                id: VerbId::StageHunk,
                danger: false,
                footer: true,
            });
            v.push(verb('v', "done", VerbId::Cancel));
        }
    }
    v.push(quiet('r', "refresh", VerbId::Refresh));
    v.push(verb('?', "keys", VerbId::Help));
    v
}

/// The diff pane's `?` entry.
pub fn diff_help_line() -> String {
    let names = |v: &Verb| match v.key_text() {
        "" => format!("{} {}", v.key, v.label),
        named => format!("{named} {}", v.label),
    };
    let read =
        diff_verbs(DiffMode::Read, false, true).iter().map(names).collect::<Vec<_>>().join(" · ");
    let lines =
        diff_verbs(DiffMode::Lines, false, true).iter().map(names).collect::<Vec<_>>().join(" · ");
    format!(
        "partial staging, not just a viewer — {read} · in line-select: {lines} · \
         on a staged diff the same key unstages · a commit's diff is read-only"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The changes rail's inner width at the default geometry: `RIGHT_W` less
    /// its box border. Derived rather than written down, so widening the rail
    /// does not quietly invalidate these tests.
    const RAIL: usize = (crate::chrome::RIGHT_W - 2) as usize;

    /// Two rows at the default width is the point of making the footer
    /// contextual: the old fixed table always took three, whatever was
    /// selected. If a reworded verb pushes this back to three, the rail has
    /// silently lost a line of content and this test says so.
    #[test]
    fn the_default_rail_gets_a_line_back() {
        assert_eq!(changes_footer_rows(RAIL), 2);
        for row in [ChangesRow::Conflict, ChangesRow::Unstaged, ChangesRow::Staged] {
            for line in lines(&changes_footer(row, 2), RAIL, MAX_ROWS) {
                assert!(line.chars().count() <= RAIL, "{row:?}: {line:?} overflows the rail");
            }
        }
    }

    /// Every verb the footer draws has to be reachable by clicking the text it
    /// drew. This is the invariant the old `CHANGES_HINTS`/`changes_hint_hit`
    /// pair maintained by hand.
    #[test]
    fn every_drawn_verb_is_clickable_where_it_is_drawn() {
        for row in [ChangesRow::Conflict, ChangesRow::Unstaged, ChangesRow::Staged] {
            let verbs = changes_footer(row, 2);
            let rendered = lines(&verbs, RAIL, MAX_ROWS);
            for span in layout(&verbs, RAIL, MAX_ROWS) {
                let text: String = rendered[span.row]
                    .chars()
                    .skip(span.start)
                    .take(span.end - span.start)
                    .collect();
                assert!(
                    text.starts_with(span.key),
                    "{:?}: span for {:?} covers {text:?}",
                    row,
                    span.key
                );
                assert_eq!(
                    hit(&verbs, RAIL, MAX_ROWS, span.row, span.start),
                    Some(span.key),
                    "{row:?}: clicking {:?} at its own start missed",
                    span.key
                );
                assert_eq!(
                    hit(&verbs, RAIL, MAX_ROWS, span.row, span.end - 1),
                    Some(span.key),
                    "{row:?}: clicking {:?} at its last column missed",
                    span.key
                );
            }
        }
    }

    /// Two verbs sharing a key on one surface leaves one unreachable — the same
    /// check `git_menu` makes on its mnemonics.
    #[test]
    fn no_surface_offers_one_key_twice() {
        for row in [
            ChangesRow::Conflict,
            ChangesRow::Unstaged,
            ChangesRow::Staged,
            ChangesRow::Commit,
            ChangesRow::None,
        ] {
            let verbs = changes_footer(row, 1);
            let mut keys: Vec<char> = verbs.iter().map(|v| v.key).collect();
            let before = keys.len();
            keys.sort_unstable();
            keys.dedup();
            assert_eq!(keys.len(), before, "{row:?} binds a key twice");
        }
    }

    /// The reason this module exists: a conflicted file used to offer nothing.
    #[test]
    fn a_conflict_offers_the_three_ways_out() {
        let ids: Vec<VerbId> =
            changes_row_verbs(ChangesRow::Conflict).iter().map(|v| v.id).collect();
        assert!(ids.contains(&VerbId::ResolveOurs));
        assert!(ids.contains(&VerbId::ResolveTheirs));
        assert!(ids.contains(&VerbId::ResolveDone));
        assert!(!ids.contains(&VerbId::Stage), "staging a conflict commits the markers");
    }

    /// A commit row cannot be staged, and a staged row cannot be staged again.
    #[test]
    fn a_row_never_offers_a_verb_it_cannot_run() {
        let commit: Vec<VerbId> =
            changes_row_verbs(ChangesRow::Commit).iter().map(|v| v.id).collect();
        assert_eq!(commit, vec![VerbId::Diff]);
        let staged: Vec<VerbId> =
            changes_row_verbs(ChangesRow::Staged).iter().map(|v| v.id).collect();
        assert!(!staged.contains(&VerbId::Stage));
        assert!(!staged.contains(&VerbId::Discard), "discard is a worktree verb");
    }

    /// `p` was bound and never drawn. Now it is drawn exactly when it would do
    /// something.
    #[test]
    fn push_appears_only_when_there_is_something_to_push() {
        assert!(!changes_always_verbs(0).iter().any(|v| v.id == VerbId::Push && v.footer));
        assert!(changes_always_verbs(3).iter().any(|v| v.id == VerbId::Push && v.footer));
    }

    /// A verb dropped from the footer for want of columns is still documented.
    #[test]
    fn a_narrow_rail_hides_verbs_from_the_footer_but_not_from_help() {
        let verbs = changes_footer(ChangesRow::Unstaged, 1);
        let spans = layout(&verbs, 12, MAX_ROWS);
        assert!(spans.len() < verbs.iter().filter(|v| v.footer).count(), "nothing was dropped");
        let help = changes_help_verbs();
        for id in [VerbId::Stage, VerbId::Discard, VerbId::Push, VerbId::Refresh, VerbId::CommitAll]
        {
            assert!(help.iter().any(|v| v.id == id), "{id:?} is not in `?`");
        }
    }

    /// The left rail's interior at the default geometry.
    const LEFT: usize = (crate::chrome::LEFT_W - 2) as usize;

    /// Both left-rail sections say everything they can do on the one row the
    /// geometry gives them.
    ///
    /// They only just fit — `t new · r restart · x kill` is 26 columns in 26 —
    /// so a reworded verb silently drops the last one off the footer, and this
    /// is what says so. Dropping it would not break the key, only the only
    /// place the key is written down.
    #[test]
    fn each_left_rail_section_fits_its_verbs_on_its_one_row() {
        for verbs in [agents_verbs(true), agents_verbs(false), procs_verbs()] {
            let drawn = verbs.iter().filter(|v| v.footer).count();
            assert_eq!(
                layout(verbs, LEFT, RAIL_FOOTER_ROWS).len(),
                drawn,
                "{:?} does not fit in {LEFT} columns",
                lines(verbs, LEFT, RAIL_FOOTER_ROWS)
            );
            for line in lines(verbs, LEFT, RAIL_FOOTER_ROWS) {
                assert!(line.chars().count() <= LEFT, "{line:?} overflows the rail");
            }
        }
    }

    /// One key, one meaning, within a section. `x` is deliberately the same
    /// verb in both — killing the row the cursor is on — which is the whole
    /// reason it is one [`VerbId`].
    #[test]
    fn no_left_rail_section_binds_a_key_twice() {
        for verbs in [agents_verbs(true), agents_verbs(false), procs_verbs()] {
            let mut keys: Vec<char> = verbs.iter().map(|v| v.key).collect();
            let before = keys.len();
            keys.sort_unstable();
            keys.dedup();
            assert_eq!(keys.len(), before, "{verbs:?} binds a key twice");
        }
        let kill = |v: &&Verb| v.id == VerbId::Kill;
        assert_eq!(
            agents_verbs(true).iter().find(kill).map(|v| v.key),
            procs_verbs().iter().find(kill).map(|v| v.key),
            "kill should be the same key in both sections"
        );
    }

    /// `[+ agent]` and `[+ term]` name keys their own sections actually bind,
    /// and name the *spawn*: a border button that resolved to `x` would kill
    /// something on a click that says `+`.
    #[test]
    fn no_button_names_a_key_its_section_does_not_have() {
        let find = |verbs: &'static [Verb], key: char| verbs.iter().find(move |v| v.key == key);
        for pinned in [true, false] {
            assert_eq!(
                find(agents_verbs(pinned), SPAWN_AGENT_KEY).map(|v| v.id),
                Some(VerbId::NewAgent)
            );
        }
        assert_eq!(find(procs_verbs(), NEW_SHELL_KEY).map(|v| v.id), Some(VerbId::NewShell));
    }

    /// Both left-rail sections offer their row's menu, and neither draws it.
    ///
    /// The menu was the right button's alone, and it holds two things nothing
    /// else does — "close others" and "close all agents" — plus a remote tab's
    /// "disconnect host", which `alt-h` also reaches. `m`
    /// is [`quiet`] rather than a fourth footer word because the one row these
    /// sections get is already full: `t new · r restart · x kill` is 26 columns
    /// in 26, so a drawn `m menu` would push `x kill` off the only place it is
    /// written down. Bound, in `?`, and not competing for a column is exactly
    /// what `quiet` is for.
    #[test]
    fn both_left_rail_sections_offer_the_row_menu_without_drawing_it() {
        for verbs in [agents_verbs(true), agents_verbs(false), procs_verbs()] {
            let menu = verbs.iter().find(|v| v.id == VerbId::Menu).expect("a menu verb");
            assert_eq!(menu.key, 'm');
            assert!(!menu.footer, "the menu should not take a column from `x kill`");
        }
    }

    /// Killing is drawn in the danger colour and spawning is not — the surface
    /// reads this rather than deciding for itself.
    #[test]
    fn killing_is_the_only_dangerous_verb_on_the_left_rail() {
        for verbs in [agents_verbs(true), agents_verbs(false), procs_verbs()] {
            for v in verbs {
                assert_eq!(v.danger, v.id == VerbId::Kill, "{v:?}");
            }
        }
    }

    /// Rendered text and hit-test geometry come from the same layout, so a
    /// separator can never end up in one and not the other.
    #[test]
    fn rendered_rows_are_exactly_as_wide_as_the_layout_says() {
        let verbs = changes_footer(ChangesRow::Unstaged, 1);
        let rendered = lines(&verbs, RAIL, MAX_ROWS);
        for span in layout(&verbs, RAIL, MAX_ROWS) {
            assert!(
                rendered[span.row].chars().count() >= span.end,
                "row {} is shorter than the span it should contain",
                span.row
            );
        }
        for line in &rendered {
            assert!(line.chars().count() <= RAIL, "{line:?} overflows the rail");
        }
    }
}
