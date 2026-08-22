// HOME — the one page that spans daemons.
//
// The port of `web/ui/home.js`, which is itself the port of `Page::Home` and
// `draw_home_page`. Three columns, and the middle one is a **real pane**:
//
//   FLEET                 STAGE                     COMPUTE
//   every agent on        the selected agent's      what each machine is
//   every machine         live screen               doing to itself
//
// The row model is `logic/fleet.ts` — `allAgentRows`, `machineRows`, `homeRows`,
// `homeTray` — imported, not reimplemented. It is pure, `test/fleet.test.ts`
// runs the lot against hand-written multi-daemon state whose ids collide on
// purpose, and it is the only reason a page that merges four machines can be
// trusted to keep them apart.
//
// ## The three things the audit found here, and where each went
//
// **The hint bar spanned the left column.** `enter open` sat under the fleet
// list, which says the key belongs to the list; it belongs to the page.
// `HintBar` is full-width and there is one, at the bottom, under all three
// columns.
//
// **The column had two header styles in it** — FLEET's own, and a full-width
// `CLEAR` band that was a header in everything but name. Both are `SectionTitle`
// now, and the band's two states are what its right-hand `action` says: a red
// count when something is waiting, `clear` when nothing is.
//
// **The compute meters ended at 1112px while their values right-aligned at
// 1268px.** `Gauge` puts the bar and the number in one gutter, so the two line
// up by construction rather than by both being roughly right.
//
// ## Projects are rows, not a second kind of header
//
// `homeRows` emits machine, project and agent rows in one sequence. The machine
// is a `SectionTitle`; the project is a `Row` carrying a dim label, because a
// second *header* style — indented, smaller, its own colour — is exactly the
// drift this rewrite exists to remove, and a subordinate row says "these belong
// to the line above" just as well. The agents under it are `Row`s you can
// select; the project above them is one you cannot.
//
// ## The pane id is the whole safety property
//
// A fleet row's `pane` is `<daemon>:<n>`, so pointing the stage at it dials
// *that machine's* socket and attaches that machine's pane. Every daemon has a
// pane 5; on this page two of them are one row apart. That is why the stage's
// title carries the machine as well as the agent, and why nothing here ever
// hands a bare integer to anything.

import { useMemo } from "react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Empty } from "@/components/Empty";
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
  allAgentRows,
  homeRows,
  homeTray,
  machineIsDown,
  machineRows,
  type AgentRow,
  type MachineRow,
} from "@/logic/fleet.ts";
import type { TermTheme } from "@/logic/palette.ts";
import { homeVerbs } from "@/logic/verbs.ts";

import { MARK_BADGE, MARK_TONE, SCROLLER, agentMark, hints, sysGauges } from "./parts.ts";

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

/// View-state changes. Nothing here reaches a daemon.
export interface HomeCallbacks {
  /// Walk the cursor to this index **among agents** — what `j`/`k` count.
  walk(sel: number): void;
  /// `enter`: go to that agent's project, on its own machine, and stage it.
  /// Both ids are qualified, and both halves are needed — the workspace to
  /// switch to and the pane to put on the stage may be on a machine that is not
  /// the active tab's.
  open(where: { ws: Qid; pane: Qid }): void;
}

