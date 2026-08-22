# Git

butai carries a working copy of git around with it because an agent workbench is
mostly a machine for producing diffs. There are two surfaces, and the split
between them is the design:

- **The CHANGES rail** — the working tree *right now*. What changed, stage it,
  commit it. It sits on the workbench beside the agents doing the changing and
  it is visible on every page.
- **The GIT space** (`alt-r`) — the repository *across branches and over time*.
  Branches, remotes, tags, stashes, worktrees over a commit graph, with whatever
  the cursor names shown as a diff.

Nothing on the GIT space stages anything, and `enter` there only ever reads.
That rule is asserted in the web client (`invariant/git-page-does-not-stage`,
`invariant/git-enter-reads`) rather than merely intended.

Underneath both, the daemon runs **two engines** and the line between them is
sharp:

| | engine | answers |
|---|---|---|
| status, stage, unstage, discard, commit, checkout, branch create/delete/rename, resolve, **apply** | libgit2 | synchronously |
| fetch, pull, push, stash, merge, rebase, cherry-pick, revert, reset, amend, tag, worktree, remote | the real `git` binary | `200` or `202`, then events |

The reason is not taste. butai's libgit2 is built with `default-features =
false`, so **no network transport is compiled into the daemon at all** — it
*cannot* reach a remote. And your remotes, `push.default`, credential helpers,
ssh-agent, hooks, signing config and sequencer state all live in your git
config, which only `git` itself reads correctly.

[protocol.md](protocol.md) owns the request and response shapes for every route
named here. This page owns the model and the workflow.

## The working-tree model

butai presents four sections, in this order:

```
Conflicts        (omitted entirely when there are none)
Unstaged
Staged
Commits
```

They map onto git exactly as you would expect, with one deliberate departure: a
**conflicted file appears in its own section and in neither of the others**.
Half of an unmerged file is in the index by definition, so listing it as
"unstaged" would offer you `s`, and staging a conflict commits the `<<<<<<<`
markers. It gets its own section and its own verbs (`o` ours, `t` theirs, `a`
resolved).

A file that is modified *and* has staged changes appears **twice** — once in
each section, with its own status code and its own diff. That is not a bug; it
is what `git status` says, and the two halves are separately stageable.

### Status codes

| code | unstaged means | staged means |
|---|---|---|
| `?` | untracked | — (becomes `A` once staged) |
| `A` | — | added to the index |
| `M` | modified in the worktree | modified in the index |
| `D` | deleted from the worktree | deletion staged |
| `R` | renamed | renamed |
| `T` | type changed (file ↔ symlink) | type changed |

Being **conflicted wins over everything else**: if libgit2 reports a file as
both conflicted and modified, butai reads it as conflicted first. That ordering
is load-bearing — it used to be the last arm, and a conflict carrying a worktree
bit showed as an ordinary `M`.

Each row also carries a `+n -n` line count, computed from a diff of that side of
the tree. An untracked file counts as every one of its lines added, wherever it
sits — that diff reads untracked content *and* recurses into untracked
directories, or a new file in a new directory would be a row with `+0` on it.

### What is not in the model

- **Ignored files.** The status scan asks for untracked files and nothing else,
  so `.gitignore`d paths never appear in CHANGES. They *do* appear in the FILES
  tree, which is an ordinary directory listing (minus `.git`).
- **Renames.** butai does not turn on libgit2's rename detection, so whether a
  move shows as `R` or as a delete plus an untracked add is git's own default,
  not something butai configures.
- **Submodules.** There is no submodule section, no recursion into a
  submodule's own status, and no submodule-aware verb. A gitlink is whatever
  `git status` and `git diff` say it is.
- **Binary files.** `git diff` answers with `Binary files … differ` and no
  hunks. The diff view shows that header; there is nothing to put a cursor on,
  and asking to stage it reports `nothing to apply`. Stage the whole file with
  `s` instead.

### Paths are relative to the worktree root

Status paths are relative to the **repository root**, not to the workspace's
directory. A workspace opened at `repo/crates/foo` still sees its changes
reported as `crates/foo/src/lib.rs`. The daemon caches that root, spelled the
way the client spelled the cwd — libgit2 canonicalizes (`/var` becomes
`/private/var` on macOS) and two spellings of one directory compare unequal,
which silently unmarks every changed file in the tree.

## How status is computed and refreshed

A status scan is a **full worktree walk**. On a large repository or a network
mount it is slow enough to freeze anything it runs on, so:

- It runs **off the daemon's event loop**, on a blocking thread, and the result
  is applied when it lands.
- Every field a client reads — branch name, upstream, ahead/behind, repository
  state, worktree root — is filled by that scan and **never touched again until
  the next one**. Resolving them on demand meant a `Repository::discover` per
  read per client, which is the difference between a workbench and a frozen one.
- A scan is requested on the daemon's **~2s sampler tick**, so the rail tracks
  edits made outside butai — by an agent, by your editor, by a script.
- A scan is also requested after every mutation and after every git operation
  finishes (a fetch moves the upstream; a push moves it back).
- Requests are **deduplicated but not dropped**. If one arrives while a scan is
  running, it is deferred and run again when that scan lands — because the
  running scan started before whatever prompted the new request, so its answer
  is already stale.

A workspace that is not a repository is probed again on the same tick with
**exponential backoff, 2s doubling to 60s**, so a `git init` or a clone landing
in the cwd grows a CHANGES rail without reopening the workspace.

### Optimistic rows

Because the rescan is asynchronous, mutations **move their own rows first**.
Staging a file moves it from Unstaged to Staged immediately (and rewrites `?`
to `A`); committing clears the staged section and inserts the new commit at the
top of Commits. The authoritative rows arrive with the next scan. Without this,
a `GET .../changes` issued right after a stage would answer with the tree from
before it.

While the scan has never run, the rail reads `(loading…)` with a branch of `…`.
If the repository cannot be opened it reads `repository unavailable`; if the
status call itself fails it reads `status failed` and the reason goes to the
daemon log — `ChangesDto` has no slot for a notice.

## Staging, unstaging, discarding

### By file

| key | route | what it does |
|---|---|---|
| `s` | `POST …/changes/stage` | `index.add_path`, or `remove_path` for a `D` row |
| `u` | `POST …/changes/unstage` | `reset_default` against HEAD; `index.remove_path` in a repository with no commits |
| `x` | `POST …/changes/discard` | untracked: delete the file (or the whole directory). Tracked: `checkout_index --force` for that one path |
| `C` | `POST …/changes/commit-all` | stage everything, then commit, in one index write |

`x` is the only rail key that confirms, and the box opens with **no** selected.
It throws away the only copy of an edit; killing an agent does not, which is why
that one does not ask. A file that is *also* staged keeps its staged version —
discard restores from the index, not from HEAD.

The daemon refuses a discard for a path that is not an unstaged row
(`no unstaged file "…"`). That guard is what stops a *clean tracked* file from
reaching the untracked branch, which deletes what it is given.

### By hunk and by line

There is no "stage this hunk" verb. **A hunk is not a thing the daemon can name
back to you**, so the client sends a *patch*:

```
POST …/git/apply  {patch, target: "index"|"worktree", reverse?}
```

One route covers four operations:

| operation | target | reverse |
|---|---|---|
| stage a hunk | `index` | no |
| unstage a hunk | `index` | yes |
| discard a hunk | `worktree` | yes |
| stage selected lines | `index` | no |

The patch is built client-side from the diff the client already has, by
`butai-protocol`'s `hunk` module — which lives in the *protocol* crate precisely
because the patch text is the wire format, and a copy of the `@@` arithmetic on
each side is two things that drift.

The rules that make a partial hunk valid, all of them:

- a **selected** `+` or `-` stays as it is;
- an **unselected `+`** is dropped — it is not going into the index and it is
  not in the file being applied to either;
- an **unselected `-` becomes context** — it *is* in the file being applied to,
  so it must be matched, just not removed;
- context is always kept;
- `\ No newline at end of file` travels with the line it annotates, and survives
  exactly when that line did;
- every `@@` count is **recomputed from the body**, never copied, and later
  hunks in the same file are re-offset as earlier ones shrink.

Selecting no lines is refused rather than sent: an all-context patch applies
cleanly and does nothing, which reads to the caller as a silent success.

**When the file changes underneath you**, the apply fails — the patch's context
lines no longer match — and the route answers `400 apply: …` with libgit2's own
message. The daemon recomputes nothing and retries nothing. The client's answer
is to re-fetch the diff: after every successful apply it refreshes the patch,
because staging a hunk removes it from the unstaged diff and the cursor would
otherwise be pointing into a file that has changed shape.

## The diff view

`d` on a CHANGES row, `enter` on a commit, or the body column of the GIT space —
all three are the same renderer, deliberately, so two diff views cannot drift in
their colours, their gutter or their hint row.

What it shows:

- A **view over the parsed patch**, not a list of strings. The rows on screen,
  the hunk under the cursor and the subset that gets staged are all the same
  parse, so the hunk you are looking at is the hunk that moves.
- `]` and `[` walk hunks across file boundaries. `space` takes the one under the
  cursor. `v` drops into line-select, where `space` picks a line and advances
  (so a run is space-space-space) and `enter` applies what is picked. `x`
  discards a hunk. `r` refreshes, `q` or `esc` closes.
- On a **staged** diff the same keys unstage, and `x` is not offered —
  reverse-applying a staged hunk to the worktree would undo an edit you are
  looking at from the other side.
- A **commit's** diff is history and offers none of it: there is no index side
  to move it to.

### Untracked files

`git diff` compares the index against the worktree, and an untracked file is on
neither side of that — so left to git a brand new file has no diff at all, which
is exactly the file you most want to read before staging it. The daemon diffs
each `?` row against `/dev/null` instead, which produces the same `new file
mode` patch that file will have the moment it *is* staged. It is a real patch,
so `space` on one of its hunks stages that hunk.

Ignored paths stay out (`--exclude-standard`), so what a diff adds is exactly
the set of `?` rows CHANGES shows. A whole-section diff prints at most **200**
untracked files: each one costs its own `git diff`, and a tree that has never
been committed would otherwise return the whole worktree as a single patch.
CHANGES still lists every one of them, and asking for one by name always works.

### Colour and the gutter

The diff is tinted **by marker only**: `+` in the ok colour, `-` in the danger
colour, `@@` in the accent colour, everything else plain. There is no syntax
highlighting of the diff's *content*. butai's highlighter
(`crates/butai-client/src/syntax.rs`) is a small dependency-free tokenizer —
comments, strings, numbers, keywords, capitalized types — and it is wired to the
**file editor**, which picks a language from the filename. The daemon used to run
diffs through syntect's diff grammar, and the marker tint is the whole of what
that produced.

The one-column gutter carries what colour cannot: `>` on the cursor row, `|` on
every row of the cursor's hunk, `#` on a picked line. The view is already three
colours deep and a fourth would not read.

### Whole-commit diffs

`GET …/show?id=` runs `git show --stat -p --first-parent`. `--first-parent`
matters: without it a merge commit is diffed against *all* its parents, a clean
merge differs from none of them, and the useful reading — what this merge brought
onto the branch — is the one nobody can get.

Reflog spellings are revisions here (`stash@{0}`, `main@{upstream}`, `HEAD@{2}`),
which is how a stash list shows a diff. A `:` is refused: `<rev>:<path>` reads a
file out of a tree, and that is `GET …/file`'s job.

## Committing

`c` opens a message prompt whose subtitle says what is about to be committed —
`3 staged file(s)`, or `nothing staged — this will fail`. `C` is the same prompt
for **stage-all-and-commit**, and its subtitle counts staged and unstaged
together.

What is committed is **the index**, exactly. The daemon writes the index tree,
takes `repo.signature()` for both author and committer, parents itself on HEAD
(or makes a root commit when there is none), and returns the short id.

Refusals, before anything is written:

- an empty or whitespace-only message → `empty commit message`;
- any conflicted file → `resolve the conflicts first`. libgit2 would refuse
  anyway, but its wording ("cannot create a tree from a not fully merged index")
  says nothing about what to do next;
- `commit-all` with nothing staged and nothing to stage → `nothing to commit`,
  so a client can tell that apart from an empty commit.

**No editor is ever opened.** The daemon has no terminal to prompt on, so every
git subcommand it runs carries `GIT_EDITOR=true` and `GIT_SEQUENCE_EDITOR=true`.
An amend without a message keeps the old one (`commit --amend --no-edit`).

Commit *signing* is your git config's business and works if it works from a
shell — but a signing key that wants a passphrase will hang and be killed by the
idle watchdog, because there is nothing to type into.

## Branches

`GET …/branches` answers with local branches (**current first**) and an
`entries` list that adds remote-tracking branches, each with its upstream, its
tip oid, and how far it has drifted. `origin/HEAD` is omitted — it is a symbolic
ref onto another row in the same list, and listing it shows the default branch
twice under two names.

| action | route | notes |
|---|---|---|
| switch | `POST …/checkout` `{branch}` | git's safe strategy: a switch that would clobber uncommitted work fails loudly |
| create and switch | `POST …/checkout` `{branch, create: true}` | branches from where you are now |
| create only | `POST …/git/branch` `{name, from?}` | `from` defaults to HEAD |
| rename | `POST …/git/branch/rename` `{from?, to}` | `from` defaults to the current branch |
| delete | `DELETE …/git/branch?name=&force=` | |

Deleting refuses the **current** branch, and refuses an unmerged one without
`force` — libgit2 itself allows that, so the check is butai's, on the grounds
that losing commits to a keystroke deserves a reason to confirm about.

On the GIT space, a branch **already checked out in another worktree** does not
offer `c checkout`: git refuses to check one out twice, so the row says where it
went instead of advertising a failure. The cross-reference comes from the
worktree list, not from a second question.

Every branch name is validated against git's own rules before it becomes an
argument — no `..`, no `@{`, no `//`, no leading or trailing `/`, no trailing `.`
or `.lock`, no dot-leading component, none of `` ~^:?*[\ `` or a space, and
**never a leading `-`**. That last one is not pedantry: `--upload-pack=<cmd>` is
an argument-position option that runs a command, so a branch name starting with
`-` is remote code execution.

## Worktrees

A worktree is a second checkout of the same repository on another branch. butai's
model is already **one workspace per directory**, so a worktree *is* a workspace:
opening one gives it its own agents, its own processes, its own CHANGES rail and
its own branch, with no stashing and no switching. That is the row the Worktree
menu group exists for; the rest of it is upkeep.

| menu row | what happens |
|---|---|
| `g w l` open worktree… | lists every checkout; choosing one does `POST /v1/workspaces {path}` — a worktree is a directory, so opening it needs no worktree-shaped route |
| `g w n` new worktree… | prompts for a **branch name**; the checkout lands beside this one |
| `g w x` remove worktree… | picker, then confirm |
| `g w p` prune gone worktrees | forgets checkouts whose directories are gone |

**Where a new one lands**: sibling of the current workspace, named after the
branch with `/` replaced by `-`. `/code/proj` on branch `spike` becomes
`/code/proj-spike`. A worktree *inside* the repository would be a directory git
then has to be told to ignore.

`GET …/git/worktrees` reports, for each checkout, the **butai workspace already
open on it** (or `null`), compared canonically so a symlinked path is not
mistaken for a different directory. That is what lets a client offer "go there"
rather than "open it again" — two workspaces on one worktree means one tree with
two CHANGES rails.

Rows are labelled from `git worktree list --porcelain`: `[current]` for the main
worktree (identified by position — git marks it no other way), `[locked]`,
`[gone]` for prunable, and `detached at abc1234` where there is no branch. The
main worktree is never removable.

Removal needs `force` when the worktree is dirty or locked. A worktree path must
be **absolute** (`worktree path must be absolute: …`) — a relative one would
resolve against the daemon's cwd rather than yours.

## Remotes and sync

The rail's title carries the three facts that change what you would do next:

```
 CHANGES (3) · main             on main, level with its upstream
 CHANGES (3) · main ↑2↓1        ahead 2, behind 1
 CHANGES (3) · main · REBASING  a sequence is running — it displaces the arrows
```

The branch is the part that gives way when the rail is narrow: it is cut to
whatever the counts and the arrows leave, and dropped altogether once fewer than
six characters of it would get through — the arrows are the half that says
whether you can push, so they are never the half that is cut.

Ahead/behind come from a revwalk against the branch's configured upstream, run
inside the same off-thread scan, and are **capped at 999**. No network is
touched: a remote-tracking ref plus the branch config is all it takes, which is
what makes the number cheap enough to ride the ordinary status tick. It is
therefore as fresh as your last fetch and no fresher.

| menu row | call |
|---|---|
| `g r f` fetch --prune | `{all: true, prune: true}` |
| `g r l` pull | `{}` |
| `g r r` pull --rebase | `{rebase: true}` |
| `g r p` push | `{}` |
| `g r u` push --set-upstream | `{set_upstream: true}` |
| `g r F` push --force-with-lease | confirmed first |
| `g r x` remove a remote… | picker |

`p` on the CHANGES rail is bound **only when there is something to push** — a
key that silently does nothing is worse than no key.

**Exactly one route accepts a URL**, and it is `POST …/git/remote`. Everywhere
else a remote is *named* and resolved through your git config; any string
containing `:` is rejected before a command line is built, because
`git fetch 'ext::sh -c whoami'` runs a shell. The URL route validates against an
**allowlist of transports** — `https`, `http`, `ssh`, `git`, `file`, `git+ssh`,
an absolute path, and scp-style `user@host:path` — because the set of installed
`git-remote-*` helpers is a property of the machine and cannot be enumerated
from this side. Every operation additionally runs with
`-c protocol.ext.allow=never`.

### How an operation runs

1. **One writer per repository**, keyed by worktree root rather than by
   workspace — two workspaces can be open on one worktree and interleaving their
   index writes loses work. While an operation runs, index mutations answer
   `409 a git operation is already running: <kind>`. Reads and status refreshes
   carry on.
2. Arguments are built by a **pure function** and validated before anything is
   spawned. A refusal here is a `400` and nothing ran.
3. The child is a `tokio::process`, so it can be raced against a timer and
   killed. `git push`'s `\r`-separated progress is parsed and re-emitted as a
   `git_op` event, throttled to one per 250ms — unthrottled it is tens of events
   a second to every subscriber, for text nobody can read at that rate.
4. If it finishes within **300ms** the route answers `200`; otherwise `202` and
   the outcome arrives on the `git_op` event. **A client must handle both** —
   which one it gets depends only on whether the operation beat the grace
   window, so the same call answers differently on different days.
5. **Check `ok`.** A rejected push is a successful call reporting a failed
   operation, not a 4xx. The `summary` is git's own closing line (stderr
   preferred — that is where "Everything up-to-date" and every rejection reason
   goes); on failure it is the last four lines of the tail, joined with `; `.
6. `DELETE …/git/op` cancels whatever is running.

**Nothing ever hangs.** `GIT_TERMINAL_PROMPT=0`, `GIT_ASKPASS=/bin/false`,
`SSH_ASKPASS=/bin/false`, `SSH_ASKPASS_REQUIRE=never` and `core.askPass=` cleared
in-process, so anything needing a credential fails immediately rather than
waiting for an answer that cannot come. An operation silent for **120s**, or
running for **600s**, is killed. `GIT_SSH_COMMAND` is deliberately *not* set:
forcing `BatchMode=yes` would override your own `core.sshCommand`, and silently
ignoring `ssh -i ~/.keys/work` is a worse failure than the one it prevents.

**This means a repository whose remote needs an interactive passphrase cannot be
pushed from butai.** Use an agent, a credential helper, or a shell.

## Integrate — merge, rebase, and being stuck

`g i m` merges a branch (`--no-edit`, optional `--no-ff`), `g i r` rebases onto
one. Interactive rebase is refused outright: there is no editor to drive the todo
list.

When a merge, rebase, cherry-pick or revert **stops on a conflict**, three things
change at once:

- `ChangesDto.state` becomes `merge` / `rebase` / `cherry_pick` / `revert`, and
  the rail's title says so in words (`· REBASING`).
- The conflicted files appear in the Conflicts section with which of the three
  merge stages each one still has — base, ours, theirs. A both-modified conflict
  has all three; a delete/modify is missing one; a both-added has no base. That
  is what lets a client know whether "take ours" can even be offered.
- **The `g` menu collapses to the way out.** Mid-sequence it shows only
  Integrate, and only `continue`, `abort` and `skip`. Nothing else can be
  started — git refuses most of it, and the rest would tangle the sequence
  further. Hiding rather than disabling: a menu of mostly-dead rows is harder to
  read than a short one.

Settling a file is three keys on the row — `o` ours, `t` theirs, `a` resolved
(you edited it by hand and the markers are gone). Taking a side writes that
stage's blob to disk (or deletes the file, when that side deleted it) and then
`git add`s the path, which is what clears the conflict from the index. It is
libgit2, not the operation runner: taking a side is two git invocations, the
runner runs one, and none of it touches config, credentials or the network — so
it answers synchronously, which for a millisecond of index work is the honest
reply.

`continue` / `abort` / `skip` go through one route (`POST …/git/sequence`) and
the **daemon picks the subcommand** from the repository's state, because a user
only ever means "carry on" or "back out". `abort` confirms first: it discards
whatever was resolved. A merge has nothing to skip and says so. `bisect` is
recognised but not driven from here; anything else answers `git is in a state
butai does not recognise — use a shell`.

`ChangesDto.state` also has an `unknown` value. Treat it as "something is in
progress that this client does not model" — never as clean.

## The commit graph

`GET …/git/log` returns a page of history with each commit's **parents** (first
parent first) and its **refs** (`head` / `branch` / `remote` / `tag`, parsed from
`--decorate=full` so a tag and a branch of the same name are told apart). The
walk is always `--topo-order`, so a parent never precedes its child.

