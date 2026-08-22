// The USAGE page: which agent account stops you first, and when it comes back.
//
// **No browser client has ever had this page.** The terminal has had it since
// 0.9; this is the parity gap being closed, and the specification is the module
// note at the top of `crates/butai-client/src/chrome/usage.rs`. Its four rules
// are design decisions, not implementation detail, so they are restated here
// rather than left to be rediscovered from the layout:
//
// **1 — Limits, not spend.** Cost never appears. The question is whether the
// account you are about to start a long job on has room, which is a different
// question from what the month has cost and is answered by different numbers.
//
// **2 — Every CLI on screen at once.** The obvious build is a list on the left
// and a detail on the right, and it is wrong here: the question is *which*
// account is closest to stopping you, and a list-and-detail answers it with
// every other account hidden behind a cursor. So there is no cursor and no
// navigation on this page — every CLI is a `Card`, stacked, about twenty rows
// in all.
//
// **3 — A window with no ceiling draws a total, not a bar.** A bar needs a
// denominator and any denominator this page invented would be read as the
// provider's. `of: null` means draw the total instead. The bar column is still
// *reserved* when a CLI has some windows with ceilings and some without —
// blank, not an empty track — so the labels and the numbers stay in columns.
//
// **4 — Colour is a level, not a verdict.** `ok` — green — is deliberately
// unused: 12% of a quota is not a success, and painting it green spends the
// loudest signal in the palette on the row nobody needs to look at. Amber means
// it bites this session; red means it is about to stop you. The same three
// levels carry the bar, the readout and the header badge.
//
// The right-hand column answers "when does it come back": the reset instant
// wherever the window has a real boundary, the total where it is rolling. A
// limit is two numbers — how full, and how long until it empties — and the
// second is the one that decides whether to start a long job now or after
// lunch.
//
// ## Two things this page owns that it would rather not
//
// **The formatters below belong in `logic/usage.ts`.** They are a
// transliteration of `usage.rs`'s `compact` / `value_text` / `until` /
// `tail_text` / `pressure` / `age` / `badge`, which are pure and already have
// tests on the Rust side. They live here only because this pass may not write
// into `src/logic/`; they are exported so the move is a cut and a paste and so
// a test can reach them. See `HANDOVER-usage-help.md`.
//
// **The clock is the page's.** Every countdown on screen must be relative to
// one instant or two rows disagree about what time it is, and something has to
// tick or the countdown is wrong the moment it is drawn. That is view state,
// not data, so it is here — and `now` can be pinned by a caller, which is what
// a test or a screenshot needs.

import * as React from "react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Empty } from "@/components/Empty";
import { HintBar } from "@/components/HintBar";
import { Meter, type MeterTone } from "@/components/Meter";
import { Row } from "@/components/Row";
import { SectionTitle } from "@/components/SectionTitle";
import { Separator } from "@/components/ui/separator";
import { cn } from "@/lib/utils";
import type { CliUsageDto, UsageDto, UsageWindowDto } from "@/protocol/generated/protocol.ts";

// ---------------------------------------------------------------------------
// The wire, read honestly
// ---------------------------------------------------------------------------

// `UsageWindowDto.of` and `.resets_ms` are `Option<u64>` in the Rust and arrive
// as `null`, but the generated binding types both as a bare `number` — the
// `#[ts(type = "number")]` that spells `u64` out for ts-rs also throws the
// `Option` away. So the compiler believes a ceiling is always there, and rule 3
// is exactly the rule that turns on it not being.
//
// These two readers are the whole workaround: the assertion widens the declared
// type back to what the daemon actually sends, in one place, with the reason
// written down.
//
// **The cast is not what makes the check compile** — TypeScript permits
// `x === null` on a `number` precisely because a value off a wire can lie, so a
// naive page compiles either way. What it buys is that the narrowing below is
// *typed* as narrowing rather than as a tautology the next reader deletes. The
// failure it prevents was measured, not imagined: rendering the bar
// unconditionally, which is what a page written from the declared type would
// do, draws a **100%-full bar in `--bad`** on a window that has no ceiling at
// all — the loudest thing in the palette reporting a limit nobody stated.
//
// **Do not spread this cast into the components**, and do not "fix" it by
// redeclaring the DTO — that would put a second and disagreeing copy of the
// wire in the client. The generator is what needs the fix; see
// `HANDOVER-usage-help.md`.

