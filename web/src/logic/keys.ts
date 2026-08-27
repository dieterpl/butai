// The keyboard, in one place.
//
// `verbs.ts` is the table; this is the only thing that reads a `keydown`. The
// split matters: the table has no DOM in it and can be run under node, and this
// file has no key letters in it — every letter it acts on came out of a table,
// which is what makes "nothing is bound that cannot be found" a property rather
// than a habit.
//
// ## The browser is the hard part, and it is not the same hard part as a terminal
//
// The TUI owns every keystroke the moment the terminal hands it one. A browser
// does not: the page sits under the browser's own accelerators and the OS's, and
// under this page's *own* terminal, which deliberately forwards Ctrl-C, Ctrl-D
// and the rest straight to the pane. So the order below is the whole design:
//
// 1. **A modal has the keyboard.** It is a question; nothing behind it moves.
// 2. **The prefix layer wins next.** `C-b` is a chord the browser has no claim
//    on, which is exactly why `docs/keys.md` has one — there it is for terminals
//    that eat Alt, here for browsers that do. Twice sends a literal through.
// 3. **The Alt layer, and only the keys some table binds.** Anything else falls
//    through untouched, so `alt-b` and `alt-f` still move by words in readline.
//    This is a promise about *absent* bindings, so `ALT_MUST_FALL_THROUGH` and
//    `check.py` hold it up.
// 4. **Bare keys, only off the stage.** On it every key is the program's, which
//    is what makes it a terminal and not a preview.
//
// What we cannot do, and do not pretend to: a browser accelerator that the page
// is not allowed to cancel arrives never, or arrives and is acted on twice.
// Alt+digit (tab switching in Chrome and Firefox on Linux and Windows) is the
// one this client binds anyway, and every such key carries a `note` in
// `verbs.ts` that the `?` reference prints beside it. The prefix spelling is the
// one that always arrives.
//
// ## Dispatch runs the click target
//
// A surface verb does not call an app method — it finds the element the registry
// says that verb reaches and clicks it. So the keyboard and the pointer are not
// two implementations that agree, they are one path with two entrances, and a
// button that moves takes its key with it.

import {
  VerbId, GLOBAL, TARGETS, altVerb, prefixVerb, altKeyName, keyName, isPrefix,
  DEFAULT_PREFIX, PREFIX_STORAGE_KEY, ALT_MUST_FALL_THROUGH,
  agentsVerbs, procsVerbs, changesFooter, ChangesRow, filesVerbs, dockerVerbs, DockerRow,
  gitFooter, GitRow, overlayVerbs, OverlayKind, homeVerbs, settingsVerbs, SettingRow,
  type Verb, type KeyEventLike, type Prefix,
} from "./verbs.ts";
import { keyMsg } from "./protocol.ts";
import type { ClientMsg, WorkspaceDetail } from "../protocol/generated/protocol.ts";

/// A clickable thing the registry can reach. `disabled` is a button's and is
/// absent on everything else, which is exactly the falsy the dispatch reads.
export type VerbEl = HTMLElement & { readonly disabled?: boolean };

/// A surface's host element — a custom element with a shadow root, plus the
/// hooks the registry asks for with `?.`. They are optional because the pages
/// are separate elements and only some of them answer.
export type SurfaceEl = HTMLElement & {
  /// FILES: an editor is open, so the table is the editing one.
  readonly editing?: unknown;
  drawFooter?(): void;
  drawFooters?(): void;
  moved?(): void;
};

/// SETTINGS also knows whether a choice is expanded, and how to leave.
export type SettingsEl = SurfaceEl & { readonly open?: unknown; leave(): void };

/// GIT also knows which of its three columns is live, and how to scroll the
/// one that is a diff rather than a list.
export type GitEl = SurfaceEl & { readonly column?: string; scrollBody(delta: number): void };

/// The stage — the only element here that talks to the daemon. `null` is in
/// the signature because `keyMsg` can answer with one, and the only caller
/// below hands its answer straight over.
export type StageEl = HTMLElement & { send(msg: ClientMsg | null): void };

/// The elements the keyboard reaches, as the shell hands them over.
export type AppElements = {
  home: SurfaceEl;
  agents: SurfaceEl;
  procs: SurfaceEl;
  changes: SurfaceEl;
  files: SurfaceEl;
  docs: SurfaceEl;
  settings: SettingsEl;
  docker: SurfaceEl;
  git: GitEl;
  stage: StageEl;
  modalRoot: HTMLElement;
};

