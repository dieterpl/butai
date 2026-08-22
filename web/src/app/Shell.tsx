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
import { useCallback, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { Toaster } from "@/components/ui/sonner";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Patch } from "@/components/Patch";
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
import { storage, storeTheme, storedTheme, useTheme } from "../theme.ts";
import { load, readPrefixSpelling, termColors } from "../logic/settings.ts";
import { api } from "../logic/api.ts";
import { daemonOf, type Qid, type QualifiedWorkspace } from "../logic/events.ts";
import { type GitActionId, type MenuCx, groupsFor, itemsFor } from "../logic/git-menu.ts";
import type { StageEvents } from "@/stage/Stage";
import type { SettingsFacts } from "@/pages/SettingsPage";
import { PAGE_TABLE } from "./pages.tsx";

/** The pages the shell can show. `work` is where a client opens. */
export const PAGES = ["work", "home", "git", "files", "docs", "docker", "usage", "settings", "help"] as const;
export type PageName = (typeof PAGES)[number];

/** A page the tab bar offers. HELP and SETTINGS are reached by key, not by tab. */
const TABS: PageName[] = ["work", "home", "git", "files", "docker", "usage"];

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
  /** The pane on the stage, qualified. Null only when there is nothing to show. */
  pane: Qid | null;
  /** The file CHANGES has open, so its row draws as selected. */
  path: string | null;
  /** SETTINGS' pinned agent, or null for "ask every time". */
  pin: string | null;
  /** A verb is in flight, or a git operation is: the remote row is disabled. */
  busy: boolean;
  /** HOME's cursor, as an index among agent rows. */
  sel: number;
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
  setTopic: (slug: string) => void;
  setRails: (open: boolean) => void;
  /** Open the `g` menu. An overlay, so the shell draws it. */
  gitMenu: () => void;
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
  const [topic, setTopic] = useState<string | undefined>(undefined);
  const [busy, setBusy] = useState(false);
  const [palette, setPalette] = useState(false);
  const [menu, setMenu] = useState(false);
  const [theme] = useState(storedTheme);
  const pal = useTheme(theme);

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
  const prefs = useMemo(() => ({ ...load(storage()), prefix: readPrefixSpelling(storage()) }), [page]);

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

  // ⌘K / ctrl-K, and `?` for help — the two bindings that work from anywhere.
  // Everything else belongs to `logic/keys.ts` and the page that has focus.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const typing = isTyping(e.target);
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setPalette((v) => !v);
        return;
      }
      if (e.key === "?" && !typing) {
        e.preventDefault();
        setPage((p) => (p === "help" ? "work" : "help"));
        return;
      }
      if (e.key === "Escape" && !typing && page === "help") setPage("work");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [page]);

  const term = useMemo(() => (pal ? termColors(pal) : { fg: "#d7dde5", bg: "#0e1116" }), [pal]);

  const on: ShellCallbacks = useMemo(
    () => ({
      setFocus,
      setPage,
      setWsId: (id: string) => {
        setWsId(id);
        // A pane belongs to a project; carrying the selection across would put
        // another project's terminal on this one's stage.
        setPane(null);
        setPath(null);
      },
      setPane,
      setPath,
      setSel,
      setTopic,
      setRails: (open: boolean) => setRails(open ? "open" : "auto"),
      gitMenu: () => setMenu(true),
    }),
    [],
  );

  const view: ShellView = {
    rails,
    pane: stagePane,
    path,
    pin: prefs.defaultAgent || null,
    // Anything in flight, plus an operation the daemon says is still running.
    // The first covers the window between the click and the reply, which is
    // exactly when a second click on `push` happens.
    busy: busy || (world.gitOp?.running === true && ws != null && world.gitOp.ws === ws.id),
    sel,
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

  const facts: SettingsFacts = { agents: agentTypes, daemonVersion };

  return (
    <TooltipProvider delayDuration={200}>
      <div className="flex h-full min-h-0 flex-col bg-background text-foreground">
        <header className="flex h-row-lg shrink-0 items-center gap-2 border-b border-border bg-card px-3 shadow-xs">
          <span className="text-14 font-semibold tracking-tight">butai</span>

          {/* The tab bar spans machines, exactly as the terminal's does: one row
              of projects with the machine as a badge, never a machine picker
              above a project picker. */}
          <nav className="flex min-w-0 items-center gap-1 overflow-x-auto">
            {spaces.map((w) => (
              <Button
                key={String(w.id)}
                size="sm"
                variant={ws && String(w.id) === String(ws.id) ? "secondary" : "ghost"}
                onClick={() => on.setWsId(String(w.id))}
              >
                <span className="truncate">{w.name}</span>
                {world.daemons.length > 1 && machineOf(w) ? (
                  <Badge variant="outline" className="ml-1">
                    {machineOf(w)}
                  </Badge>
                ) : null}
                {unreadOf(w) ? <span className="ml-1 text-primary">•</span> : null}
              </Button>
            ))}
          </nav>

          <div className="flex-1" />

          <div className="flex items-center gap-1">
            {TABS.map((p) => (
              <Button key={p} size="sm" variant={page === p ? "secondary" : "ghost"} onClick={() => setPage(p)}>
                {p}
              </Button>
            ))}
          </div>

          <Button size="sm" variant="ghost" onClick={() => setPalette(true)} title="Command palette (⌘K)">
            ⌘K
          </Button>
          <Button
            size="icon-sm"
            variant={page === "settings" ? "secondary" : "ghost"}
            aria-label="settings"
            onClick={() => setPage((p) => (p === "settings" ? "work" : "settings"))}
          >
            ⚙
          </Button>
        </header>

        <main className="min-h-0 flex-1 overflow-hidden">
          {!world.loaded ? (
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
              on={on}
            />
          )}
        </main>

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
                      {p}
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
                  <CommandItem value="theme dark" onSelect={() => { storeTheme("web-dark"); location.reload(); }}>
                    Dark
                  </CommandItem>
                  <CommandItem value="theme light" onSelect={() => { storeTheme("web-light"); location.reload(); }}>
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

        <PatchDialog patch={patch} onClose={() => setPatch(null)} />

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

/**
 * The `g` menu: `git-menu.ts`'s table, drawn.
 *
 * **One deliberate difference from the terminal's.** There the menu is a stack
 * of flat lists — choose a group, the list is replaced — because a terminal list
 * cannot be filtered as you type. Here it is one searchable list with the groups
 * as headings, which is the same table read the same way and reaches `push
 * --force-with-lease` in four keystrokes instead of two navigations. The rows,
 * the grouping and which rows are worth offering mid-sequence are all still
 * `git-menu.ts`'s answer: `groupsFor`/`itemsFor` are what filter, so a row this
 * client offers and the terminal does not is impossible.
 */
function GitMenu({
  open,
  cx,
  onOpenChange,
  onPick,
}: {
  open: boolean;
  cx: MenuCx;
  onOpenChange: (open: boolean) => void;
  onPick: (action: GitActionId) => void;
}) {
  return (
    <CmdDialog open={open} onOpenChange={onOpenChange}>
      <CmdContent className="p-0">
        <Command>
          <CommandInput placeholder="A git operation…" />
          <CommandList>
            <CommandEmpty>Nothing matches.</CommandEmpty>
            {groupsFor(cx).map((g) => (
              <CommandGroup key={g.id} heading={g.label}>
                {itemsFor(g.id, cx).map((i) => (
                  <CommandItem key={i.action} value={`${g.label} ${i.label}`} onSelect={() => onPick(i.action)}>
                    {i.label}
                  </CommandItem>
                ))}
              </CommandGroup>
            ))}
          </CommandList>
        </Command>
      </CmdContent>
    </CmdDialog>
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

function unreadOf(w: QualifiedWorkspace): number {
  return (w as { unread?: number }).unread ?? 0;
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
