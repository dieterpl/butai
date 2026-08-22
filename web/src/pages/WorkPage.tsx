// WORK — the workbench: three rails, one live pane, one hint bar.
//
// The port of `web/ui/work.js`, which is itself the port of `butai-app.js`'s
// work page and the four rail elements inside it (`<butai-agents>`,
// `<butai-processes>`, `<butai-system>`, `<butai-changes>`). Between them those
// carried four of `web/UI-REWRITE.md`'s nine symptoms, and each is fixed by
// *deleting* something rather than restyling it:
//
//   | symptom | what is gone |
//   |---|---|
//   | eight rows all reading `crates/butai-client/src/…` | the single truncating box; `Path` is two |
//   | `+102 -0` floating after the filename | free-flowing text; `DiffStat` is two fixed cells |
//   | outline · grey fill · blue fill in one rail | three hand-rolled `<button>` styles; one `Button` |
//   | SYSTEM bars ending at an arbitrary x | the sparkline; `Meter` always draws a track |
//
// ## This page draws. It does not decide.
//
// Every row here is a prop and every gesture is a callback: the shell owns the
// world, the selection and the keyboard. `verbs.ts`, `dom.ts` and `fleet.ts` are
// imported rather than reimplemented, which is what keeps this a *view*-layer
// port — and what keeps the footer teaching the same keys the terminal does.
//
// The one piece of state that is genuinely this page's is the commit message,
// because it is a half-typed sentence and not a fact about the repository. It is
// also the bug that state fixes: the vanilla rail rebuilt its `<input>` on every
// pushed record and had to put the caret back by hand afterwards, `try`/`catch`
// and all. A controlled React input is never replaced, so there is no caret to
// restore.
//
// ## One hint bar, for the surface the keyboard is on
//
// The vanilla client draws a footer *inside* each rail, which is three footers
// on screen teaching keys that only work in one of them. `HintBar` spans the
// page and shows the focused surface's verbs — the same table, packed by the
// same `fits()` at the same column count the terminal uses, because which verbs
// earn a column is the terminal's decision and not a layout choice.

import { useState } from "react";
import type { ReactNode } from "react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ScrollArea } from "@/components/ui/scroll-area";
import { DiffStat } from "@/components/DiffStat";
import { Empty } from "@/components/Empty";
import { Gauge } from "@/components/Gauge";
import { HintBar, type Hint } from "@/components/HintBar";
import { Notice } from "@/components/Notice";
import { Path } from "@/components/Path";
import { Row } from "@/components/Row";
import { SectionTitle } from "@/components/SectionTitle";
import { Stage, type StageEvents } from "@/stage/Stage";
import { cn } from "@/lib/utils";
import type { World } from "@/app/world.ts";
import { CHANGES_COLS, RAIL_COLS } from "@/logic/dom.ts";
import { daemonOf } from "@/logic/events.ts";
import type { Qid, QualifiedAgent, QualifiedProcess, QualifiedWorkspace } from "@/logic/events.ts";
import type { TermTheme } from "@/logic/palette.ts";
import { ChangesRow, GLOBAL, VerbId, agentsVerbs, changesFooter, procsVerbs } from "@/logic/verbs.ts";
import type {
  ChangesDto,
  ConflictFile,
  FileChange,
  RepoState,
  ResolveSide,
  SequenceAction,
  SysDto,
} from "@/protocol/generated/protocol.ts";

import { MARK_BADGE, MARK_TONE, SCROLLER, agentMark, hints, procBadge, sysGauges } from "./parts.ts";

// ---------------------------------------------------------------------------
// What the page is handed
// ---------------------------------------------------------------------------

/// Which surface the keyboard is on, as this page spells the four it lights up.
///
/// The prop below is a plain `string`, because the shell's focus is one string
/// across every page (`Shell.tsx`'s `PageProps`) and a union here would make
/// every other page's spelling a compile error in *its* file. **Anything that
/// is not one of these reads as the stage**, which is the arm that draws no
/// ring and offers the way back out — the same default the vanilla page had.
export type WorkFocus = "stage" | "agents" | "procs" | "changes" | "system";