/// The shell, as much of it as the keyboard touches.
///
/// `butai-app.js` is the view layer and is still JavaScript, so its shape is
/// written out rather than imported. Every member below is one this file
/// actually calls, which makes it the list of what a client has to provide to
/// get the keyboard — and the row-kind methods are typed by the table that
/// consumes them, so a surface cannot answer with a kind its own table has
/// never heard of.
export type App = {
  el: AppElements;
  /// Where the cursor is on each surface, by surface name.
  cursor: Record<string, number>;
  focus: string;
  page: string;
  settings: { readonly defaultAgent?: string | null };
  currentWs(): WorkspaceDetail | null;
  changesRowKind(): ChangesRow;
  settingsRowKind(): SettingRow;
  dockerRowKind(): DockerRow;
  gitRowKind(): GitRow;
  overlayKind(): OverlayKind;
  modalOpen(): boolean;
  showPrefix(on: boolean): void;
  toast(msg: string, level?: string): void;
  poll(): void;
  pointHome(): void;
  setPage(p: string): void;
  setFocus(name: string, moveTerminal?: boolean): void;
  cycleFocus(delta: number): void;
  toggleSpace(p: string): void;
  walkSpace(delta: number): void;
  walkWorkspace(delta: number): void;
  selectWorkspaceAt(i: number): void;
  newWorkspace(): void;
  killWorkspace(ws: WorkspaceDetail): void;
  spawnAgent(pick: boolean): void;
  newProcess(): void;
  toggleZen(force?: boolean): void;
  showHelp(): void;
  showAlerts(): void;
  pasteImage(): void;
  setFont(delta: number): void;
};

/// One surface's registry entry. Named for the entry rather than the surface,
/// because `verbs.ts` already exports a `Surface` and it is a different thing —
/// there it is a section of the `?` reference.
export type SurfaceEntry = {
  el(app: App): SurfaceEl;
  /// The click targets that are *rows* here — a function on the one surface
  /// whose answer changes.
  rows: readonly string[] | ((app: App) => readonly string[]);
  /// Verb id -> the click targets that verb reaches, in preference order.
  verbs: Readonly<Partial<Record<VerbId, readonly string[]>>>;
  footer?(app: App): void;
  moved?(app: App): void;
  scroll?(app: App, delta: number): void;
  table(app: App): readonly Verb[];
};

/// A composed path, as *this program's* `Event` declares it.
///
/// `@types/bun` merges its own `composedPath(): [EventTarget?]` into the DOM's
/// `Event`, so the call resolves to a one-element tuple rather than an array —
/// and the moment the `?:` below makes that a union with `[]`, `find` loses its
/// narrowing overload and answers `EventTarget`. Widening it to one array type
/// is what puts the narrowing back.
type EventPath = readonly (EventTarget | undefined)[];

