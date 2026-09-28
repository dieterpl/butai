// BOOTH — every workspace across every connected machine. The fleet cursor
// selects the live preview; streaming never takes focus away from the list.
// Fleet, stage and compute share the terminal's three-column geometry. The pure
// logic/fleet model owns grouping, attention ranking, folding and qualified ids.

import { useEffect, useMemo, useRef } from "react";

import { AgentStatus } from "@/components/AgentStatus";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Empty } from "@/components/Empty";
import { Meter } from "@/components/Meter";
import { Gauge } from "@/components/Gauge";
import { HintBar } from "@/components/HintBar";
import { Row } from "@/components/Row";
import { SectionTitle } from "@/components/SectionTitle";
import { Stat } from "@/components/Stat";
import { Stage, type StageEvents } from "@/stage/Stage";
import { cn } from "@/lib/utils";
import type { World } from "@/app/world.ts";
import { RAIL_COLS } from "@/logic/dom.ts";
import type { Qid } from "@/logic/events.ts";
import {
  HomeRowKind,
  NO_FOLDS,
  allAgentRows,
  fleetSpaces,
  homePreview,
  homeRows,
  homeTray,
  machineIsDown,
  machinePressure,
  machineRows,
  toggleFold,
  type AgentRow,
  type Folds,
  type HomeRow,
  type MachineRow,
  type SpaceRow,
} from "@/logic/fleet.ts";
import type { TermTheme } from "@/logic/palette.ts";
import { homeVerbs } from "@/logic/verbs.ts";

import { MARK_TONE, SCROLLER, agentMark, hints, loadTone, num, sysGauges } from "./parts.ts";

// ---------------------------------------------------------------------------
// What the page is handed
// ---------------------------------------------------------------------------

/// Everything this page does that is not drawing.
///
/// One method, because the fleet's whole table is four keys: it navigates and
/// it opens. `homeVerbs()` says so, and a `x kill` here would be a key for a
/// thing that is not there.
export interface HomeActions {
  /// A footer hint is a button, so clicking one dispatches the key it draws.
  press(surface: string, key: string): void;
}

/// View-state changes. Nothing here reaches a daemon — except `start`, which
/// is the one thing on this page that does, and goes through `actions`.
export interface HomeCallbacks {
  /// Walk the cursor to this index in the **row list** — machines and projects
  /// included, folded-away rows excluded.
  walk(sel: number): void;
  /// Focus a chat's live preview, revealing it if its project was folded.
  preview(row: AgentRow): void;
  /// `enter` on an agent: go to its project, on its own machine, and stage it.
  /// Both ids are qualified, and both halves are needed — the workspace to
  /// switch to and the pane to put on the stage may be on a machine that is not
  /// the active tab's.
  open(where: { ws: Qid; pane: Qid }): void;
  /// The explicit workspace `open` button travels there. A press elsewhere on
  /// the row only selects its preview.
  go(ws: Qid): void;
  /// Fold or unfold a machine or a project.
  fold(next: Folds): void;
  /// Start this project's preferred agent in it, without moving the page.
  start(space: SpaceRow): void;
  /// Close that workspace, and everything running in it. Asks first — this is
  /// the one press on the page that takes something away.
  close(space: SpaceRow): void;
  /// End this exact chat, using its own workspace and machine.
  closeChat(row: AgentRow): void;
}

export interface HomePageProps {
  /// Every daemon and every workspace. The fleet is derived from it here rather
  /// than passed in: `allAgentRows` and `machineRows` are pure, so the page and
  /// the shell reading the same world cannot disagree about what is in the
  /// list — and the cursor below counts rows of exactly that list.
  world: World;
  actions: HomeActions;
  on: HomeCallbacks;
  /// The cursor's index in the row list. A machine and a project are rows you
  /// *can* now sit on — starting a session belongs to a project and so does
  /// going somewhere — and the agent under the cursor is derived from the row
  /// rather than tracked beside it.
  sel?: number | undefined;
  /// What is folded away, and which machines have their gauges out.
  folds?: Folds | undefined;
  /// SETTINGS' pinned agent — what a project's `+` falls back to naming when
  /// the project itself declares none.
  pin?: string | null | undefined;
  /// The pane on the stage. `null`/absent follows the cursor, which is the
  /// ordinary case — see below.
  pane?: Qid | null | undefined;
  /// What a `"default"` cell on the stage resolves to. A prop because a canvas
  /// cannot inherit a custom property — see `Stage`.
  theme: TermTheme;
  fontPx?: number | undefined;
  /// The stage's own events — a bell, a refused pane, a version mismatch. The
  /// page only forwards them; dropping a refused pane is the shell's call.
  stage?: StageEvents | undefined;
}

