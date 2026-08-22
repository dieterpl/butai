//! A worktree's status, cached: conflicted / unstaged / staged sections plus
//! recent commits, and the mutations that change them.
//!
//! **It is not a rail.** It was one — this file drew a CHANGES column and
//! answered its keys — and every client draws that column for itself now, from
//! the [`ChangesDto`](butai_protocol::api::ChangesDto) this produces. What is
//! left is the half a client cannot do for itself: walking the worktree, which
//! is slow enough to need its own thread, and the index writes behind
//! `POST .../changes/{stage,unstage,discard}` and `.../git/*`.
//!
//! **Why anything is cached at all.** Every field a DTO reads is filled by the
//! off-thread scan and never touched again until the next one. Resolving the
//! branch, the upstream or the worktree root on demand meant a
//! `Repository::discover` per read, per client — which on a network-mounted
//! worktree was the difference between a workbench and a frozen one.
//!
//! The mutations also move their own rows before returning, so a DTO built
//! immediately after a stage is already right. The authoritative rows arrive
//! when the rescan lands; this is what stops the reads in between lying.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use butai_protocol::api::{self, RepoState, AHEAD_BEHIND_CAP};
use git2::{Repository, Status, StatusOptions};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    Header(&'static str),
    /// A file with unresolved conflicts. Deliberately not an `Unstaged` row:
    /// staging one of these commits half a merge, so it gets its own section
    /// and its own verbs.
    Conflict {
        path: PathBuf,
        base: bool,
        ours: bool,
        theirs: bool,
    },
    Unstaged {
        path: PathBuf,
        code: char,
        stat: (usize, usize),
    },
    Staged {
        path: PathBuf,
        code: char,
        stat: (usize, usize),
    },
    Commit {
        id: String,
        summary: String,
    },
    Empty(&'static str),
}

/// The rows of one status view, split by section. A named struct rather than a
/// tuple because the sections are all `Vec<Row>`: positionally, a swapped pair
/// type-checks and silently moves files between staged and unstaged.
#[derive(Default)]
struct Sections {
    conflicts: Vec<Row>,
    unstaged: Vec<Row>,
    staged: Vec<Row>,
    commits: Vec<Row>,
}

/// A precomputed git status view. Producing it walks the whole worktree, which
/// is slow on large repos or network filesystems, so it is built off the core
/// event-loop thread (see `ServerCore::request_git_refresh`) and applied later.
/// `Send` because `Row` is — `git2::Repository` never crosses the boundary.
/// Public only so it can ride the public [`Event`](crate::core::Event) enum;
/// its fields are private, so it stays opaque outside this crate.
pub struct GitSnapshot {
    rows: Vec<Row>,
    /// Resolved here because the scan already holds a `Repository`; caching it
    /// keeps [`GitPane::branch`] off the filesystem when a DTO is built.
    branch: String,
    /// The worktree root. Status paths are relative to *this*, not to the
    /// workspace cwd, which may be deeper. See [`GitPane::repo_root`].
    repo_root: PathBuf,
    head: HeadInfo,
}

/// Everything about HEAD and its upstream that [`GitPane::to_dto`] reports.
/// Resolved by the off-thread scan for the same reason the branch name is: each
/// field otherwise costs a repository open per read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HeadInfo {
    /// `origin/main`, or `None` when the branch tracks nothing.
    upstream: Option<String>,
    ahead: usize,
    behind: usize,
    state: RepoState,
    detached: bool,
    /// HEAD's full commit message, so `amend` can prefill without opening the
    /// repository on the actor thread. Empty in a repository with no commits.
    message: String,
}

pub struct GitPane {
    workdir: PathBuf,
    /// The worktree root, refreshed with each snapshot. Everything that turns a
    /// status path back into a real path needs it, and like `branch` it is read
    /// whenever a DTO is built, so it must never hit disk.
    repo_root: PathBuf,
    rows: Vec<Row>,
    /// Last known branch name, refreshed with each snapshot. Read by every
    /// `ChangesDto`, so it must never hit disk.
    branch: String,
    /// HEAD and upstream, refreshed with each snapshot. Same no-disk rule as
    /// `branch`, and for the same reason.
    head: HeadInfo,
    /// Every path a tree marker should light up, per filter. Rebuilt whenever
    /// the rows change and never on the way out — see [`Marked`].
    marked: Arc<Marked>,
}

/// The paths a `●` belongs on, closed over their ancestors.
///
/// ## Why this is a set and not a scan
///
/// The tree used to answer "is anything under this directory changed?" by
/// walking the whole change set per entry — `changed.iter().any(starts_with)` —
/// after rebuilding that set, path by path, on *every* directory listing. At
/// 5,000 changed files one listing of a 200-directory root measured 17.5 ms
/// against 0.8 ms clean, and the rebuild half of that ran on the core loop,
/// where it stalls every pane in every session.
///
/// Closing the set over ancestors moves the work to where the answer changes —
/// once per rescan, off the request path entirely — and turns every entry,
/// file and directory alike, into one hash lookup.
///
/// The two sets are built together because they are one walk over the same
/// rows, and because a directory's marker under a filter is exactly "an
/// ancestor of a path that filter keeps".
#[derive(Debug, Default)]
pub(crate) struct Marked {
    all: HashSet<PathBuf>,
    docs: HashSet<PathBuf>,
}

impl Marked {
    /// Build both sets from the changed paths, rooted at `repo_root`.
    fn build(paths: impl Iterator<Item = PathBuf>, repo_root: &Path) -> Self {
        let mut out = Self::default();
        for path in paths {
            // A file is a doc by its own name; a directory on the way up
            // disqualifies the whole path when it is one of the two nobody
            // means, so `target/notes.md` marks nothing on the DOCS rail.
            let is_doc_leaf =
                path.file_name().map(|n| api::is_doc(&n.to_string_lossy(), false)).unwrap_or(false);
            let mut doc_path = is_doc_leaf;
            let mut cur = path.as_path();
            out.all.insert(cur.to_path_buf());
            if doc_path {
                out.docs.insert(cur.to_path_buf());
            }
            while let Some(parent) = cur.parent() {
                if parent == cur {
                    break;
                }
                cur = parent;
                if doc_path {
                    let keeps = cur
                        .file_name()
                        .map(|n| api::is_doc(&n.to_string_lossy(), true))
                        .unwrap_or(true);
                    if keeps {
                        out.docs.insert(cur.to_path_buf());
                    } else {
                        doc_path = false;
                    }
                }
                out.all.insert(cur.to_path_buf());
                // Everything above the worktree root belongs to no listing this
                // route can serve, so the walk stops there rather than seeding
                // the set with `/`, `/home`, and every step in between.
                if cur == repo_root {
                    break;
                }
            }
        }
        out
    }

    /// Should this absolute path carry a marker under `filter`?
    pub(crate) fn contains(&self, path: &Path, filter: api::TreeFilter) -> bool {
        match filter {
            api::TreeFilter::All => self.all.contains(path),
            api::TreeFilter::Docs => self.docs.contains(path),
        }
    }
}

impl GitPane {
    pub fn new(cwd: &Path) -> anyhow::Result<Self> {
        // Fail fast when there is no repository at all. `discover` only walks up
        // for a `.git` dir (cheap); the expensive status scan is deferred to an
        // off-thread `compute`/`apply` so opening a workspace never blocks the
        // core loop (a slow scan here manifested as a black, frozen TUI).
        Repository::discover(cwd)
            .map_err(|_| anyhow::anyhow!("{} is not inside a git repository", cwd.display()))?;
        Ok(Self {
            workdir: cwd.to_path_buf(),
            // Provisional: resolving the real root costs a `canonicalize`, and
            // the first snapshot overwrites it before any row exists to resolve.
            repo_root: cwd.to_path_buf(),
            rows: vec![Row::Empty("  (loading…)")],
            branch: "…".into(),
            head: HeadInfo::default(),
            marked: Arc::new(Marked::default()),
        })
    }

    /// The marker set behind the tree's `●`, shared rather than rebuilt.
    ///
    /// Handed out as an `Arc` because the listing itself is answered on a
    /// blocking thread: the caller clones this pointer on the core loop and
    /// takes it with it, where it used to clone every path in the change set.
    pub(crate) fn marked(&self) -> Arc<Marked> {
        Arc::clone(&self.marked)
    }

    /// Recompute the marker sets from the current rows.
    ///
    /// Called from the two places rows change and nowhere else. It is the only
    /// thing keeping a marker honest after a stage or a discard, so a third
    /// mutation path that forgets it would leave the tree lit for a file that
    /// is no longer changed.
    fn rebuild_marked(&mut self) {
        self.marked = Arc::new(Marked::build(self.changed_paths().into_iter(), &self.repo_root));
    }

    fn repo(&self) -> Result<Repository, git2::Error> {
        Repository::discover(&self.workdir)
    }