/// Which DOM target each surface's keys click, and where its rows are.
///
/// The keys are not written here — they come from the tables — but the mapping
/// from a verb to the thing on screen has to live somewhere, and this is the
/// one place. A verb id with no entry is a verb that does nothing, which
/// `check.py`'s `verbs/dispatchable` reports.
const SURFACES: Record<string, SurfaceEntry> = {
  // HOME's fleet. The only surface whose rows come from a *cross-daemon* list,
  // which is why `moved` exists: the preview is a live socket to one machine and
  // it has to follow the cursor onto whichever machine the next row is on.
  //
  // `moved` reads the row element rather than an index into a model, so the
  // screen in the middle and the row under the cursor are one fact. A list that
  // redrew under the cursor would otherwise preview the row you left.
  home: {
    el: (app) => app.el.home,
    rows: ["home.row"],
    verbs: {
      [VerbId.OpenAgent]: ["home.open"],
      // The three the fleet gained when its rows became projects rather than
      // headers over agents. Each is a button on the row as well as a key, so
      // each names the element the press lands on — the same rule the AGENTS
      // rail's `[+ agent]` follows.
      [VerbId.NewAgent]: ["home.new"],
      [VerbId.Kill]: ["home.close"],
      [VerbId.Fold]: ["home.fold"],
      [VerbId.FoldAll]: ["home.foldAll"],
    },
    footer: (app) => app.el.home?.drawFooter?.(),
    moved: (app) => app.pointHome(),
    table: () => homeVerbs(),
  },
  agents: {
    el: (app) => app.el.agents,
    rows: ["agents.row"],
    verbs: {
      [VerbId.NewAgent]: ["agents.new"],
      // `agents.pick` exists only while an agent is pinned, which is the only
      // state in which the two verbs differ: unpinned, `a` opens the very
      // chooser `A` would, so the one button is the honest answer to both.
      [VerbId.PickAgent]: ["agents.pick", "agents.new"],
      [VerbId.Ack]: ["agents.ack"],
      [VerbId.Kill]: ["agents.kill"],
    },
    // Two arms, and which one is live is a *setting*: pinned, `a` spawns that
    // agent and `A` is the only route to the others, so both are worth a
    // column. The footer, the dispatch and `?` all read this one call.
    table: (app) => agentsVerbs(!!app.settings.defaultAgent),
  },
  procs: {
    el: (app) => app.el.procs,
    rows: ["procs.row"],
    verbs: {
      [VerbId.NewShell]: ["procs.new"],
      [VerbId.Restart]: ["procs.restart"],
      [VerbId.Kill]: ["procs.kill"],
    },
    table: () => procsVerbs(),
  },
  changes: {
    el: (app) => app.el.changes,
    rows: ["changes.row"],
    verbs: {
      [VerbId.Stage]: ["changes.stage"],
      [VerbId.Unstage]: ["changes.unstage"],
      [VerbId.ResolveOurs]: ["changes.ours"],
      [VerbId.ResolveTheirs]: ["changes.theirs"],
      [VerbId.ResolveDone]: ["changes.resolved"],
      [VerbId.Commit]: ["changes.commit"],
      [VerbId.CommitAll]: ["changes.commitAll"],
      [VerbId.Branch]: ["changes.branch"],
      [VerbId.Push]: ["changes.push"],
      [VerbId.Pull]: ["changes.pull"],
      [VerbId.Fetch]: ["changes.fetch"],
      [VerbId.SeqContinue]: ["changes.seq.continue"],
      [VerbId.SeqAbort]: ["changes.seq.abort"],
      [VerbId.Diff]: ["changes.row"],
    },
    footer: (app) => app.el.changes?.drawFooter?.(),
    // The rail's verbs follow the row the cursor is on, exactly as the TUI's do.
    //
    // The second read of `currentWs()` repeats the call rather than the value,
    // exactly as the JS did — it is guarded by the first, which the compiler
    // cannot see across two calls, so the `!`s say what the `&&` already knows.
    table: (app) => changesFooter(app.changesRowKind(), {
      ahead: app.currentWs()?.changes?.ahead || 0,
      sequence: !!(app.currentWs()?.changes?.state && app.currentWs()!.changes!.state !== "clean"),
    }),
  },
  // FILES and DOCS are one surface over two listings — the terminal's
  // `page_tree()`, which asks which of its two `Files` structs a key, a click
  // or a fetch means rather than letting each site decide. So `el` follows the
  // page and everything else here is written once: one cursor, one table, one
  // section of `?`, and a `docs.row` target that would have had to shadow
  // `files.row` in the registry never exists.
  files: {
    el: (app) => (app.page === "docs" ? app.el.docs : app.el.files),
    rows: ["files.row"],
    verbs: {
      [VerbId.Edit]: ["files.edit"],
      [VerbId.Save]: ["files.save"],
      [VerbId.CancelEdit]: ["files.cancel"],
      [VerbId.ViewFile]: ["files.view.file"],
      [VerbId.ViewDiff]: ["files.view.diff"],
      [VerbId.Download]: ["files.download"],
      [VerbId.Upload]: ["files.upload"],
      [VerbId.DeleteFile]: ["files.delete"],
    },
    table: (app) => filesVerbs(!!SURFACES.files!.el(app).editing),
  },
  // The one page that is about this client rather than about a project.
  //
  // `rows` is a function for the GIT page's reason and a sharper one: while a
  // choice is expanded the cursor is walking *its options*, and the row it
  // belongs to has not moved. Two lists, and only one of them is live.
  settings: {
    el: (app) => app.el.settings,
    rows: (app) => (app.el.settings?.open != null ? ["settings.option"] : ["settings.row"]),
    verbs: {
      [VerbId.SettingChange]: ["settings.row"],
      [VerbId.SettingChoose]: ["settings.option"],
      [VerbId.SettingKeep]: ["settings.keep"],
      [VerbId.SettingToggle]: ["settings.row"],
      [VerbId.SettingBigger]: ["settings.bigger"],
      [VerbId.SettingSmaller]: ["settings.smaller"],
      [VerbId.SettingAuto]: ["settings.auto"],
      [VerbId.CloseSettings]: ["settings.close"],
    },
    footer: (app) => app.el.settings?.drawFooter?.(),
    // Walking an open theme list *is* the preview: the palette on screen is a
    // function of where the cursor is, so it has to be recomputed after any
    // move. The same hook HOME's live pane uses, for the same reason.
    moved: (app) => app.el.settings?.moved?.(),
    table: (app) => settingsVerbs(app.settingsRowKind()),
  },
  docker: {
    el: (app) => app.el.docker,
    rows: ["docker.stack", "docker.container"],
    verbs: {
      [VerbId.DockerRestart]: ["docker.restart"],
      [VerbId.DockerStop]: ["docker.stop"],
      [VerbId.DockerShell]: ["docker.shell"],
      [VerbId.DockerLogs]: ["docker.stack", "docker.container"],
    },
    table: (app) => dockerVerbs(app.dockerRowKind()),
  },
  // Two lists in one surface, and `rows` names both: `tab` walks REFS →
  // HISTORY → the commit body, but the *cursor* the bare keys act on is
  // whichever list the page says is live, so `rowsOf` is asked for that one.
  // This is the only surface whose rows depend on its own sub-focus, which is
  // why `rows` is a function here.
  git: {
    el: (app) => app.el.git,
    rows: (app) => (app.el.git?.column === "history" ? ["git.commit"]
      : app.el.git?.column === "body" ? [] : ["git.ref"]),
    scroll: (app, delta) => app.el.git?.scrollBody(delta),
    verbs: {
      [VerbId.Checkout]: ["git.checkout"],
      [VerbId.Merge]: ["git.merge"],
      [VerbId.DeleteBranch]: ["git.branch.delete"],
      [VerbId.Fetch]: ["git.fetch"],
      [VerbId.TagDelete]: ["git.tag.delete"],
      [VerbId.StashPop]: ["git.stash.pop"],
      [VerbId.StashDrop]: ["git.stash.drop"],
      [VerbId.RemoveWorktree]: ["git.worktree.remove"],
      [VerbId.CopySha]: ["git.sha"],
      [VerbId.Revert]: ["git.revert"],
      [VerbId.CherryPick]: ["git.pick"],
      [VerbId.GitMenu]: ["git.menu"],
      [VerbId.Refresh]: ["git.refresh"],
      [VerbId.ScopeAll]: ["git.scope.all"],
      [VerbId.CancelOp]: ["git.op.cancel"],
      // The five reads `enter` means, all of them the row itself. `Open` is
      // what runSurface turns `enter` into, so these are here for the registry
      // and for `?`, not as a second path.
      [VerbId.Scope]: ["git.ref"],
      [VerbId.GoToChanges]: ["git.ref"],
      [VerbId.OpenWorktree]: ["git.ref"],
      [VerbId.Show]: ["git.ref", "git.commit"],
    },
    footer: (app) => app.el.git?.drawFooters?.(),
    table: (app) => gitFooter(app.gitRowKind()),
  },
};

