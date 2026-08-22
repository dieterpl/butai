// The GIT page — the repository over time, composed from the kit.
//
// The TypeScript port of `web/ui/git.js`, which is the port of `<butai-git>`,
// which is the port of `Page::Git` (`crates/butai-client/src/chrome/mod.rs`).
// What the page *is* has not changed through any of those, and the reasons are
// worth keeping in front of whoever reads this next:
//
// **Not a second CHANGES rail, and the difference is the whole design.** CHANGES
// is the working tree right now — what I changed, stage it, commit it — and it
// stays on the AGENTS page beside the agents doing the changing. This page is the
// repository *over time and across branches*. Two rules follow:
//
//  1. **The CHANGES rail does not move, shrink, or lose a verb.** Nothing here
//     stages anything. The one place they touch is the `working tree` row at the
//     top of REFS: it carries the dirty count and `enter` on it goes to the rail
//     that owns staging, so "where did staging go" is answered by a row on the
//     screen rather than by documentation. `check.py`'s
//     `ui/git-page-does-not-stage` is that rule as an assertion — it greps the
//     source for the six staging routes and this file calls none of them.
//     (Deliberately not listed here: the check greps for their *spelling*, so a
//     page that named them even in a comment would fail its own assertion.)
//  2. **Nothing here mutates on `enter`.** `enter` reads — it scopes the graph or
//     loads a diff. Checkout, merge, delete and drop are lettered verbs. That is
//     `enterRef` below, which `ui/git-enter-reads` reads out of the source.
//
// ## What the TypeScript port changes
//
// Only the props and the drawing. The layout, the six reads, the row kinds, the
// verb wiring and every comment above are the JS page's.
//
//  * **`ws` is the workspace, not its id.** `PAGES.md`'s `PageProps` hands a page
//    the qualified `QualifiedWorkspace`, so the dirty count comes off
//    `ws.changes` — the same push-channel slice the JS page took as its own
//    `changes` prop, one prop shorter.
//  * **Which column has the keyboard is the shell's**, arriving as `focus` and
//    leaving through `on.focus`. The JS page held it in `useState` and the
//    keyboard reached in through `app.el.git.column`; a page that is handed its
//    focus is the same page with the side channel removed, which is the point of
//    the refactor this client is part of.
//  * **`actions` is an interface rather than a bag of names.** `ui/page.js`'s
//    `wire()` filled in whatever the shell had not implemented with a function
//    that said so. Here [`GitActions`] is a type: a shell that has not wired
//    `checkout` does not compile, which is the same guarantee earlier.
//
// ## What it does not touch
//
// `logic/graph.ts` computes the lanes and `logic/git-menu.ts` is the command set
// behind `g`. Both are DOM-free logic with tests of their own (`test/graph.test.ts`
// is the lane assignment's spec) — so they are imported, not reimplemented, and
// the glyph string this draws is theirs verbatim.
//
// ## Keys
//
// The bare keys arrive with the shell: `keys.ts` dispatches a verb by finding the
// element that carries its target and clicking it, which is why every verb on
// this page is a real element with its `data-verb` on it. Until then the
// `HintBar` and the row buttons are the same verbs, reachable by pointer.

import * as React from "react";
import { useEffect, useRef, useState } from "react";
import { ChevronDown, RefreshCw } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { Empty } from "@/components/Empty";
import { HintBar, type Hint } from "@/components/HintBar";
import { Patch } from "@/components/Patch";
import { Row } from "@/components/Row";
import { SectionTitle } from "@/components/SectionTitle";
import { cn } from "@/lib/utils";

import type { World } from "../app/world.ts";
import { api } from "../logic/api.ts";
import { GIT_COLS } from "../logic/dom.ts";
import { localId, type Qid, type QualifiedGitOp, type QualifiedWorkspace } from "../logic/events.ts";
import { MAX_LANES, glyphs, graphRows, graphWidth } from "../logic/graph.ts";
import { GitRow, VerbId, click, gitFooter, gitRowVerbs, keyText, type TargetId, type Verb } from "../logic/verbs.ts";
import type {
  BranchDto,
  BranchesDto,
  ChangesDto,
  LogEntryDto,
  RemoteDto,
  StashDto,
  WorktreeDto,
} from "../protocol/generated/protocol.ts";
import { hints } from "./parts.ts";

/// How many commits one page of history is. The daemon clamps `limit` to 500;
/// this is the terminal's own page size, and `more` says when there is another.
const PAGE = 120;

/// The terminal gives each of this page's two footers two rows of 50 columns
/// (`git_list_width`'s upper clamp, less the frame). The packing decides which
/// verbs are worth writing down, not where they go — `HintBar` decides that, and
/// it spans the page.
const FOOTER_ROWS = 2;

// The click target each verb is declared under in `verbs.ts`'s registry — the
// same pairs `keys.ts`'s `SURFACES.git.verbs` dispatches through. A verb that
// reaches this table with no target throws inside `vtarget`, which is the point:
// a button that no key can reach must not be drawable.
const TARGET: Partial<Record<VerbId, TargetId>> = {
  [VerbId.Checkout]: "git.checkout",
  [VerbId.Merge]: "git.merge",
  [VerbId.DeleteBranch]: "git.branch.delete",
  [VerbId.Fetch]: "git.fetch",
  [VerbId.TagDelete]: "git.tag.delete",
  [VerbId.StashPop]: "git.stash.pop",
  [VerbId.StashDrop]: "git.stash.drop",
  [VerbId.RemoveWorktree]: "git.worktree.remove",
  [VerbId.CopySha]: "git.sha",
  [VerbId.Revert]: "git.revert",
  [VerbId.CherryPick]: "git.pick",
};