/// `machine:project`, or just the project with one daemon connected — the same
/// rule the tab bar's badge follows, and the same one `AllAgentRow::host` has.
function where(row: AgentRow): string {
  return row.host ? `${row.host}:${row.workspace}` : row.workspace;
}

// ---------------------------------------------------------------------------
// FLEET
// ---------------------------------------------------------------------------

function CloseChat({ row, on }: { row: AgentRow; on: HomeCallbacks }) {
  const label = `Close ${row.agent.title} in ${where(row)}`;
  return (
    <Button
      size="sm"
      variant="ghost"
      className="shrink-0 text-bad"
      title={label}
      aria-label={label}
      onClick={(e) => {
        e.stopPropagation();
        on.closeChat(row);
      }}
    >
      x
    </Button>
  );
}

/// The agents that need you, copied to the top.
///
/// **Copies, not moves** — `homeTray` keeps each row's `sel`, so clicking one
/// walks the single cursor to the original rather than being a second thing you
/// can select. The section is drawn whether or not it has anything in it: "no
/// agent is waiting on you" is worth a line, and a region that appears and
/// disappears moves the list underneath it every time it does.
function Tray({
  rows,
  previewed,
  on,
}: {
  rows: readonly AgentRow[];
  previewed: number | null;
  on: HomeCallbacks;
}) {
  const tray = homeTray(rows);
  return (
    <>
      <div role="listbox" aria-label="needs you" className="h-[4lh] shrink-0 overflow-hidden">
        {!tray.length ? <Empty>nothing needs you</Empty> : null}
        {tray.slice(0, 4).map(({ row, sel: at }) => {
          // The mark, not a hard-coded `[?]`. The tray ranks three states —
          // blocked, then an unread crash, then an unread turn — and the old
          // page drew the waiting glyph for all three, which is a third
          // vocabulary for a fact the two lists below it already agree on.
          const m = agentMark(row.agent);
          return (
            <Row
              key={row.pane}
              // The tray holds *copies*, so it highlights the previewed
              // agent's copy rather than owning a cursor of its own —
              // otherwise every waiting agent is two things you can select.
              selected={at === previewed}
              onSelect={() => on.preview(row)}
              title={`${row.agent.title} — ${m.label} · ${where(row)}`}
            >
              <span className={cn("shrink-0 font-mono", MARK_TONE[m.tone])}><AgentStatus agent={row.agent} sprite /></span>
              <span className="min-w-0 flex-1 truncate">{row.agent.title}</span>
              <span className="shrink-0 truncate text-11 text-dim">{where(row)}</span>
              <CloseChat row={row} on={on} />
            </Row>
          );
        })}
      </div>
      <SectionTitle
        action={
          tray.length ? <Badge variant="destructive">{tray.length}</Badge> : <Badge variant="outline">clear</Badge>
        }
      >
        needs you
      </SectionTitle>
    </>
  );
}