/// Every overlay's keys click one of these.
const OVERLAY_TARGETS: Readonly<Partial<Record<VerbId, readonly string[]>>> = {
  [VerbId.Accept]: ["overlay.accept"],
  [VerbId.Cancel]: ["overlay.cancel"],
  [VerbId.Clear]: ["overlay.clear"],
  [VerbId.ClearAll]: ["overlay.clearAll"],
  [VerbId.NewFolder]: ["overlay.newFolder"],
};

export class Keys {
  app: App;
  pending: boolean;
  prefix: Prefix;

  constructor(app: App) {
    this.app = app;
    this.pending = false;      // the prefix is armed
    this.prefix = readPrefix();
  }

  attach(signal: AbortSignal) {
    // Capture, on `window`: the terminal's own keydown listener is on an
    // element several shadow roots down, and a capturing listener above it is
    // the only way to decide a key is the workbench's *before* the pane sees
    // it. Everything this file does not claim is left alone, so the terminal
    // stays byte-for-byte what stages 2 and 3 made it.
    window.addEventListener("keydown", (e) => this.onKey(e), { capture: true, signal });
    // The registry again: a click on a declared row is the same act as walking
    // to it, so it moves the cursor rather than leaving the two disagreeing.
    window.addEventListener("click", (e) => this.onClick(e), { capture: true, signal });
  }