    /// The directory the pane was opened in, used as the cwd for `git push`.
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// The worktree root every status path is relative to. Cached, so the API
    /// routes and the core actor thread can use it without opening the
    /// repository. Falls back to the pane's cwd for a repo with no worktree.
    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    /// Build the status view by walking the worktree. Opens its own repository
    /// handle from `workdir`, so this is a free function safe to run on a
    /// blocking thread — nothing here borrows `self` or a `git2` type.
    pub(crate) fn compute(workdir: &Path) -> GitSnapshot {
        let mut rows = Vec::new();
        let Ok(repo) = Repository::discover(workdir) else {
            rows.push(Row::Empty("repository unavailable"));
            return GitSnapshot {
                rows,
                branch: "no branch".into(),
                repo_root: workdir.to_path_buf(),
                head: HeadInfo::default(),
            };
        };
        let branch = head_shorthand(&repo);
        let repo_root = worktree_root_as_spelled(workdir, repo.workdir());
        let head = head_info(&repo);

        let mut opts = StatusOptions::new();
        opts.include_untracked(true).recurse_untracked_dirs(true);
        let statuses = match repo.statuses(Some(&mut opts)) {
            Ok(s) => s,
            Err(e) => {
                // This used to go into the pane's own notice line, which only
                // the rail the daemon drew could show. There is no such rail
                // any more and `ChangesDto` has no slot for it, so the log is
                // where a failed scan is now visible at all.
                tracing::warn!("git status in {}: {e}", workdir.display());
                rows.push(Row::Empty("status failed"));
                return GitSnapshot { rows, branch, repo_root, head };
            }
        };

        let unstaged_stats = diff_file_stats(&repo, DiffSide::Workdir);
        let staged_stats = diff_file_stats(&repo, DiffSide::Index);
        let stages = conflict_stages(&repo);
        let mut sections = Sections::default();
        for entry in statuses.iter() {
            let s = entry.status();
            let p = status_path(&entry);
            // A conflicted file goes to its own section and to neither of the
            // others: half of it is in the index by definition, so listing it as
            // "staged" or "unstaged" invites an action that would commit an
            // unresolved merge.
            if s.contains(Status::CONFLICTED) {
                let (base, ours, theirs) = stages.get(&p).copied().unwrap_or((false, false, false));
                sections.conflicts.push(Row::Conflict { path: p, base, ours, theirs });
                continue;
            }
            if let Some(code) = worktree_code(s) {
                let stat = unstaged_stats.get(&p).copied().unwrap_or((0, 0));
                sections.unstaged.push(Row::Unstaged { path: p.clone(), code, stat });
            }
            if let Some(code) = index_code(s) {
                let stat = staged_stats.get(&p).copied().unwrap_or((0, 0));
                sections.staged.push(Row::Staged { path: p, code, stat });
            }
        }

        sections.commits = recent_commits(&repo, 15).unwrap_or_default();
        rows.extend(layout_rows(sections));
        GitSnapshot { rows, branch, repo_root, head }
    }

    /// Split the current rows back into their three sections, so an optimistic
    /// update can move entries between them without a worktree walk.
    fn sections(&self) -> Sections {
        let mut out = Sections::default();
        for row in &self.rows {
            match row {
                Row::Conflict { .. } => out.conflicts.push(row.clone()),
                Row::Unstaged { .. } => out.unstaged.push(row.clone()),
                Row::Staged { .. } => out.staged.push(row.clone()),
                Row::Commit { .. } => out.commits.push(row.clone()),
                Row::Header(_) | Row::Empty(_) => {}
            }
        }
        out
    }

    /// Re-emit the rows from already-classified sections, preserving selection
    /// by identity.
    ///
    /// Mutations no longer rescan inline (that walks the whole worktree), but
    /// they *read* `self.rows` to find their target and the API replies from
    /// them — so leaving stale rows behind would make the next mutation fail
    /// and reads lie. This applies the change locally; the off-thread rescan
    /// the core schedules then reconciles against the real index.
    fn set_sections(&mut self, sections: Sections) {
        self.rows = layout_rows(sections);
        self.rebuild_marked();
    }

    /// Move one path from unstaged to staged (or back) in the local rows.
    fn move_row(&mut self, path: &Path, to_staged: bool) {
        let mut sections = self.sections();
        let (from, to) = if to_staged {
            (&mut sections.unstaged, &mut sections.staged)
        } else {
            (&mut sections.staged, &mut sections.unstaged)
        };
        let Some(idx) = from.iter().position(|r| match r {
            Row::Unstaged { path: p, .. } | Row::Staged { path: p, .. } => p == path,
            _ => false,
        }) else {
            return;
        };
        let row = from.remove(idx);
        let (p, code, stat) = match row {
            Row::Unstaged { path, code, stat } | Row::Staged { path, code, stat } => {
                (path, code, stat)
            }
            _ => return,
        };
        // An untracked file becomes an addition once staged, and a staged
        // addition is untracked again once unstaged.
        let code = match (to_staged, code) {
            (true, '?') => 'A',
            (false, 'A') => '?',
            (_, c) => c,
        };
        let moved = if to_staged {
            Row::Staged { path: p, code, stat }
        } else {
            Row::Unstaged { path: p, code, stat }
        };
        to.push(moved);
        to.sort_by_key(row_path);
        self.set_sections(sections);
    }

    /// Drop one path from the unstaged section (after a discard).
    fn drop_unstaged_row(&mut self, path: &Path) {
        let mut sections = self.sections();
        sections.unstaged.retain(|r| !matches!(r, Row::Unstaged { path: p, .. } if p == path));
        self.set_sections(sections);
    }

    /// Install a precomputed snapshot.
    pub(crate) fn apply(&mut self, snap: GitSnapshot) {
        self.rows = snap.rows;
        self.branch = snap.branch;
        self.repo_root = snap.repo_root;
        self.head = snap.head;
        // After `repo_root`, never before: the marker paths are rooted at it.
        self.rebuild_marked();
    }