/**
 * The window's ceiling, or null when nothing published one.
 *
 * Zero is read as absent as well as null. `of: 0` is a denominator that came
 * from nowhere, and the alternatives are worse in both directions: a bar
 * against zero is a full bar or a NaN, and `4.4M / 0 tokens` reports a limit
 * nobody stated — which is the exact failure the DTO's own comment warns about.
 */
export function ceiling(w: UsageWindowDto): number | null {
  const of = w.of as number | null | undefined;
  return typeof of === "number" && Number.isFinite(of) && of > 0 ? of : null;
}

/** When this window empties, or null for a rolling one with no boundary. */
export function resetAt(w: UsageWindowDto): number | null {
  const at = w.resets_ms as number | null | undefined;
  return typeof at === "number" && Number.isFinite(at) && at > 0 ? at : null;
}

// ---------------------------------------------------------------------------
// The formatters — `usage.rs`, transliterated
// ---------------------------------------------------------------------------

/**
 * How tightly a window is drawn: a level, never a verdict.
 *
 * `usage.rs`'s `Role::Info` / `Attention` / `Danger`, under names that do not
 * collide with the kit's tones. See rule 4 above for why there is no fourth
 * level for "plenty left".
 */
export type Level = "info" | "attention" | "danger";

/** `usage.rs::pressure`. */
export function pressure(used: number, of: number): Level {
  if (!(of > 0)) return "info";
  const pct = Math.floor((Math.max(0, used) * 100) / of);
  if (pct >= 90) return "danger";
  if (pct >= 75) return "attention";
  return "info";
}

/** How full, as the whole number the bar and the badge both show. */
export function percentOf(used: number, of: number): number {
  if (!(of > 0)) return 0;
  return Math.min(999, Math.floor((Math.max(0, used) * 100) / of));
}

/**
 * `4394242` -> `4.4M`. `usage.rs::compact`.
 *
 * Token counts are read as magnitudes, and nine digits of precision on a number
 * whose denominator is unknown is false authority.
 */
export function compact(n: number): string {
  const v = Number.isFinite(n) ? Math.max(0, Math.floor(n)) : 0;
  if (v >= 100_000_000) return `${Math.floor(v / 1_000_000)}M`;
  if (v >= 1_000_000) return `${(v / 1_000_000).toFixed(1)}M`;
  if (v >= 100_000) return `${Math.floor(v / 1_000)}k`;
  if (v >= 1_000) return `${(v / 1_000).toFixed(1)}k`;
  return String(v);
}

/**
 * The value column for one window — `4.4M tokens`, or `4.4M / 20.0M tokens`
 * once a ceiling gives it a denominator. `usage.rs::value_text`.
 *
 * A percentage is its own denominator: `56%`, never `56 / 100 %`.
 */
export function valueText(w: UsageWindowDto): string {
  if (w.unit === "percent") return `${Math.max(0, Math.floor(w.used))}%`;
  const unit = w.unit === "requests" ? "requests" : "tokens";
  const of = ceiling(w);
  return of === null ? `${compact(w.used)} ${unit}` : `${compact(w.used)} / ${compact(of)} ${unit}`;
}

/**
 * How long until a window empties — `4d 6h`, `3h 12m`, `8m`. `usage.rs::until`.
 *
 * Two units at most: the hour matters when the reset is today and stops
 * mattering when it is Tuesday, and a countdown to the second on a five-hour
 * window is a number that changes while you read it.
 */