`?all=1` walks every branch, tag and remote-tracking branch, **plus `HEAD`** so a
detached checkout still shows the commit you are standing on. It is deliberately
*not* `git log --all`: `refs/stash` and `refs/notes` stay out, because a stash is
two synthetic commits that are not history and would sit in the graph as if
someone had committed them. `all` and `rev` together are a `400` — they name
different walks.

Lanes are assigned client-side, in one pass down the page:

- **One row per commit**, always. `git log --graph` emits connector rows between
  commits, which is prettier and makes the list stop being a list — the cursor
  indexes commits, so a non-commit row is one the cursor must skip and every
  "diff what I am on" is off by the number of connectors above it. Joins and
  forks are drawn *on* the commit's row, beside its node.
- Every lane waiting for a commit converges on it; the **leftmost** is where the
  node goes, so a long-running branch keeps its column instead of drifting right
  each time something merges in.
- The **first parent inherits the node's lane**, which is what makes a branch a
  straight line down the page rather than a staircase.
- Glyphs are `●` for a commit, `◆` for a merge, `│ ╯ ╮ ─` for the edges. No
  arrows — those are East-Asian-ambiguous width and render two cells wide in some
  terminals, which shifts every cell after them.
- Past the lane limit the row is marked `…` rather than truncated silently:
  silent truncation reads as "this repository has six branches", which is a
  different and wrong statement.