/// The five ref reads, held together. `loaded` is false until they have all
/// answered — a repository that is not one answers 404 on every one of them, so
/// "no rows and loaded" is the empty state and "no rows and not loaded" is
/// "loading…".
interface RepoData {
  loaded: boolean;
  branches: BranchesDto | null;
  tags: string[];
  stashes: StashDto[];
  remotes: RemoteDto[];
  worktrees: WorktreeDto[];
}

/// One page of history, and whether the walk had more.
interface LogPage {
  loaded: boolean;
  commits: LogEntryDto[];
  more: boolean;
}

/// What the body box is showing: a commit, a stash, or the error that came back
/// instead. `patch` is raw unified-diff text, straight from `DiffDto`.
interface BodyView {
  title: string;
  patch: string;
}

const NO_DATA: RepoData = { loaded: false, branches: null, tags: [], stashes: [], remotes: [], worktrees: [] };
const NO_LOG: LogPage = { loaded: false, commits: [], more: false };

/// Which of the page's three boxes the keyboard is on. The body is a diff rather
/// than a list, which is why it offers no row verbs and why the terminal's body
/// box has no footer of its own.
export type GitColumn = "refs" | "history" | "body";

// ---------------------------------------------------------------------------
// The REFS list, as rows
// ---------------------------------------------------------------------------

/// A heading. Not a click target, and the cursor steps over it — which is the
/// one thing that makes this flat list walkable and drawable from one array.
interface Heading {
  kind: "head";
  head: string;
}

/// One walkable REFS row. A discriminated union on `kind`, whose values are
/// `verbs.ts`'s [`GitRow`] — so the row the drawing holds and the row the verb
/// table answers for are the same word rather than two that have to be kept in
/// step.
type RefRowData =
  | { kind: typeof GitRow.WorkingTree; dirty: number }
  | {
      kind:
        | typeof GitRow.Branch
        | typeof GitRow.CurrentBranch
        | typeof GitRow.BranchElsewhere
        | typeof GitRow.RemoteBranch;
      entry: BranchDto;
      current: boolean;
      /// The worktree holding this branch, when it is not this one.
      elsewhere: string | null;
    }
  | { kind: typeof GitRow.Remote; remote: RemoteDto }
  | { kind: typeof GitRow.Tag; tag: string }
  | { kind: typeof GitRow.Stash; stash: StashDto }
  | { kind: typeof GitRow.Worktree; wt: WorktreeDto; here: boolean };

type RefItem = Heading | RefRowData;

/// The HISTORY cursor's row, in the shape the verb table wants. It carries only
/// what the verbs need — the summary is for the body's title, not for drawing.
interface CommitRowData {
  kind: typeof GitRow.Commit;
  id: string;
  summary: string;
}

/// Whichever list is live, or nothing.
type LiveRow = RefRowData | CommitRowData | null;

/// What a row can run, keyed by verb id. `null` for a verb the page knows about
/// and cannot run *right now* — `X cancel` with no operation in flight.
type Runs = Partial<Record<VerbId, (() => void) | null>>;

/// Lay REFS out, in the order it is drawn.
///
/// The port of `chrome::ref_rows`, and pure: flat, with headings in it, and *the
/// same list* is what the cursor walks and what `enter` reads.
function refRows(data: RepoData, changes: ChangesDto | null, here: number | null): RefItem[] {
  const rows: RefItem[] = [];
  // The working tree leads, and it is the only row here that is about *now*.
  if (changes) {
    // `conflicted` landed after 0.5 and `?? []` is what an older daemon needs —
    // the DTO types it as required because the *current* daemon always sends it.
    const dirty = changes.staged.length + changes.unstaged.length + (changes.conflicted ?? []).length;
    rows.push({ kind: GitRow.WorkingTree, dirty });
  }
  const b = data.branches;
  const current = b ? b.current : null;
  // `entries` landed after 0.8 as well; an older daemon sends `branches` alone
  // and this page draws no branch rows rather than half a row each.
  const entries: BranchDto[] = b?.entries ?? [];
  const locals = entries.filter((e) => !e.remote);
  if (locals.length) {
    rows.push({ kind: "head", head: "branches" });
    for (const entry of locals) {
      // A branch checked out in another worktree cannot be checked out here, so
      // the row says where it went instead of offering a verb that fails. Matched
      // on the daemon's own `workspace` field rather than on the path: `git
      // worktree list` reports libgit2's canonical spelling while a workspace's
      // cwd is whatever was passed in, and comparing the two as strings answers
      // "not here" for the checkout you are standing in.
      const wt = data.worktrees.find((w) => w.branch === entry.name && !(here != null && w.workspace === here));
      rows.push({
        kind: entry.name === current ? GitRow.CurrentBranch : wt ? GitRow.BranchElsewhere : GitRow.Branch,
        entry,
        current: entry.name === current,
        elsewhere: wt ? wt.path : null,
      });
    }
  }
  const remoteBranches = entries.filter((e) => e.remote);
  if (remoteBranches.length) {
    rows.push({ kind: "head", head: "remote branches" });
    for (const entry of remoteBranches) {
      rows.push({ kind: GitRow.RemoteBranch, entry, current: false, elsewhere: null });
    }
  }
  if (data.remotes.length) {
    rows.push({ kind: "head", head: "remotes" });
    for (const r of data.remotes) rows.push({ kind: GitRow.Remote, remote: r });
  }
  if (data.tags.length) {
    rows.push({ kind: "head", head: "tags" });
    for (const t of data.tags) rows.push({ kind: GitRow.Tag, tag: t });
  }
  if (data.stashes.length) {
    rows.push({ kind: "head", head: "stashes" });
    for (const s of data.stashes) rows.push({ kind: GitRow.Stash, stash: s });
  }
  // One worktree is just "this repository"; the section earns its rows only once
  // there is somewhere else to go.
  if (data.worktrees.length > 1) {
    rows.push({ kind: "head", head: "worktrees" });
    for (const w of data.worktrees) {
      rows.push({ kind: GitRow.Worktree, wt: w, here: here != null && w.workspace === here });
    }
  }
  return rows;
}