  // -- the cursor -----------------------------------------------------------
  // Which click targets are *rows* on this surface. A function on the one
  // surface whose answer changes (GIT walks two lists), a constant everywhere
  // else — resolved here so nothing downstream has to know the difference.
  rowTargets(s: SurfaceEntry): readonly string[] {
    return typeof s.rows === "function" ? s.rows(this.app) : s.rows;
  }

  rowsOf(name: string): VerbEl[] {
    const s = SURFACES[name];
    if (!s) return [];
    const host = s.el(this.app);
    if (!host || !host.shadowRoot) return [];
    // The guard is two lines up; the compiler drops what it knows about a
    // property at the closure boundary, which is all the `!` is saying.
    return this.rowTargets(s).flatMap(
      (t) => [...host.shadowRoot!.querySelectorAll<VerbEl>('[data-verb="' + t + '"]')]);
  }

  currentRow(name: string): VerbEl | null {
    const rows = this.rowsOf(name);
    if (!rows.length) return null;
    const i = Math.max(0, Math.min(rows.length - 1, this.app.cursor[name] ?? 0));
    // Clamped to the list's own bounds on the line above — the `!`s in this
    // file are all that same fact, which the compiler cannot carry.
    return rows[i]!;
  }

  /// Re-apply the cursor after a redraw. The rails rebuild their rows on every
  /// pushed record, so the cursor is app state and this puts it back on screen.
  paint() {
    // A surface whose footer follows the cursor has to be told the cursor
    // moved. The CHANGES rail was the only one when this was written and its
    // call was inlined here; the GIT page has two footers, and pinning them to
    // one surface's method left them drawing the verbs of whatever row the page
    // last *rendered* — the keys were right and the only place they are written
    // down was a row behind.
    for (const s of Object.values(SURFACES)) s.footer?.(this.app);
    // …and a surface whose *contents* follow the cursor has to be told too.
    // HOME's is the pane in the middle of the page: it is not a footer, it is a
    // WebSocket to one machine, and it has to be re-pointed after the rows are
    // back and before anything reads them.
    for (const name of Object.keys(SURFACES)) {
      const rows = this.rowsOf(name);
      const focused = this.app.focus === name;
      const i = rows.length ? Math.max(0, Math.min(rows.length - 1, this.app.cursor[name] ?? 0)) : -1;
      rows.forEach((r, n) => r.classList.toggle("cur", n === i && focused));
      const host = SURFACES[name]!.el(this.app);
      if (host) host.classList.toggle("focused", focused);
    }
    // After the cursor is on, not before: `moved` resolves the row the cursor is
    // on, and asking for it mid-repaint would answer with the previous one.
    for (const s of Object.values(SURFACES)) s.moved?.(this.app);
  }

  move(name: string, delta: number) {
    const rows = this.rowsOf(name);
    // A surface with nothing to walk can still say what j/k mean — the GIT
    // page's third column is a diff, not a list, and scrolling it is the only
    // reading of "down" there is.
    if (!rows.length) { SURFACES[name]!.scroll?.(this.app, delta); return; }
    const i = Math.max(0, Math.min(rows.length - 1, (this.app.cursor[name] ?? 0) + delta));
    this.app.cursor[name] = i;
    this.paint();
    rows[i]!.scrollIntoView({ block: "nearest" });
  }

  onClick(e: MouseEvent) {
    // `instanceof` rather than the JS's `n.getAttribute &&`: the same "is this
    // a thing with attributes" question, spelled so the compiler can narrow an
    // `EventTarget` on the answer. See [`EventPath`] for why the path is named
    // as one type before anything is asked of it.
    const el = ((e.composedPath ? e.composedPath() : []) as EventPath).find(
      (n): n is VerbEl => n instanceof HTMLElement && !!n.getAttribute("data-verb"));
    if (!el) return;
    const target = el.getAttribute("data-verb")!;
    for (const [name, s] of Object.entries(SURFACES)) {
      if (!this.rowTargets(s).includes(target)) continue;
      const rows = this.rowsOf(name);
      const i = rows.indexOf(el);
      // The cursor only. Where the *keyboard* goes is the row's business —
      // opening a pane puts it on the stage, opening a diff puts it on FILES —
      // and deciding that here would take the terminal away from a mouse user
      // who just clicked an agent to watch it.
      if (i >= 0) { this.app.cursor[name] = i; this.paint(); }
      return;
    }
  }