- A page with **no parents at all** — an older daemon — degrades to the plain
  list of dots the page had before the graph existed.

## What butai deliberately does not do

Drop to a shell (`t` on PROCESSES) for all of these:

- **Interactive rebase.** Refused: no editor for the todo list.
- **`git add -i`, `git rebase --edit-todo`, `git bisect`.** Bisect state is
  recognised so butai does not lie about it, and driven nowhere.
- **Anything needing a credential prompt or a signing passphrase.** See above.
- **`git clone`.** There is no route. Clone in a shell, then open the directory
  as a workspace — the CHANGES rail attaches on its own within a couple of
  seconds.
- **Reflog, blame, bisect, notes, sparse checkout, LFS, submodule commands.**
  Not modelled at any layer.
- **Reading a file out of a tree** (`git show <rev>:<path>`). Refused by the
  `show` validator on purpose; `GET …/file` reads files.
- **Configuring git.** butai reads your config and never writes it. The one
  exception is `POST …/git/remote`, which is `git remote add`.

## Failure modes, and what you see

Refusals that happen **before anything runs** (`400`):

| message | cause |
|---|---|
| `no unstaged file "x"` / `no staged file "x"` | the path is not a row on that side — including a clean tracked file |
| `x is not conflicted` | resolve on a row that is not in Conflicts |
| `no conflict recorded for x` | the index has no unmerged entry for it |
| `empty commit message` | whitespace-only message |
| `resolve the conflicts first` | commit with anything unmerged |
| `nothing to commit` | `commit-all` with an entirely clean tree |
| `empty branch name` / `no branch "x"` | checkout or delete of a name that is not there |
| `x is the current branch` | deleting the branch you are on |
| `x is not merged — delete with force to discard it` | unmerged delete without `force` |
| `HEAD is detached — name the branch to rename` | bare rename on a detached HEAD |
| `no such revision "x"` | `git/branch` with an unresolvable `from` |
| `invalid branch name "x": contains '..'` (and the rest of git's rules) | ref-name validation |
| `branch name may not start with '-': "x"` | the option-injection guard |
| `remote must be a configured name, not a URL: "x"` | a URL where a remote name belongs |
| `refusing a transport helper "ext" in "ext::sh -c …"` | helper form in `remote add` |
| `unsupported remote url "x"` | outside the transport allowlist |
| `worktree path must be absolute: "x"` | relative worktree path |
| `a new worktree branch needs a name` | `new_branch` with no branch |
| `--set-upstream needs a remote` | nothing to record the upstream against |
| `?all= and ?rev= name different walks` | both passed to `git/log` |
| `path escapes workspace` / `path escapes the repository` | traversal, after percent-decoding |
| `empty patch` / `apply: <libgit2>` | `git/apply` with nothing, or a patch that no longer matches |
| `nothing in progress` / `a merge has nothing to skip` / `bisect is not driven from here` / `git is in a state butai does not recognise — use a shell` | sequence verbs |

Other statuses:

| status | meaning |
|---|---|
| `404 workspace {id} is not a git repository` | no CHANGES pane; `/diff` answers the same rather than returning git's usage text |
| `409 a git operation is already running: <kind>` | the repository's write lock is held |
| `200` with `ok: false` | the operation ran and git rejected it — read `summary` |

Failures **while running**:

| message | cause |
|---|---|
| `no output for 120s — giving up` | idle watchdog; whatever git or ssh was waiting on, it stopped saying so |
| `still running after 600s — giving up` | hard timeout |
| `cancelled` | `DELETE …/git/op` |
| `git: <io error>` | the `git` binary could not be spawned |
| `<kind> failed` | non-zero exit with no output to quote |

Client-side notices in the diff view: `nothing to pick in this hunk`,
`no lines picked`, `nothing to apply`, `(no differences)`.

### One known defect

`DELETE …/git/remote` used to be the second one, and it is fixed: the daemon
built `git remote remove -- <name>`, and `git remote remove` rejects the `--`
that `git remote add` accepts (`usage: git remote remove <name>`, exit 129), so
the route answered `200` with `ok: false` and left the remote in place. The
separator is gone from that one arm; the remote *name* is still validated, which
is what the separator was covering. It remains the cleanest illustration of why
"check `ok`" is not pedantry: the operation ran, and failing was not a 4xx.

- **The terminal's `g r F` does not force.** The menu sends
  `{"force": true}`, and the route's body field is `force_with_lease`; the
  unknown field is ignored and an ordinary push runs. You get the confirmation
  box, then a plain push — which will be rejected as non-fast-forward whenever
  the force was actually needed. The web client sends `force_with_lease` and is
  unaffected.

## Where this lives

| section | file |
|---|---|
| the working-tree model, status codes, sections | `crates/butai-server/src/pane/git.rs` |
| status scan, caching, refresh scheduling, repo probe | `crates/butai-server/src/pane/git.rs`, `crates/butai-server/src/core.rs` (`request_git_refresh`, `attach_new_repos`) |
| stage / unstage / discard / commit / commit-all / checkout | `crates/butai-server/src/pane/git.rs`, `crates/butai-server/src/core.rs` (`api_git`) |
| partial staging: patch parse, subset, reverse | `crates/butai-protocol/src/hunk.rs` |
| applying a patch to the index or worktree | `crates/butai-server/src/pane/git.rs` (`apply_patch`, `patch_text`) |
| the operation runner: argv, validation, timeouts, progress | `crates/butai-server/src/git_op.rs` |
| the write lock, grace window, `git_op` events | `crates/butai-server/src/core.rs` (`start_git_op`, `on_git_op_done`) |
| read-only git: log, stashes, remotes, tags, worktrees, conflict sides | `crates/butai-server/src/core.rs` (`git_read`, `build_log`, `build_stashes`, …) |
| diffs, untracked-file patches, whole-commit shows | `crates/butai-server/src/core.rs` (`build_diff`, `untracked_patch`, `build_show`) |
| worktree listing, labels, path validation | `crates/butai-server/src/git_worktree.rs` |
| the diff view, cursor, line-select, gutter | `crates/butai-client/src/chrome/mod.rs` (`DiffView`, `draw_diff_in`) |
| the GIT space: REFS rows, scope, verbs | `crates/butai-client/src/chrome/mod.rs` (`Git`, `ref_rows`, `git_columns`) |
| commit lanes and glyphs | `crates/butai-client/src/graph.rs` |
| the `g` menu: groups, rows, what is offered when | `crates/butai-client/src/git_menu.rs` |
| CHANGES keys, prompts, confirmations, the flows behind them | `crates/butai-client/src/workbench.rs` (`handle_changes_key`, `run_git`, `run_menu_action`) |
| the file highlighter the editor uses | `crates/butai-client/src/syntax.rs` |
| routes, bodies and status codes | `crates/butai-server/src/http_conn.rs`, [protocol.md](protocol.md) |
| the browser client's port of all of it | `web/butai-git.js`, `web/git-menu.js`, `web/graph.js`, [`web/README.md`](../web/README.md) |