export interface HomePageProps {
  /// Every daemon and every workspace. The fleet is derived from it here rather
  /// than passed in: `allAgentRows` and `machineRows` are pure, so the page and
  /// the shell reading the same world cannot disagree about what is in the
  /// list — and the cursor below counts rows of exactly that list.
  world: World;
  actions: HomeActions;
  on: HomeCallbacks;
  /// The cursor's index among agents. A header is not a row you can select, and
  /// clicking a machine's name is not a request to open somebody's agent.
  sel?: number | undefined;
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

/// The agents that need you, copied to the top.
///
/// **Copies, not moves** — `homeTray` keeps each row's `sel`, so clicking one
/// walks the single cursor to the original rather than being a second thing you
/// can select. The section is drawn whether or not it has anything in it: "no
/// agent is waiting on you" is worth a line, and a region that appears and
/// disappears moves the list underneath it every time it does.
function Tray({ rows, sel, on }: { rows: readonly AgentRow[]; sel: number; on: HomeCallbacks }) {
  const tray = homeTray(rows);
  return (
    <>
      <SectionTitle
        action={
          tray.length ? <Badge variant="destructive">{tray.length}</Badge> : <Badge variant="outline">clear</Badge>
        }
      >
        needs you
      </SectionTitle>
      {!tray.length ? <Empty>nothing waiting</Empty> : null}
      <div role="listbox" aria-label="needs you">
        {tray.map(({ row, sel: at }) => {
          // The mark, not a hard-coded `[?]`. The tray ranks three states —
          // blocked, then an unread crash, then an unread turn — and the old
          // page drew the waiting glyph for all three, which is a third
          // vocabulary for a fact the two lists below it already agree on.
          const m = agentMark(row.agent);
          return (
            <Row
              key={row.pane}
              selected={at === sel}
              onSelect={() => on.walk(at)}
              title={`${row.agent.title} — ${m.label} · ${where(row)}`}
            >
              <span className={cn("shrink-0 font-mono", MARK_TONE[m.tone])}>{m.glyph}</span>
              <span className="min-w-0 flex-1 truncate">{row.agent.title}</span>
              <span className="shrink-0 truncate text-11 text-dim">{where(row)}</span>
            </Row>
          );
        })}
      </div>
    </>
  );
}

/// Machine header, project row, then that project's agents — one sequence,
/// headers included, exactly as `homeRows` builds it, so the drawing and the
/// cursor cannot disagree about which row is which.
function FleetList({
  rows,
  machines,
  sel,
  on,
}: {
  rows: readonly AgentRow[];
  machines: readonly MachineRow[];
  sel: number;
  on: HomeCallbacks;
}) {
  const list = homeRows(rows, machines);
  if (!list.length) return <Empty>no agents on any machine</Empty>;
  return (
    <>
      {list.map((r, i) => {
        if (r.kind === HomeRowKind.Machine) {
          return (
            <SectionTitle key={`m${r.daemon}${i}`} action={<Badge variant="outline">{r.agents}</Badge>}>
              {r.label ?? ""}
            </SectionTitle>
          );
        }
        if (r.kind === HomeRowKind.Space) {
          return (
            <Row key={`s${r.ws}${i}`} compact>
              <span className="min-w-0 truncate pl-3 text-11 uppercase tracking-caps text-dim">
                {r.name || String(r.ws)}
              </span>
            </Row>
          );
        }
        const m = agentMark(r.row.agent);
        const badge = MARK_BADGE[m.tone];
        return (
          <Row
            key={r.row.pane}
            selected={r.sel === sel}
            onSelect={() => on.walk(r.sel)}
            title={`${r.row.agent.title} — ${m.label} · ${where(r.row)}`}
          >
            <span className={cn("shrink-0 font-mono", MARK_TONE[m.tone])}>{m.glyph}</span>
            <span className="min-w-0 flex-1 truncate">{r.row.agent.title}</span>
            <Badge variant={badge.variant} className={badge.className}>
              {m.short}
            </Badge>
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
          </Row>
        );
      })}
    </>
  );
}

// ---------------------------------------------------------------------------
// COMPUTE
// ---------------------------------------------------------------------------

/// One block per machine, and never one number for four of them.
///
/// A daemon that is down is a marker here rather than an absence: "the gpu box
/// has nothing open" and "the gpu box is unreachable" are not the same sentence,
/// and this is the page where the difference is most useful.
function Machine({ m }: { m: MachineRow }) {
  const down = machineIsDown(m);
  const sys = m.sys;
  const gauges = down ? [] : sysGauges(sys);
  const conts = sys?.containers.length ?? 0;
  const stacks = sys?.stacks.length ?? 0;
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
      >
        {m.label}
      </SectionTitle>
      <div className="py-1">
        {down ? (
          <Empty title={m.error ?? undefined}>
            <span className="min-w-0 truncate not-italic text-bad">⚠ {m.error}</span>
          </Empty>
        ) : null}
        {!down && !sys ? <Empty>no telemetry yet</Empty> : null}
        {gauges.map((g) => (
          <Gauge key={g.key} label={g.label} value={g.value} tone={g.tone} text={g.text} />
        ))}
        {!down && (conts || stacks) ? (
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

export function HomePage({ world, actions, on, sel = 0, pane, theme, fontPx, stage }: HomePageProps) {
  // `world.workspaces` and `world.daemons` are replaced wholesale by the
  // reducers, so identity is a sound dependency: a push that changed neither
  // does not rebuild the fleet.
  const rows = useMemo(() => allAgentRows(world.workspaces, world.daemons), [world.workspaces, world.daemons]);
  const machines = useMemo(() => machineRows(world.daemons, rows), [world.daemons, rows]);

  const cursor = rows[Math.min(sel, Math.max(0, rows.length - 1))] ?? null;
  // The stage follows the cursor: the screen in the middle and the row under
  // the cursor read *one* fact. Deriving the pane separately is how a list that
  // redrew under the cursor previews the row you left.
  const shown = pane ?? (cursor ? cursor.pane : null);
  const title = cursor ? `${cursor.agent.title} · ${where(cursor)}` : "stage";

  return (
    <div className="flex h-full min-h-0 min-w-0 flex-col">
      <div
        className={cn(
          "grid min-h-0 flex-1",
          "[grid-template-columns:1fr]",
          "md:[grid-template-columns:minmax(220px,26%)_1fr]",
          "xl:[grid-template-columns:minmax(220px,26%)_1fr_minmax(200px,20%)]",
        )}
      >
        <section className="hidden min-h-0 min-w-0 flex-col border-r border-border bg-card md:flex">
          <SectionTitle action={<Badge variant="outline">{rows.length}</Badge>}>fleet</SectionTitle>
          <Tray rows={rows} sel={sel} on={on} />
          <ScrollArea className={cn("min-h-0 flex-1 border-t border-border", SCROLLER)}>
            <div className="pb-1">
              <FleetList rows={rows} machines={machines} sel={sel} on={on} />
            </div>
          </ScrollArea>
        </section>

        <section className="flex min-h-0 min-w-0 flex-col">
          <SectionTitle>{title}</SectionTitle>
          <Stage
            pane={shown}
            theme={theme}
            className="min-h-0 flex-1"
            {...(fontPx != null ? { fontPx } : {})}
            {...(stage ?? {})}
          />
        </section>

        <section className="hidden min-h-0 min-w-0 flex-col border-l border-border bg-card xl:flex">
          <SectionTitle>compute</SectionTitle>
          <ScrollArea className={cn("min-h-0 flex-1", SCROLLER)}>
            <div className="flex flex-col gap-2 p-2">
              {!machines.length ? <Empty>no machines</Empty> : null}
              {machines.map((m) => (
                <Machine key={m.daemon} m={m} />
              ))}
            </div>
          </ScrollArea>
        </section>
      </div>
      <HintBar keys={hints(homeVerbs(), RAIL_COLS, 1, (k) => actions.press("home", k))} />
    </div>
  );
}