  // -- dispatch -------------------------------------------------------------
  onKey(e: KeyboardEvent) {
    const app = this.app;
    const path: EventPath = e.composedPath ? e.composedPath() : [];
    // Any stage on the page, not only the work page's: HOME re-carves one in
    // the middle of the band it takes, and DOCKER embeds one for its logs. "On
    // the stage every key is the program's" is a claim about the pane the caret
    // is in, and pinning it to one element is what made HOME's preview a screen
    // you could click, scroll and not type into.
    const inStage = path.some((n) => n instanceof Element && n.tagName === "BUTAI-STAGE");
    const inField = !inStage && path.some(
      (n) => n instanceof Element && (n.tagName === "INPUT" || n.tagName === "TEXTAREA"));
    const take = () => { e.preventDefault(); e.stopImmediatePropagation(); };

    // 1. The prefix, armed. Whatever comes next belongs to the workbench —
    //    including a key nothing binds, which is swallowed rather than typed,
    //    because a prefix that sometimes leaks the second key is worse than one
    //    that never does.
    if (this.pending) {
      this.pending = false;
      app.showPrefix(false);
      if (isModifier(e)) { this.pending = true; app.showPrefix(true); return; }
      take();
      if (isPrefix(e, this.prefix)) {
        // Twice sends a literal one through, the way tmux and the TUI do.
        app.el.stage.send(keyMsg(e));
        return;
      }
      const v = prefixVerb(keyName(e));
      if (v) this.runGlobal(v.id, keyName(e));
      else app.toast("C-" + this.prefix.key + " " + keyName(e) + " is not bound — ? for the list", "info");
      return;
    }

    // 2. The prefix itself.
    if (isPrefix(e, this.prefix)) {
      take();
      this.pending = true;
      app.showPrefix(true);
      return;
    }

    // 3. A modal is a question: it has the keyboard, and nothing behind it
    //    moves. Not while the caret is in one of its fields, though — those
    //    already answer Enter and Escape themselves, and taking them here is
    //    how "Escape closes the new-folder row" became "Escape closes the
    //    picker you were using it from".
    if (app.modalOpen() && !inField) {
      if (this.runOverlay(e)) take();
      return;
    }
    if (app.modalOpen()) return;

    // 4. The Alt layer. A key no table binds is *not touched* — that is the
    //    whole contract of this layer, and it is why alt-b still moves back a
    //    word in the shell on the stage.
    if (e.altKey && !e.ctrlKey && !e.metaKey) {
      const name = altKeyName(e);
      const v = altVerb(name);
      if (!v) return;
      take();
      this.runGlobal(v.id, name);
      return;
    }

    // 5. A field on the page (the commit box) keeps its keys.
    if (inField) return;

    // 6. On the stage, every key is the program's.
    if (app.focus === "stage" || inStage) return;

    // 7. Bare keys, for the surface the cursor is on.
    if (this.runSurface(app.focus, e)) take();
  }

  /// Run a global verb. `arg` is the key it was reached by, which is only ever
  /// used by the one verb that has nine of them.
  runGlobal(id: VerbId, arg: string): void {
    const app = this.app;
    switch (id) {
      // Not a toggle, unlike the spaces: HOME is a peer of the workspace chips,
      // so it is entered and left rather than cycled through, and a second
      // `alt-0` must not fall through to whatever was behind it.
      case VerbId.SpaceHome: return app.setPage("home");
      case VerbId.FocusFleet: return app.setFocus("home");
      case VerbId.SpaceWork: return app.setPage("work");
      case VerbId.SpaceFiles: return app.toggleSpace("files");
      case VerbId.SpaceDocker: return app.toggleSpace("docker");
      case VerbId.SpaceGit: return app.toggleSpace("git");
      case VerbId.SpaceDocs: return app.toggleSpace("docs");
      // Entered and left rather than cycled, like HOME and unlike the spaces:
      // pressing it again is a request for the page you came from, which is
      // exactly what leaving does — so both spellings go through one method.
      case VerbId.SpaceSettings:
        return app.page === "settings" ? app.el.settings.leave() : app.setPage("settings");
      case VerbId.SpaceNext: return app.walkSpace(1);
      case VerbId.SpacePrev: return app.walkSpace(-1);
      case VerbId.Workspace: return app.selectWorkspaceAt(parseInt(arg, 10) - 1);
      case VerbId.WorkspaceNext: return app.walkWorkspace(1);
      case VerbId.WorkspacePrev: return app.walkWorkspace(-1);
      case VerbId.NewWorkspace: return app.newWorkspace();
      case VerbId.CloseWorkspace: {
        const ws = app.currentWs();
        return ws ? app.killWorkspace(ws) : app.toast("no workspace");
      }
      case VerbId.FocusAgents: return app.setFocus("agents");
      case VerbId.FocusProcs: return app.setFocus("procs");
      case VerbId.FocusChanges: return app.setFocus("changes");
      case VerbId.FocusStage: return app.setFocus("stage");
      case VerbId.FocusOff: return app.setFocus("agents");
      // `A` always asks, whatever is pinned; `a` spawns the pin when there is
      // one. Two ids, because with a pin they are two verbs.
      case VerbId.PickAgent: return app.spawnAgent(true);
      case VerbId.NewShell: return app.newProcess();
      case VerbId.Zen: return app.toggleZen();
      case VerbId.Help: return app.showHelp();
      case VerbId.Alerts: return app.showAlerts();
      case VerbId.PasteImage: return app.pasteImage();
      case VerbId.FontBigger: return app.setFont(1);
      case VerbId.FontSmaller: return app.setFont(-1);
      default: return undefined;
    }
  }