/// Everything on this page that writes.
///
/// One method per gesture, and every one of them is also a key in `verbs.ts` —
/// that is the rule the click registry enforces in the vanilla client and the
/// property this page keeps: nothing here is reachable by pointer alone.
///
/// None of them throws. `src/app/actions.ts` reports through a toast and
/// returns, so this page has no `try` in it and no error state of its own.
export interface WorkActions {
  /// Spawn an agent. `choose` forces the picker even when SETTINGS has pinned
  /// one — `a` and `A` in the AGENTS table.
  spawn(choose: boolean): void;
  /// "I saw it": clear a pane's bell without opening it. The daemon clears one
  /// when a client *looks* at a pane, and reading the rail is not looking.
  ack(pane: Qid): void;
  kill(pane: Qid): void;
  newProc(): void;
  restart(pane: Qid): void;
  stage(path: string): void;
  unstage(path: string): void;
  commit(message: string): void;
  commitAll(message: string): void;
  resolve(path: string, take: ResolveSide): void;
  sequence(action: SequenceAction): void;
  fetch(): void;
  pull(): void;
  push(): void;
  branch(): void;
  openDiff(what: { path: string; staged: boolean }): void;
  showCommit(id: string): void;
  /// A footer hint is a button, so clicking one dispatches the key it draws.
  /// The surface is passed because the same letter means different things on
  /// two rails — `x` kills, `t` starts a shell — and the dispatch is the
  /// shell's one table.
  press(surface: string, key: string): void;
}

/// View-state changes. Nothing here reaches the daemon.
export interface WorkCallbacks {
  /// Put this pane on the stage.
  selectPane(pane: Qid): void;
  /// The left rail as a drawer, under the `md` breakpoint.
  rails(open: boolean): void;
}

/// The shell's view state for this page: a cursor, a draft selection, and two
/// preferences. Held above the page because the keyboard moves all of it and
/// the keyboard is not the page's.
export interface WorkView {
  /// The pane on the stage, qualified — `"gpu:5"`, never `5`.
  pane: Qid | null;
  /// The path the CHANGES rail has open, so its row draws as selected.
  path: string | null;
  /// Which kind of row the changes cursor is on. It feeds the footer and
  /// nothing else, which is why it is the union rather than a string — the
  /// verb table takes exactly this type.
  ///
  /// Optional because a shell that does not track a cursor in that rail should
  /// say so rather than name a kind: absent is `ChangesRow.None`, and the
  /// footer then offers only the verbs that apply whatever is selected.
  changesRow?: ChangesRow | undefined;
  /// SETTINGS' default agent, or null. It is why the AGENTS header has two
  /// buttons or one.
  pin: string | null;
  /// A git operation is in flight, or a sequence is in progress: the remote
  /// verbs are disabled.
  busy: boolean;
  /// `"open"` slides the left rail over the stage — what the burger does below
  /// `md`, where three columns are unusable.
  rails: "auto" | "open";
}

export interface WorkPageProps {
  /// Every daemon and every workspace. Read for one thing only: the telemetry
  /// of the machine `ws` is on — see [`systemFor`].
  world: World;
  /// The current workspace, already qualified. `null` while there is nothing
  /// selected: the rails then say so rather than drawing empty lists, because
  /// "no workspace" and "an empty workspace" are different sentences and only
  /// one of them is a reason to press `+ agent`.
  ws: QualifiedWorkspace | null;
  actions: WorkActions;
  /// One of [`WorkFocus`]; anything else reads as the stage.
  focus: string;
  on: WorkCallbacks;
  view: WorkView;
  /// What a `"default"` cell on the stage resolves to. A prop because a canvas
  /// cannot inherit a custom property — see `Stage`.
  theme: TermTheme;
  /// The stage's cell size in CSS pixels. Omitted leaves the renderer's own.
  fontPx?: number | undefined;
  /// The stage's own events — a bell, a refused pane, a daemon whose version
  /// disagrees with this client's. Optional, and the page only forwards them:
  /// dropping a selection the daemon has refused is the shell's call, and
  /// without this there would be no way for it to hear about one.
  stage?: StageEvents | undefined;
}

// ---------------------------------------------------------------------------
// A rail section
// ---------------------------------------------------------------------------

interface SecProps {
  title: string;
  action?: ReactNode;
  focused?: boolean | undefined;
  /// Take the space that is left, and scroll. AGENTS and PROCESSES do; SYSTEM
  /// is as tall as its gauges and sits at the bottom, as it does in the
  /// terminal.
  grow?: boolean | undefined;
  /// The body is a homogeneous list of selectable rows, so it is the `listbox`
  /// `Row`'s `role="option"` needs around it. Off for CHANGES, whose body is
  /// four lists, two grids of buttons and a text field — a listbox with those
  /// in it is worse than no listbox at all.
  list?: boolean | undefined;
  children?: ReactNode;
}