/// Which kind the verb table should answer for — a worktree you are standing in
/// is `ThisWorktree`, which offers nothing, because git refuses to remove the
/// checkout it is run from and the row would advertise a failure.
function kindOf(row: LiveRow): GitRow {
  if (!row) return GitRow.None;
  if (row.kind === GitRow.Worktree && row.here) return GitRow.ThisWorktree;
  return row.kind;
}

/// This workspace's bare id on its own daemon — the number [`WorktreeDto`] speaks.
///
/// The ids crossing to the browser are `<daemon>:<n>`; the daemon's own records
/// are not, because the bridge relays them without reading them. This is the one
/// place a qualified id is deliberately taken apart, and it is safe because the
/// number is only ever compared against records that came from *this* workspace's
/// own daemon — see `refRows`.
function bareId(id: Qid | null | undefined): number | null {
  if (id == null) return null;
  const local = localId(id);
  if (local != null) return local;
  // A world with no daemon keys in it — the reducers' own test shape — carries
  // bare ids already.
  const n = Number(id);
  return Number.isFinite(n) ? n : null;
}

/// `↑n ↓n` for a branch against its upstream, or nothing when it has none.
function drift(e: BranchDto): string {
  const up = e.ahead ? "↑" + e.ahead : "";
  const down = e.behind ? "↓" + e.behind : "";
  return up + (up && down ? " " : "") + down;
}

function leaf(path: string): string {
  const i = path.lastIndexOf("/");
  return i >= 0 ? path.slice(i + 1) : path;
}

// ---------------------------------------------------------------------------
// Reaching the logic layer
// ---------------------------------------------------------------------------

/// The click registry, in React's spelling — `ui/page.js`'s `vclick`, minus the
/// handler.
///
/// `verbs.ts`'s [`click`] is "the only way to put a click handler on anything",
/// and it throws for a target that is not declared in `TARGETS`; that is what
/// stops a button existing with no key that reaches it. The call below is made
/// **for the assertion** and its `onclick` is dropped, because React needs its
/// own `onClick` and because attaching one here would be a bug: `Row` composes a
/// caller's `onClick` with its `onSelect`, so a row given both would run its verb
/// twice — once opening a worktree, once opening it again.
///
/// Keeping `data-verb` is not decoration either: `keys.ts` dispatches a verb by
/// finding the element carrying its target and clicking it, so an element without
/// one is a verb the keyboard cannot reach once the shell lands.
function vtarget(target: TargetId | undefined): { "data-verb": string } {
  click(String(target), () => {});
  return { "data-verb": String(target) };
}

/// A surface's verb table as [`HintBar`] keys — `ui/page.js`'s `hintKeys`, with
/// `parts.ts` doing the packing.
///
/// The packing is the load-bearing half and it is shared: `fits()` at the
/// terminal's own column count, which reads like a layout decision and is not
/// one. It decides *which verbs are worth writing down*, and that answer has to
/// be the terminal's — two clients that draw different key lists are two clients
/// teaching different keys.
///
/// What differs from WORK and HOME is only how a hint is *run*. Those pages hand
/// `hints` a `press(surface, key)` and let the shell's one dispatch table resolve
/// it; this page wires the row's own function, because the GIT footer's contract
/// is stronger: **one table, read twice** — the row draws a button for each
/// lettered entry and the bar wires the same function to the key it is spelled
/// with, so the bar cannot teach a key the row does not answer. The property that
/// buys is the second half of `runFor`: a verb the page cannot run *right now*
/// gets no function, so `HintBar` draws it as documentation rather than as a
/// button that looks live and does nothing. `X cancel` with no operation in
/// flight is that case, and it is why `verbs.ts` marks it quiet.
function hintKeys(
  verbs: readonly Verb[],
  cols: number,
  rows: number,
  runFor: (v: Verb) => (() => void) | null,
): Hint[] {
  return hints(verbs, cols, rows).map((h) => {
    const v = verbs.find((x) => keyText(x.key) === h.key);
    const run = v ? runFor(v) : null;
    return run ? { ...h, onSelect: run } : h;
  });
}

// ---------------------------------------------------------------------------
// The page's props
// ---------------------------------------------------------------------------

/// Everything on this page that writes.
///
/// The shell owns them because it owns the confirmations, the toasts and the
/// refresh that follows — a page that ran them itself would be a second copy of
/// that policy. Every one of them reports through a toast and returns, so no call
/// site here needs a `try`.
///
/// Named here rather than imported, as `WorkActions` and `HomeActions` are: the
/// interface is the page's statement of what it needs, and anything with these
/// methods satisfies it. **`app/actions.ts`'s `Actions` does not, yet** — it
/// carries the rails' verbs and the three remote ones, and not one of the
/// fifteen below. See `HANDOVER-git.md`.
export interface GitActions {
  /// Go to the rail that owns staging. The one place this page and CHANGES touch.
  goToChanges(): void;
  /// Open the `g` command menu — `logic/git-menu.ts` is its table.
  gitMenu(): void;
  checkout(branch: string): void;
  merge(branch: string): void;
  deleteBranch(branch: string): void;
  fetchRemote(name: string): void;
  deleteTag(name: string): void;
  stashPop(index: number): void;
  stashDrop(index: number): void;
  /// Open a worktree *as a workspace*, with its own agents, processes and rail.
  openWorktree(wt: WorktreeDto): void;
  removeWorktree(path: string): void;
  copySha(sha: string): void;
  revert(rev: string): void;
  cherryPick(rev: string): void;
  /// `DELETE .../git/op` — the only way to stop an operation in flight.
  cancelOp(): void;
}

