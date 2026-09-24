// The app shell: the tab bar, the page router, the command palette and the
// overlays every page shares.
//
// `web/ui/` never got this far — it had pages and a preview harness and no shell,
// which is why it had no keys, no overlays and no way to move between projects.
// Everything here is the part that was missing.
//
// The shell owns exactly three kinds of thing and no more: **which workspace is
// current**, **which page is up and where the cursor is on it**, and **the three
// overlays a verb needs and a page must not own** — the question, the warning and
// the reader. The world comes from `world.ts`, every write goes through
// `actions.ts`, and each page is a pure component handed its slice. A shell that
// also fetched would be the third place the daemon is reached from.
//
// ## Why the cursor is here and not in the page
//
// `WorkPage` takes `view.pane`, `HomePage` takes `sel`, `HelpPage` takes `topic`.
// All three are the *keyboard's* position, the keyboard is one thing across the
// whole client, and a page that kept its own would lose it every time you left
// and came back. So they are state here, they arrive as props, and they change
// through `on` — which is the half of the glue that never reaches a daemon.

import type React from "react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { Toaster } from "@/components/ui/sonner";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Patch } from "@/components/Patch";
import { NO_FOLDS, type Folds } from "../logic/fleet.ts";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { Dialog as CmdDialog, DialogContent as CmdContent } from "@/components/ui/dialog";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { TooltipProvider } from "@/components/ui/tooltip";

import { Actions, type AskField } from "./actions.ts";
import { useWorld } from "./world.ts";
import { storage, storeTheme, useTheme } from "../theme.ts";
import { load, save, readPrefixSpelling, termColors } from "../logic/settings.ts";
import { api } from "../logic/api.ts";
import { daemonOf, type Qid, type QualifiedWorkspace } from "../logic/events.ts";
import { type GitActionId, type MenuCx, type GroupId, groupsFor, itemsFor } from "../logic/git-menu.ts";
import type { StageEvents } from "@/stage/Stage";
import type { SettingsFacts } from "@/pages/SettingsPage";
import { VIEWS, cycleView, pageLabel } from "./navigation";
import { VerbId, altVerb, prefixVerb, keyName, isPrefix } from "../logic/verbs";
import { PAGE_TABLE } from "./pages.tsx";

/** The pages the shell can show. `work` is where a client opens. */
export const PAGES = ["work", "home", "git", "files", "docs", "docker", "usage", "settings", "help"] as const;
export type PageName = (typeof PAGES)[number];


/**
 * Where the keyboard is, and what it has chosen. None of it reaches a daemon.
 *
 * A superset of each page's own view interface rather than a union type: the
 * pages were written against exactly the fields they draw (`WorkView` is six of
 * these, `HelpPage` takes one), and a shell that held one bag per page would be
 * eight cursors that disagree the first time a key moves the wrong one.
 */
export interface ShellView {
  /** `"open"` slides the left rail over the stage — the burger, below `md`. */
  rails: "auto" | "open";
  zen: boolean;
  procsHeight: number | null;
  systemHeight: number | null;
  leftRail: number;
  rightRail: number;
  /** The pane on the stage, qualified. Null only when there is nothing to show. */
  pane: Qid | null;
  /** The file CHANGES has open, so its row draws as selected. */
  path: string | null;
  /** SETTINGS' pinned agent, or null for "ask every time". */
  pin: string | null;
  /** A verb is in flight, or a git operation is: the remote row is disabled. */
  busy: boolean;
  /** HOME's cursor, as an index into the *row* list — machines and projects
   * included, folded-away rows excluded. It counted agents, back when a header
   * was not a thing you could put a cursor on. */
  sel: number;
  /** What HOME has folded away, and which machines have their gauges out. */
  folds: Folds;
  /** HELP's open topic, by slug. Undefined lets the page keep its own. */
  topic: string | undefined;
  /** The prefix as the user spells it, for the pages that document keys. */
  prefix: string;
  /** The stage's cell size, from SETTINGS. */
  fontPx: number;
}

/** Everything a page can ask the shell to move. Nothing here is a write. */
export interface ShellCallbacks {
  setFocus: (f: string) => void;
  setPage: (p: PageName) => void;
  setWsId: (id: string) => void;
  setPane: (pane: Qid | null) => void;
  setPath: (path: string | null) => void;
  setSel: (sel: number) => void;
  setFolds: (folds: Folds) => void;
  setTopic: (slug: string) => void;
  setRails: (open: boolean) => void;
  /** Open the `g` menu. An overlay, so the shell draws it. */
  gitMenu: () => void;
  closeUtility: () => void;
}

/** The shell's one question, as `actions.ts` asks it. */
interface Question {
  title: string;
  fields: readonly AskField[];
  submit: string;
  resolve: (values: string[] | null) => void;
}