// `flex-auto` rather than `flex-1`, and the difference is visible: `flex-1` is
// `flex:1 1 0%`, which gives AGENTS and PROCESSES *half the rail each* whatever
// is in them — eleven agents scrolling inside 400px above one shell sitting in
// 400px of nothing. Growing from the content instead gives the long list the
// room and the short one what it needs.
//
// `ring-primary`, where a `Row`'s selection ring is `ring-ring`. The two say
// different things and the terminal already distinguishes them: `--focus` is
// translucent because it draws *around* something you can see, and which rail
// has the keyboard is the brand colour there (`:host(.focused)`) for the same
// reason it is here — at one hairline around a whole column, a translucent ring
// is a ring nobody sees.
function Sec({ title, action, focused, grow, list, children }: SecProps) {
  const body = (
    <div className="py-1" role={list ? "listbox" : undefined} aria-label={list ? title : undefined}>
      {children}
    </div>
  );
  return (
    <section
      className={cn(
        "flex min-h-0 min-w-0 flex-col border-b border-border",
        grow ? "flex-auto" : "shrink-0",
        focused && "ring-1 ring-inset ring-primary",
      )}
    >
      <SectionTitle action={action}>{title}</SectionTitle>
      {grow ? <ScrollArea className={cn("min-h-0 flex-1", SCROLLER)}>{body}</ScrollArea> : body}
    </section>
  );
}

// ---------------------------------------------------------------------------
// AGENTS
// ---------------------------------------------------------------------------

export interface AgentsRailProps {
  agents: readonly QualifiedAgent[];
  selPane: Qid | null;
  /// What the rail says instead of rows — "(none)", or why the list is empty.
  note?: string | null | undefined;
  /// SETTINGS' default agent.
  pin: string | null;
  focused: boolean;
  actions: WorkActions;
  on: WorkCallbacks;
}

/// The agents in this workspace, and everything you can do to one.
///
/// `pin` is why the header has two buttons or one: unpinned there is a single
/// verb, and a `...` beside `+ agent` would be a second button for the same
/// thing under a second name.
export function AgentsRail({ agents, selPane, note, pin, focused, actions, on }: AgentsRailProps) {
  const action = (
    <>
      <Button
        size="sm"
        variant="outline"
        onClick={() => actions.spawn(false)}
        title={pin ? `Spawn ${pin} (a)` : "Spawn agent (a)"}
      >
        {pin ? `+ ${pin}` : "+ agent"}
      </Button>
      {pin ? (
        <Button size="sm" variant="ghost" onClick={() => actions.spawn(true)} title="Choose which (A)">
          ...
        </Button>
      ) : null}
    </>
  );
  return (
    <Sec title="agents" action={action} focused={focused} grow list>
      {!agents.length ? <Empty>{note ?? "(none)"}</Empty> : null}
      {agents.map((a) => {
        const m = agentMark(a);
        const badge = MARK_BADGE[m.tone];
        return (
          <Row
            key={a.pane}
            selected={a.pane === selPane}
            onSelect={() => on.selectPane(a.pane)}
            title={`${a.title} — ${m.label}`}
          >
            <span className={cn("shrink-0 font-mono", MARK_TONE[m.tone])}>{m.glyph}</span>
            <span className="min-w-0 flex-1 truncate">{a.title}</span>
            <Badge variant={badge.variant} className={badge.className}>
              {m.short}
            </Badge>
            {a.state === "waiting" ? (
              // The daemon clears a bell when a client *looks* at a pane, and
              // reading the rail is not looking. This is "yes, I saw it".
              <Button
                size="icon-sm"
                variant="ghost"
                title="Answered — clear waiting (c)"
                onClick={(e) => {
                  e.stopPropagation();
                  actions.ack(a.pane);
                }}
              >
                ✓
              </Button>
            ) : null}
            <Button
              size="icon-sm"
              variant="ghost"
              title="Kill (x)"
              onClick={(e) => {
                e.stopPropagation();
                actions.kill(a.pane);
              }}
            >
              ✕
            </Button>
          </Row>
        );
      })}
    </Sec>
  );
}

// ---------------------------------------------------------------------------
// PROCESSES
// ---------------------------------------------------------------------------