/// The view-state changes this page hands back to the shell.
export interface GitPageCallbacks {
  /// Which of the three boxes the keyboard is on, after a click moved it.
  ///
  /// Clicking a row *is* walking to it — `keys.ts` makes the same claim from the
  /// other side, moving its cursor on any click that lands on a declared row —
  /// so the two never disagree about which list is live.
  focus(where: GitColumn): void;
}

export interface GitPageProps {
  world: World;
  /// The current workspace, already qualified. `ws.changes` is the push-channel
  /// slice that keeps the working-tree row's dirty count following the CHANGES
  /// rail live.
  ws: QualifiedWorkspace | null;
  actions: GitActions;
  /// Which rail/panel the keyboard is on.
  ///
  /// `string` rather than [`GitColumn`], and deliberately: the shell holds one
  /// focus for every surface — it starts on `"stage"` — so a page that demanded
  /// its own three would be typed against a value the shell cannot promise.
  /// Anything this page does not recognise reads as `refs`.
  focus: string;
  on: GitPageCallbacks;
  /// The git operation in flight, if any.
  ///
  /// Not part of `PageProps`: `world.ts` ignores `git_op` records ("handled by
  /// the pages that care"), and a page cannot subscribe to a stream — so the
  /// shell passes the last one down. See `HANDOVER-git.md`.
  op?: QualifiedGitOp | null;
}