export function Shell() {
  const [world, refresh] = useWorld();
  const [page, setPage] = useState<PageName>("work");
  const [wsId, setWsId] = useState<string | null>(null);
  const [focus, setFocus] = useState("stage");
  const [rails, setRails] = useState<"auto" | "open">("auto");
  const [pane, setPane] = useState<Qid | null>(null);
  const [path, setPath] = useState<string | null>(null);
  const [sel, setSel] = useState(0);
  const [folds, setFolds] = useState<Folds>(NO_FOLDS);
  const [topic, setTopic] = useState<string | undefined>(undefined);
  const [prefsRevision, setPrefsRevision] = useState(0);
  const [busy, setBusy] = useState(false);
  const [palette, setPalette] = useState(false);
  const [viewsOpen, setViewsOpen] = useState(false);
  const [hostsOpen, setHostsOpen] = useState(false);
  const [zen, setZen] = useState(() => load(storage()).zen);
  const [layout, setLayout] = useState(false);
  const [procsHeight, setProcsHeight] = useState<number | null>(null);
  const [systemHeight, setSystemHeight] = useState<number | null>(null);
  const [attached, setAttached] = useState(true);
  const returnPage = useRef<PageName>("work");
  const prefixPending = useRef(false);
  const [menu, setMenu] = useState(false);

  // The two overlays a verb needs and a page must not own, as promises.
  // `actions.ts` asks and awaits; the shell is what has a DOM to ask in. One of
  // each rather than one per caller, so "are you sure" and "which branch" read
  // the same wherever they come from.
  const [ask, setAsk] = useState<{ q: string; detail?: string; resolve: (ok: boolean) => void } | null>(null);
  const [question, setQuestion] = useState<Question | null>(null);
  const [patch, setPatch] = useState<{ title: string; text: string } | null>(null);

  const confirm = useCallback(
    (q: string, detail?: string) =>
      new Promise<boolean>((resolve) => setAsk({ q, ...(detail ? { detail } : {}), resolve })),
    [],
  );
  const askFor = useCallback(
    (title: string, fields: readonly AskField[], submit = "OK") =>
      new Promise<string[] | null>((resolve) => setQuestion({ title, fields, submit, resolve })),
    [],
  );
  const showPatch = useCallback((title: string, text: string) => setPatch({ title, text }), []);

  const actions = useMemo(
    () => new Actions({ refresh, confirm, ask: askFor, patch: showPatch, onBusy: setBusy }),
    [refresh, confirm, askFor, showPatch],
  );

  // SETTINGS writes `localStorage` directly and this reads it back, keyed on the
  // page so leaving SETTINGS picks up what was changed there. A subscription
  // would be the tidier answer and there is nothing to subscribe to: the store
  // is a string in `localStorage`, which is exactly why the theme picker in the
  // palette below still reloads.
  const prefs = useMemo(() => ({ ...load(storage()), prefix: readPrefixSpelling(storage()) }), [page, prefsRevision]);
  const pal = useTheme(prefs.theme);

  // The busiest workspace by default: the rails are what is being looked at, and
  // a project with nothing open shows none of them.
  const spaces = world.workspaces;
  const ws: QualifiedWorkspace | null = detailed(
    spaces.find((w) => String(w.id) === wsId) ?? [...spaces].sort((a, b) => weight(b) - weight(a))[0] ?? null,
  );

  // What the stage streams: the selection, while it still names a pane this
  // workspace has, and otherwise the same fallback `renderWorkspace` uses — a
  // live agent, then whatever the daemon staged, then the first pane there is.
  // Without the check, switching projects would leave the previous project's
  // pane on screen, which is the one bug a qualified id cannot catch: it is a
  // real pane, on the right machine, in the wrong project.
  const stagePane = pane != null && paneIn(ws, pane) ? pane : defaultPane(ws);

  // The two facts SETTINGS can only be told. The agent types are per machine and
  // unioned because the page's picker is about the client's default; the version
  // arrives on the stage's `hello` and is stored when a pane streams.
  const [daemonVersion, setDaemonVersion] = useState<string | null>(null);
  const [agentTypes, setAgentTypes] = useState<readonly string[]>([]);
  const daemonKeys = world.daemons.map((d) => d.key).join(",");
  useEffect(() => {
    if (!daemonKeys) return undefined;
    let alive = true;
    Promise.all(daemonKeys.split(",").map((k) => api.agentTypes(k))).then((lists) => {
      if (alive) setAgentTypes([...new Set(lists.flat())].sort());
    });
    return () => {
      alive = false;
    };
  }, [daemonKeys]);

  const toggleUtility = useCallback((next: "help" | "settings") => {
    if (page === next) setPage(returnPage.current);
    else {
      if (page !== "help" && page !== "settings") returnPage.current = page;
      setPage(next);
    }
  }, [page]);

  const term = useMemo(() => (pal ? termColors(pal) : { fg: "#d7dde5", bg: "#0e1116" }), [pal]);

  const on: ShellCallbacks = useMemo(
    () => ({
      setFocus,
      setPage: (next: PageName) => {
        if (next === "help" || next === "settings") {
          if (page !== "help" && page !== "settings") returnPage.current = page;
        }
        setPage(next);
      },
      setWsId: (id: string) => {
        setWsId(id);
        if (page === "home") setPage("work");
        // A pane belongs to a project; carrying the selection across would put
        // another project's terminal on this one's stage.
        setPane(null);
        setPath(null);
        setPatch(null);
      },
      setPane: (next: Qid | null) => { setPane(next); setPatch(null); },
      setPath,
      setSel,
      setFolds,
      setTopic,
      setRails: (open: boolean) => setRails(open ? "open" : "auto"),
      gitMenu: () => setMenu(true),
      closeUtility: () => setPage(returnPage.current),
    }),
    [page],
  );

  const view: ShellView = {
    rails,
    zen,
    procsHeight,
    systemHeight,
    leftRail: prefs.leftRail,
    rightRail: prefs.rightRail,
    pane: stagePane,
    path,
    pin: prefs.defaultAgent || null,
    // Anything in flight, plus an operation the daemon says is still running.
    // The first covers the window between the click and the reply, which is
    // exactly when a second click on `push` happens.
    busy: busy || (world.gitOp?.running === true && ws != null && world.gitOp.ws === ws.id),
    sel,
    folds,
    topic,
    prefix: prefs.prefix,
    fontPx: prefs.fontPx,
  };

  // The stage's own three, forwarded to every page that draws one. A refused
  // pane is dropped *here* rather than in the page, which is what the pages'
  // headers say the shell is for: the page has no selection to drop.
  const stage: StageEvents = useMemo(
    () => ({
      onDaemonVersion: (info) => {
        setDaemonVersion(info.version);
        if (info.problem) toast.warning(info.problem);
      },
      onPaneRefused: (info) => {
        toast.error(info.error);
        setPane((cur) => (cur != null && String(cur) === String(info.pane) ? null : cur));
      },
    }),
    [],
  );

  const toggleLayout = () => {
    if (!layout) { setPage("work"); setZen(false); setFocus("agents"); (document.activeElement as HTMLElement)?.blur(); }
    setLayout(v => !v);
  };

  const newWorkspace = async () => {
    const daemon = (ws ? machineOf(ws) : null) || world.daemons.find(d => d.primary)?.key;
    if (!daemon) { actions.toast("no host connected"); return; }
    const values = await askFor("NEW WORKSPACE", [{ label: "Directory" }, { label: "Name" }], "Open");
    if (!values?.[0]?.trim()) return;
    const made = await actions.newWorkspace(daemon, values[0].trim(), values[1]?.trim());
    if (made != null) { on.setWsId(String(made)); setPage("work"); }
  };

  // Capture the app chords before the terminal's input sink forwards them.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (document.querySelector('[role="dialog"]')) { prefixPending.current = false; return; }
      const inStage = !!(e.target as HTMLElement)?.closest('[data-slot="stage"]');
      const typing = isTyping(e.target) && !inStage;
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k" && !inStage) {
        e.preventDefault(); e.stopImmediatePropagation(); setPalette(v => !v); return;
      }
      if (typing) return;
      const key = keyName(e);
      if (layout && !e.altKey && !e.ctrlKey && !e.metaKey) {
        e.preventDefault(); e.stopImmediatePropagation();
        if (key === "esc" || key === "enter") setLayout(false);
        else if (key === "tab") setFocus(focus === "agents" ? "procs" : focus === "procs" ? "changes" : "agents");
        else if (["arrowleft", "arrowright", "h", "l"].includes(key)) {
          const right = focus === "changes";
          const rail = document.querySelector<HTMLElement>(right ? '[data-surface="changes"]' : '.work-rails');
          const width = rail?.getBoundingClientRect().width ?? (right ? 319 : 235);
          const other = document.querySelector<HTMLElement>(right ? '.work-rails' : '[data-surface="changes"]')?.getBoundingClientRect().width ?? 0;
          const cap = Math.max(180, Math.min(640, window.innerWidth - other - 168));
          const next = Math.max(180, Math.min(cap, width + (key === "arrowright" || key === "l" ? 16.8 : -16.8)));
          save(storage(), { ...prefs, [right ? "rightRail" : "leftRail"]: Math.round(next) });
          setPrefsRevision(v => v + 1);
        } else if (["arrowup", "arrowdown", "j", "k"].includes(key) && focus !== "changes") {
          const delta = key === "arrowup" || key === "k" ? 36 : -36;
          const proc = document.querySelector<HTMLElement>('[data-surface="procs"]')?.getBoundingClientRect().height ?? 108;
          const system = document.querySelector<HTMLElement>('[data-surface="system"]')?.getBoundingClientRect().height ?? 18;
          const height = document.querySelector<HTMLElement>('.work-rails')?.getBoundingClientRect().height ?? 400;
          if (focus === "agents") setProcsHeight(Math.max(54, Math.min(height - system - 54, proc - delta)));
          else {
            const nextSystem = Math.max(18, Math.min(height - 108, system - delta));
            setSystemHeight(nextSystem);
            setProcsHeight(Math.max(54, proc + system - nextSystem));
          }
        }
        return;
      }

      const prefixKey = prefs.prefix.replace(/^C-/, "").toLowerCase();
      const prefix = isPrefix(e, { ctrl: prefs.prefix.startsWith("C-"), key: prefixKey });
      if (prefix && !prefixPending.current) {
        prefixPending.current = true; e.preventDefault(); e.stopImmediatePropagation(); return;
      }
      if (prefix && prefixPending.current) { prefixPending.current = false; return; }
      const pending = prefixPending.current;
      prefixPending.current = false;
      const verb = pending ? prefixVerb(key) : e.altKey ? altVerb(key) : null;
      let handled = true;
      switch (verb?.id) {
        case VerbId.SpaceNext: setPage(cycleView(page, 1)); break;
        case VerbId.SpacePrev: setPage(cycleView(page, -1)); break;
        case VerbId.SpaceWork: setPage("work"); break;
        case VerbId.SpaceFiles: setPage(page === "files" ? "work" : "files"); break;
        case VerbId.SpaceDocs: setPage(page === "docs" ? "work" : "docs"); break;
        case VerbId.SpaceGit: setPage(page === "git" ? "work" : "git"); break;
        case VerbId.SpaceDocker: setPage(page === "docker" ? "work" : "docker"); break;
        case VerbId.SpaceHome: case VerbId.FocusFleet: setPage("home"); setFocus("home"); (document.activeElement as HTMLElement)?.blur(); break;
        case VerbId.SpaceSettings: toggleUtility("settings"); break;
        case VerbId.Help: toggleUtility("help"); break;
        case VerbId.Workspace: { const w = spaces[Number(key) - 1]; if (w) on.setWsId(String(w.id)); break; }
        case VerbId.WorkspaceNext: case VerbId.WorkspacePrev: {
          const at = spaces.findIndex(w => String(w.id) === String(ws?.id));
          const w = spaces[(at + (verb.id === VerbId.WorkspaceNext ? 1 : -1) + spaces.length) % spaces.length];
          if (w) on.setWsId(String(w.id)); break;
        }
        case VerbId.FocusOff:
          if (page === "home") { setFocus("home"); (document.activeElement as HTMLElement)?.blur(); break; }
          setFocus("agents"); (document.activeElement as HTMLElement)?.blur(); break;
        case VerbId.FontBigger: case VerbId.FontSmaller:
          save(storage(), { ...prefs, fontPx: prefs.fontPx + (verb.id === VerbId.FontBigger ? 1 : -1) });
          setPrefsRevision(v => v + 1); break;
        case VerbId.FocusAgents: case VerbId.FocusProcs: case VerbId.FocusChanges:
          setPage("work"); setFocus(verb.id === VerbId.FocusProcs ? "procs" : verb.id === VerbId.FocusChanges ? "changes" : "agents");
          (document.activeElement as HTMLElement)?.blur(); break;
        case VerbId.FocusStage: setFocus("stage"); document.querySelector<HTMLElement>('[data-slot="stage"] textarea, [data-slot="stage"] canvas')?.focus(); break;
        case VerbId.NewWorkspace: void newWorkspace(); break;
        case VerbId.CloseWorkspace: if (ws) void actions.closeWorkspace(ws.id, ws.name); break;
        case VerbId.NewShell: if (ws) void actions.newProc(ws.id); break;
        case VerbId.PickAgent: if (ws) void actions.spawnPick(ws.id, true, view.pin); break;
        case VerbId.Layout: toggleLayout(); break;
        case VerbId.Zen: setZen(v => !v); break;
        default:
          if (!inStage && !e.altKey && !e.ctrlKey && !e.metaKey && key === "?") toggleUtility("help");
          else if (!inStage && key === "esc" && page === "help") setPage(returnPage.current);
          else handled = pending;
      }
      if (handled) { e.preventDefault(); e.stopImmediatePropagation(); }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  });

  const facts: SettingsFacts = { agents: agentTypes, daemonVersion };

  return (
    <TooltipProvider delayDuration={200}>
      <div className="flex h-full min-h-0 flex-col bg-background text-foreground">
        <header className="flex h-row-lg shrink-0 items-center gap-1 border-b border-border bg-background px-1">
          <Button size="sm" variant={page === "home" ? "default" : "ghost"} bracket={page === "home"}
            onClick={() => { setPage("home"); setFocus("home"); }} aria-pressed={page === "home"}>
            booth{spaces.some(w => workspaceAttention(w) && !world.daemons.find(d => d.key === machineOf(w))?.error) ? " !" : ""}
          </Button>
          <span className="text-border" aria-hidden="true">│</span>
          <nav aria-label="Workspaces" className="flex min-w-0 flex-1 items-center gap-1 overflow-x-auto">
            {spaces.map((w, i) => {
              const active = page !== "home" && String(w.id) === String(ws?.id);
              const down = !!world.daemons.find(d => d.key === machineOf(w))?.error;
              return <div key={String(w.id)} className={`flex shrink-0 items-center ${active ? "bg-primary text-primary-foreground" : down ? "text-faint" : workspaceAttention(w) ? "text-bad" : "text-dim"}`}>
                <button type="button" className="whitespace-nowrap px-1" aria-current={active ? "page" : undefined}
                  onClick={() => on.setWsId(String(w.id))}>
                  {active ? "[ " : "  "}{down ? "·" : ""}{i + 1}:{world.daemons.length > 1 ? `${machineOf(w)}:` : ""}{w.name}{workspaceAttention(w) ? " !" : ""}
                </button>
                {active ? <><Button size="sm" variant="ghost" className="text-inherit" aria-label={`Close ${w.name}`}
                  onClick={() => void actions.closeWorkspace(w.id, w.name)}>x</Button><span className="pr-1">]</span></> : null}
              </div>;
            })}
          </nav>
          <div className="hidden w-[10ch] shrink-0 justify-end min-[360px]:flex">
            <Button size="sm" variant="outline" aria-label="Switch view" aria-haspopup="dialog" aria-expanded={viewsOpen}
              onClick={() => setViewsOpen(true)}>{VIEWS.includes(page) ? pageLabel(page) : "views"} v</Button>
          </div>
          <Button size="sm" variant="ghost" className="hidden min-[480px]:inline-flex" onClick={() => setHostsOpen(true)}>
            {world.daemons.length > 1 ? `${world.daemons.length} hosts` : "+ host"}
          </Button>
          <Button size="sm" variant="ghost" className="hidden min-[480px]:inline-flex" onClick={() => void newWorkspace()}>+ new</Button>
        </header>

        <main className="min-h-0 flex-1 overflow-hidden">
          {!attached ? (
            <div className="flex h-full items-center justify-center"><Button onClick={() => setAttached(true)}>attach</Button></div>
          ) : !world.loaded ? (
            <div className="flex h-full items-center justify-center text-13 text-dim">connecting…</div>
          ) : world.error ? (
            <div className="flex h-full items-center justify-center px-8 text-center text-13 text-bad">
              {world.error}
            </div>
          ) : (
            <PageBody
              page={page}
              world={world}
              ws={ws}
              actions={actions}
              focus={focus}
              view={view}
              term={term}
              stage={stage}
              facts={facts}
              patch={page === "work" ? patch : null}
              on={on}
            />
          )}
        </main>

        <footer className="flex h-row shrink-0 items-center gap-1 border-t border-border bg-status-bg px-1 text-status-fg">
          <span className="min-w-0 flex-1 truncate">{layout ? "LAYOUT · ←/→ width · ↑/↓ height · tab rail · enter done" : page === "home" ? "booth" : ws ? `${ws.name} · ${pageLabel(page)}` : pageLabel(page)}</span>
          <Button size="sm" variant={layout ? "default" : "secondary"} onClick={toggleLayout} aria-pressed={layout}>layout</Button>
          <Button size="sm" variant="secondary" onClick={() => setAttached(false)}>detach</Button>
          <Button size="sm" variant={page === "help" ? "default" : "secondary"} onClick={() => toggleUtility("help")}>help</Button>
          <Button size="sm" variant={page === "settings" ? "default" : "secondary"} onClick={() => toggleUtility("settings")}>settings</Button>
        </footer>

        <Dialog open={viewsOpen} onOpenChange={setViewsOpen}>
          <DialogContent className="sm:max-w-xs">
            <DialogHeader><DialogTitle>Views</DialogTitle></DialogHeader>
            <div className="flex flex-col" role="menu" aria-label="Workspace views" onKeyDown={e => {
              if (!["j", "k", "ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
              e.preventDefault();
              const buttons = Array.from(e.currentTarget.querySelectorAll<HTMLButtonElement>("button"));
              const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
              const next = e.key === "Home" ? 0 : e.key === "End" ? buttons.length - 1 :
                (at + (e.key === "j" || e.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length;
              buttons[next]?.focus();
            }}>
              {VIEWS.map(p => <button key={p} type="button" role="menuitemradio" aria-checked={page === p}
                autoFocus={page === p || (p === "work" && !VIEWS.includes(page))}
                className={`flex h-row items-center px-2 text-left hover:bg-sel ${page === p ? "bg-sel" : ""}`}
                onClick={() => { setPage(p); setViewsOpen(false); }}>
                <span className="w-3">{page === p ? ">" : " "}</span>{pageLabel(p)}
              </button>)}
            </div>
          </DialogContent>
        </Dialog>
        <Dialog open={hostsOpen} onOpenChange={setHostsOpen}>
          <DialogContent>
            <DialogHeader><DialogTitle>Hosts</DialogTitle></DialogHeader>
            {world.daemons.map(d => <div key={d.key} className="flex items-center gap-2">
              <span className="min-w-0 flex-1 truncate">{d.label} · {d.error ?? "connected"}</span>
              {!d.primary ? <Button size="sm" variant="destructive" onClick={() => void actions.removeDaemon(d.key)}>remove</Button> : null}
            </div>)}
            <Button variant="outline" onClick={async () => {
              setHostsOpen(false);
              const values = await askFor("ADD HOST", [{ label: "Socket path" }, { label: "Name" }], "Add");
              if (values?.[0]?.trim()) void actions.addDaemon(values[0].trim(), values[1]?.trim() || undefined);
            }}>+ host</Button>
          </DialogContent>
        </Dialog>

        {/* Everything the shell can do, by name. The palette is how a page's
            verb is reached without knowing its key — which is what makes the
            key tables an accelerant rather than the only way in. */}
        <CmdDialog open={palette} onOpenChange={setPalette}>
          <CmdContent className="p-0">
            <Command>
              <CommandInput placeholder="Go to a page, or a project…" />
              <CommandList>
                <CommandEmpty>Nothing matches.</CommandEmpty>
                <CommandGroup heading="Pages">
                  {PAGES.map((p) => (
                    <CommandItem key={p} value={`page ${p}`} onSelect={() => { setPage(p); setPalette(false); }}>
                      {pageLabel(p)}
                    </CommandItem>
                  ))}
                </CommandGroup>
                <CommandGroup heading="Projects">
                  {spaces.map((w) => (
                    <CommandItem
                      key={String(w.id)}
                      value={`project ${w.name} ${machineOf(w) ?? ""}`}
                      onSelect={() => { on.setWsId(String(w.id)); setPage("work"); setPalette(false); }}
                    >
                      {w.name}
                      {machineOf(w) ? <span className="ml-2 text-dim">{machineOf(w)}</span> : null}
                    </CommandItem>
                  ))}
                </CommandGroup>
                <CommandGroup heading="Appearance">
                  <CommandItem value="theme dark" onSelect={() => { storeTheme("blueprint-dark"); location.reload(); }}>
                    Dark
                  </CommandItem>
                  <CommandItem value="theme light" onSelect={() => { storeTheme("blueprint-light"); location.reload(); }}>
                    Light
                  </CommandItem>
                </CommandGroup>
              </CommandList>
            </Command>
          </CmdContent>
        </CmdDialog>

        <GitMenu
          open={menu}
          cx={{ inSequence: !!ws?.changes && ws.changes.state !== "clean" }}
          onOpenChange={setMenu}
          onPick={(action) => {
            setMenu(false);
            if (ws) void actions.gitAction(ws.id, action);
            else actions.toast("no workspace");
          }}
        />

        <PromptDialog
          question={question}
          onDone={(v) => {
            const q = question;
            setQuestion(null);
            q?.resolve(v);
          }}
        />

        <PatchDialog patch={page === "work" ? null : patch} onClose={() => setPatch(null)} />

        <Dialog open={!!ask} onOpenChange={(o) => { if (!o && ask) { ask.resolve(false); setAsk(null); } }}>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>{ask?.q}</DialogTitle>
              {ask?.detail ? <DialogDescription>{ask.detail}</DialogDescription> : null}
            </DialogHeader>
            <DialogFooter>
              <Button variant="outline" onClick={() => { ask?.resolve(false); setAsk(null); }}>
                Cancel
              </Button>
              <Button variant="destructive" onClick={() => { ask?.resolve(true); setAsk(null); }}>
                Yes, do it
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>

        <Toaster />
      </div>
    </TooltipProvider>
  );
}

/**
 * The one question, whatever it is about.
 *
 * A field with `options` is a choice and draws a `Select`; without, it is free
 * text. Enter submits, because every question here has one obvious answer and
 * reaching for the mouse to confirm the default is the friction this replaces.
 * The port of `web/ui/actions.js`'s `PromptDialog`, unchanged in behaviour.
 */
function PromptDialog({ question, onDone }: { question: Question | null; onDone: (v: string[] | null) => void }) {
  const [vals, setVals] = useState<string[]>([]);
  useEffect(() => {
    setVals(question ? question.fields.map((f) => f.value ?? "") : []);
  }, [question]);
  if (!question) return null;
  const submit = () => onDone(vals);
  return (
    <Dialog open onOpenChange={(o) => { if (!o) onDone(null); }}>
      <DialogContent
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            submit();
          }
        }}
      >
        <DialogHeader>
          <DialogTitle>{question.title}</DialogTitle>
        </DialogHeader>
        {question.fields.map((f, i) => (
          <label key={f.label} className="grid gap-1">
            <span className="text-11 text-dim">{f.label}</span>
            {f.options ? (
              <Select value={vals[i] ?? ""} onValueChange={(v) => setVals((cur) => replace(cur, i, v))}>
                <SelectTrigger className="w-full" aria-label={f.label}>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {f.options.map((o) => (
                    <SelectItem key={o} value={o}>
                      {o}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            ) : (
              <Input
                autoFocus={i === 0}
                aria-label={f.label}
                value={vals[i] ?? ""}
                onChange={(e) => setVals((cur) => replace(cur, i, e.target.value))}
              />
            )}
          </label>
        ))}
        <DialogFooter>
          <Button variant="outline" onClick={() => onDone(null)}>
            Cancel
          </Button>
          <Button onClick={submit}>{question.submit}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/**
 * A diff, read. Wider than the default dialog because a unified diff has a
 * natural measure and wrapping one is worse than scrolling it.
 */
function PatchDialog({ patch, onClose }: { patch: { title: string; text: string } | null; onClose: () => void }) {
  if (!patch) return null;
  return (
    <Dialog open onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogContent className="sm:max-w-4xl">
        <DialogHeader>
          <DialogTitle className="font-mono text-13">{patch.title}</DialogTitle>
        </DialogHeader>
        <Patch text={patch.text} className="max-h-[70vh]" />
      </DialogContent>
    </Dialog>
  );
}

/** The terminal's git menu: choose a group, then an operation; Escape goes up. */
function GitMenu({ open, cx, onOpenChange, onPick }: {
  open: boolean;
  cx: MenuCx;
  onOpenChange: (open: boolean) => void;
  onPick: (action: GitActionId) => void;
}) {
  const [group, setGroup] = useState<GroupId | null>(null);
  useEffect(() => { if (!open) setGroup(null); }, [open]);
  const rows = group == null
    ? groupsFor(cx).map(g => ({ key: g.key, label: g.label, pick: () => setGroup(g.id) }))
    : [{ key: "", label: "..", pick: () => setGroup(null) }, ...itemsFor(group, cx).map(i => ({
        key: i.key, label: i.label, pick: () => onPick(i.action),
      }))];
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-sm" onEscapeKeyDown={e => {
        if (group != null) { e.preventDefault(); setGroup(null); }
      }}>
        <DialogHeader><DialogTitle>GIT{group ? ` · ${group}` : ""}</DialogTitle></DialogHeader>
        <div key={group ?? "root"} role="menu" aria-label="Git operations" onKeyDown={e => {
          const row = rows.find(r => r.key && r.key === e.key);
          if (row) { e.preventDefault(); row.pick(); return; }
          if (!["j", "k", "ArrowDown", "ArrowUp"].includes(e.key)) return;
          e.preventDefault();
          const buttons = Array.from(e.currentTarget.querySelectorAll<HTMLButtonElement>("button"));
          const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
          buttons[(at + (e.key === "j" || e.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length]?.focus();
        }}>
          {rows.map((row, i) => <button key={row.label} type="button" role="menuitem" autoFocus={i === 0}
            className="flex h-row w-full items-center gap-2 px-1 text-left hover:bg-sel focus:bg-sel focus:outline-none"
            onClick={row.pick}><span className="w-3 text-faint">{row.key}</span>{row.label}</button>)}
        </div>
        <span className="text-faint">j/k move · enter choose · esc {group ? "back" : "close"}</span>
      </DialogContent>
    </Dialog>
  );
}

function replace(list: readonly string[], i: number, v: string): string[] {
  const out = list.slice();
  out[i] = v;
  return out;
}

/**
 * A workspace whose rails are lists, whatever arrived.
 *
 * **A summary and a detail disagree about their own shape**: `agents`,
 * `processes` and `changes` are *counts* on `WorkspaceSummary` and lists on
 * `WorkspaceDetail`, and `world.ts` puts a summary into the list as a
 * `QualifiedWorkspace` (`s as unknown as QualifiedWorkspace`) because the summary
 * is how a workspace first appears — the detail follows on its own record. So
 * between those two records the type is a claim about a shape the value does not
 * have, and every page's contract ("already qualified", `agents:
 * QualifiedAgent[]`) is briefly untrue.
 *
 * That window is short and it is real: Firefox drew this client inside it and
 * `ws.agents.find is not a function` took the whole page down. The pages are
 * right to trust their props, so the shim is here, at the one place a page's
 * `ws` is chosen. Counts are not converted into rows — there are none to make;
 * an empty rail for the frame before the detail lands is the honest drawing.
 */
function detailed(w: QualifiedWorkspace | null): QualifiedWorkspace | null {
  if (!w) return null;
  const agents = Array.isArray(w.agents) ? w.agents : [];
  const processes = Array.isArray(w.processes) ? w.processes : [];
  const changes = w.changes && typeof w.changes === "object" ? w.changes : null;
  if (agents === w.agents && processes === w.processes && changes === w.changes) return w;
  return { ...w, agents, processes, changes };
}

// A workspace is "busy" by what is open in it — agents count for more than
// processes, and a dirty tree for something. The same ordering the preview used,
// so opening the client lands where it used to.
//
// `count` because this runs over the raw list, where a workspace may still be a
// summary: `agents` is then the number itself, and `.length` on a number is
// `undefined`, which would sort every project by NaN.
function weight(w: QualifiedWorkspace): number {
  return count(w.agents) * 10 + count(w.processes) + count(w.changes?.unstaged);
}

function count(v: unknown): number {
  if (typeof v === "number") return v;
  return Array.isArray(v) ? v.length : 0;
}

/** Which machine a workspace is on, for the badge beside its name. */
function machineOf(w: QualifiedWorkspace): string | null {
  return w.daemon ?? daemonOf(w.id);
}

function workspaceAttention(w: QualifiedWorkspace): boolean {
  const summary = w as QualifiedWorkspace & { waiting?: number; questions?: number };
  return (summary.waiting ?? 0) > 0 || (summary.questions ?? 0) > 0 || (Array.isArray(w.agents) && w.agents.some(a => (a.question || a.state === "waiting")));
}

/** Whether this workspace still has that pane. */
function paneIn(ws: QualifiedWorkspace | null, pane: Qid): boolean {
  if (!ws) return false;
  const id = String(pane);
  return [...ws.agents, ...ws.processes].some((p) => String(p.pane) === id);
}

/**
 * The pane a workspace opens on: a live agent, then the daemon's staged pane,
 * then the first row there is.
 *
 * `renderWorkspace`'s rule, and `web/ui/pages.js` had the same function for the
 * same reason — the stage showing nothing while three agents run in the rail is
 * read as a broken terminal rather than as an empty selection.
 */
function defaultPane(ws: QualifiedWorkspace | null): Qid | null {
  if (!ws) return null;
  const live = ws.agents.find((a) => a.exited == null);
  if (live) return live.pane;
  if (ws.stage != null) return ws.stage;
  return ws.agents[0]?.pane ?? ws.processes[0]?.pane ?? null;
}

/** Whether a key event came from somewhere a `?` is a literal question mark. */
function isTyping(t: EventTarget | null): boolean {
  const el = t as HTMLElement | null;
  if (!el || !el.tagName) return false;
  const tag = el.tagName.toLowerCase();
  return tag === "input" || tag === "textarea" || tag === "select" || el.isContentEditable;
}

/** What every page is handed. `pages.tsx` is what turns it into each page's own. */
export interface PageProps {
  page: PageName;
  world: ReturnType<typeof useWorld>[0];
  ws: QualifiedWorkspace | null;
  actions: Actions;
  focus: string;
  view: ShellView;
  term: { fg: string; bg: string };
  /** The stage's own events — a bell, a refused pane, a version mismatch. */
  stage: StageEvents;
  patch: { title: string; text: string } | null;
  /** The two things SETTINGS can only be told. */
  facts: SettingsFacts;
  on: ShellCallbacks;
}

function PageBody(props: PageProps) {
  const { page } = props;
  const Lazy = PAGE_TABLE[page];
  if (!Lazy) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-2 text-13 text-dim">
        <span>the {page} page is not ported yet</span>
        <span className="text-11 text-faint">it lands in this phase; the shell is already routing to it</span>
      </div>
    );
  }
  return <Lazy {...props} />;
}

export { toast };