export interface ProcessesRailProps {
  processes: readonly QualifiedProcess[];
  selPane: Qid | null;
  note?: string | null | undefined;
  focused: boolean;
  actions: WorkActions;
  on: WorkCallbacks;
}

export function ProcessesRail({ processes, selPane, note, focused, actions, on }: ProcessesRailProps) {
  const action = (
    <Button size="sm" variant="outline" title="Start a shell (t)" onClick={() => actions.newProc()}>
      + term
    </Button>
  );
  return (
    <Sec title="processes" action={action} focused={focused} grow list>
      {!processes.length ? <Empty>{note ?? "(none)"}</Empty> : null}
      {processes.map((p) => {
        const badge = procBadge(p.status);
        return (
          <Row
            key={p.pane}
            selected={p.pane === selPane}
            onSelect={() => on.selectPane(p.pane)}
            title={`${p.name} — ${p.command}`}
          >
            <span className="min-w-0 shrink-0 truncate font-medium">{p.name}</span>
            {/* The command is `font-mono`: it is a line you would compare
                character by character against the one in `.butai.toml`, which
                is the kit's test for mono. */}
            <span className="min-w-0 flex-1 truncate font-mono text-dim">{p.command}</span>
            <Badge variant={badge.variant} className={badge.className}>
              {p.status}
            </Badge>
            <Button
              size="icon-sm"
              variant="ghost"
              title="Restart (r)"
              onClick={(e) => {
                e.stopPropagation();
                actions.restart(p.pane);
              }}
            >
              ⟳
            </Button>
            <Button
              size="icon-sm"
              variant="ghost"
              title="Kill (x)"
              onClick={(e) => {
                e.stopPropagation();
                actions.kill(p.pane);
              }}
            >
              ✕
            </Button>
          </Row>
        );
      })}
    </Sec>
  );
}

// ---------------------------------------------------------------------------
// SYSTEM
// ---------------------------------------------------------------------------

/// One machine's load, as gauges rather than sparklines.
///
/// The sparkline is what the audit was looking at: twelve block characters at
/// `letter-spacing:-1px` end at whatever x the last sample puts them, with
/// nothing behind them to measure against and the right-aligned value floating
/// clear. `Meter` always draws a track and shares the gutter with the number, so
/// the bar and the value are one reading. What is lost is the last sixteen
/// samples; what is gained is being able to tell 40% from 90% at a glance, which
/// is the question a rail gauge is actually asked.
export function SystemRail({ system, focused }: { system: SysDto | null; focused: boolean }) {
  const gauges = sysGauges(system);
  return (
    <Sec title="system" focused={focused}>
      {!gauges.length ? <Empty>no telemetry</Empty> : null}
      {gauges.map((g) => (
        <Gauge key={g.key} label={g.label} value={g.value} tone={g.tone} text={g.text} />
      ))}
    </Sec>
  );
}

// ---------------------------------------------------------------------------
// CHANGES
// ---------------------------------------------------------------------------

// The status letter's colour, by what it means rather than by where the row is:
// an untracked file is not a modification, and a staged one is already safe.
function codeTone(code: string, staged: boolean): string {
  if (code === "?") return "text-bad";
  return staged ? "text-ok" : "text-warn";
}

// Which side of a conflict is still there. A delete/modify conflict has no
// "theirs" to take — the vanilla rail leaves that button out, and this one draws
// it disabled with the reason in its title, because a row whose buttons come and
// go is a row you have to re-read, and "there is no theirs" is the thing you
// wanted to know anyway.
//
// The three of them sit *under* the path rather than beside it, and that is the
// truncation bug again in its narrowest form: `ours · theirs · done` is 150px of
// a 320px rail, which left `crates/butai-client/src/workbench.rs` about eight
// characters — and this is the one list where the file is the whole question.
function conflictSide(f: ConflictFile): string {
  return f.ours && f.theirs ? "both" : f.ours ? "deleted by them" : f.theirs ? "deleted by us" : "gone";
}

const SEQ_LABEL: Readonly<Partial<Record<RepoState, string>>> = Object.freeze({
  merge: "merging",
  rebase: "rebasing",
  cherry_pick: "cherry-picking",
  revert: "reverting",
  bisect: "bisecting",
});