/// Both reads that may 404 on a repository that is not one, folded to `null`.
/// That is not an error worth a toast, it is the empty state.
function ok<T>(p: Promise<T>): Promise<T | null> {
  return p.then((v) => v, () => null);
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

/**
 * GIT: refs, history with the commit-lane graph, and one revision in the body.
 *
 * Reads are this page's own — six of them, because the daemon serves six answers
 * and inventing a seventh aggregate route for one client is the side channel the
 * refactor exists to remove. Everything that *writes* is `actions`.
 */
export function GitPage(props: GitPageProps) {
  // `act` rather than `props.actions` at every call site: `check.py`'s
  // `ui/git-enter-reads` reads `enterRef`'s arms for `act.<name>`, and the rule
  // it asserts is worth more than the two saved characters.
  const { ws, actions: act, focus, on, op = null } = props;

  // The qualified workspace id, as `api.ts` wants it — every route below refuses
  // a bare one before it leaves the browser.
  const id = ws ? String(ws.id) : null;
  const changes = ws?.changes ?? null;

  const [data, setData] = useState<RepoData>(NO_DATA);
  const [log, setLog] = useState<LogPage>(NO_LOG);
  // What the history is a history *of*. Held rather than recomputed: it is what
  // the next page of log is fetched with, and the header says it — a graph scoped
  // to one branch and one scoped to everything look alike until you read it.
  const [scope, setScope] = useState<string | null>(null);
  // One cursor per list.
  //
  // The *column* is the shell's, arriving as `focus`; the two indexes are this
  // page's, which is the one place it departs from `WorkPage`'s `view` prop and
  // is worth the sentence. GIT is **one surface with two lists**, and the shell's
  // cursor is one number per surface — `keys.ts` handles that by making the row
  // target a function of the live column, so `cursor.git` means a REFS row or a
  // commit depending on where the keyboard is. Two indexes held here and one
  // column held there is that same arrangement without the ambiguity; if the
  // shell wants them, they lift to a `view` prop unchanged.
  const [sel, setSel] = useState<{ refs: number; history: number }>({ refs: 0, history: 0 });
  const [body, setBody] = useState<BodyView | null>(null);
  // Bumped by `r refresh`, by a finished operation and by a branch switch — the
  // three things that change every list on this page at once.
  const [nonce, setNonce] = useState(0);
  const reload = () => setNonce((n) => n + 1);

  const column: GitColumn = focus === "history" || focus === "body" ? focus : "refs";

  // The five ref reads. A repository that is not a repository answers 404 on all
  // of them.
  useEffect(() => {
    if (id == null) {
      setData(NO_DATA);
      return undefined;
    }
    let live = true;
    void Promise.all([
      ok(api.branches(id)),
      ok(api.tags(id)),
      ok(api.stashes(id)),
      ok(api.remotes(id)),
      ok(api.worktrees(id)),
    ]).then(([branches, tags, stashes, remotes, worktrees]) => {
      // The workspace changed under the fetch. Throwing the answer away beats
      // drawing another repository's branches under this one's name.
      if (!live) return;
      setData({
        loaded: true,
        branches,
        tags: tags ?? [],
        stashes: stashes ?? [],
        remotes: remotes ?? [],
        worktrees: worktrees ?? [],
      });
    });
    return () => {
      live = false;
    };
  }, [id, nonce]);

  // The history, which is the one read a scope changes. `all` and `rev` are
  // exclusive — the daemon 400s both — and `rev` is spread in rather than passed
  // as `undefined`, which `exactOptionalPropertyTypes` refuses and which would
  // put a bare `&rev=` on the query either way.
  useEffect(() => {
    if (id == null) {
      setLog(NO_LOG);
      return undefined;
    }
    let live = true;
    void ok(api.gitLog(id, { limit: PAGE, all: !scope, ...(scope ? { rev: scope } : {}) })).then((d) => {
      if (!live) return;
      setLog({ loaded: true, commits: d?.commits ?? [], more: !!d?.more });
    });
    return () => {
      live = false;
    };
  }, [id, scope, nonce]);

  // A branch switch or a finished sequence changes every list here, and it
  // arrives on the push channel rather than through anything we did.
  const seen = useRef<string | null>(null);
  useEffect(() => {
    const now = changes ? changes.branch + " " + changes.state : null;
    if (seen.current != null && now != null && now !== seen.current) reload();
    seen.current = now;
  }, [changes?.branch, changes?.state]);

  // …and so does an operation that just stopped, which is the whole reason the
  // daemon pushes `git_op`: a `git push` from another client shows up here.
  const opHere = op != null && ws != null && op.ws === ws.id;
  useEffect(() => {
    if (op && !op.running && ws != null && op.ws === ws.id) reload();
  }, [op]);

  // A new workspace is a new repository: every cursor, scope and body belongs to
  // the old one.
  useEffect(() => {
    setScope(null);
    setSel({ refs: 0, history: 0 });
    setBody(null);
  }, [id]);

  /// Load a revision into the body. `rev` is a sha or `stash@{n}` — both go
  /// through `GET .../show`, which is the same widget the FILES page uses.
  const shown = useRef(0);
  const showRev = (rev: string, title?: string) => {
    if (id == null) return;
    const mine = ++shown.current;
    const name = title || rev;
    setBody({ title: name, patch: "loading…" });
    void api.show(id, rev).then(
      (d) => {
        if (shown.current === mine) setBody({ title: name, patch: d.patch || "(no changes)" });
      },
      (e: unknown) => {
        if (shown.current === mine) setBody({ title: name, patch: e instanceof Error ? e.message : String(e) });
      },
    );
  };

  /// Point HISTORY at one ref, or at everything.
  const scopeTo = (rev: string | null) => {
    setScope(rev || null);
    setSel((s) => ({ ...s, history: 0 }));
  };

  const items = refRows(data, changes, bareId(ws?.id));
  const walkable = items.filter((r): r is RefRowData => r.kind !== "head");
  const refRow = walkable[Math.min(sel.refs, Math.max(0, walkable.length - 1))] ?? null;
  const commit = log.commits[Math.min(sel.history, Math.max(0, log.commits.length - 1))] ?? null;

  /// What `enter` means on a REFS row. **Every arm is a read** — a scope, a diff,
  /// or moving to another surface. Nothing here writes the repository, and
  /// `check.py`'s `ui/git-enter-reads` reads this function's arms and is what says
  /// so without a browser — the same assertion the two older clients are held to.
  const enterRef = (row: RefRowData) => {
    switch (row.kind) {
      case GitRow.WorkingTree:
        // The one place this page and the CHANGES rail touch. Staging lives
        // there and stays there; this is the way to it.
        return act.goToChanges();
      case GitRow.Branch:
      case GitRow.CurrentBranch:
      case GitRow.BranchElsewhere:
      case GitRow.RemoteBranch:
        return scopeTo(row.entry.name);
      case GitRow.Tag:
        return scopeTo(row.tag);
      case GitRow.Stash:
        // `show` resolves `stash@{n}` — the revision allowlist admits `@{}`
        // precisely so a stash can be read without a route of its own.
        return showRev(`stash@{${row.stash.index}}`, `stash@{${row.stash.index}}`);
      case GitRow.Worktree:
        return act.openWorktree(row.wt);
      default:
        return undefined; // a remote has no `enter`: its verb is `f fetch`
    }
  };

  /// What one row can do, keyed by the verb ids `verbs.ts` names.
  ///
  /// **One table, read twice.** The row draws a button for each lettered entry,
  /// and the `HintBar` wires the same function to the key that entry is spelled
  /// with — so the bar cannot teach a key the row does not answer, and neither of
  /// them can carry a label `verbs.ts` has not written down.
  const runs = (row: LiveRow): Runs => {
    if (!row) return {};
    switch (row.kind) {
      case GitRow.WorkingTree:
        return { [VerbId.GoToChanges]: () => act.goToChanges() };
      case GitRow.Branch:
        return {
          [VerbId.Scope]: () => scopeTo(row.entry.name),
          [VerbId.Checkout]: () => act.checkout(row.entry.name),
          [VerbId.Merge]: () => act.merge(row.entry.name),
          [VerbId.DeleteBranch]: () => act.deleteBranch(row.entry.name),
        };
      case GitRow.CurrentBranch:
        // No checkout (you are standing on it) and no delete.
        return { [VerbId.Scope]: () => scopeTo(row.entry.name) };
      case GitRow.BranchElsewhere:
      // No checkout: git refuses to check a branch out twice. And none on a
      // remote branch either — the daemon resolves `refs/heads/<name>`, so
      // `origin/main` asks for a local branch of that name and always fails.
      case GitRow.RemoteBranch:
        return { [VerbId.Scope]: () => scopeTo(row.entry.name), [VerbId.Merge]: () => act.merge(row.entry.name) };
      case GitRow.Remote:
        return { [VerbId.Fetch]: () => act.fetchRemote(row.remote.name) };
      case GitRow.Tag:
        return { [VerbId.Scope]: () => scopeTo(row.tag), [VerbId.TagDelete]: () => act.deleteTag(row.tag) };
      case GitRow.Stash:
        return {
          [VerbId.Show]: () => enterRef(row),
          [VerbId.StashPop]: () => act.stashPop(row.stash.index),
          [VerbId.StashDrop]: () => act.stashDrop(row.stash.index),
        };
      case GitRow.Worktree:
        return row.here
          ? {} // nowhere to go, nothing to remove
          : {
              [VerbId.OpenWorktree]: () => act.openWorktree(row.wt),
              [VerbId.RemoveWorktree]: () => act.removeWorktree(row.wt.path),
            };
      case GitRow.Commit:
        return {
          [VerbId.Show]: () => showRev(row.id, row.summary),
          [VerbId.CopySha]: () => act.copySha(row.id),
          [VerbId.Revert]: () => act.revert(row.id),
          [VerbId.CherryPick]: () => act.cherryPick(row.id),
        };
      default:
        return {};
    }
  };

  // The verbs that apply wherever the cursor is. `X cancel` is only live while an
  // operation is running, which is why `verbs.ts` marks it quiet.
  const pageRuns: Runs = {
    [VerbId.GitMenu]: () => act.gitMenu(),
    [VerbId.Refresh]: () => reload(),
    [VerbId.ScopeAll]: () => scopeTo(null),
    [VerbId.CancelOp]: opHere && op.running ? () => act.cancelOp() : null,
  };

  // Which row the bare keys act on: whichever list is live. The body is a diff
  // and not a list, so it offers no row verbs at all.
  const liveRow: LiveRow =
    column === "history"
      ? commit
        ? { kind: GitRow.Commit, id: commit.id, summary: commit.summary }
        : null
      : column === "refs"
        ? refRow
        : null;
  const liveRuns = runs(liveRow);
  const keys = hintKeys(
    gitFooter(kindOf(liveRow)),
    GIT_COLS,
    FOOTER_ROWS,
    (v) => liveRuns[v.id] ?? pageRuns[v.id] ?? null,
  );

  // The lanes over the *whole* page, not the visible slice: a lane opened by a
  // merge above the fold still has to be drawn passing through the rows on
  // screen, and a graph that restarted at the scroll offset would invent a
  // different shape at every scroll position.
  const lanes = graphRows(log.commits, MAX_LANES);
  const laneWidth = graphWidth(lanes, MAX_LANES) || 1;

  const pick = (col: "refs" | "history", i: number, run?: () => void) => () => {
    on.focus(col);
    setSel((s) => (col === "refs" ? { ...s, refs: i } : { ...s, history: i }));
    if (run) run();
  };

  // Which box has the keyboard, said the way `Row` says it: a ring, inset, on the
  // one focus token. `Card` has no `live` variant of its own — see the handover.
  const live = (col: GitColumn) => (column === col ? "ring-1 ring-inset ring-ring" : "");

  return (
    // The shell provides one of these and nesting is allowed, so the two header
    // tooltips work whether or not this page is mounted inside it.
    <TooltipProvider delayDuration={200}>
      <div className="flex h-full min-h-0 flex-col">
        {/* One layout at every width, following the Docker page: the sectioned
            list column, and a body box taking the rest. Under 768px there is no
            room for two, so they stack — and the grid row is pinned to
            `minmax(0,1fr)` because an `auto` row grows to fit its content, and
            the content here is a history that can be five hundred rows. */}
        <div
          className={cn(
            "flex min-h-0 flex-1 flex-col gap-3 p-3",
            "md:grid md:[grid-template-columns:minmax(320px,460px)_1fr] md:[grid-template-rows:minmax(0,1fr)]",
          )}
        >
          <div className="flex min-h-0 flex-1 flex-col gap-3">
            {/* Two behaviours, and the breakpoint is where the page stops being
                two columns. From `md` up REFS is sized by its own content and
                clamped at 45% of the column — a repository with three branches
                gets a short box, HISTORY gets the rest. Stacked, that same rule
                gives 45% of half a screen to REFS and whatever is left to
                HISTORY, which was a three-pixel sliver: below `md` the two lists
                simply share the column instead. */}
            <Card className={cn(LIST_CARD, "flex-1 md:max-h-[45%] md:flex-none", live("refs"))}>
              <SectionTitle
                action={
                  <>
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <Button
                          size="sm"
                          variant="ghost"
                          {...vtarget("git.menu")}
                          onClick={() => act.gitMenu()}
                        >
                          git
                          <ChevronDown />
                        </Button>
                      </TooltipTrigger>
                      <TooltipContent>Everything else git can do (g)</TooltipContent>
                    </Tooltip>
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <Button
                          size="icon-sm"
                          variant="ghost"
                          aria-label="refresh"
                          {...vtarget("git.refresh")}
                          onClick={() => reload()}
                        >
                          <RefreshCw />
                        </Button>
                      </TooltipTrigger>
                      <TooltipContent>Read it all again (r)</TooltipContent>
                    </Tooltip>
                  </>
                }
              >
                refs
              </SectionTitle>
              <ScrollArea type="auto" className="min-h-0 flex-1">
                <div role="listbox" aria-label="refs" className="flex min-w-0 flex-col">
                  {!items.length ? (
                    <Empty>{data.loaded ? "not a git repository" : "loading…"}</Empty>
                  ) : (
                    items.map((row, i) =>
                      row.kind === "head" ? (
                        <SectionTitle key={`h${i}`}>{row.head}</SectionTitle>
                      ) : (
                        <RefRow
                          key={`r${i}`}
                          row={row}
                          selected={column === "refs" && row === refRow}
                          runs={runs(row)}
                          onSelect={pick("refs", walkable.indexOf(row), () => enterRef(row))}
                        />
                      ),
                    )
                  )}
                </div>
              </ScrollArea>
            </Card>

            <Card className={cn(LIST_CARD, "flex-1", live("history"))}>
              <SectionTitle
                action={
                  <Tooltip>
                    <TooltipTrigger asChild>
                      <Button size="sm" variant="ghost" {...vtarget("git.scope.all")} onClick={() => scopeTo(null)}>
                        all refs
                      </Button>
                    </TooltipTrigger>
                    <TooltipContent>Every branch, tag and remote (esc)</TooltipContent>
                  </Tooltip>
                }
              >
                {`history · ${scope || "all refs"}`}
              </SectionTitle>
              {/* Native overflow rather than `ScrollArea`: this is the one list
                  that has to scroll *sideways* — a commit subject is not
                  truncated here — and the kit's `ScrollArea` draws a vertical
                  bar only. The base stylesheet themes the native ones. */}
              <div className="min-h-0 flex-1 overflow-auto">
                {!log.commits.length ? (
                  <Empty>{log.loaded ? "no commits" : "loading…"}</Empty>
                ) : (
                  // `w-max min-w-full` so every row is as wide as the widest one:
                  // a subject longer than the column scrolls sideways rather than
                  // being cut, and the selection band still reaches the full width
                  // of the list instead of stopping at each row's own text.
                  <div role="listbox" aria-label="history" className="flex w-max min-w-full flex-col">
                    {log.commits.map((c, i) => {
                      const lane = lanes[i];
                      return (
                        <CommitRow
                          key={c.id}
                          commit={c}
                          current={data.branches ? data.branches.current : null}
                          glyph={lane ? glyphs(lane, MAX_LANES) : "●"}
                          laneWidth={laneWidth}
                          selected={column === "history" && i === sel.history}
                          onSelect={pick("history", i, () => showRev(c.id, c.summary))}
                        />
                      );
                    })}
                    {log.more ? <Empty>{`… ${PAGE} commits shown; the walk has more`}</Empty> : null}
                  </div>
                )}
              </div>
            </Card>
          </div>

          <Card className={cn(LIST_CARD, "min-w-0 flex-1", live("body"))}>
            <CardHeader className="gap-0 px-3 py-2">
              <CardTitle className="truncate text-14">{body ? body.title : "commit"}</CardTitle>
            </CardHeader>
            {opHere && op.running ? (
              // The operation banner. A git operation is the one thing on this
              // page that takes real time, and `DELETE .../git/op` is the only
              // way to stop one — a route the daemon has served since 0.8 that no
              // client had ever called.
              <SectionTitle
                action={
                  <>
                    <Badge variant="outline" className={cn(TONE.warn, "min-w-0 truncate")}>
                      {op.progress || "running…"}
                    </Badge>
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <Button
                          size="sm"
                          variant="ghost"
                          {...vtarget("git.op.cancel")}
                          onClick={() => act.cancelOp()}
                        >
                          cancel
                        </Button>
                      </TooltipTrigger>
                      <TooltipContent>Stop it (X)</TooltipContent>
                    </Tooltip>
                  </>
                }
              >
                {op.kind}
              </SectionTitle>
            ) : null}
            {body ? (
              <Patch text={body.patch} className="min-h-0 flex-1" />
            ) : (
              <Empty>{ws == null ? "no workspace open" : data.loaded ? "Enter on a commit to read it." : "loading…"}</Empty>
            )}
          </Card>
        </div>

        <HintBar keys={keys} />
      </div>
    </TooltipProvider>
  );
}

// A card that holds a list rather than prose. `Card`'s own `py-6 gap-6` is the
// padding a paragraph wants and is wrong under a `SectionTitle`, whose hairline
// has to reach both edges; `overflow-hidden` is what makes it stop at the rounded
// corner. One constant rather than the same three overrides on three cards.
const LIST_CARD = "flex min-h-0 flex-col gap-0 overflow-hidden py-0";

// `Badge` ships shadcn's six variants and not one of them is a *state*: the kit
// gives `Notice` ok/warn/bad and `Badge` nothing. Rather than invent a seventh
// variant inside a page, the two states this page needs are spelled as tokens on
// `variant="outline"` — no colour literal, and one place to change them.
const TONE = {
  warn: "border-warn/40 bg-warn/10 text-warn",
  accent: "border-primary/40 bg-primary/10 text-primary",
  ok: "border-ok/40 bg-ok/10 text-ok",
} as const;

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

/// The lettered verbs of one row, as buttons.
///
/// `enter` is not among them: `enter` is the row itself, and a row that also
/// carried a button for it would be two ways to click the same thing. Shown on
/// hover, and always on the selected row — the cursor is where the keys apply, so
/// that is where they are worth spelling out.
///
/// The label and the key come from `verbs.ts` rather than from here, so a row
/// cannot offer a verb the table has not written down, and a reworded verb is
/// reworded in one place. `title` rather than a `Tooltip`: a list of forty
/// branches would otherwise carry a hundred Radix roots for text that is one
/// attribute.
function RowVerbs({ kind, runs, selected }: { kind: GitRow; runs: Runs; selected: boolean }) {
  const items = gitRowVerbs(kind).filter((v) => v.key !== "enter" && runs[v.id]);
  if (!items.length) return null;
  return (
    <span className={cn("shrink-0 items-center gap-1", selected ? "flex" : "hidden group-hover:flex")}>
      {items.map((v) => (
        <Button
          key={v.key}
          size="sm"
          variant={v.danger ? "destructive" : "ghost"}
          title={`${v.label} (${v.key})`}
          {...vtarget(TARGET[v.id])}
          onClick={(e) => {
            // Otherwise the row underneath takes the click as well and `enter`
            // runs beside the verb.
            e.stopPropagation();
            runs[v.id]?.();
          }}
        >
          {v.label}
        </Button>
      ))}
    </span>
  );
}

function RefRow({
  row,
  selected,
  runs,
  onSelect,
}: {
  row: RefRowData;
  selected: boolean;
  runs: Runs;
  onSelect: () => void;
}) {
  return (
    <Row selected={selected} onSelect={onSelect} title={titleOf(row)} {...vtarget("git.ref")}>
      <RefBody row={row} />
      <RowVerbs kind={kindOf(row)} runs={runs} selected={selected} />
    </Row>
  );
}

function RefBody({ row }: { row: RefRowData }): React.ReactElement {
  switch (row.kind) {
    case GitRow.WorkingTree:
      return (
        <>
          <span className={cn("min-w-0 flex-1 truncate", row.dirty ? "text-foreground" : "text-dim")}>
            working tree
          </span>
          {row.dirty ? (
            <Badge variant="outline" className={TONE.warn}>{`${row.dirty} changed`}</Badge>
          ) : (
            <Badge variant="outline">clean</Badge>
          )}
        </>
      );
    case GitRow.Branch:
    case GitRow.CurrentBranch:
    case GitRow.BranchElsewhere:
    case GitRow.RemoteBranch: {
      const e = row.entry;
      const right = row.elsewhere ? "⇢" + leaf(row.elsewhere) : drift(e);
      return (
        <>
          <span className="w-2 shrink-0 font-semibold text-ok">{row.current ? ">" : ""}</span>
          <span
            className={cn(
              "min-w-0 flex-1 truncate",
              row.current ? "font-semibold text-ok" : e.remote ? "text-dim" : "text-foreground",
            )}
          >
            {e.name}
          </span>
          {right ? (
            // `↑3 ↓12` is mono and tabular — two branches' drift has to be
            // comparable down the column. `⇢bmux-ui` is a directory's name and
            // takes the row's own font, because a name compared to nothing is
            // exactly what the kit's mono rule is not for.
            <span
              className={cn(
                "shrink-0 whitespace-nowrap",
                row.elsewhere ? "text-dim" : "font-mono tabular-nums text-warn",
              )}
            >
              {right}
            </span>
          ) : null}
        </>
      );
    }
    // The url truncates from its *end*, unlike a path: a remote's host is the
    // front of the string, so `Path` — which keeps the basename and eats the
    // directory — would throw away the half that matters here.
    case GitRow.Remote:
      return (
        <>
          <span className="shrink-0 text-foreground">{row.remote.name}</span>
          <span className="min-w-0 flex-1 truncate font-mono text-dim">{row.remote.url}</span>
        </>
      );
    case GitRow.Tag:
      return <span className="min-w-0 flex-1 truncate text-primary">{row.tag}</span>;
    case GitRow.Stash:
      return (
        <>
          <span className="shrink-0 font-mono tabular-nums text-foreground">{row.stash.index}</span>
          <span className="min-w-0 flex-1 truncate text-dim">{row.stash.message}</span>
        </>
      );
    case GitRow.Worktree:
      return (
        <>
          <span className={cn("shrink-0", row.here ? "text-dim" : "text-foreground")}>{leaf(row.wt.path)}</span>
          <span className="min-w-0 flex-1 truncate text-dim">{row.wt.branch || "detached"}</span>
          {row.here ? (
            <Badge variant="outline">here</Badge>
          ) : row.wt.workspace != null ? (
            <Badge variant="outline" className={TONE.accent}>
              open
            </Badge>
          ) : null}
        </>
      );
    default:
      return <span className="min-w-0 flex-1 truncate">?</span>;
  }
}

function titleOf(row: RefRowData): string | undefined {
  switch (row.kind) {
    case GitRow.WorkingTree:
      return row.dirty ? `${row.dirty} changed files — enter goes to the CHANGES rail` : "nothing to commit";
    case GitRow.Remote:
      return `${row.remote.name} ${row.remote.url}`;
    case GitRow.Worktree:
      return row.wt.path;
    case GitRow.Stash:
      return `stash@{${row.stash.index}} ${row.stash.message}`;
    case GitRow.BranchElsewhere:
      return `${row.entry.name} is checked out in ${row.elsewhere ?? "another worktree"}`;
    case GitRow.Tag:
      return row.tag;
    default:
      return row.entry.name;
  }
}

// A ref chip's colour. Four kinds a reader can tell apart: the branch HEAD is on
// is the filled one, a branch is green, a remote branch is quiet, a tag is the
// brand colour outlined. HEAD itself is never drawn twice — it is *which* branch
// chip is filled.
const REF_TONE: Record<string, string> = { branch: TONE.ok, tag: TONE.accent };

// The lane glyphs get a fixed `ch` width so the shas line up down the page
// rather than stepping in and out with the branching.
//
// The subject is **not truncated**, and that is the fix the audit asked for: a
// subject cut mid-word by an ellipsis loses exactly the part that tells two
// commits apart. The list scrolls sideways instead — which is the thing a browser
// has and a 50-column terminal box does not.
//
// Which is also why a commit row carries no verb buttons where a REFS row does.
// A row wider than the box has no visible right edge to hang them off, and
// buttons parked past the fold are worse than none — so `y sha`, `v revert` and
// `p pick` live in the `HintBar`, which spans the page and follows the cursor.
// Every REFS row kind answers to different letters, which is why those stay on
// the row.
function CommitRow({
  commit,
  current,
  glyph,
  laneWidth,
  selected,
  onSelect,
}: {
  commit: LogEntryDto;
  current: string | null;
  glyph: string;
  laneWidth: number;
  selected: boolean;
  onSelect: () => void;
}) {
  // HEAD is not drawn twice: the daemon sends it as its own ref (named `HEAD`),
  // and what it means is *which branch chip is the filled one*. Matched against
  // `branches.current` rather than against "some head ref is on this commit",
  // which is what the vanilla client did — on a commit carrying two branches it
  // filled both, and one of them was checked out in another worktree.
  const refs = (commit.refs ?? []).filter((r) => r.kind !== "head");
  return (
    <Row
      selected={selected}
      onSelect={onSelect}
      title={`${commit.author} · ${commit.date} · ${commit.summary}`}
      {...vtarget("git.commit")}
    >
      {/* The one place geometry is inline: the column is as wide as the widest
          row's lanes, which is a number at runtime. */}
      <span className="shrink-0 whitespace-pre font-mono text-primary" style={{ minWidth: `${laneWidth}ch` }}>
        {glyph}
      </span>
      <span className="shrink-0 font-mono tabular-nums text-dim">{commit.id.slice(0, 7)}</span>
      {refs.map((r) =>
        r.kind === "branch" && r.name === current ? (
          <Badge key={`${r.kind}/${r.name}`}>{r.name}</Badge>
        ) : (
          <Badge key={`${r.kind}/${r.name}`} variant="outline" className={REF_TONE[r.kind]}>
            {r.name}
          </Badge>
        ),
      )}
      <span className="shrink-0 whitespace-nowrap">{commit.summary}</span>
    </Row>
  );
}