    /// Stage every unstaged change (adds, modifications, and deletions) in one
    /// index write. Returns how many files were staged, or a message on failure
    /// so both the `C` key and the `commit-all` API can surface it. Mirrors
    /// [`stage_path`](Self::stage_path)'s per-file delete handling.
    ///
    /// The rows are *not* refreshed here — that is a full worktree walk, which
    /// the core now runs off-thread. Callers that need the post-stage count use
    /// the returned value rather than re-reading [`staged_summary`].
    pub fn stage_all(&mut self) -> Result<usize, String> {
        let pending: Vec<(PathBuf, char)> = self
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Unstaged { path, code, .. } => Some((path.clone(), *code)),
                _ => None,
            })
            .collect();
        self.repo()
            .and_then(|repo| {
                let mut index = repo.index()?;
                for (path, code) in &pending {
                    if *code == 'D' {
                        index.remove_path(path)?;
                    } else {
                        index.add_path(path)?;
                    }
                }
                index.write()
            })
            .map_err(|e| format!("stage all: {e}"))?;
        for (path, _) in &pending {
            self.move_row(path, true);
        }
        Ok(pending.len())
    }

    /// Commit the index with `message`; returns the short id or an error
    /// string. Used by the commit overlay.
    pub fn commit_with(&mut self, message: &str) -> Result<String, String> {
        let message = message.trim();
        if message.is_empty() {
            return Err("empty commit message".into());
        }
        // `write_tree` refuses an unmerged index anyway, but its wording is
        // libgit2's ("cannot create a tree from a not fully merged index") and
        // says nothing about what to do. Answer in terms of what to do next.
        if self.conflict_count() > 0 {
            return Err("resolve the conflicts first".into());
        }
        let result = self.repo().and_then(|repo| {
            let sig = repo.signature()?;
            let mut index = repo.index()?;
            let tree_id = index.write_tree()?;
            let tree = repo.find_tree(tree_id)?;
            let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
            let parents: Vec<&git2::Commit> = parent.iter().collect();
            repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        });
        match result {
            Ok(oid) => {
                let short = format!("{:.7}", oid.to_string());
                let mut sections = self.sections();
                // The commit consumed the whole index, so the staged section is
                // empty until the off-thread rescan confirms it.
                sections.staged.clear();
                sections
                    .commits
                    .insert(0, Row::Commit { id: short.clone(), summary: message.to_string() });
                self.set_sections(sections);
                Ok(short)
            }
            Err(e) => Err(format!("commit: {e}")),
        }
    }

    /// (files, additions, deletions) currently staged — for the overlay.
    pub fn staged_summary(&self) -> (usize, usize, usize) {
        let mut files = 0;
        let (mut adds, mut dels) = (0, 0);
        for row in &self.rows {
            if let Row::Staged { stat, .. } = row {
                files += 1;
                adds += stat.0;
                dels += stat.1;
            }
        }
        (files, adds, dels)
    }

    /// The cached branch name. Read by every `ChangesDto` and every workspace
    /// summary, so it deliberately does no filesystem work — the value is
    /// refreshed off-thread by [`compute`](Self::compute). Opening the
    /// repository here cost a full `Repository::discover` per read per client,
    /// which froze the whole workbench on network-mounted worktrees.
    pub fn branch(&self) -> &str {
        &self.branch
    }

    /// Local branches (current first) plus the checked-out branch name.
    ///
    /// A free function taking a path, so it can run on a blocking thread — like
    /// [`compute`](Self::compute), and for the same reason: it opens a
    /// repository, and doing that on the core actor froze the daemon whenever
    /// the worktree lived on a hung mount.
    pub(crate) fn branches_at(workdir: &Path) -> butai_protocol::api::BranchesDto {
        use butai_protocol::api::BranchesDto;
        let Ok(repo) = Repository::discover(workdir) else {
            return BranchesDto { current: None, branches: Vec::new(), entries: Vec::new() };
        };
        let current = repo
            .head()
            .ok()
            .filter(|h| h.is_branch())
            .and_then(|h| h.shorthand().map(str::to_string));

        let mut local = branch_entries(&repo, git2::BranchType::Local);
        let mut remote = branch_entries(&repo, git2::BranchType::Remote);
        local.sort_by(|a, b| a.name.cmp(&b.name));
        remote.sort_by(|a, b| a.name.cmp(&b.name));
        // Current branch first, in both lists, so neither the old field nor the
        // new one has to be re-sorted by whoever draws it.
        if let Some(cur) = &current {
            if let Some(i) = local.iter().position(|e| &e.name == cur) {
                let e = local.remove(i);
                local.insert(0, e);
            }
        }
        let branches: Vec<String> = local.iter().map(|e| e.name.clone()).collect();
        local.extend(remote);
        BranchesDto { current, branches, entries: local }
    }

    /// Check out a branch, optionally creating it at HEAD first. Uses git's safe
    /// strategy, so a switch that would clobber uncommitted changes fails
    /// loudly rather than losing work.
    pub fn checkout(&mut self, name: &str, create: bool) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("empty branch name".into());
        }
        let repo = self.repo().map_err(|e| format!("git: {e}"))?;
        if create {
            let head =
                repo.head().and_then(|h| h.peel_to_commit()).map_err(|e| format!("HEAD: {e}"))?;
            repo.branch(name, &head, false).map_err(|e| format!("create branch: {e}"))?;
        }
        let refname = format!("refs/heads/{name}");
        let obj = repo.revparse_single(&refname).map_err(|_| format!("no branch {name:?}"))?;
        repo.checkout_tree(&obj, None).map_err(|e| format!("checkout: {e}"))?;
        repo.set_head(&refname).map_err(|e| format!("set HEAD: {e}"))?;
        Ok(())
    }

    /// Create a branch without switching to it. `from` defaults to HEAD.
    pub fn create_branch(&mut self, name: &str, from: Option<&str>) -> Result<(), String> {
        let name = crate::git_op::valid_ref_name(name.trim())?;
        let repo = self.repo().map_err(|e| format!("git: {e}"))?;
        let target = match from {
            Some(rev) => {
                let rev = crate::git_op::valid_rev(rev)?;
                repo.revparse_single(rev)
                    .and_then(|o| o.peel_to_commit())
                    .map_err(|_| format!("no such revision {rev:?}"))?
            }
            None => {
                repo.head().and_then(|h| h.peel_to_commit()).map_err(|e| format!("HEAD: {e}"))?
            }
        };
        repo.branch(name, &target, false).map_err(|e| format!("create branch: {e}"))?;
        Ok(())
    }

    /// Delete a branch. Without `force`, libgit2 still allows deleting an
    /// unmerged branch, so the check is ours: losing commits to a keystroke is
    /// the thing a client confirms, and it needs a reason to confirm about.
    pub fn delete_branch(&mut self, name: &str, force: bool) -> Result<(), String> {
        let name = crate::git_op::valid_ref_name(name.trim())?;
        let repo = self.repo().map_err(|e| format!("git: {e}"))?;
        if repo.head().ok().and_then(|h| h.shorthand().map(str::to_string)).as_deref() == Some(name)
        {
            return Err(format!("{name} is the current branch"));
        }
        let mut branch = repo
            .find_branch(name, git2::BranchType::Local)
            .map_err(|_| format!("no branch {name:?}"))?;
        if !force {
            // Unmerged means its commits are reachable from nowhere else.
            let tip = branch.get().peel_to_commit().map_err(|e| format!("{name}: {e}"))?;
            let head = repo.head().and_then(|h| h.peel_to_commit());
            if let Ok(head) = head {
                let merged = repo.graph_descendant_of(head.id(), tip.id()).unwrap_or(false)
                    || head.id() == tip.id();
                if !merged {
                    return Err(format!("{name} is not merged — delete with force to discard it"));
                }
            }
        }
        branch.delete().map_err(|e| format!("delete branch: {e}"))?;
        Ok(())
    }

    /// Rename a branch. `from` defaults to the current one.
    pub fn rename_branch(&mut self, from: Option<&str>, to: &str) -> Result<(), String> {
        let to = crate::git_op::valid_ref_name(to.trim())?;
        let repo = self.repo().map_err(|e| format!("git: {e}"))?;
        let from = match from {
            Some(f) => crate::git_op::valid_ref_name(f.trim())?.to_string(),
            None => repo
                .head()
                .ok()
                .filter(|h| h.is_branch())
                .and_then(|h| h.shorthand().map(str::to_string))
                .ok_or("HEAD is detached — name the branch to rename")?,
        };
        let mut branch = repo
            .find_branch(&from, git2::BranchType::Local)
            .map_err(|_| format!("no branch {from:?}"))?;
        branch.rename(to, false).map_err(|e| format!("rename branch: {e}"))?;
        self.branch = to.to_string();
        Ok(())
    }

    /// Absolute paths of every changed file **that is still in the worktree**,
    /// for the tree's markers. Status paths are repo-root relative and the
    /// pane's cwd may be deeper, so they join onto the root — joining onto
    /// `workdir` silently mismarked every file in a workspace opened below it.
    ///
    /// ## Why a deletion is not in here
    ///
    /// The tree lists what is on disk, so a deleted file can never be a row in
    /// it — but its ancestors were marked all the same, and following that `●`
    /// down arrived at a directory with nothing marked in it. The change is
    /// real; the CHANGES rail is where it lives, and it shows it correctly.
    ///
    /// Which side deleted it is what decides, and only the worktree side counts:
    ///
    /// - a worktree deletion (`Unstaged{D}`) means gone, whatever the index
    ///   says — this is also the `Staged{M}` + `Unstaged{D}` case, where the
    ///   staged row would otherwise have marked a file that is not there;
    /// - a staged deletion with no worktree row at all means gone too;
    /// - a staged deletion *with* one — `git rm --cached`, the file still on
    ///   disk and now untracked — means present, and it keeps its marker.
    pub fn changed_paths(&self) -> Vec<PathBuf> {
        let mut wt_deleted: HashSet<&Path> = HashSet::new();
        let mut wt_row: HashSet<&Path> = HashSet::new();
        let mut index_deleted: HashSet<&Path> = HashSet::new();
        for row in &self.rows {
            match row {
                Row::Unstaged { path, code: 'D', .. } => wt_deleted.insert(path.as_path()),
                Row::Unstaged { path, .. } => wt_row.insert(path.as_path()),
                Row::Staged { path, code: 'D', .. } => index_deleted.insert(path.as_path()),
                _ => false,
            };
        }
        let gone =
            |p: &Path| wt_deleted.contains(p) || (index_deleted.contains(p) && !wt_row.contains(p));
        // Deduplicated: one file can hold a staged row and an unstaged row at
        // once, and naming it twice is noise to every caller.
        let mut seen: HashSet<&Path> = HashSet::new();
        self.rows
            .iter()
            .filter_map(|r| match r {
                Row::Unstaged { path, .. }
                | Row::Staged { path, .. }
                | Row::Conflict { path, .. } => {
                    (!gone(path) && seen.insert(path.as_path())).then(|| self.repo_root.join(path))
                }
                _ => None,
            })
            .collect()
    }

    /// Number of changed entries (conflicted + unstaged + staged rows).
    pub fn change_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| {
                matches!(r, Row::Unstaged { .. } | Row::Staged { .. } | Row::Conflict { .. })
            })
            .count()
    }

    /// Number of files with unresolved conflicts.
    pub fn conflict_count(&self) -> usize {
        self.rows.iter().filter(|r| matches!(r, Row::Conflict { .. })).count()
    }

    /// What the repository is in the middle of. Cached; never hits disk.
    pub fn state(&self) -> RepoState {
        self.head.state
    }

    /// Commits ahead of and behind the upstream. Both zero without one.
    pub fn ahead_behind(&self) -> (usize, usize) {
        (self.head.ahead, self.head.behind)
    }

    /// The whole point of this file: what every client draws its CHANGES
    /// column from.
    pub fn to_dto(&self) -> butai_protocol::api::ChangesDto {
        use butai_protocol::api::{ChangesDto, CommitDto, ConflictFile, FileChange};
        let mut staged = Vec::new();
        let mut unstaged = Vec::new();
        let mut recent_commits = Vec::new();
        let mut conflicted = Vec::new();
        for row in &self.rows {
            match row {
                Row::Conflict { path, base, ours, theirs } => conflicted.push(ConflictFile {
                    path: path.display().to_string(),
                    base: *base,
                    ours: *ours,
                    theirs: *theirs,
                }),
                Row::Unstaged { path, code, stat } => unstaged.push(FileChange {
                    path: path.display().to_string(),
                    code: code.to_string(),
                    added: stat.0,
                    deleted: stat.1,
                }),
                Row::Staged { path, code, stat } => staged.push(FileChange {
                    path: path.display().to_string(),
                    code: code.to_string(),
                    added: stat.0,
                    deleted: stat.1,
                }),
                Row::Commit { id, summary } => recent_commits
                    .push(CommitDto { id: format!("{id:.7}"), summary: summary.clone() }),
                _ => {}
            }
        }
        ChangesDto {
            branch: self.branch().to_string(),
            staged,
            unstaged,
            recent_commits,
            conflicted,
            upstream: self.head.upstream.clone(),
            ahead: self.head.ahead,
            behind: self.head.behind,
            state: self.head.state,
            detached: self.head.detached,
        }
    }

    /// Settle one conflicted file.
    ///
    /// Deliberately libgit2 rather than the operation runner: taking a side
    /// needs two git invocations (`checkout --ours` then `add`), the runner runs
    /// one, and none of this touches config, credentials or the network. Doing
    /// it here also answers synchronously, which for a millisecond of index work
    /// is the honest reply.
    ///
    /// `Resolved` is `git add` — the file was edited by hand and the conflict
    /// markers are gone.
    pub fn resolve_path(
        &mut self,
        path: &str,
        take: butai_protocol::api::ResolveSide,
    ) -> Result<(), String> {
        use butai_protocol::api::ResolveSide;
        let p = PathBuf::from(path);
        let conflicted =
            self.rows.iter().any(|r| matches!(r, Row::Conflict { path: q, .. } if q == &p));
        if !conflicted {
            return Err(format!("{path} is not conflicted"));
        }
        let repo = self.repo().map_err(|e| format!("git: {e}"))?;
        let root = repo.workdir().unwrap_or(&self.workdir).to_path_buf();

        if let ResolveSide::Ours | ResolveSide::Theirs = take {
            // Stage 2 is "ours", stage 3 is "theirs"; stage 1 is the merge base.
            let want = if matches!(take, ResolveSide::Ours) { 2 } else { 3 };
            let index = repo.index().map_err(|e| format!("index: {e}"))?;
            let entry = index
                .conflicts()
                .map_err(|e| format!("conflicts: {e}"))?
                .flatten()
                .find(|c| {
                    let e = c.our.as_ref().or(c.their.as_ref()).or(c.ancestor.as_ref());
                    e.map(|e| String::from_utf8_lossy(&e.path) == path).unwrap_or(false)
                })
                .ok_or_else(|| format!("no conflict recorded for {path}"))?;
            let side = if want == 2 { entry.our } else { entry.their };
            match side {
                Some(e) => {
                    let blob = repo.find_blob(e.id).map_err(|e| format!("blob: {e}"))?;
                    std::fs::write(root.join(&p), blob.content())
                        .map_err(|e| format!("write {path}: {e}"))?;
                }
                // That side deleted the file — taking it means deleting it.
                None => {
                    let _ = std::fs::remove_file(root.join(&p));
                }
            }
        }

        // Whichever way it was settled, adding the path is what clears the
        // conflict from the index.
        let mut index = repo.index().map_err(|e| format!("index: {e}"))?;
        if root.join(&p).exists() {
            index.add_path(&p).map_err(|e| format!("add: {e}"))?;
        } else {
            index.remove_path(&p).map_err(|e| format!("remove: {e}"))?;
        }
        index.write().map_err(|e| format!("index: {e}"))?;
        Ok(())
    }

    /// Stage one unstaged file by path (API equivalent of `s`).
    pub fn stage_path(&mut self, path: &str) -> Result<(), String> {
        let code = self.rows.iter().find_map(|r| match r {
            Row::Unstaged { path: p, code, .. } if p.to_string_lossy() == path => Some(*code),
            _ => None,
        });
        let Some(code) = code else {
            return Err(format!("no unstaged file {path:?}"));
        };
        let p = PathBuf::from(path);
        let result = self.repo().and_then(|repo| {
            let mut index = repo.index()?;
            if code == 'D' {
                index.remove_path(&p)?;
            } else {
                index.add_path(&p)?;
            }
            index.write()
        });
        match result {
            Ok(()) => {
                self.move_row(&p, true);
                Ok(())
            }
            Err(e) => Err(format!("stage: {e}")),
        }
    }

    /// Throw away one file's worktree changes (the confirmed half of `x`, and
    /// the API's `changes/discard`): untracked files are deleted, tracked ones
    /// are restored from the index — `git restore <path>`. A file that is also
    /// staged keeps its staged version. Unrecoverable, so callers confirm first.
    pub fn discard_path(&mut self, path: &str) -> Result<(), String> {
        let code = self.rows.iter().find_map(|r| match r {
            Row::Unstaged { path: p, code, .. } if p.to_string_lossy() == path => Some(*code),
            _ => None,
        });
        let Some(code) = code else {
            return Err(format!("no unstaged file {path:?}"));
        };
        let repo = self.repo().map_err(|e| format!("git: {e}"))?;
        if code == '?' {
            // Status paths are repo-root relative; the pane's cwd may be deeper.
            let root = repo.workdir().unwrap_or(&self.workdir);
            let abs = root.join(path);
            let removed = if abs.is_dir() {
                std::fs::remove_dir_all(&abs)
            } else {
                std::fs::remove_file(&abs)
            };
            removed.map_err(|e| format!("discard: {e}"))?;
        } else {
            let mut co = git2::build::CheckoutBuilder::new();
            co.force().path(path);
            repo.checkout_index(None, Some(&mut co)).map_err(|e| format!("discard: {e}"))?;
        }
        self.drop_unstaged_row(Path::new(path));
        Ok(())
    }

    /// Unstage one staged file by path (API equivalent of `u`).
    pub fn unstage_path(&mut self, path: &str) -> Result<(), String> {
        let staged = self
            .rows
            .iter()
            .any(|r| matches!(r, Row::Staged { path: p, .. } if p.to_string_lossy() == path));
        if !staged {
            return Err(format!("no staged file {path:?}"));
        }
        let p = PathBuf::from(path);
        let result = self.repo().and_then(|repo| {
            match repo.head().ok().and_then(|h| h.peel(git2::ObjectType::Commit).ok()) {
                Some(head) => repo.reset_default(Some(&head), [&p]),
                None => {
                    let mut index = repo.index()?;
                    index.remove_path(&p)?;
                    index.write()
                }
            }
        });
        match result {
            Ok(()) => {
                self.move_row(&p, false);
                Ok(())
            }
            Err(e) => Err(format!("unstage: {e}")),
        }
    }
}