  /// A bare key on a focused surface. Returns true if it was ours.
  ///
  /// The lookup is the table and nothing else — there is not one key letter in
  /// this method, which is what `invariant/keys-come-from-the-table` holds up.
  /// The arrows are the exception and are not a second row in any table: they
  /// are `j` and `k` under the names a keyboard gives them.
  runSurface(name: string, e: KeyEventLike): boolean {
    const s = SURFACES[name];
    if (!s) return false;
    const table = s.table(this.app);
    const arrows: Record<string, VerbId> = { arrowdown: VerbId.Down, arrowup: VerbId.Up };
    const arrow = arrows[keyName(e)];
    // `C-s` is spelled with the control flag rather than as a character.
    const spelled = e.ctrlKey ? "C-" + keyName(e) : keyName(e);
    const v = arrow ? { id: arrow } : table.find((x) => x.key === spelled);
    if (!v) return false;
    switch (v.id) {
      case VerbId.Down: this.move(name, 1); return true;
      case VerbId.Up: this.move(name, -1); return true;
      case VerbId.FocusCycle: this.app.cycleFocus(e.shiftKey ? -1 : 1); return true;
      case VerbId.FocusStage: this.app.setFocus("stage"); return true;
      case VerbId.Help: this.app.showHelp(); return true;
      // A surface that names its own refresh target reloads *itself* — the GIT
      // page's six reads are its own and a whole-world poll does not touch
      // them. The rails have no such button and fall back to the poll, which is
      // for the case where something changed the tree behind their back;
      // `verbs/dispatchable` is what found that missing — it was in the table,
      // in the reference, and did nothing.
      case VerbId.Refresh:
        if ((s.verbs || {})[VerbId.Refresh]) return this.clickVerb(name, v.id);
        this.app.poll();
        return true;
      // Opening the row *is* clicking it, on every list — there is no separate
      // element to look for, so this is the one verb the registry answers with
      // the row itself.
      case VerbId.Open: {
        const row = this.currentRow(name);
        if (row) row.click();
        return true;
      }
      default: return this.clickVerb(name, v.id);
    }
  }

  /// Click whatever the registry says this verb reaches: inside the row the
  /// cursor is on first, then anywhere on the surface.
  clickVerb(name: string, id: VerbId): boolean {
    const s = SURFACES[name]!;
    const targets = (s.verbs || {})[id];
    if (!targets) return false;
    const row = this.currentRow(name);
    const host = s.el(this.app);
    for (const scope of [row, host && host.shadowRoot]) {
      if (!scope) continue;
      for (const t of targets) {
        // A shadow root has no `matches` — which is why the JS asked for it
        // before calling it, and `in` is that same question spelled so the
        // compiler narrows on the answer.
        const el = "matches" in scope && scope.matches('[data-verb="' + t + '"]')
          ? scope
          : scope.querySelector<VerbEl>('[data-verb="' + t + '"]');
        if (el && !el.disabled) { el.click(); return true; }
      }
    }
    // Bound, on a row that cannot run it — a staged file has no `s stage`.
    // Saying so beats a key that silently does nothing, which is the report
    // that made the TUI's tables contextual in the first place.
    this.app.toast("nothing here to " + id.toLowerCase(), "info");
    return true;
  }