export interface ChangesRailProps {
  changes: ChangesDto | null;
  note?: string | null | undefined;
  /// A git operation is in flight. It disables the remote verbs, because
  /// pushing halfway through a rebase is never what anyone means and the daemon
  /// would refuse it anyway.
  busy: boolean;
  focused: boolean;
  /// The path whose diff is open.
  selected: string | null;
  actions: WorkActions;
}

/// The working tree, and everything you can do to it.
export function ChangesRail({ changes, note, busy, focused, selected, actions }: ChangesRailProps) {
  // This page's one piece of state, and the reason it is here: a half-typed
  // sentence is not a fact about the repository, so it cannot come from above
  // and must survive every pushed record that redraws the rail around it.
  const [draft, setDraft] = useState("");

  if (!changes) {
    return (
      <Sec title="changes" focused={focused} grow>
        <Empty>{note ?? "not a git repository"}</Empty>
      </Sec>
    );
  }

  const ch = changes;
  const conflicted = ch.conflicted ?? [];
  const n = ch.staged.length + ch.unstaged.length + conflicted.length;
  const seq = ch.state !== "clean";
  const stop = busy || seq;
  const arrows = (ch.ahead ? "↑" + ch.ahead : "") + (ch.behind ? "↓" + ch.behind : "");
  const commit = (all: boolean) => {
    const msg = draft.trim();
    if (!msg) return;
    (all ? actions.commitAll : actions.commit)(msg);
    setDraft("");
  };

  // The branch is a *button* in the header's action slot — the one place a
  // section header has for the thing it is about. Four headers differed only in
  // what they put there; this is what that slot is for.
  const action = (
    <>
      <Button size="sm" variant="ghost" title="Switch branch (b)" onClick={() => actions.branch()}>
        {ch.branch}
      </Button>
      {arrows ? (
        <Badge variant="outline" title={ch.upstream ? `vs ${ch.upstream}` : undefined}>
          {arrows}
        </Badge>
      ) : null}
      <Badge variant="outline">{n}</Badge>
    </>
  );

  const fileRow = (f: FileChange, staged: boolean) => (
    <Row
      key={(staged ? "s:" : "u:") + f.path}
      selected={selected === f.path}
      onSelect={() => actions.openDiff({ path: f.path, staged })}
    >
      <span className={cn("w-3 shrink-0 text-center font-mono", codeTone(f.code, staged))}>{f.code}</span>
      <Path path={f.path} />
      <DiffStat added={f.added} deleted={f.deleted} />
      <Button
        size="icon-sm"
        variant="ghost"
        title={staged ? "Unstage (u)" : "Stage (s)"}
        onClick={(e) => {
          e.stopPropagation();
          (staged ? actions.unstage : actions.stage)(f.path);
        }}
      >
        {staged ? "−" : "+"}
      </Button>
    </Row>
  );

  return (
    <Sec title="changes" action={action} focused={focused} grow>
      {seq ? (
        <Notice variant="bad" className="m-3 flex flex-wrap items-center gap-2 p-2">
          <Badge variant="destructive">{SEQ_LABEL[ch.state] ?? "in progress"}</Badge>
          <span className="text-11 text-dim">
            {conflicted.length ? `${conflicted.length} conflicted` : "no conflicts left"}
          </span>
          <span className="ml-auto flex items-center gap-2">
            <Button
              size="sm"
              variant="outline"
              disabled={conflicted.length > 0}
              title={conflicted.length ? "Resolve the conflicts first" : "Carry on (y)"}
              onClick={() => actions.sequence("continue")}
            >
              Continue
            </Button>
            <Button
              size="sm"
              variant="destructive"
              title="Give up on it (n)"
              onClick={() => actions.sequence("abort")}
            >
              Abort
            </Button>
          </span>
        </Notice>
      ) : null}

      {conflicted.length ? <SectionTitle>conflicts</SectionTitle> : null}
      {conflicted.map((f) => (
        <div key={`c:${f.path}`} className="pb-1">
          <Row selected={selected === f.path} onSelect={() => actions.openDiff({ path: f.path, staged: false })}>
            <span className="w-3 shrink-0 text-center font-mono font-semibold text-bad">!</span>
            <Path path={f.path} />
            <Badge variant="destructive">{conflictSide(f)}</Badge>
          </Row>
          <div className="grid grid-cols-3 gap-2 px-3">
            <Button
              size="sm"
              variant="outline"
              disabled={!f.ours}
              title={f.ours ? "Keep our version (o)" : "Deleted by us — there is no ours to take"}
              onClick={() => actions.resolve(f.path, "ours")}
            >
              ours
            </Button>
            <Button
              size="sm"
              variant="outline"
              disabled={!f.theirs}
              title={f.theirs ? "Keep their version (t)" : "Deleted by them — there is no theirs to take"}
              onClick={() => actions.resolve(f.path, "theirs")}
            >
              theirs
            </Button>
            <Button
              size="sm"
              variant="outline"
              title="I edited it by hand — mark resolved (a)"
              onClick={() => actions.resolve(f.path, "resolved")}
            >
              done
            </Button>
          </div>
        </div>
      ))}

      <SectionTitle>unstaged</SectionTitle>
      {ch.unstaged.length ? ch.unstaged.map((f) => fileRow(f, false)) : <Empty>(clean)</Empty>}

      <SectionTitle>staged</SectionTitle>
      {ch.staged.length ? ch.staged.map((f) => fileRow(f, true)) : <Empty>(nothing staged)</Empty>}

      <div className="grid grid-cols-3 gap-2 p-3">
        <Button size="sm" variant="outline" disabled={stop} title="git fetch --prune (f)" onClick={() => actions.fetch()}>
          Fetch
        </Button>
        <Button
          size="sm"
          variant="outline"
          disabled={stop}
          title={`${ch.behind ? `${ch.behind} behind` : "git pull"} (P)`}
          onClick={() => actions.pull()}
        >
          Pull
        </Button>
        <Button
          size="sm"
          variant="outline"
          disabled={stop}
          title={`${ch.ahead ? `${ch.ahead} to push` : "git push"} (p)`}
          onClick={() => actions.push()}
        >
          Push
        </Button>
      </div>

      <div className="flex flex-col gap-2 px-3 pb-3">
        <Input
          placeholder="Commit message"
          value={draft}
          aria-label="Commit message"
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") commit(false);
          }}
        />
        <div className="grid grid-cols-2 gap-2">
          <Button
            size="sm"
            variant="secondary"
            onClick={() => commit(false)}
            disabled={!ch.staged.length || !!conflicted.length}
            title={conflicted.length ? "Resolve the conflicts first" : "Commit (c)"}
          >
            Commit
          </Button>
          <Button
            size="sm"
            onClick={() => commit(true)}
            disabled={!n || !!conflicted.length}
            title={conflicted.length ? "Resolve the conflicts first" : "Stage everything, then commit (C)"}
          >
            Commit all
          </Button>
        </div>
      </div>

      {ch.recent_commits.length ? <SectionTitle>recent</SectionTitle> : null}
      {ch.recent_commits.map((c) => (
        <Row key={c.id} onSelect={() => actions.showCommit(c.id)} title="Show this commit's diff (d)">
          <span className="shrink-0 font-mono tabular-nums text-primary">{c.id}</span>
          <span className="min-w-0 flex-1 truncate text-dim">{c.summary}</span>
        </Row>
      ))}
    </Sec>
  );
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