export function until(resetMs: number, nowMs: number): string {
  const secs = Math.max(0, Math.floor((resetMs - nowMs) / 1000));
  if (secs < 60) return "now";
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  if (secs < 86_400) return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
  return `${Math.floor(secs / 86_400)}d ${Math.floor((secs % 86_400) / 3600)}h`;
}

/**
 * The right-hand column for one window: when it comes back, or what it holds.
 * `usage.rs::tail_text`.
 *
 * The reset wins the slot wherever there is one, because the bar has already
 * said how full the window is. A percentage with no boundary has said
 * everything it has to say on the bar; repeating it out here would be
 * furniture.
 */
export function tailText(w: UsageWindowDto, nowMs: number): string {
  const at = resetAt(w);
  if (at !== null) return `resets in ${until(at, nowMs)}`;
  if (w.unit === "percent") return "";
  return valueText(w);
}

/**
 * How long ago the daemon sampled. `usage.rs::age`.
 *
 * A stale limit is worse than no limit, so the age is always drawn.
 */
export function age(sampledMs: number, nowMs: number): string {
  if (!sampledMs) return "never";
  const secs = Math.max(0, Math.floor((nowMs - sampledMs) / 1000));
  if (secs < 60) return `${secs}s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  return `${Math.floor(secs / 3600)}h ago`;
}

/**
 * The one number worth carrying onto the pages that are *not* this one.
 * `usage.rs::badge`.
 *
 * Only a window with a ceiling produces one — published by the provider or
 * declared by the user. Without one there is no proportion, and `39.0M` on a
 * rail nobody can act on. So a machine whose CLIs publish nothing and which has
 * declared nothing shows no badge, which is correct: there is no threshold to
 * have crossed.
 *
 * Exported because the rail badge is the *same* number: a second implementation
 * over there is two answers to "how close am I".
 */
export function worst(dto: UsageDto | null | undefined): { text: string; level: Level } | null {
  let best: { pct: number; used: number; of: number } | null = null;
  for (const cli of dto?.clis ?? []) {
    for (const w of cli.windows) {
      const of = ceiling(w);
      if (of === null) continue;
      const pct = Math.floor((Math.max(0, w.used) * 100) / of);
      if (!best || pct > best.pct) best = { pct, used: w.used, of };
    }
  }
  if (!best) return null;
  return { text: `${Math.min(999, best.pct)}%`, level: pressure(best.used, best.of) };
}

/** Panes burning this account right now, as the head line says it. */
function agentsText(cli: CliUsageDto): string {
  const n = cli.panes.length;
  if (n === 0) return "";
  return n === 1 ? "1 agent" : `${n} agents`;
}

/** Whether this CLI has numbers at all. The other three states carry a note instead. */
function isMetered(cli: CliUsageDto): boolean {
  return cli.state === "metered" || cli.state === "counted";
}

/**
 * The head line's middle: the account behind the name, or — for the states with
 * nothing to meter — why there is nothing.
 *
 * A row that says what is not there and why is what stops someone hunting for a
 * number that does not exist.
 */
function headDetail(cli: CliUsageDto): string {
  if (!isMetered(cli)) return cli.note ?? "";
  return [cli.plan, cli.account, cli.version].filter((s): s is string => !!s).join(" · ");
}

// ---------------------------------------------------------------------------
// Level to paint
// ---------------------------------------------------------------------------

// Three levels, three tones, and no green anywhere in either table. `info` is
// the brand blue rather than a neutral grey because `Meter`'s own note calls
// `accent` "a neutral proportion" — it is the tone that says *this is a
// measurement* without saying anything about how the measurement went.
const METER_TONE: Record<Level, MeterTone> = { info: "accent", attention: "warn", danger: "bad" };
const READOUT_TONE: Record<Level, string> = {
  info: "text-foreground",
  attention: "text-warn",
  danger: "text-bad",
};
const BADGE_TONE: Record<Level, string> = {
  info: "border-border text-dim",
  attention: "border-warn/40 bg-warn/10 text-warn",
  danger: "border-bad/40 bg-bad/10 text-bad",
};

// The label column, and the bar column beside it. Both fixed, because the whole
// point of a stack of accounts is that it reads *down*: a label column sized to
// its own row would put every CLI's bars in a different place. The longest
// label the daemon writes is `week · all models`.
const LABEL_W = "w-36";
const BAR_W = "w-40";
const PCT_W = "w-12";

// How wide the content grows, however wide the window is. `usage.rs` caps at 78
// columns for the reason `Prose` caps at 78 characters: the numbers are
// right-aligned against *this*, not against the panel, so a wide screen does
// not strand `39.1M tokens` a foot from the label it belongs to.
const CONTENT_W = "max-w-3xl";

// How often the countdowns are re-read. `until` is written in minutes, so this
// is the coarsest tick that can never show a stale minute.
const TICK_MS = 30_000;

/**
 * One clock reading for the whole frame.
 *
 * Pinned by the caller or ticking on its own; either way every countdown in one
 * render is relative to the same instant, which is what stops two rows on the
 * same screen disagreeing about what time it is.
 */
function useClock(pinned: number | undefined): number {
  const [now, setNow] = React.useState<number>(() => pinned ?? Date.now());
  React.useEffect(() => {
    if (pinned !== undefined) return;
    const t = setInterval(() => setNow(Date.now()), TICK_MS);
    return () => clearInterval(t);
  }, [pinned]);
  return pinned ?? now;
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

export interface UsagePageProps {
  /**
   * `GET /api/usage` for the daemon whose workspace is open — an account limit
   * is machine-scoped, not workspace-scoped, and the terminal reads it off the
   * *active* daemon (`workbench.rs`'s `refresh_usage`). Null until it answers.
   */
  usage: UsageDto | null;
  /**
   * False until the first reply. It is the only thing that tells "still asking"
   * apart from "nothing configured", and those two want opposite reactions.
   */
  loaded?: boolean | undefined;
  /** Which machine these limits are on, when this bridge speaks for more than one. */
  machine?: string | null | undefined;
  /** Pin the clock, for a test or a screenshot. Absent means the page keeps its own. */
  now?: number | undefined;
  /**
   * Re-read the limits. Absent leaves `r refresh` in the footer as
   * documentation rather than as a button that does nothing — which is
   * `HintBar`'s contract, and the reason it has two entry shapes.
   */
  onRefresh?: (() => void) | undefined;
}

export function UsagePage({ usage, loaded, machine, now: pinned, onRefresh }: UsagePageProps) {
  const now = useClock(pinned);
  const clis = usage?.clis ?? [];
  const sampled = age(usage?.sampled_ms ?? 0, now);
  const headline = worst(usage);

  return (
    <div data-page="usage" className="flex h-full min-h-0 flex-col bg-background">
      <SectionTitle
        action={
          <>
            {headline ? (
              <Badge
                variant="outline"
                className={cn("font-mono tabular-nums", BADGE_TONE[headline.level])}
                title="the fullest window with a ceiling, across every account"
              >
                {headline.text}
              </Badge>
            ) : null}
            <span className="text-11 text-faint">sampled {sampled}</span>
            {onRefresh ? (
              <Button variant="ghost" size="sm" onClick={onRefresh}>
                refresh
              </Button>
            ) : null}
          </>
        }
      >
        usage{machine ? ` · ${machine}` : ""}
      </SectionTitle>

      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className={cn("flex w-full flex-col gap-3 p-3", CONTENT_W)}>
          {clis.length === 0 ? (
            // The terminal's own two strings, because they are the same two
            // facts and a client that reworded them would teach a second name
            // for one state.
            <Empty>{loaded ? "no agent CLIs configured" : "reading account standing…"}</Empty>
          ) : (
            clis.map((cli) => <CliBlock key={cli.name} cli={cli} now={now} />)
          )}
        </div>
      </div>

      <HintBar keys={[{ key: "r", label: "refresh", onSelect: onRefresh }]} />
    </div>
  );
}

/**
 * One account, whole.
 *
 * A `Card` rather than a run of rows with a divider: the unit of this page is
 * the account, and a block with an edge round it is what makes "every CLI at
 * once" read as five answers rather than as one long list. `py-0 gap-0` because
 * `Card`'s own padding is for prose and this is rows.
 */
function CliBlock({ cli, now }: { cli: CliUsageDto; now: number }) {
  const detail = headDetail(cli);
  const agents = isMetered(cli) ? agentsText(cli) : "";

  return (
    <Card className="gap-0 overflow-hidden py-0">
      <Row className="gap-3">
        {/* Mono: the name is the `[[agents]]` entry you type after
            `butai agent spawn`, not a label. */}
        <span className="shrink-0 font-mono text-13 font-semibold">{cli.name}</span>
        {detail ? <span className="min-w-0 flex-1 truncate text-12 text-faint">{detail}</span> : <span className="flex-1" />}
        {agents ? <span className="shrink-0 text-12 text-faint">{agents}</span> : null}
      </Row>

      {cli.windows.length > 0 ? (
        <>
          <Separator />
          <div className="py-1">
            {cli.windows.map((w, i) => (
              <WindowRow key={`${w.label}/${i}`} w={w} now={now} />
            ))}
            {/* The provenance line, under the windows it explains and indented
                to the bar column so it reads as belonging to them rather than
                as another window. */}
            {cli.note ? (
              <Row compact className="gap-3">
                <span aria-hidden="true" className={cn("shrink-0", LABEL_W)} />
                <span className="min-w-0 truncate text-faint">{cli.note}</span>
              </Row>
            ) : null}
          </div>
        </>
      ) : null}
    </Card>
  );
}

/**
 * One window: how full on the left, when it comes back on the right.
 *
 * The bar and the percentage exist only where a ceiling actually came from
 * somewhere — see rule 3. Their columns stay reserved when it did not, so a CLI
 * whose windows disagree about having a ceiling still reads as a table.
 */
function WindowRow({ w, now }: { w: UsageWindowDto; now: number }) {
  const of = ceiling(w);
  const level = of === null ? "info" : pressure(w.used, of);
  const tail = tailText(w, now);
  // The reset is a fact about the clock, not a pressure level — a window that
  // is 95% full is still red on the bar, but *when* it comes back is not an
  // alarm and is drawn faint so the bar keeps the eye. Without a boundary the
  // tail is the number itself, and takes the number's tone.
  const tailTone = resetAt(w) !== null ? "text-faint" : READOUT_TONE[level];

  return (
    <Row compact className="gap-3">
      <span className={cn("shrink-0 truncate text-dim", LABEL_W)}>{w.label}</span>
      <div className={cn("shrink-0", BAR_W)}>
        {of === null ? null : (
          <Meter
            value={w.used}
            max={of}
            tone={METER_TONE[level]}
            aria-label={w.label}
            aria-valuetext={valueText(w)}
          />
        )}
      </div>
      <span
        data-numeric
        className={cn("shrink-0 text-right font-mono", PCT_W, of === null ? "" : READOUT_TONE[level])}
      >
        {of === null ? "" : `${percentOf(w.used, of)}%`}
      </span>
      {/* The full reading, for the pointer. The tail spends its column on the
          reset wherever there is one, which is the right trade on a terminal
          row and a free one here — a title costs no columns at all. */}
      <span data-numeric className={cn("ml-auto shrink-0 truncate font-mono", tailTone)} title={valueText(w)}>
        {tail}
      </span>
    </Row>
  );
}

export default UsagePage;