/// `  M path/to/file      +12 -3` with the stat right-aligned.
/// Print a diff as unified-diff text, faithfully enough to apply.
///
/// The rendering path used to trim and reflow this text for display, which
/// lost the `\ No newline at end of file` markers — invisible in a viewer, and
/// fatal in a patch. The one canonical form is produced here and everything
/// else (display, selection, apply) is derived from it, so what you are looking
/// at and what gets applied cannot be two different things.
pub fn patch_text(diff: &git2::Diff) -> Result<String, git2::Error> {
    let mut out = String::new();
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        // For `+`, `-` and context, libgit2 hands over the line without its
        // marker. For file headers, hunk headers and the no-newline markers it
        // hands over text that is already complete, newline included.
        if matches!(line.origin(), '+' | '-' | ' ') {
            out.push(line.origin());
        }
        out.push_str(&String::from_utf8_lossy(line.content()));
        true
    })?;
    Ok(out)
}

/// Apply `patch` to the index or the worktree, optionally backwards.
///
/// The whole of partial staging lands here: stage a hunk (forward, to the
/// index), unstage one (backwards, to the index), discard one (backwards, to
/// the worktree). libgit2 rather than `git apply` because this only ever writes
/// the index or a tracked file — the boundary the rest of the daemon keeps,
/// where anything touching refs, hooks or the network shells out to real git.
///
/// libgit2 has no reverse flag, so [`butai_protocol::hunk::Patch::reversed`]
/// does it on the model and hands back text.
pub fn apply_patch(
    workdir: &Path,
    patch: &str,
    target: butai_protocol::api::ApplyTarget,
    reverse: bool,
) -> Result<(), String> {
    use butai_protocol::api::ApplyTarget;
    let text = if reverse {
        butai_protocol::hunk::Patch::parse(patch).reversed().to_text()
    } else {
        patch.to_string()
    };
    if text.trim().is_empty() {
        return Err("empty patch".into());
    }
    let repo = Repository::discover(workdir).map_err(|e| format!("git: {e}"))?;
    let diff = git2::Diff::from_buffer(text.as_bytes()).map_err(|e| format!("patch: {e}"))?;
    let location = match target {
        ApplyTarget::Index => git2::ApplyLocation::Index,
        ApplyTarget::Worktree => git2::ApplyLocation::WorkDir,
    };
    repo.apply(&diff, location, None).map_err(|e| format!("apply: {e}"))
}

enum DiffSide {
    Workdir,
    Index,
}

/// Per-file (+added, -deleted) line counts for one side of the tree.
fn diff_file_stats(
    repo: &Repository,
    side: DiffSide,
) -> std::collections::HashMap<PathBuf, (usize, usize)> {
    let mut map = std::collections::HashMap::new();
    let mut opts = git2::DiffOptions::new();
    // `recurse_untracked_dirs` for the same reason the status walk sets it:
    // without it libgit2 reports a new *directory* as one delta with no content,
    // so every file inside it counted `+0 -0` while a new file at the root
    // counted correctly. The status scan lists them individually either way, so
    // the row was there with nothing on it.
    opts.include_untracked(true).show_untracked_content(true).recurse_untracked_dirs(true);
    let diff = match side {
        DiffSide::Workdir => repo.diff_index_to_workdir(None, Some(&mut opts)),
        DiffSide::Index => {
            let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
            repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts))
        }
    };
    if let Ok(diff) = diff {
        let _ = diff.foreach(
            &mut |_, _| true,
            None,
            None,
            Some(&mut |delta, _hunk, line| {
                if let Some(p) = delta.new_file().path().or_else(|| delta.old_file().path()) {
                    let e = map.entry(p.to_path_buf()).or_insert((0usize, 0usize));
                    match line.origin() {
                        '+' => e.0 += 1,
                        '-' => e.1 += 1,
                        _ => {}
                    }
                }
                true
            }),
        );
    }
    map
}

/// A status entry's path, whatever bytes the filesystem spelled it with.
///
/// This used to be `entry.path()`, which is `Option<&str>` and answers `None`
/// for a name that is not valid UTF-8 — and the scan skipped those entries with
/// no log line and no row. A file was then missing from the CHANGES rail *and*
/// from every marker in the tree, so `git status` and the workbench disagreed
/// about how many changes there were and nothing said why.
///
/// **The name still reaches a client lossily**, because every path on the wire
/// is a JSON string: it draws with replacement characters, and staging it by
/// path will not match. That is a smaller problem than a change the workbench
/// cannot see at all, and a visible broken row is diagnosable where silence is
/// not. Carrying such a name faithfully needs an encoding change to the
/// protocol, which is a bigger question than this bug.
fn status_path(entry: &git2::StatusEntry<'_>) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(entry.path_bytes()))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(entry.path_bytes()).into_owned())
    }
}

fn worktree_code(s: Status) -> Option<char> {
    // CONFLICTED first: libgit2 usually reports it alone, but when it does not,
    // an unmerged file that reads as a plain `M` invites you to stage half a
    // merge. Whatever else is set, being conflicted is the thing to say.
    if s.contains(Status::CONFLICTED) {
        Some('!')
    } else if s.contains(Status::WT_NEW) {
        Some('?')
    } else if s.contains(Status::WT_MODIFIED) {
        Some('M')
    } else if s.contains(Status::WT_DELETED) {
        Some('D')
    } else if s.contains(Status::WT_RENAMED) {
        Some('R')
    } else if s.contains(Status::WT_TYPECHANGE) {
        Some('T')
    } else {
        None
    }
}

fn index_code(s: Status) -> Option<char> {
    if s.contains(Status::INDEX_NEW) {
        Some('A')
    } else if s.contains(Status::INDEX_MODIFIED) {
        Some('M')
    } else if s.contains(Status::INDEX_DELETED) {
        Some('D')
    } else if s.contains(Status::INDEX_RENAMED) {
        Some('R')
    } else if s.contains(Status::INDEX_TYPECHANGE) {
        Some('T')
    } else {
        None
    }
}

/// Assemble the three sections into the displayed row list, inserting the
/// headers and the "nothing here" placeholders. Shared by the off-thread scan
/// and the optimistic in-place updates so both produce identical structure.
fn layout_rows(sections: Sections) -> Vec<Row> {
    let mut rows = Vec::new();
    // Conflicts first: they are what is blocking you, and unlike the other
    // sections the header is omitted entirely when there are none, so a clean
    // tree looks exactly as it did before conflicts were modelled.
    if !sections.conflicts.is_empty() {
        rows.push(Row::Header("Conflicts"));
        rows.extend(sections.conflicts);
    }
    rows.push(Row::Header("Unstaged"));
    if sections.unstaged.is_empty() {
        rows.push(Row::Empty("  (clean)"));
    } else {
        rows.extend(sections.unstaged);
    }
    rows.push(Row::Header("Staged"));
    if sections.staged.is_empty() {
        rows.push(Row::Empty("  (nothing staged)"));
    } else {
        rows.extend(sections.staged);
    }
    rows.push(Row::Header("Commits"));
    if sections.commits.is_empty() {
        rows.push(Row::Empty("  (no commits yet)"));
    } else {
        rows.extend(sections.commits);
    }
    rows
}