/// Machine header, project row, then that project's agents — one sequence,
/// headers included, exactly as `homeRows` builds it, so the drawing and the
/// cursor cannot disagree about which row is which.
function FleetList({
  list,
  sel,
  previewed,
  folds,
  on,
}: {
  list: readonly HomeRow[];
  sel: number;
  previewed: number | null;
  folds: Folds;
  on: HomeCallbacks;
}) {
  if (!list.length) return <Empty>no machines connected</Empty>;
  return (
    <>
      {list.map((r, i) => {
        if (r.kind === HomeRowKind.Machine) {
          // The whole row folds. A machine has no workspace to open and no pane
          // to preview, so there is nothing else pressing it could mean.
          return (
            <Row
              key={`m${r.daemon}${i}`}
              compact
              selected={i === sel}
              data-home-row={i}
              onSelect={() => {
                on.walk(i);
                on.fold({ ...folds, machines: toggleFold(folds.machines, r.label ?? "") });
              }}
            >
              <span className="shrink-0 font-mono text-dim">{r.folded ? ">" : "v"}</span>
              <span className="min-w-0 flex-1 truncate">{r.label ?? ""}</span>
              <Badge variant="outline">{r.agents === 0 ? "nothing open" : r.agents}</Badge>
            </Row>
          );
        }
        if (r.kind === HomeRowKind.Space) {
          const space = r.space;
          return (
            <Row
              key={`s${space.ws}${i}`}
              compact
              selected={i === sel}
              data-home-row={i}
              // Selecting a project previews it. Folding and travelling are
              // explicit controls, so a broad row click cannot unexpectedly
              // hide its chats or throw the user into another workspace.
              onSelect={() => on.walk(i)}
              onKeyDown={(e) => {
                if (e.target === e.currentTarget && e.key === "Enter") {
                  e.preventDefault();
                  on.go(space.ws);
                }
              }}
            >
              <button
                type="button"
                className="shrink-0 pl-1 font-mono text-dim hover:text-foreground"
                title={r.folded ? `Show chats in ${space.name}` : `Hide chats in ${space.name}`}
                onClick={(e) => {
                  e.stopPropagation();
                  on.walk(i);
                  on.fold({ ...folds, spaces: toggleFold(folds.spaces, space.ws) });
                }}
              >
                └─ {r.folded ? ">" : "v"}
              </button>
              <span className="min-w-0 truncate">{space.name || String(space.ws)}</span>
              {!space.agents.length ? <span className="shrink-0 text-dim">no agents</span> : null}
              <span className="flex-1" />
              <Button
                size="sm"
                variant="ghost"
                title={`Open ${space.name}`}
                onClick={(e) => {
                  e.stopPropagation();
                  on.go(space.ws);
                }}
              >
                open
              </Button>
              <Button
                size="sm"
                variant="ghost"
                title={
                  space.preferred
                    ? `Start ${space.preferred} in ${space.name} (a)`
                    : `Start an agent in ${space.name} (a)`
                }
                onClick={(e) => {
                  e.stopPropagation();
                  on.start(space);
                }}
              >
                {space.preferred ? `+ ${space.preferred}` : "+"}
              </Button>
              {/* On the cursor's row only, which is the tab bar's rule for its
                  own `[x]` and for the same reason: this ends a workspace and
                  everything running in it, so a press that lands on it has to
                  be a press that aimed at it. */}
              {i === sel ? (
                <Button
                  size="sm"
                  variant="ghost"
                  className="text-bad"
                  title={`Close ${space.name} and kill what is running in it (x)`}
                  onClick={(e) => {
                    e.stopPropagation();
                    on.close(space);
                  }}
                >
                  x
                </Button>
              ) : null}
            </Row>
          );
        }
        const m = agentMark(r.row.agent);
        return (
          <Row
            key={r.row.pane}
            selected={i === sel || r.sel === previewed}
            data-home-row={i}
            onSelect={() => on.preview(r.row)}
            onKeyDown={(e) => {
              if (e.target === e.currentTarget && e.key === "Enter") {
                e.preventDefault();
                on.open({ ws: r.row.ws, pane: r.row.pane });
              }
            }}
            title={`${r.row.agent.title} — ${m.label} · ${where(r.row)}`}
          >
            <span className="shrink-0 pl-2 text-dim">{list[i + 1]?.kind === HomeRowKind.Agent ? "├─" : "└─"}</span>
            <span className={cn("shrink-0 font-mono", MARK_TONE[m.tone])}><AgentStatus agent={r.row.agent} sprite /></span>
            <span className="min-w-0 flex-1 truncate">{r.row.agent.title}</span>
            <Button
              size="sm"
              variant="ghost"
              title="Go to this agent's project and stage it (enter)"
              onClick={(e) => {
                e.stopPropagation();
                on.open({ ws: r.row.ws, pane: r.row.pane });
              }}
            >
              open
            </Button>
            <CloseChat row={r.row} on={on} />
          </Row>
        );
      })}
    </>
  );
}