// Where the keyboard goes from the stage, off the Alt layer. Nothing else is
// worth a hint here: with the pane focused every unmodified key is the
// program's, which is what makes it a terminal and not a preview, so the only
// useful thing to write down is the way back out.
const OFF_STAGE: Hint[] = [
  VerbId.FocusOff,
  VerbId.FocusAgents,
  VerbId.FocusProcs,
  VerbId.FocusChanges,
].flatMap((id) => {
  const g = GLOBAL.find((v) => v.id === id);
  // A verb with no Alt spelling has no way *out of the stage*: the prefix layer
  // reaches it, but the prefix is a chord and this bar is about the one key.
  return g?.alt ? [{ key: `alt-${g.alt}`, label: g.label }] : [];
});

/// What [`workHints`] reads: everything the footer's shape depends on that is
/// not the focus itself.
export interface WorkHintOpts {
  pin?: string | null | undefined;
  changesRow?: ChangesRow | undefined;
  ahead?: number | undefined;
  sequence?: boolean | undefined;
}

/// The verbs of whichever surface has the keyboard.
///
/// Exported because the shell's `?` reference and its key dispatch read the
/// same tables: a footer word *is* a key here, and the two must not be able to
/// disagree about which.
export function workHints(
  focus: string,
  opts?: WorkHintOpts | null,
  press?: ((surface: string, key: string) => void) | undefined,
): Hint[] {
  const o = opts ?? {};
  if (focus === "agents") return hints(agentsVerbs(!!o.pin), RAIL_COLS, 1, (k) => press?.("agents", k));
  if (focus === "procs") return hints(procsVerbs(), RAIL_COLS, 1, (k) => press?.("procs", k));
  if (focus === "changes") {
    return hints(
      changesFooter(o.changesRow ?? ChangesRow.None, { ahead: o.ahead ?? 0, sequence: !!o.sequence }),
      CHANGES_COLS,
      3,
      (k) => press?.("changes", k),
    );
  }
  return OFF_STAGE;
}