/// Sort key for the file rows within a section.
fn row_path(row: &Row) -> PathBuf {
    match row {
        Row::Unstaged { path, .. } | Row::Staged { path, .. } => path.clone(),
        _ => PathBuf::new(),
    }
}

/// Short name of `HEAD` ("main", a tag, or a detached short id), or "no branch"
/// in an empty repository. Takes an already-open repo so callers on the render
/// path never pay for a `Repository::discover`.
fn head_shorthand(repo: &Repository) -> String {
    repo.head()
        .ok()
        .and_then(|h| h.shorthand().map(str::to_string))
        .unwrap_or_else(|| "no branch".into())
}

/// Which of the three merge stages each unmerged path still has.
///
/// A both-modified conflict has all three; delete/modify is missing `ours` or
/// `theirs`; both-added is missing `base`. Clients use it to decide which of
/// "take ours"/"take theirs" can even be offered.
fn conflict_stages(repo: &Repository) -> std::collections::HashMap<PathBuf, (bool, bool, bool)> {
    let mut out = std::collections::HashMap::new();
    let Ok(index) = repo.index() else { return out };
    let Ok(conflicts) = index.conflicts() else { return out };
    for c in conflicts.flatten() {
        // Any one of the three stages carries the path; they agree.
        let path = c
            .our
            .as_ref()
            .or(c.their.as_ref())
            .or(c.ancestor.as_ref())
            .map(|e| PathBuf::from(String::from_utf8_lossy(&e.path).into_owned()));
        let Some(path) = path else { continue };
        out.insert(path, (c.ancestor.is_some(), c.our.is_some(), c.their.is_some()));
    }
    out
}

/// Resolve HEAD, its upstream and any in-progress sequence.
///
/// Runs inside the off-thread scan, alongside the worktree walk that dominates
/// it. `graph_ahead_behind` is a revwalk bounded by how far the branches have
/// diverged; the reported counts are capped, though the walk itself is not —
/// libgit2 offers no bounded form, and next to a full `statuses()` it does not
/// register.
fn head_info(repo: &Repository) -> HeadInfo {
    let state = match repo.state() {
        git2::RepositoryState::Clean => RepoState::Clean,
        git2::RepositoryState::Merge => RepoState::Merge,
        git2::RepositoryState::Revert | git2::RepositoryState::RevertSequence => RepoState::Revert,
        git2::RepositoryState::CherryPick | git2::RepositoryState::CherryPickSequence => {
            RepoState::CherryPick
        }
        git2::RepositoryState::Bisect => RepoState::Bisect,
        // `git rebase --continue` drives all three, so the distinction buys a
        // client nothing.
        git2::RepositoryState::Rebase
        | git2::RepositoryState::RebaseInteractive
        | git2::RepositoryState::RebaseMerge => RepoState::Rebase,
        _ => RepoState::Unknown,
    };

    let mut info = HeadInfo { state, ..HeadInfo::default() };
    let Ok(head) = repo.head() else {
        // No HEAD at all: a repository with no commits yet.
        return info;
    };
    info.detached = !head.is_branch();
    info.message = head
        .peel_to_commit()
        .ok()
        .and_then(|c| c.message().map(str::to_string))
        .unwrap_or_default();

    let (Some(refname), Some(local)) = (head.name(), head.target()) else { return info };
    let Ok(upstream_ref) = repo.branch_upstream_name(refname) else { return info };
    let Some(upstream_ref) = upstream_ref.as_str().map(str::to_string) else { return info };
    // `refs/remotes/origin/main` reads as `origin/main` everywhere a user sees it.
    info.upstream =
        Some(upstream_ref.strip_prefix("refs/remotes/").unwrap_or(&upstream_ref).to_string());
    if let Ok(upstream_oid) = repo.refname_to_id(&upstream_ref) {
        if let Ok((ahead, behind)) = repo.graph_ahead_behind(local, upstream_oid) {
            info.ahead = ahead.min(AHEAD_BEHIND_CAP);
            info.behind = behind.min(AHEAD_BEHIND_CAP);
        }
    }
    info
}

/// Every branch of one kind, with its upstream and how far it has drifted.
///
/// The same revwalk [`head_info`] does for the checked-out branch, run over
/// each ref instead of one — which is why it lives beside it.
///
/// **The cap does not bound the work.** `graph_ahead_behind` counts the whole
/// divergence and [`AHEAD_BEHIND_CAP`] only clamps the number afterwards, so
/// this is one full merge-base walk *per local branch* — the cost `head_info`
/// paid for one branch, now paid for all of them. Fine for the tens of branches
/// a working repository has; if it ever shows up on a repository with hundreds,
/// the fix is to skip the walk for branches whose tip equals their upstream's,
/// not to trust a cap that is applied too late to help.
fn branch_entries(
    repo: &Repository,
    kind: git2::BranchType,
) -> Vec<butai_protocol::api::BranchDto> {
    use butai_protocol::api::BranchDto;
    let remote = kind == git2::BranchType::Remote;
    let Ok(iter) = repo.branches(Some(kind)) else { return Vec::new() };
    let mut out = Vec::new();
    for (branch, _) in iter.flatten() {
        let Ok(Some(name)) = branch.name() else { continue };
        // `origin/HEAD` is a symbolic ref onto another row in this same list,
        // so listing it would show the default branch twice under two names.
        if remote && name.ends_with("/HEAD") {
            continue;
        }
        let name = name.to_string();
        let Some(tip_oid) = branch.get().target() else { continue };

        // A remote-tracking branch is somebody else's tip: it has no upstream
        // of its own, and asking git for one answers about the local branch of
        // the same name.
        let mut upstream = None;
        let (mut ahead, mut behind) = (0, 0);
        if !remote {
            if let Some(up) = branch
                .upstream()
                .ok()
                .and_then(|u| u.name().ok().flatten().map(str::to_string).zip(u.get().target()))
            {
                let (up_name, up_oid) = up;
                upstream = Some(up_name);
                if let Ok((a, b)) = repo.graph_ahead_behind(tip_oid, up_oid) {
                    ahead = a.min(AHEAD_BEHIND_CAP);
                    behind = b.min(AHEAD_BEHIND_CAP);
                }
            }
        }

        out.push(BranchDto { name, remote, upstream, ahead, behind, tip: tip_oid.to_string() });
    }
    out
}

/// Parse one `git log --decorate=full --format=%D` field.
///
/// The field looks like `HEAD -> refs/heads/main, refs/remotes/origin/main,
/// tag: refs/tags/v0.8.0`, and is empty for the overwhelming majority of
/// commits. `HEAD -> X` yields *two* entries — HEAD and the branch — because a
/// client wants to mark the checked-out tip without re-deriving which branch
/// that is.
///
/// Pure, so the shapes below are unit-tested rather than discovered against a
/// repository that happens to have a tag in it.
pub(crate) fn parse_decoration(field: &str) -> Vec<butai_protocol::api::RefDecoration> {
    use butai_protocol::api::{RefDecoration, RefKind};
    let classify = |s: &str| -> Option<RefDecoration> {
        let s = s.trim();
        if s == "HEAD" {
            return Some(RefDecoration { name: "HEAD".into(), kind: RefKind::Head });
        }
        // The `tag:` marker comes before the refname, not instead of it.
        let s = s.strip_prefix("tag:").map(str::trim).unwrap_or(s);
        for (prefix, kind) in [
            ("refs/heads/", RefKind::Branch),
            ("refs/remotes/", RefKind::Remote),
            ("refs/tags/", RefKind::Tag),
        ] {
            if let Some(name) = s.strip_prefix(prefix) {
                return Some(RefDecoration { name: name.to_string(), kind });
            }
        }
        None
    };
    field
        .split(',')
        .flat_map(|part| {
            // `HEAD -> refs/heads/main` is one comma-separated part naming two
            // refs. Anything else splits to a single side.
            let mut sides = part.split("->");
            let first = sides.next().unwrap_or_default();
            match sides.next() {
                Some(second) => vec![classify(first), classify(second)],
                None => vec![classify(first)],
            }
        })
        .flatten()
        .collect()
}

/// The worktree root, spelled the way `workdir` is spelled.
///
/// libgit2 canonicalizes its answer (`/var` becomes `/private/var` on macOS),
/// but a workspace's cwd is whatever the client passed in, and the Files tree
/// builds its entry paths from *that*. Two spellings of one directory compare
/// unequal, which would silently unmark every changed file — so libgit2's answer
/// is used only to learn how far up the root is, and the caller's spelling is
/// kept. Falls back to `workdir` for a repository with no worktree.
fn worktree_root_as_spelled(workdir: &Path, git_root: Option<&Path>) -> PathBuf {
    let depth = git_root
        .and_then(|root| {
            let here = workdir.canonicalize().ok()?;
            let root = root.canonicalize().ok()?;
            Some(here.strip_prefix(root).ok()?.components().count())
        })
        .unwrap_or(0);
    let mut root = workdir.to_path_buf();
    for _ in 0..depth {
        if !root.pop() {
            return workdir.to_path_buf();
        }
    }
    root
}