// ---------------------------------------------------------------------------
// COMPUTE
// ---------------------------------------------------------------------------

/// One row per machine, and never one number for four of them.
///
/// The column used to draw the whole gauge stack per machine — right for WORK's
/// rail, which describes the one machine you are working on, and wrong here,
/// where the question is which of four machines is in trouble and the answer did
/// not fit on screen. So a machine is a line: what it is, how many agents it is
/// running, and `machinePressure` — the worst of its readings, named. Not the
/// CPU: a box at 30% CPU with a full root filesystem is in trouble and its CPU
/// number says it is fine.
///
/// Nothing is lost. Pressing it expands the stack, drawn by the very
/// `sysGauges` WORK's rail uses, so the two cannot come to two opinions of what
/// 41% means.
///
/// A daemon that is down is a marker here rather than an absence: "the gpu box
/// has nothing open" and "the gpu box is unreachable" are not the same sentence,
/// and this is the page where the difference is most useful.
function Machine({ m, open, onToggle }: { m: MachineRow; open: boolean; onToggle: () => void }) {
  const down = machineIsDown(m);
  const sys = m.sys;
  const gauges = down || !open ? [] : sysGauges(sys);
  const conts = sys?.containers.length ?? 0;
  const stacks = sys?.stacks.length ?? 0;
  const p = machinePressure(sys);
  if (!open) return <Row compact onSelect={onToggle} title={down ? m.error ?? "host is away" : `${p.label} ${num(p.pct)}%`}>
    <span className="text-dim">&gt;</span><span className="min-w-0 truncate">{m.label}</span>
    {!down && sys ? <Meter value={p.pct} tone={loadTone(p.pct)} className="w-auto flex-1" /> : <span className="flex-1" />}
    <span className="text-dim">{m.agents}</span>
    <span className={down ? "text-bad" : p.pct >= 85 ? "text-bad" : "text-dim"}>{down ? "away" : sys ? `${p.label} ${num(p.pct)}%` : "—"}</span>
  </Row>;

  return (
    // `py-0 gap-0`: shadcn's `Card` is 24px of padding and a 24px gap between
    // its children, which is a card on a marketing page. This is a rail block
    // in a 200px column, and `SectionTitle` brings its own 32px row and its own
    // hairline — see `HANDOVER-work-home.md`.
    <Card className="gap-0 overflow-hidden py-0">
      <SectionTitle
        action={
          <Badge variant={down ? "destructive" : "outline"}>{down ? "unreachable" : `${m.agents} agents`}</Badge>
        }
        onClick={onToggle}
      >
        <span className="mr-1 font-mono text-dim">{open ? "v" : ">"}</span>
        {m.label}
      </SectionTitle>
      <div className="py-1">
        {down ? (
          <Empty title={m.error ?? undefined}>
            <span className="min-w-0 truncate not-italic text-bad">⚠ {m.error}</span>
          </Empty>
        ) : null}
        {!down && !sys ? <Empty>no telemetry yet</Empty> : null}
        {/* The summary: one meter, and the name of whatever is worst. */}
        {!down && sys && !open ? (
          <Gauge label={p.label.toLowerCase()} value={p.pct} tone={loadTone(p.pct)} text={num(p.pct) + "%"} />
        ) : null}
        {gauges.map((g) => (
          <Gauge key={g.key} label={g.label} value={g.value} tone={g.tone} text={g.text} history={g.history} traffic={g.traffic} readingOnly={g.readingOnly} />
        ))}
        {!down && open && (conts || stacks) ? (
          <>
            <Stat compact label="containers" value={conts} />
            <Stat compact label="stacks" value={stacks} />
          </>
        ) : null}
      </div>
    </Card>
  );
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

export function HomePage({
  world,
  actions,
  on,
  sel = 0,
  folds = NO_FOLDS,
  pin = null,
  theme,
  fontPx,
  stage,
}: HomePageProps) {
  // `world.workspaces` and `world.daemons` are replaced wholesale by the
  // reducers, so identity is a sound dependency: a push that changed neither
  // does not rebuild the fleet.
  const rows = useMemo(() => allAgentRows(world.workspaces, world.daemons), [world.workspaces, world.daemons]);
  const machines = useMemo(() => machineRows(world.daemons, rows), [world.daemons, rows]);
  const spaces = useMemo(
    () => fleetSpaces(world.workspaces, world.daemons, rows, pin),
    [world.workspaces, world.daemons, rows, pin],
  );
  const list = useMemo(() => homeRows(spaces, machines, folds), [spaces, machines, folds]);

  const at = Math.min(sel, Math.max(0, list.length - 1));
  // The stage follows the cursor: the screen in the middle and the row under
  // the cursor read *one* fact. Deriving the pane separately is how a list that
  // redrew under the cursor previews the row you left.
  //
  // On a project row that is the agent in it which most needs you, so walking
  // the fleet is a fly-over of each project's screen — see `homePreview`.
  const previewed = homePreview(list, at);
  const cursor = previewed == null ? null : (rows[previewed] ?? null);
  const shown = cursor ? cursor.pane : null;
  const title = cursor ? `${cursor.agent.title} · ${where(cursor)}` : "stage";

  // The cursor belongs to the fleet model, not to whichever DOM node happened
  // to receive the last click. Follow it after keyboard/tray moves so an old
  // focus ring cannot remain on one row while the selection band moves to
  // another. Skip the initial render: BOOTH must not steal focus on entry.
  const fleet = useRef<HTMLDivElement | null>(null);
  const previous = useRef(at);
  useEffect(() => {
    if (previous.current === at) return;
    previous.current = at;
    const el = fleet.current?.querySelector<HTMLElement>(`[data-home-row="${at}"]`);
    if (el && el !== document.activeElement) el.focus();
  }, [at]);

  return (
    <div className="flex h-full min-h-0 min-w-0 flex-col">
      <div
        className={cn(
          "grid min-h-0 flex-1",
          "[grid-template-columns:1fr] [grid-template-rows:minmax(10rem,40%)_minmax(0,1fr)]",
          "md:[grid-template-rows:1fr] md:[grid-template-columns:clamp(22ch,25%,40ch)_1fr_clamp(20ch,25%,36ch)]",
        )}
      >
        <section className="flex min-h-0 min-w-0 flex-col border-b border-border bg-card md:border-b-0 md:border-r">
          <SectionTitle>fleet ({rows.length})</SectionTitle>
          <Tray rows={rows} previewed={previewed} on={on} />
          <ScrollArea className={cn("min-h-0 flex-1", SCROLLER)}>
            <div className="pb-1" ref={fleet} role="listbox" aria-label="fleet">
              <FleetList list={list} sel={at} previewed={previewed} folds={folds} on={on} />
            </div>
          </ScrollArea>
        </section>

        <section className="flex min-h-0 min-w-0 flex-col shadow-[inset_0_0_0_1px_var(--color-border)]">
          <SectionTitle>{title}</SectionTitle>
          <Stage
            autoFocus={false}
            pane={shown}
            theme={theme}
            className="min-h-0 flex-1"
            {...(fontPx != null ? { fontPx } : {})}
            {...(stage ?? {})}
          />
        </section>

        <section className="hidden min-h-0 min-w-0 flex-col border-l border-border bg-card md:flex">
          <SectionTitle>compute</SectionTitle>
          <ScrollArea className={cn("min-h-0 flex-1", SCROLLER)}>
            <div className="flex flex-col gap-2 p-2">
              {!machines.length ? <Empty>no machines</Empty> : null}
              {machines.map((m) => (
                <Machine
                  key={m.daemon}
                  m={m}
                  open={folds.expanded.has(m.label)}
                  onToggle={() => on.fold({ ...folds, expanded: toggleFold(folds.expanded, m.label) })}
                />
              ))}
            </div>
          </ScrollArea>
        </section>
      </div>
      <HintBar keys={hints(homeVerbs(), RAIL_COLS, 1, (k) => actions.press("home", k))} />
    </div>
  );
}
