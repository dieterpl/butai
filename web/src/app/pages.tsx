// The router's table: which component draws which page, and the adapter each
// one needs.
//
// The pages were written against their own prop shapes rather than one shared
// blob, and that is the right way round — a page that takes exactly what it
// draws can be rendered in a test or a gallery without a whole world behind it,
// and `FilesPage` needing four props is a statement that it depends on four
// things. The cost is this file, which is the one place that knows how the
// shell's state maps onto each of them.
//
// ## Three things happen here and nowhere else
//
// 1. **The workspace is bound.** `actions.ts` takes a qualified workspace id on
//    every call, because a verb is always *about* a project; a page's own
//    interface does not, because a page only ever has one. This file is the
//    join, and `withWs` is what says "no workspace" once instead of in thirty
//    methods.
// 2. **A cursor becomes a verb.** `press(surface, key)` is on two pages'
//    actions interfaces and is the only member of one that is not a call: it
//    needs the verb tables *and* the cursor, and the cursor is the shell's. So
//    it resolves the key through `verbs.ts` — the same table the footer drew the
//    hint from — and dispatches.
// 3. **A view verb is separated from a write.** `openWorktree` opens a workspace
//    when the worktree has none and switches tab when it does; `goToChanges` is
//    two `setState` calls with no daemon in them at all. Both arrive on a page's
//    *actions* interface, because from the page they are one gesture.

import { useEffect, useRef, useState } from "react";
import type { PageName, PageProps } from "./Shell.tsx";
import { api } from "../logic/api.ts";
import {
  HomeRowKind,
  allAgentRows,
  fleetSpaces,
  homePreview,
  homeRows,
  machineRows,
  toggleAllSpaces,
  toggleFold,
} from "../logic/fleet.ts";
import { daemonOf, qid, type Qid, type QualifiedWorkspace } from "../logic/events.ts";
import {
  ChangesRow,
  VerbId,
  agentsVerbs,
  changesHelpVerbs,
  homeVerbs,
  procsVerbs,
  type Verb,
} from "../logic/verbs.ts";
import type { UsageDto } from "../protocol/generated/protocol.ts";

import { WorkPage, type WorkActions, type WorkView } from "../pages/WorkPage.tsx";
import { HomePage, type HomeActions, type HomeCallbacks } from "../pages/HomePage.tsx";
import { GitPage, type GitActions } from "../pages/GitPage.tsx";
import { FilesPage, type FilesActions } from "../pages/FilesPage.tsx";
import { DockerPage, FOLLOWER, type DockerActions } from "../pages/DockerPage.tsx";
import { SettingsPage } from "../pages/SettingsPage.tsx";
import { UsagePage } from "../pages/UsagePage.tsx";
import { HelpPage } from "../pages/HelpPage.tsx";

// ---------------------------------------------------------------------------
// Binding the workspace
// ---------------------------------------------------------------------------

/**
 * Run `f` with the current workspace's qualified id, or say there is none.
 *
 * `web/ui/actions.js` opened every verb with `if (!wsId) { say("no workspace"); return; }`
 * and that guard is the whole content of this: a client with no project open can
 * still press `stage`, and "nothing happened" is the worst of the three possible
 * answers.
 */
function withWs(p: PageProps, f: (ws: Qid) => unknown): void {
  if (p.ws) void f(p.ws.id);
  else p.actions.toast("no workspace");
}

/** What a pane is called, for the question `kill` asks before ending it. */
function paneLabel(ws: QualifiedWorkspace | null, pane: Qid): string {
  const id = String(pane);
  const agent = ws?.agents.find((a) => String(a.pane) === id);
  if (agent) return agent.title || "this agent";
  const proc = ws?.processes.find((x) => String(x.pane) === id);
  return proc ? proc.name : "this pane";
}

/**
 * Which kind of row the CHANGES cursor is on, from the file it has open.
 *
 * The shell tracks one path rather than a row index, and the kind falls out of
 * looking that path up: a file is conflicted, staged or unstaged and cannot be
 * two of those — `ChangesDto`'s own contract is that a conflicted file is *not*
 * in `unstaged`. So the footer gets the right arm of `changesFooter` without the
 * shell holding a second piece of state that could disagree with the first.
 */