fn recent_commits(repo: &Repository, limit: usize) -> Result<Vec<Row>, git2::Error> {
    let mut walk = repo.revwalk()?;
    walk.push_head()?;
    let mut out = Vec::new();
    for oid in walk.take(limit) {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        out.push(Row::Commit {
            id: oid.to_string(),
            summary: commit.summary().unwrap_or("<no message>").to_string(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod decoration_tests {
    use butai_protocol::api::RefKind;

    use super::parse_decoration;

    fn parsed(field: &str) -> Vec<(String, RefKind)> {
        parse_decoration(field).into_iter().map(|d| (d.name, d.kind)).collect()
    }

    /// The overwhelming majority of commits carry no decoration at all, and
    /// `%D` spells that as an empty field — which `split(',')` still yields one
    /// (empty) part for.
    #[test]
    fn an_undecorated_commit_has_no_refs() {
        assert!(parsed("").is_empty());
    }

    /// `HEAD -> refs/heads/main` names two refs in one comma-separated part.
    /// Collapsing it to one is how a client loses either the branch chip or its
    /// only way of knowing which tip is checked out.
    #[test]
    fn head_and_its_branch_are_both_reported() {
        assert_eq!(
            parsed("HEAD -> refs/heads/main"),
            vec![("HEAD".into(), RefKind::Head), ("main".into(), RefKind::Branch)]
        );
    }

    /// The real shape from a decorated tip: local branch under HEAD, the
    /// remote-tracking branch, and both tag flavours.
    #[test]
    fn every_ref_kind_is_told_apart_by_its_prefix() {
        assert_eq!(
            parsed(
                "HEAD -> refs/heads/main, refs/remotes/origin/main, \
                 tag: refs/tags/v0.8.0, refs/heads/fix/rail"
            ),
            vec![
                ("HEAD".into(), RefKind::Head),
                ("main".into(), RefKind::Branch),
                ("origin/main".into(), RefKind::Remote),
                ("v0.8.0".into(), RefKind::Tag),
                ("fix/rail".into(), RefKind::Branch),
            ]
        );
    }

    /// A detached HEAD decorates as bare `HEAD` with no arrow. It used to fall
    /// through the arrow branch and report nothing at all.
    #[test]
    fn a_detached_head_is_still_a_ref() {
        assert_eq!(
            parsed("HEAD, refs/tags/v1"),
            vec![("HEAD".into(), RefKind::Head), ("v1".into(), RefKind::Tag),]
        );
    }

    /// Branch names may contain `/`, and `refs/heads/` must be stripped once,
    /// not greedily — `feat/refs/heads` is a legal branch name.
    #[test]
    fn only_the_leading_prefix_is_stripped() {
        assert_eq!(
            parsed("refs/heads/feat/refs/heads"),
            vec![("feat/refs/heads".into(), RefKind::Branch)]
        );
    }

    /// Anything this version does not model is dropped rather than guessed at:
    /// a `refs/notes/` or `refs/stash` decoration is not a branch.
    #[test]
    fn an_unmodelled_ref_is_dropped() {
        assert!(parsed("refs/notes/commits").is_empty());
        assert_eq!(parsed("refs/stash, refs/heads/main"), vec![("main".into(), RefKind::Branch)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worktree scan is off-thread in production
    /// (`ServerCore::request_git_refresh`); tests drive the same
    /// `compute` + `apply` pair synchronously.
    fn refresh(pane: &mut GitPane) {
        let snap = GitPane::compute(&pane.workdir);
        pane.apply(snap);
    }

    fn setup_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        (dir, repo)
    }

    fn rows_text(pane: &GitPane) -> Vec<String> {
        pane.rows
            .iter()
            .map(|r| match r {
                Row::Header(h) => format!("H:{h}"),
                Row::Conflict { path, .. } => format!("X:{}", path.display()),
                Row::Unstaged { path, code, .. } => format!("U:{code}:{}", path.display()),
                Row::Staged { path, code, .. } => format!("S:{code}:{}", path.display()),
                Row::Commit { summary, .. } => format!("C:{summary}"),
                Row::Empty(t) => format!("E:{}", t.trim()),
            })
            .collect()
    }

    /// The one path a client has to a path's status code: find the row.
    fn row_code(pane: &GitPane, name: &str) -> Option<char> {
        pane.rows.iter().find_map(|r| match r {
            Row::Unstaged { path, code, .. } | Row::Staged { path, code, .. }
                if path.to_string_lossy() == name =>
            {
                Some(*code)
            }
            _ => None,
        })
    }

    /// The `+n -n` a row carries, by path.
    fn row_stat(pane: &GitPane, name: &str) -> Option<(usize, usize)> {
        pane.rows.iter().find_map(|r| match r {
            Row::Unstaged { path, stat, .. } | Row::Staged { path, stat, .. }
                if path.to_string_lossy() == name =>
            {
                Some(*stat)
            }
            _ => None,
        })
    }

    /// A new file in a new directory counts its lines like any other.
    ///
    /// The stats come from a diff, and libgit2 reports an untracked *directory*
    /// as a single delta with no content unless it is asked to recurse — so a
    /// new file at the worktree root counted correctly while every file under a
    /// new directory showed `+0`. The status walk recurses, so the rows were
    /// there all along with nothing on them.
    #[test]
    fn a_new_file_in_a_new_directory_counts_its_lines() {
        let (dir, _repo) = setup_repo();
        std::fs::write(dir.path().join("root.txt"), "one\n").unwrap();
        std::fs::create_dir_all(dir.path().join("brand")).unwrap();
        std::fs::write(dir.path().join("brand/new.txt"), "alpha\nbeta\ngamma\n").unwrap();

        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane);
        assert_eq!(row_stat(&pane, "root.txt"), Some((1, 0)), "{:?}", rows_text(&pane));
        assert_eq!(row_stat(&pane, "brand/new.txt"), Some((3, 0)), "{:?}", rows_text(&pane));
    }

    /// The old `branches` field and the new `entries` must describe the same
    /// set of local branches in the same order. They are read by different
    /// clients — the branch picker takes the names, the GIT page takes the
    /// entries — and a repository where the two disagree is one where the
    /// picker and the page offer different branches.
    #[test]
    fn the_name_list_and_the_entry_list_agree() {
        let (dir, repo) = setup_repo();
        commit_file(dir.path(), &repo, "a.txt", "one\n");
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("zeta", &head, false).unwrap();
        repo.branch("alpha", &head, false).unwrap();

        let dto = GitPane::branches_at(dir.path());
        let current = dto.current.clone().expect("on a branch");
        let locals: Vec<String> =
            dto.entries.iter().filter(|e| !e.remote).map(|e| e.name.clone()).collect();
        assert_eq!(dto.branches, locals, "the two lists disagree");
        assert_eq!(dto.branches.first(), Some(&current), "current branch is not first");
        assert!(dto.branches.contains(&"alpha".to_string()));
        // Sorted after the current one, not in ref-iteration order.
        assert_eq!(&dto.branches[1..], &["alpha".to_string(), "zeta".to_string()]);
    }

    /// A branch with no upstream reports zero drift rather than guessing, and
    /// the tip is the commit the branch actually points at — the field the GIT
    /// page scopes its graph by, so a wrong one points the whole page at
    /// another branch's history.
    #[test]
    fn a_branch_entry_carries_its_tip_and_no_phantom_upstream() {
        let (dir, repo) = setup_repo();
        commit_file(dir.path(), &repo, "a.txt", "one\n");
        let head = repo.head().unwrap().peel_to_commit().unwrap();

        let dto = GitPane::branches_at(dir.path());
        let cur = dto.current.clone().unwrap();
        let entry = dto.entries.iter().find(|e| e.name == cur).expect("current branch listed");
        assert_eq!(entry.tip, head.id().to_string(), "tip is not HEAD's commit");
        assert!(!entry.remote);
        assert_eq!(entry.upstream, None, "invented an upstream");
        assert_eq!((entry.ahead, entry.behind), (0, 0));
    }

    /// `branch()` is read by every `ChangesDto` this workspace serves, which is
    /// once per client per refresh. It used to run `Repository::discover` each
    /// time — a full repo open per read, which froze the workbench on a network
    /// worktree. Deleting the repository out from under the pane proves the
    /// value is served from the snapshot and never from disk.
    #[test]
    fn branch_is_cached_and_never_touches_disk() {
        let (dir, repo) = setup_repo();
        commit_file(dir.path(), &repo, "a.txt", "one\n");
        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane);
        let branch = pane.branch().to_string();
        assert!(!branch.is_empty() && branch != "…", "branch not resolved: {branch:?}");

        drop(repo);
        std::fs::remove_dir_all(dir.path().join(".git")).unwrap();
        assert_eq!(pane.branch(), branch, "branch() re-read the repository");
        assert_eq!(pane.to_dto().branch, branch, "the DTO re-read the repository");
    }

    /// A pane opened below the worktree root still resolves changed files to
    /// real paths. `git status` answers relative to the root, so joining onto
    /// the pane's own cwd produced `root/sub/sub/a.txt` — a path that does not
    /// exist, which silently unmarked every changed file in the Files tree.
    #[test]
    fn changed_paths_are_rooted_at_the_worktree_not_the_pane_cwd() {
        let (dir, repo) = setup_repo();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        commit_file(dir.path(), &repo, "sub/a.txt", "one\n");
        std::fs::write(dir.path().join("sub/a.txt"), "two\n").unwrap();

        let mut pane = GitPane::new(&dir.path().join("sub")).unwrap();
        refresh(&mut pane);

        let changed = pane.changed_paths();
        assert_eq!(changed, vec![dir.path().join("sub/a.txt")], "changed paths: {changed:?}");
        assert!(
            changed.iter().all(|p| p.exists()),
            "named a path that does not exist: {changed:?}"
        );
    }

    /// A deleted file cannot be a row in a tree of files that exist, so it must
    /// not mark the directory it left either — that `\u25cf` was a promise that
    /// following it arrived somewhere, and it arrived at nothing.
    #[test]
    fn a_deleted_file_marks_nothing_in_the_tree() {
        let (dir, repo) = setup_repo();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        commit_file(dir.path(), &repo, "sub/gone.txt", "one\n");
        // The directory outlives the file, which is the whole shape of the bug.
        std::fs::remove_file(dir.path().join("sub/gone.txt")).unwrap();

        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane);

        // The change is real and the rail still reports it...
        assert_eq!(pane.change_count(), 1, "the deletion left the CHANGES rail");
        // ...but nothing in the tree is marked by it, at any depth.
        assert!(pane.changed_paths().is_empty(), "{:?}", pane.changed_paths());
        let m = pane.marked();
        for p in ["sub/gone.txt", "sub", ""] {
            let abs = dir.path().join(p);
            assert!(!m.contains(&abs, api::TreeFilter::All), "{p} must not be marked");
        }
    }

    /// The case a plain "drop `D` rows" rule gets wrong: staged *content* for a
    /// file whose worktree copy is then deleted. The staged row is not a
    /// deletion, but the file is still not on disk.
    #[test]
    fn a_staged_change_does_not_mark_a_file_the_worktree_deleted() {
        let (dir, repo) = setup_repo();
        commit_file(dir.path(), &repo, "a.txt", "one\n");
        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane);
        pane.stage_path("a.txt").unwrap();
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        refresh(&mut pane);

        assert!(pane.changed_paths().is_empty(), "{:?}", pane.changed_paths());
    }

    /// And the other direction: `git rm --cached` leaves the file on disk, so
    /// the staged deletion must not take its marker away.
    #[test]
    fn a_staged_deletion_still_marks_a_file_that_is_on_disk() {
        let (dir, repo) = setup_repo();
        commit_file(dir.path(), &repo, "a.txt", "one\n");
        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane);
        // Drop it from the index without touching the worktree.
        let mut index = repo.index().unwrap();
        index.remove_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        refresh(&mut pane);

        assert!(dir.path().join("a.txt").exists(), "the fixture deleted the file");
        assert_eq!(
            pane.changed_paths(),
            vec![dir.path().join("a.txt")],
            "a file still on disk lost its marker"
        );
    }

    /// A name the filesystem allows but UTF-8 does not used to be skipped by
    /// the scan entirely — missing from the rail *and* from every marker, so
    /// `git status` and the workbench disagreed about the count and nothing
    /// said why.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_filename_still_reaches_the_rail() {
        use std::os::unix::ffi::OsStrExt;
        let (dir, repo) = setup_repo();
        commit_file(dir.path(), &repo, "a.txt", "one\n");

        let raw = std::ffi::OsStr::from_bytes(&[0x01, 0x4a, 0xb6, 0x40]);
        let odd = dir.path().join(raw);
        std::fs::write(&odd, "x\n").unwrap();

        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane);

        assert_eq!(pane.change_count(), 1, "the untracked file never arrived");
        assert_eq!(pane.changed_paths(), vec![odd.clone()], "and it carries its real bytes");
        assert!(pane.marked().contains(&odd, api::TreeFilter::All), "so the tree can mark it");
    }

    /// `repo_root` is read on the same paths as `branch()`, so it carries the
    /// same no-disk guarantee — and it keeps the caller's spelling, because the
    /// Files tree compares it against paths built from the workspace cwd.
    #[test]
    fn repo_root_is_cached_spelled_as_given_and_never_touches_disk() {
        let (dir, repo) = setup_repo();
        commit_file(dir.path(), &repo, "a.txt", "one\n");
        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane);
        assert_eq!(pane.repo_root(), dir.path(), "libgit2's canonical spelling leaked out");

        drop(repo);
        std::fs::remove_dir_all(dir.path().join(".git")).unwrap();
        assert_eq!(pane.repo_root(), dir.path(), "repo_root() re-read the repository");
    }

    /// Commit `contents` to `name` on the current branch, with HEAD as parent.
    /// `commit_file` deliberately makes root commits; this builds history.
    fn commit_on_head(dir: &Path, repo: &Repository, name: &str, contents: &str, msg: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(name)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents).unwrap();
    }

    /// A repo stopped mid-merge with exactly one conflicted file, `a.txt`.
    /// Returns only the directory: every `git2` handle is scoped so the
    /// repository is closed before the pane opens its own.
    fn conflicted_repo() -> tempfile::TempDir {
        let (dir, repo) = setup_repo();
        commit_on_head(dir.path(), &repo, "a.txt", "base\n", "base");
        let ours = repo.head().unwrap().shorthand().unwrap().to_string();
        {
            let base = repo.head().unwrap().peel_to_commit().unwrap();
            repo.branch("theirs", &base, false).unwrap();
        }

        // One side of the conflict on the original branch...
        commit_on_head(dir.path(), &repo, "a.txt", "ours\n", "ours");

        // ...the other on `theirs`, touching the same line.
        {
            let obj = repo.revparse_single("refs/heads/theirs").unwrap();
            repo.checkout_tree(&obj, None).unwrap();
        }
        repo.set_head("refs/heads/theirs").unwrap();
        commit_on_head(dir.path(), &repo, "a.txt", "theirs\n", "theirs");
        let theirs_id = repo.head().unwrap().peel_to_commit().unwrap().id();

        {
            let obj = repo.revparse_single(&format!("refs/heads/{ours}")).unwrap();
            repo.checkout_tree(&obj, None).unwrap();
        }
        repo.set_head(&format!("refs/heads/{ours}")).unwrap();

        {
            let annotated = repo.find_annotated_commit(theirs_id).unwrap();
            repo.merge(&[&annotated], None, None).unwrap();
        }
        assert!(repo.index().unwrap().has_conflicts(), "the fixture did not conflict");
        dir
    }

    /// A conflicted file gets its own section and leaves `unstaged` entirely.
    ///
    /// This is the load-bearing half of conflict support: half of an unmerged
    /// file is in the index by definition, so listing it as ordinary unstaged
    /// work offers `s` on it — and staging a conflict commits the `<<<<<<<`
    /// markers.
    #[test]
    fn a_conflict_leaves_the_unstaged_list_and_gets_its_own_section() {
        let dir = conflicted_repo();
        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane);

        let rows = rows_text(&pane);
        assert!(rows.contains(&"H:Conflicts".to_string()), "no Conflicts header: {rows:?}");
        assert!(rows.contains(&"X:a.txt".to_string()), "a.txt is not conflicted: {rows:?}");
        assert!(
            !rows.iter().any(|r| r.starts_with("U:") || r.starts_with("S:")),
            "the conflict also showed as ordinary work: {rows:?}"
        );
        assert_eq!(pane.conflict_count(), 1);
        assert_eq!(pane.state(), RepoState::Merge, "repository state not reported");

        // ...and it reaches clients the same way.
        let dto = pane.to_dto();
        assert_eq!(dto.conflicted.len(), 1, "{dto:?}");
        assert_eq!(dto.conflicted[0].path, "a.txt");
        assert!(dto.conflicted[0].base && dto.conflicted[0].ours && dto.conflicted[0].theirs);
        assert!(dto.unstaged.is_empty() && dto.staged.is_empty(), "{dto:?}");
        assert_eq!(dto.state, RepoState::Merge);
    }

    /// The rail can now settle a conflict, which is the whole reason the verb
    /// tables are contextual.
    ///
    /// `resolve_path` and its REST route both predate this; the TUI simply had
    /// no key that reached them, so the one client that could resolve a
    /// conflict was the web app. Driving it through [`GitPane::run_key`] tests
    /// the path a keystroke *and* a click on the footer both take.
    /// Which verbs a conflicted row offers is the client's question now, and
    /// [`butai_core::verbs`] answers it there. What stays here is the half a
    /// client cannot do for itself: taking a side settles the index.
    #[test]
    fn taking_a_side_settles_a_conflict() {
        let dir = conflicted_repo();
        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane);
        assert_eq!(pane.conflict_count(), 1, "{:?}", rows_text(&pane));

        pane.resolve_path("a.txt", butai_protocol::api::ResolveSide::Theirs).unwrap();
        assert!(!pane.repo().unwrap().index().unwrap().has_conflicts(), "still unmerged");
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "theirs\n");

        refresh(&mut pane);
        assert_eq!(pane.conflict_count(), 0);
        let rows = rows_text(&pane);
        assert!(!rows.iter().any(|r| r.starts_with("X:")), "conflict row survived: {rows:?}");
    }

    /// Upstream tracking is reported without ever touching the network: a
    /// remote-tracking ref plus the branch config is all `branch_upstream_name`
    /// and `graph_ahead_behind` need, which is what makes ahead/behind cheap
    /// enough to ride the ordinary status tick.
    #[test]
    fn ahead_and_behind_are_counted_against_the_upstream() {
        let (dir, repo) = setup_repo();
        commit_on_head(dir.path(), &repo, "a.txt", "one\n", "one");
        let branch = repo.head().unwrap().shorthand().unwrap().to_string();
        let base = repo.head().unwrap().target().unwrap();

        // Stand in for `origin`: a remote (whose URL is never contacted — it
        // exists so `set_upstream` can resolve a fetch refspec), a
        // remote-tracking ref at the current commit, and a branch tracking it.
        repo.remote("origin", &format!("file://{}", dir.path().display())).unwrap();
        repo.reference(&format!("refs/remotes/origin/{branch}"), base, true, "test").unwrap();
        {
            let mut b = repo.find_branch(&branch, git2::BranchType::Local).unwrap();
            b.set_upstream(Some(&format!("origin/{branch}"))).unwrap();
        }

        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane);
        assert_eq!(pane.to_dto().upstream.as_deref(), Some(format!("origin/{branch}").as_str()));
        assert_eq!(pane.ahead_behind(), (0, 0), "in step with the upstream");

        // Two local commits the upstream has not seen.
        commit_on_head(dir.path(), &repo, "a.txt", "two\n", "two");
        commit_on_head(dir.path(), &repo, "a.txt", "three\n", "three");
        refresh(&mut pane);
        assert_eq!(pane.ahead_behind(), (2, 0));

        let dto = pane.to_dto();
        assert_eq!((dto.ahead, dto.behind), (2, 0));
        assert!(!dto.detached);
    }

    /// A clean repository reports a clean state and no conflict section, so the
    /// rail looks exactly as it did before conflicts were modelled.
    #[test]
    fn a_clean_repo_grows_no_conflict_section() {
        let (dir, repo) = setup_repo();
        commit_file(dir.path(), &repo, "a.txt", "one\n");
        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane);

        assert!(!rows_text(&pane).contains(&"H:Conflicts".to_string()));
        assert_eq!(pane.conflict_count(), 0);
        assert_eq!(pane.state(), RepoState::Clean);
        assert_eq!(pane.ahead_behind(), (0, 0), "no upstream means no divergence");
        assert!(pane.to_dto().upstream.is_none());
    }

    /// An unmerged file must read as conflicted whatever else git says about
    /// it. It used to be the last arm, so a conflict that also carried a
    /// worktree bit showed as an ordinary `M` — and staging one of those
    /// commits half a merge.
    #[test]
    fn a_conflicted_file_reads_as_conflicted_first() {
        assert_eq!(worktree_code(Status::CONFLICTED), Some('!'));
        assert_eq!(worktree_code(Status::CONFLICTED | Status::WT_MODIFIED), Some('!'));
        assert_eq!(worktree_code(Status::CONFLICTED | Status::WT_NEW), Some('!'));
        // Unchanged for everything else.
        assert_eq!(worktree_code(Status::WT_MODIFIED), Some('M'));
        assert_eq!(worktree_code(Status::WT_NEW), Some('?'));
        assert_eq!(worktree_code(Status::INDEX_MODIFIED), None);
    }

    #[test]
    fn stage_commit_flow() {
        let (dir, _repo) = setup_repo();
        std::fs::write(dir.path().join("a.txt"), "hello\n").unwrap();

        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane); // new() defers the scan; tests need it populated now
        assert!(rows_text(&pane).contains(&"U:?:a.txt".to_string()), "{:?}", rows_text(&pane));

        // Stage it. `stage_path` moves the row itself so the next `to_dto` is
        // right; the authoritative rows catch up when the off-thread rescan
        // lands, which the core schedules separately.
        pane.stage_path("a.txt").unwrap();
        assert_eq!(row_code(&pane, "a.txt"), Some('A'), "row not moved: {:?}", rows_text(&pane));
        refresh(&mut pane);
        assert!(rows_text(&pane).contains(&"S:A:a.txt".to_string()), "{:?}", rows_text(&pane));

        let (files, adds, _dels) = pane.staged_summary();
        assert_eq!(files, 1);
        assert!(adds >= 1, "diffstat missing: {adds}");
        pane.commit_with("first commit").unwrap();
        refresh(&mut pane);
        let rows = rows_text(&pane);
        assert!(rows.contains(&"C:first commit".to_string()), "{rows:?}");
        assert!(rows.contains(&"E:(clean)".to_string()), "{rows:?}");
        assert!(pane.commit_with("   ").is_err(), "empty message must fail");
    }

    #[test]
    fn stage_all_commits_every_change_at_once() {
        let (dir, repo) = setup_repo();
        // A committed file we delete, plus two new files: stage_all must handle
        // adds and a delete ('D') in one pass.
        commit_file(dir.path(), &repo, "old.txt", "keep\n");
        std::fs::remove_file(dir.path().join("old.txt")).unwrap();
        std::fs::write(dir.path().join("a.txt"), "aaa\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "bbb\n").unwrap();

        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane); // new() defers the scan; tests need it populated now

        // One index write covers all three, deletion included.
        assert_eq!(pane.stage_all().unwrap(), 3);
        refresh(&mut pane);
        assert_eq!(pane.staged_summary().0, 3, "all staged: {:?}", rows_text(&pane));

        pane.commit_with("all at once").unwrap();
        refresh(&mut pane);
        let rows = rows_text(&pane);
        assert!(rows.contains(&"C:all at once".to_string()), "{rows:?}");
        assert!(rows.contains(&"E:(clean)".to_string()), "{rows:?}");
    }

    /// A clean tree stages nothing, so a client asking to "stage all and
    /// commit" can tell there is nothing to commit rather than making an empty
    /// one. The count is the whole signal — the rail used to keep a private
    /// "nothing to commit" notice that no other client could read.
    #[test]
    fn stage_all_on_a_clean_tree_stages_nothing() {
        let (dir, repo) = setup_repo();
        commit_file(dir.path(), &repo, "a.txt", "one\n");
        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane);
        assert_eq!(pane.stage_all().unwrap(), 0);
        assert_eq!(pane.staged_summary().0, 0);
    }

    #[test]
    fn unstage_returns_file_to_worktree() {
        let (dir, repo) = setup_repo();
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        // Initial commit so unstage has a HEAD to reset against.
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();

        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane); // new() defers the scan; tests need it populated now
        pane.stage_path("a.txt").unwrap();
        refresh(&mut pane);
        assert!(rows_text(&pane).contains(&"S:M:a.txt".to_string()));

        pane.unstage_path("a.txt").unwrap();
        refresh(&mut pane);
        let rows = rows_text(&pane);
        assert!(rows.contains(&"U:M:a.txt".to_string()), "{rows:?}");
        assert!(rows.contains(&"E:(nothing staged)".to_string()), "{rows:?}");
    }

    /// Commit `a.txt` with `contents` so tests have a HEAD to restore from.
    fn commit_file(dir: &Path, repo: &Repository, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(name)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
    }

    #[test]
    fn discard_restores_tracked_file_from_the_index() {
        let (dir, repo) = setup_repo();
        commit_file(dir.path(), &repo, "a.txt", "one\n");
        std::fs::write(dir.path().join("a.txt"), "wrecked\n").unwrap();

        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane); // new() defers the scan; tests need it populated now
        assert!(rows_text(&pane).contains(&"U:M:a.txt".to_string()));

        pane.discard_path("a.txt").unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "one\n");
        refresh(&mut pane);
        let rows = rows_text(&pane);
        assert!(rows.contains(&"E:(clean)".to_string()), "{rows:?}");
    }

    #[test]
    fn discard_recreates_a_deleted_file() {
        let (dir, repo) = setup_repo();
        commit_file(dir.path(), &repo, "a.txt", "one\n");
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();

        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane); // new() defers the scan; tests need it populated now
        assert!(rows_text(&pane).contains(&"U:D:a.txt".to_string()));
        pane.discard_path("a.txt").unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "one\n");
    }

    #[test]
    fn discard_deletes_an_untracked_file() {
        let (dir, _repo) = setup_repo();
        std::fs::write(dir.path().join("junk.txt"), "scratch\n").unwrap();

        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane); // new() defers the scan; tests need it populated now
        assert!(rows_text(&pane).contains(&"U:?:junk.txt".to_string()));
        pane.discard_path("junk.txt").unwrap();
        assert!(!dir.path().join("junk.txt").exists(), "untracked file still on disk");
    }

    #[test]
    fn discard_leaves_staged_work_alone() {
        let (dir, repo) = setup_repo();
        commit_file(dir.path(), &repo, "a.txt", "one\n");
        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();

        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane); // new() defers the scan; tests need it populated now
        pane.stage_path("a.txt").unwrap();
        refresh(&mut pane);
        assert!(rows_text(&pane).contains(&"S:M:a.txt".to_string()));

        // Staged-only: there is nothing to discard and the file is untouched.
        assert!(pane.discard_path("a.txt").is_err(), "staged rows must not be discardable");
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "two\n");

        // Staged, then modified again: discard rewinds to the *staged* version.
        std::fs::write(dir.path().join("a.txt"), "three\n").unwrap();
        refresh(&mut pane);
        pane.discard_path("a.txt").unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "two\n");
        refresh(&mut pane);
        assert!(rows_text(&pane).contains(&"S:M:a.txt".to_string()), "staged row lost");
    }

    /// Discarding is unrecoverable, so it is never a side effect of asking for
    /// something else. Confirming it is the client's job; refusing a path that
    /// is not an unstaged change is this side's — and a *clean tracked* file is
    /// the case that bites, because the untracked branch of `discard_path`
    /// deletes what it is given. A path with no row must not reach it.
    #[test]
    fn discard_refuses_a_file_with_nothing_to_discard() {
        let (dir, repo) = setup_repo();
        commit_file(dir.path(), &repo, "a.txt", "one\n");
        let mut pane = GitPane::new(dir.path()).unwrap();
        refresh(&mut pane); // new() defers the scan; tests need it populated now
        assert!(rows_text(&pane).contains(&"E:(clean)".to_string()), "{:?}", rows_text(&pane));

        assert!(pane.discard_path("a.txt").is_err(), "a clean file is not discardable");
        assert!(dir.path().join("a.txt").exists(), "a clean tracked file was deleted");
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "one\n");
    }

    #[test]
    fn not_a_repo_errors() {
        let dir = tempfile::tempdir().unwrap();
        assert!(GitPane::new(dir.path()).is_err());
    }

    /// A directory is marked exactly when a changed file sits below it, so the
    /// `\u25cf` on a folder is a promise that following it arrives somewhere.
    ///
    /// The set is closed over ancestors precisely so this is one lookup rather
    /// than a scan of the whole change set per entry.
    #[test]
    fn marked_closes_over_every_ancestor_up_to_the_root() {
        let root = PathBuf::from("/w");
        let m = Marked::build([PathBuf::from("/w/a/b/c.rs")].into_iter(), &root);

        for p in ["/w/a/b/c.rs", "/w/a/b", "/w/a", "/w"] {
            assert!(m.contains(Path::new(p), api::TreeFilter::All), "{p} should be marked");
        }
        // Nothing above the worktree, and no sibling.
        assert!(!m.contains(Path::new("/"), api::TreeFilter::All));
        assert!(!m.contains(Path::new("/w/a/d"), api::TreeFilter::All));
    }

    /// The bug this whole parameter exists for: a `.rs` marked its parents on
    /// the DOCS rail, which then filtered the `.rs` out of the listing, so the
    /// trail of markers ended in an empty box.
    #[test]
    fn a_code_file_marks_nothing_under_the_docs_filter() {
        let root = PathBuf::from("/w");
        let m = Marked::build([PathBuf::from("/w/src/chrome/mod.rs")].into_iter(), &root);

        assert!(m.contains(Path::new("/w/src"), api::TreeFilter::All), "still marked unfiltered");
        for p in ["/w/src/chrome/mod.rs", "/w/src/chrome", "/w/src", "/w"] {
            assert!(!m.contains(Path::new(p), api::TreeFilter::Docs), "{p} must not be marked");
        }
    }

    /// And the other half: writing still marks the whole way up, or the DOCS
    /// rail would have no markers at all.
    #[test]
    fn writing_marks_its_ancestors_under_the_docs_filter() {
        let root = PathBuf::from("/w");
        let m = Marked::build([PathBuf::from("/w/docs/design.md")].into_iter(), &root);

        for p in ["/w/docs/design.md", "/w/docs", "/w"] {
            assert!(m.contains(Path::new(p), api::TreeFilter::Docs), "{p} should be marked");
        }
    }

    /// A directory the filter drops breaks the chain above it too — otherwise
    /// `target/` full of generated markdown would light up the root.
    #[test]
    fn a_dropped_directory_stops_the_docs_chain() {
        let root = PathBuf::from("/w");
        let m = Marked::build([PathBuf::from("/w/target/doc/README.md")].into_iter(), &root);

        assert!(m.contains(Path::new("/w/target"), api::TreeFilter::All));
        assert!(!m.contains(Path::new("/w"), api::TreeFilter::Docs), "root must stay clean");
        assert!(!m.contains(Path::new("/w/target"), api::TreeFilter::Docs));
    }
}