// The telemetry for the machine this workspace is on, never this box's — a rail
// about a machine that shows a different machine's load is worse than blank.
// The primary's is the fallback, which is what a single-daemon client reads.
function systemFor(world: World, ws: QualifiedWorkspace | null): SysDto | null {
  const key = ws ? (ws.daemon ?? daemonOf(ws.id)) : null;
  const d = key == null ? undefined : world.daemons.find((x) => x.key === key);
  return d?.system ?? world.system ?? null;
}

// `logs:` followers are real `docker logs -f` processes the DOCKER page spawns
// and reaps. They are the client's own plumbing, so the rail that lists what
// *you* started does not list them.
function visible(procs: readonly QualifiedProcess[] | null | undefined): QualifiedProcess[] {
  return (procs ?? []).filter((p) => !String(p.name ?? "").startsWith("logs:"));
}

export function WorkPage({ world, ws, actions, focus, on, view, theme, fontPx, stage }: WorkPageProps) {
  // Below the width where three columns are unusable the rails give way and the
  // page is the pane, which is what every page here does with its rails. But
  // "give way" must not mean "become unreachable": `rails="open"` slides the
  // left one over the stage, which is what the vanilla client's burger does at
  // 860px, and the shell owns that bit of state exactly as it owns the rest.
  const drawer = view.rails === "open";
  // The bridge served a stand-in because its detail fetch failed. The rails are
  // empty because we do not know what is in them, which must not read as
  // "nothing".
  const note = ws?.detail_error ? `unavailable — ${ws.detail_error}` : null;
  const ch = ws ? ws.changes : null;

  return (
    <div className="flex h-full min-h-0 min-w-0 flex-col">
      <div
        className={cn(
          "relative grid min-h-0 flex-1",
          "[grid-template-columns:1fr]",
          "md:[grid-template-columns:minmax(240px,300px)_1fr]",
          "xl:[grid-template-columns:minmax(240px,300px)_1fr_minmax(260px,320px)]",
        )}
      >
        {/* The same scrim `ui/dialog.tsx` draws, because it is the same
            gesture: something is in front of the page and the way out is to
            click past it. */}
        {drawer ? (
          <div className="absolute inset-0 z-10 bg-black/50 md:hidden" onClick={() => on.rails(false)} />
        ) : null}
        <aside
          className={cn(
            "min-h-0 min-w-0 flex-col border-r border-border bg-card",
            drawer
              ? "absolute inset-y-0 left-0 z-20 flex w-4/5 max-w-xs shadow-lg md:static md:w-auto md:max-w-none"
              : "hidden md:flex",
          )}
        >
          <AgentsRail
            agents={ws ? ws.agents : []}
            selPane={view.pane}
            note={note}
            pin={view.pin}
            focused={focus === "agents"}
            actions={actions}
            on={on}
          />
          <ProcessesRail
            processes={ws ? visible(ws.processes) : []}
            selPane={view.pane}
            note={note}
            focused={focus === "procs"}
            actions={actions}
            on={on}
          />
          <SystemRail system={systemFor(world, ws)} focused={focus === "system"} />
        </aside>

        <Stage
          pane={view.pane}
          theme={theme}
          className="min-w-0"
          {...(fontPx != null ? { fontPx } : {})}
          {...(stage ?? {})}
        />

        <aside className="hidden min-h-0 min-w-0 flex-col border-l border-border bg-card xl:flex">
          <ChangesRail
            changes={ch}
            note={note}
            busy={view.busy}
            selected={view.path}
            focused={focus === "changes"}
            actions={actions}
          />
        </aside>
      </div>
      <HintBar
        keys={workHints(
          focus,
          {
            pin: view.pin,
            changesRow: view.changesRow,
            ahead: ch ? ch.ahead : 0,
            sequence: !!ch && ch.state !== "clean",
          },
          actions.press,
        )}
      />
    </div>
  );
}