function changesRowOf(ws: QualifiedWorkspace | null, path: string | null): ChangesRow | null {
  const ch = ws?.changes;
  if (!ch || !path) return null;
  if (ch.conflicted.some((f) => f.path === path)) return ChangesRow.Conflict;
  if (ch.staged.some((f) => f.path === path)) return ChangesRow.Staged;
  if (ch.unstaged.some((f) => f.path === path)) return ChangesRow.Unstaged;
  return null;
}

// ---------------------------------------------------------------------------
// A footer hint is a button
// ---------------------------------------------------------------------------

/**
 * The verb a key runs on a surface, out of the table the footer drew it from.
 *
 * Reading the table rather than switching on the letter is the property
 * `verbs.ts` exists for: `x` kills on two rails and `t` starts a shell on one and
 * takes theirs on another, and a dispatch written in letters is where that goes
 * wrong. CHANGES uses `changesHelpVerbs()` — every key the rail answers,
 * whatever is selected — because a key stays bound when it does not earn a
 * footer column.
 */
function verbFor(surface: string, key: string, pin: string | null): Verb | null {
  const table =
    surface === "agents" ? agentsVerbs(!!pin)
    : surface === "procs" ? procsVerbs()
    : surface === "changes" ? changesHelpVerbs()
    : surface === "home" ? homeVerbs()
    : [];
  return table.find((v) => v.key === key) ?? null;
}

/**
 * Run the verb a footer key names, on WORK.
 *
 * The verbs that need a cursor take the one the shell has: `view.pane` for the
 * rails — the pane on the stage is the selected pane, which is what makes `x`
 * unambiguous — and `view.path` for CHANGES, whose kind decides whether `s`
 * stages or `u` unstages.
 *
 * **Navigation is not dispatched.** `j`, `k`, `tab`, `esc` and `enter` are
 * `quiet` in every table, so no footer draws them and nothing here can be
 * reached by clicking one; the keyboard that will send them is `logic/keys.ts`'s
 * and it moves a cursor rather than running a verb. Saying so is the same answer
 * `web/ui/actions.js` gave, and for the same reason: acting on the wrong row is
 * worse than not acting.
 */
function pressWork(p: PageProps, act: WorkActions, surface: string, key: string): void {
  const verb = verbFor(surface, key, p.view.pin);
  if (!verb) {
    p.actions.toast(`${key} is not bound on ${surface}`);
    return;
  }
  const pane = p.view.pane;
  const path = p.view.path;
  const needPane = (f: (pane: Qid) => void) => (pane != null ? f(pane) : p.actions.toast("nothing selected"));
  const needPath = (f: (path: string) => void) => (path ? f(path) : p.actions.toast("no file selected"));

  switch (verb.id) {
    case VerbId.NewAgent: return act.spawn(false);
    case VerbId.PickAgent: return act.spawn(true);
    case VerbId.Ack: return needPane((x) => act.ack(x));
    case VerbId.Kill: return needPane((x) => act.kill(x));
    case VerbId.NewShell: return act.newProc();
    case VerbId.Restart: return needPane((x) => act.restart(x));
    case VerbId.Stage: return needPath((x) => act.stage(x));
    case VerbId.Unstage: return needPath((x) => act.unstage(x));
    case VerbId.Diff:
      return needPath((x) => act.openDiff({ path: x, staged: changesRowOf(p.ws, x) === ChangesRow.Staged }));
    case VerbId.ResolveOurs: return needPath((x) => act.resolve(x, "ours"));
    case VerbId.ResolveTheirs: return needPath((x) => act.resolve(x, "theirs"));
    case VerbId.ResolveDone: return needPath((x) => act.resolve(x, "resolved"));
    case VerbId.Commit: return act.commit("");
    case VerbId.CommitAll: return act.commitAll("");
    case VerbId.SeqContinue: return act.sequence("continue");
    case VerbId.SeqAbort: return act.sequence("abort");
    case VerbId.Fetch: return act.fetch();
    case VerbId.Pull: return act.pull();
    case VerbId.Push: return act.push();
    case VerbId.Branch: return act.branch();
    case VerbId.Refresh: return p.actions.refresh();
    case VerbId.Help: return p.on.setPage("help");
    case VerbId.FocusStage: return p.on.setFocus("stage");
    default:
      p.actions.toast(`${verb.label} is the keyboard's, not a button`);
  }
}