  /// Overlay keys. Every one of them clicks a declared target, so a picker with
  /// no "clear all" simply has no `C`, and the same table describes all of them.
  runOverlay(e: KeyboardEvent): boolean {
    const app = this.app;
    const root = app.el.modalRoot;
    // A row that carries its own letter — the `g` menu's mnemonics. The letter
    // is read off the row rather than out of a table because it *is* the row's
    // data (`git-menu.ts`'s ITEMS), and it is drawn beside the row, so it is
    // findable exactly where it is bound. Checked before the overlay table so a
    // group's `c` is its own row and not the picker's.
    if (!e.ctrlKey && !e.altKey && !e.metaKey) {
      const k = keyName(e);
      if (k.length === 1) {
        const row = root.querySelector<VerbEl>(
          '[data-verb="overlay.row"][data-mnemonic="' + cssEscape(k) + '"]');
        if (row) { row.click(); return true; }
      }
    }
    const arrows: Record<string, VerbId> = { arrowdown: VerbId.Down, arrowup: VerbId.Up };
    const arrow = arrows[keyName(e)];
    const v = arrow ? { id: arrow } : overlayVerbs(app.overlayKind()).find((x) => x.key === keyName(e));
    if (!v) return false;
    if (v.id === VerbId.Down) return this.moveOverlay(1);
    if (v.id === VerbId.Up) return this.moveOverlay(-1);
    // Enter takes the row the cursor is on before it takes the modal's own
    // primary button: walking a list and pressing Enter means "this one".
    if (v.id === VerbId.Accept) {
      const row = root.querySelector<VerbEl>('[data-verb="overlay.row"].cur');
      if (row) { row.click(); return true; }
    }
    for (const t of OVERLAY_TARGETS[v.id] || []) {
      const el = root.querySelector<VerbEl>('[data-verb="' + t + '"]');
      if (el && !el.disabled) { el.click(); return true; }
    }
    return false;
  }

  /// Re-read the prefix after SETTINGS changed it.
  ///
  /// Read once in the constructor, because it never moved before this page
  /// existed. Changing it and finding the old one still armed until a reload is
  /// exactly the kind of "the setting saved but nothing happened" this client
  /// has none of anywhere else.
  reloadPrefix() {
    this.prefix = readPrefix();
    this.pending = false;
    this.app.showPrefix(false);
  }

  moveOverlay(delta: number): boolean {
    const rows = [...this.app.el.modalRoot.querySelectorAll<VerbEl>('[data-verb="overlay.row"]')];
    if (!rows.length) return false;
    let i = rows.findIndex((r) => r.classList.contains("cur"));
    i = Math.max(0, Math.min(rows.length - 1, (i < 0 ? (delta > 0 ? -1 : 0) : i) + delta));
    rows.forEach((r, n) => r.classList.toggle("cur", n === i));
    rows[i]!.scrollIntoView({ block: "nearest" });
    return true;
  }
}

function isModifier(e: KeyboardEvent): boolean {
  return e.key === "Shift" || e.key === "Control" || e.key === "Alt" || e.key === "Meta";
}

/// Quote one character for an attribute selector. The menu's mnemonics are
/// letters today, but `F` (push --force-with-lease) is one keystroke away from
/// somebody adding `"` or `\`, and a broken selector throws rather than missing.
function cssEscape(ch: string): string {
  return ch.replace(/["\\]/g, "\\$&");
}

/// The prefix, from this browser's own storage. There is no config file here —
/// the bridge deliberately has none (stage 5) and SETTINGS is stage 9 — so this
/// is the escape hatch, spelled the way `[general] prefix` is: `C-b`, `C-a`.
function readPrefix(): Prefix {
  try {
    const raw = window.localStorage?.getItem(PREFIX_STORAGE_KEY);
    const m = /^C-(\S)$/.exec((raw || "").trim());
    // One capture group, so a match has one — the `!` is the regex, restated.
    if (m) return { ctrl: true, key: m[1]!.toLowerCase() };
  } catch (_) { /* storage can throw outright; the default is the fallback */ }
  return DEFAULT_PREFIX;
}

export {
  SURFACES, ALT_MUST_FALL_THROUGH, GLOBAL, TARGETS, OverlayKind, ChangesRow, DockerRow, GitRow,
  SettingRow,
};