// ---------------------------------------------------------------------------
// USAGE
// ---------------------------------------------------------------------------

/**
 * `GET /api/usage`, on the page that shows it.
 *
 * Not in `world.ts`, deliberately. The world is what every page reads and what
 * the event stream keeps current; usage is one page's data, has no event, and
 * costs a round trip to each machine's agent CLIs — polling it for pages that
 * never draw it would be work nobody asked for. Fetched when USAGE is opened and
 * refreshed on demand.
 *
 * `daemon` is the *active* machine, not the primary. An account limit is a fact
 * about the box the CLI logs in from, so a bridge serving two machines would
 * otherwise report the wrong account confidently — the terminal reads the active
 * daemon for the same reason (`workbench.rs`'s `refresh_usage`).
 */
function useUsage(daemon: string | null): [UsageDto | null, boolean, () => void] {
  const [usage, setUsage] = useState<UsageDto | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [nonce, setNonce] = useState(0);
  useEffect(() => {
    let alive = true;
    setLoaded(false);
    (daemon ? api.usage(daemon) : api.usage())
      .then((u) => {
        if (alive) {
          setUsage(u);
          setLoaded(true);
        }
      })
      .catch(() => {
        if (alive) setLoaded(true);
      });
    return () => {
      alive = false;
    };
  }, [daemon, nonce]);
  return [usage, loaded, () => setNonce((n) => n + 1)];
}

function Usage(p: PageProps) {
  const daemon = p.ws ? (p.ws.daemon ?? daemonOf(p.ws.id)) : null;
  const [usage, loaded, refresh] = useUsage(daemon);
  const entry = p.world.daemons.find((d) => d.key === daemon) ?? p.world.daemons.find((d) => d.primary);
  return <UsagePage usage={usage} loaded={loaded} machine={entry?.label ?? null} onRefresh={refresh} />;
}

// ---------------------------------------------------------------------------
// WORK
// ---------------------------------------------------------------------------

function Work(p: PageProps) {
  const act: WorkActions = {
    spawn: (choose) => withWs(p, (w) => p.actions.spawnPick(w, choose, p.view.pin)),
    ack: (pane) => withWs(p, (w) => p.actions.ack(w, pane)),
    kill: (pane) => withWs(p, (w) => p.actions.kill(w, pane, paneLabel(p.ws, pane))),
    newProc: () => withWs(p, (w) => p.actions.newProc(w)),
    restart: (pane) => withWs(p, (w) => p.actions.restart(w, pane, paneLabel(p.ws, pane))),
    stage: (path) => withWs(p, (w) => p.actions.stage(w, path)),
    unstage: (path) => withWs(p, (w) => p.actions.unstage(w, path)),
    // An empty message is the footer's `c`, which has no field to read: the rail
    // passes what was typed into its own box, and anything else asks.
    commit: (message) =>
      withWs(p, (w) => (message.trim() ? p.actions.commit(w, message) : p.actions.commitAsk(w, false))),
    commitAll: (message) =>
      withWs(p, (w) => (message.trim() ? p.actions.commitAll(w, message) : p.actions.commitAsk(w, true))),
    resolve: (path, take) => withWs(p, (w) => p.actions.resolve(w, path, take)),
    sequence: (action) => withWs(p, (w) => p.actions.sequence(w, action)),
    fetch: () => withWs(p, (w) => p.actions.fetch(w)),
    pull: () => withWs(p, (w) => p.actions.pull(w)),
    push: () => withWs(p, (w) => p.actions.push(w)),
    branch: () => withWs(p, (w) => p.actions.branch(w)),
    openDiff: (what) => {
      // Two things, and only one of them is a call: the row draws as selected
      // because the shell moved its cursor, and the diff arrives because the
      // daemon was asked. A page that took them as two gestures would be a page
      // that can show a diff for a row it has not selected.
      p.on.setPath(what.path);
      withWs(p, (w) => p.actions.openDiff(w, what));
    },
    showCommit: (id) => withWs(p, (w) => p.actions.showCommit(w, id)),
    press: (surface, key) => pressWork(p, act, surface, key),
  };

  const row = changesRowOf(p.ws, p.view.path);
  const view: WorkView = {
    pane: p.view.pane,
    path: p.view.path,
    pin: p.view.pin,
    busy: p.view.busy,
    rails: p.view.rails,
    ...(row ? { changesRow: row } : {}),
  };

  return (
    <WorkPage
      world={p.world}
      ws={p.ws}
      actions={act}
      focus={p.focus}
      on={{ selectPane: (pane) => p.on.setPane(pane), rails: (open) => p.on.setRails(open) }}
      view={view}
      theme={p.term}
      fontPx={p.view.fontPx}
      stage={p.stage}
    />
  );
}

// ---------------------------------------------------------------------------
// HOME
// ---------------------------------------------------------------------------

function Home(p: PageProps) {
  // The same rows the page draws, from the same pure functions: the cursor
  // counts rows of exactly that list, so deriving it twice is the one thing
  // here that must not drift — which is why `fleet.ts` is pure and neither side
  // keeps a copy.
  const rows = allAgentRows(p.world.workspaces, p.world.daemons);
  const machines = machineRows(p.world.daemons, rows);
  const spaces = fleetSpaces(p.world.workspaces, p.world.daemons, rows, p.view.pin);
  const list = homeRows(spaces, machines, p.view.folds);
  const at = Math.min(p.view.sel, Math.max(0, list.length - 1));
  const row = list[at] ?? null;
  const previewed = homePreview(list, at);
  const cursor = previewed == null ? null : (rows[previewed] ?? null);

  const on: HomeCallbacks = {
    walk: (sel) => p.on.setSel(sel),
    // Both halves are needed and both are the shell's: the workspace to switch
    // to and the pane to stage may be on a machine that is not the active tab's.
    open: ({ ws, pane }) => {
      p.on.setWsId(String(ws));
      p.on.setPane(pane);
      p.on.setPage("work");
    },
    // A project has no pane to stage, so this goes there and leaves the
    // workspace showing whatever it had.
    go: (ws) => {
      p.on.setWsId(String(ws));
      p.on.setPage("work");
    },
    fold: (folds) => p.on.setFolds(folds),
    // Starting an agent leaves you on HOME: the new row appears in the fleet
    // and the preview points at it, which is the whole of what you wanted to
    // see. A button that started something *and* threw the page onto another
    // machine is the bug that made agent rows two-step.
    start: (space) => {
      void p.actions.spawnPick(space.ws, false, space.preferred);
    },
  };

  const act: HomeActions = {
    press: (surface, key) => {
      const verb = verbFor(surface, key, p.view.pin);
      // `enter` reads the row: an agent goes to that agent, a project goes to
      // that workspace, a machine folds — one key, and the row says which.
      if (verb?.id === VerbId.OpenAgent) {
        if (row?.kind === HomeRowKind.Space) on.go(row.space.ws);
        else if (cursor && row?.kind === HomeRowKind.Agent) on.open({ ws: cursor.ws, pane: cursor.pane });
        else p.actions.toast("nothing to open here");
        return;
      }
      if (verb?.id === VerbId.NewAgent) {
        if (row?.kind === HomeRowKind.Space) on.start(row.space);
        else p.actions.toast("put the cursor on a project to start an agent");
        return;
      }
      if (verb?.id === VerbId.Fold) {
        const f = p.view.folds;
        if (row?.kind === HomeRowKind.Machine) {
          on.fold({ ...f, machines: toggleFold(f.machines, row.label ?? "") });
        } else if (row?.kind === HomeRowKind.Space) {
          on.fold({ ...f, spaces: toggleFold(f.spaces, row.space.ws) });
        } else if (row?.kind === HomeRowKind.Agent) {
          // `z` on an agent folds the project it is *in*, and takes the cursor
          // up to that row — the only move that leaves the cursor on something
          // you can still see.
          const header = list.slice(0, at).findLastIndex((r) => r.kind === HomeRowKind.Space);
          const space = header >= 0 ? list[header] : null;
          if (space && space.kind === HomeRowKind.Space) {
            p.on.setSel(header);
            on.fold({ ...f, spaces: toggleFold(f.spaces, space.space.ws) });
          }
        }
        return;
      }
      if (verb?.id === VerbId.FoldAll) {
        on.fold(toggleAllSpaces(p.view.folds, spaces));
        return;
      }
      if (verb?.id === VerbId.Help) {
        p.on.setPage("help");
        return;
      }
      p.actions.toast(verb ? `${verb.label} is the keyboard's, not a button` : `${key} is not bound on ${surface}`);
    },
  };

  return (
    <HomePage
      world={p.world}
      actions={act}
      on={on}
      sel={at}
      folds={p.view.folds}
      pin={p.view.pin}
      pane={cursor ? cursor.pane : null}
      theme={p.term}
      fontPx={p.view.fontPx}
      stage={p.stage}
    />
  );
}

// ---------------------------------------------------------------------------
// GIT
// ---------------------------------------------------------------------------

function Git(p: PageProps) {
  const act: GitActions = {
    // Neither of these reaches a daemon. The first is two pieces of shell state,
    // the second is an overlay the shell draws — and both arrive here because
    // from the page they are one gesture, which is the right way for a page to
    // ask.
    goToChanges: () => {
      p.on.setPage("work");
      p.on.setFocus("changes");
    },
    gitMenu: () => p.on.gitMenu(),
    checkout: (branch) => withWs(p, (w) => p.actions.checkout(w, branch)),
    merge: (branch) => withWs(p, (w) => p.actions.merge(w, branch)),
    deleteBranch: (branch) => withWs(p, (w) => p.actions.deleteBranch(w, branch)),
    fetchRemote: (name) => withWs(p, (w) => p.actions.fetchRemote(w, name)),
    deleteTag: (name) => withWs(p, (w) => p.actions.deleteTag(w, name)),
    stashPop: (index) => withWs(p, (w) => p.actions.stashPop(w, index)),
    stashDrop: (index) => withWs(p, (w) => p.actions.stashDrop(w, index)),
    openWorktree: (wt) => {
      // A worktree butai already has a workspace for is a *tab*, not a
      // `newWorkspace`: opening a second workspace on one directory would give
      // two rails over the same tree, and the daemon's own row carries the id
      // precisely so a client can offer "go there" instead.
      if (wt.workspace != null && p.ws) {
        p.on.setWsId(String(qid(p.ws.daemon ?? daemonOf(p.ws.id), wt.workspace)));
        p.on.setPage("work");
        return;
      }
      withWs(p, async (w) => {
        const made = await p.actions.openWorktree(w, wt);
        if (made != null) {
          p.on.setWsId(String(made));
          p.on.setPage("work");
        }
      });
    },
    removeWorktree: (path) => withWs(p, (w) => p.actions.removeWorktree(w, path)),
    copySha: (sha) => void p.actions.copySha(sha),
    revert: (rev) => withWs(p, (w) => p.actions.revert(w, rev)),
    cherryPick: (rev) => withWs(p, (w) => p.actions.cherryPick(w, rev)),
    cancelOp: () => withWs(p, (w) => p.actions.cancelOp(w)),
  };

  return (
    <GitPage
      world={p.world}
      ws={p.ws}
      actions={act}
      focus={p.focus}
      on={{ focus: (where) => p.on.setFocus(where) }}
      op={p.world.gitOp}
    />
  );
}

// ---------------------------------------------------------------------------
// FILES and DOCS
// ---------------------------------------------------------------------------

function files(p: PageProps): FilesActions {
  return {
    upload: async (req) => {
      if (!p.ws) {
        p.actions.toast("no workspace");
        return false;
      }
      return p.actions.upload(p.ws.id, req);
    },
    deleteFile: async (path) => {
      if (!p.ws) {
        p.actions.toast("no workspace");
        return false;
      }
      return p.actions.deleteFile(p.ws.id, path);
    },
    toast: (message) => p.actions.toast(message),
  };
}

function Files(p: PageProps) {
  return <FilesPage ws={p.ws} kind="files" prefix={p.view.prefix} actions={files(p)} />;
}

function Docs(p: PageProps) {
  return <FilesPage ws={p.ws} kind="docs" prefix={p.view.prefix} actions={files(p)} />;
}

// ---------------------------------------------------------------------------
// DOCKER
// ---------------------------------------------------------------------------

/**
 * The follower panes in this workspace — every process DOCKER named `logs:…`.
 *
 * By prefix rather than by remembering what was started: the daemon's process
 * list is the truth, a reload loses any state the client kept, and a follower
 * left behind by a previous visit is exactly the one that has to be reaped.
 */
function followers(ws: QualifiedWorkspace | null): Qid[] {
  return (ws?.processes ?? []).filter((x) => x.name.startsWith(FOLLOWER)).map((x) => x.pane);
}

function Docker(p: PageProps) {
  const act: DockerActions = {
    logs: (req) =>
      withWs(p, (w) => p.actions.dockerLogs(w, req, followers(p.ws).filter((pane) => !isNamed(p.ws, pane, req.name)))),
    run: (req) => withWs(p, (w) => p.actions.dockerRun(w, req)),
  };

  // Reap on the way out. A `docker logs -f` follower is a live process with a
  // PTY behind it, so one left per visit is a machine full of them by lunchtime
  // — and this is the half the page cannot do, because by the time it matters
  // the page is gone. `killPaneBeacon` is the same reap for a closing tab, where
  // an ordinary fetch does not outlive the document.
  const last = useRef<{ ws: Qid; panes: Qid[] } | null>(null);
  useEffect(() => {
    last.current = p.ws ? { ws: p.ws.id, panes: followers(p.ws) } : null;
  });
  // Registered once, for the life of the page. What to reap comes off the ref
  // above rather than out of this closure, so a follower started after mount is
  // still the one that goes.
  useEffect(() => {
    const beacon = () => {
      const cur = last.current;
      if (cur) for (const pane of cur.panes) void api.killPaneBeacon(String(cur.ws), String(pane));
    };
    window.addEventListener("pagehide", beacon);
    return () => {
      window.removeEventListener("pagehide", beacon);
      const cur = last.current;
      if (cur) for (const pane of cur.panes) void p.actions.stopLogs(cur.ws, pane);
    };
  }, []);

  return (
    <DockerPage
      world={p.world}
      ws={p.ws}
      pane={followers(p.ws)[0] ?? null}
      theme={p.term}
      fontPx={p.view.fontPx}
      stage={p.stage}
      actions={act}
    />
  );
}

function isNamed(ws: QualifiedWorkspace | null, pane: Qid, name: string): boolean {
  const id = String(pane);
  return (ws?.processes ?? []).some((x) => String(x.pane) === id && x.name === name);
}

// ---------------------------------------------------------------------------
// SETTINGS, USAGE's siblings
// ---------------------------------------------------------------------------

function Settings(p: PageProps) {
  return (
    <SettingsPage
      world={p.world}
      actions={p.actions}
      on={{ close: () => p.on.setPage("work") }}
      facts={p.facts}
      ws={p.ws}
      focus={p.focus}
    />
  );
}

function Help(p: PageProps) {
  return (
    <HelpPage
      prefix={p.view.prefix}
      topic={p.view.topic}
      onTopic={(slug) => p.on.setTopic(slug)}
      onClose={() => p.on.setPage("work")}
    />
  );
}

export const PAGE_TABLE: Record<PageName, (p: PageProps) => React.ReactNode> = {
  work: Work,
  home: Home,
  git: Git,
  files: Files,
  docs: Docs,
  docker: Docker,
  usage: Usage,
  settings: Settings,
  help: Help,
};
