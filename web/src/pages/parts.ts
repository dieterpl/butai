// What WORK and HOME share, and the kit deliberately does not carry.
//
// The port of `web/ui/parts.js`. The test each entry had to pass is "would a
// page that is not about agents want this?" — everything that passes it belongs
// in `src/components/` instead, and everything that does not would otherwise be
// copied into the second page that needs it. Three things pass:
//
//   * the agent and process **status marks**, mapped from the logic layer's
//     classes onto the kit's colour roles. WORK's AGENTS rail and HOME's fleet
//     draw the same glyph for the same state, and they must, or the two lists
//     are two vocabularies for one fact.
//   * `hints` — `verbs.ts`'s tables packed into `HintBar` entries.
//   * `sysGauges` — one `SysDto` as the rows a column of `Gauge`s draws. WORK's
//     SYSTEM rail and HOME's COMPUTE column each spelled this out in the old
//     client, which is two answers to "what does a machine's telemetry look
//     like" and they had already drifted apart by a `°`.
//
// The glyphs come from `dom.ts` and the verbs from `verbs.ts`; neither is
// reimplemented here. `parts.js`'s fourth export, `Note`, is gone: the kit's
// `Empty` is that component now, and better — it is never interactive, which is
// the one thing the hand-rolled note could not promise.
//
// No JSX below, deliberately. Everything here is a table or a pure function, so
// it is `.ts` rather than `.tsx` and a page cannot import a *shape* from it by
// accident — shapes live in `src/components/`.

import type { VariantProps } from "class-variance-authority";

import type { badgeVariants } from "@/components/ui/badge";
import type { Hint } from "@/components/HintBar";
import type { MeterTone } from "@/components/Meter";
import { AGENT_MARK, fmtGb, loadClass, procClass } from "@/logic/dom.ts";
import { fits, keyText, type Verb } from "@/logic/verbs.ts";
import type { QualifiedAgent } from "@/logic/events.ts";
import type { AgentDto, AgentState, DiskDto, SysDto } from "@/protocol/generated/protocol.ts";

// ---------------------------------------------------------------------------
// Status marks
// ---------------------------------------------------------------------------

/// The class `AGENT_MARK` gives a state — `needs`, `work`, `done`, `idle`,
/// `dead`. Read off the table rather than written out, so a sixth state added
/// to the daemon's `AgentState` cannot be missing a tone here without the
/// compiler saying so.
export type MarkTone = (typeof AGENT_MARK)[AgentState][1];

/// How a status word is drawn as a [`Badge`].
///
/// Two fields rather than one because **shadcn's `Badge` has no `ok` or `warn`
/// variant** — it ships `default`, `secondary`, `destructive`, `outline`,
/// `ghost` and `link`, and these two rails need the palette's four state
/// colours: an agent that finished is not the same news as one that crashed,
/// and a process reporting `FAIL(1)` is not the same news as one reporting
/// `ok`. The tint is `Notice`'s own recipe — `border-x/40 bg-x/10 text-x`
/// against the same tokens — so no colour is named here that the palette does
/// not own, and the two pages read one table rather than each tinting its own.
///
/// It belongs in `Badge` as a `tone` variant. See `HANDOVER-work-home.md`.
export interface BadgeLook {
  variant: VariantProps<typeof badgeVariants>["variant"];
  className?: string | undefined;
}

/// The tone of an agent's mark, by the class `AGENT_MARK` gives its state.
///
/// `working` is the brand colour rather than the warning one. The terminal's
/// rail draws it amber and HOME draws it blue — the same state, two answers,
/// which is the drift in miniature — and blue is the right one: an agent that
/// is working is the thing you *want* to be happening, and amber is what the
/// page spends on a machine at 70% memory.
export const MARK_TONE: Readonly<Record<MarkTone, string>> = Object.freeze({
  needs: "text-bad",
  work: "text-primary",
  done: "text-ok",
  idle: "text-dim",
  dead: "text-dim",
});

/// The badge beside it. `idle` and `dead` are bare outlines: a list where every
/// row carries a filled pill is a list with no emphasis left to spend on the
/// one row that has something to say.
export const MARK_BADGE: Readonly<Record<MarkTone, BadgeLook>> = Object.freeze({
  needs: { variant: "destructive" },
  work: { variant: "default" },
  done: { variant: "outline", className: "border-ok/40 bg-ok/10 text-ok" },
  idle: { variant: "outline" },
  dead: { variant: "outline" },
});

/// The badge's word, where `AGENT_MARK`'s is a sentence.
///
/// "done — your turn" is sixteen characters in a rail that is 240 wide, and it
/// spends them saying what the `[v]` beside it already said — so the row it
/// belongs to has nothing left to print the agent's name in, which is the only
/// part that is not already on screen twice. The long form is not lost: it is
/// the row's `title`, where a sentence is free.
const MARK_LABEL: Readonly<Record<MarkTone, string>> = Object.freeze({
  needs: "needs you",
  work: "working",
  done: "done",
  idle: "idle",
  dead: "exited",
});

/// Glyph, tone, word and sentence for an agent, exited or otherwise.
export interface Mark {
  glyph: string;
  tone: MarkTone;
  /// The badge's word.
  short: string;
  /// The row's `title` — the sentence the badge had no room for.
  label: string;
}

/// Glyph, tone, word and sentence for an agent, exited or otherwise.
///
/// The `exited` arm is the caller's job in the vanilla client and is done twice
/// there, once per page. It is one thing: an agent with an `exited` code is dead
/// whatever its last known state said, and the code is worth printing.
export function agentMark(agent: QualifiedAgent | AgentDto | null | undefined): Mark {
  if (agent && agent.exited != null) {
    const said = "exited " + agent.exited;
    return { glyph: "[x]", tone: "dead", short: said, label: said };
  }
  // The `??` is unreachable through the types and deliberate anyway: the state
  // arrives off a wire, and a daemon that grows a sixth one must draw a row
  // rather than a blank.
  const [glyph, tone, label] = (agent && AGENT_MARK[agent.state]) ?? AGENT_MARK.idle;
  return { glyph, tone, short: MARK_LABEL[tone], label };
}

const OUTLINE: BadgeLook = Object.freeze({ variant: "outline" });

/// A process's status, as a badge. `run` is the norm on this page, so it is an
/// outline; `ok` and `FAIL…` are the two that are worth a colour.
const PROC_BADGE: Readonly<Record<string, BadgeLook>> = Object.freeze({
  ok: { variant: "outline", className: "border-ok/40 bg-ok/10 text-ok" },
  fail: { variant: "destructive" },
  busy: { variant: "outline", className: "border-warn/40 bg-warn/10 text-warn" },
  done: OUTLINE,
  run: OUTLINE,
});

export function procBadge(status: string): BadgeLook {
  return PROC_BADGE[procClass(status)] ?? OUTLINE;
}

// ---------------------------------------------------------------------------
// Telemetry
// ---------------------------------------------------------------------------

/// One row of a machine's telemetry, as a [`Gauge`] takes it.
export interface GaugeSpec {
  key: string;
  label: string;
  /// Where the bar sits, 0..100 — a percentage in every case, including RAM,
  /// whose readout is in gigabytes.
  value: number;
  tone: MeterTone;
  /// The readout, written out: `"62% 71°"`, `"11.4/32G"`, `"0% 12.0/24G"`.
  /// A number and a suffix cannot say the composite ones, and the bar under a
  /// GPU is only the first half of what its line reports.
  text: string;
}

/// `loadClass` as a `Meter` tone. One rule for "how loaded is loaded", in
/// `dom.ts`, spelled here in the kit's vocabulary rather than restated.
export function loadTone(pct: number): MeterTone {
  const c = loadClass(pct);
  return c === "bad" ? "bad" : c === "warn" ? "warn" : "ok";
}

function num(n: number | null | undefined): string {
  return Number(n || 0).toFixed(0);
}

/// A used/total pair of capacities, in the unit the *total* earns.
///
/// One unit for the pair, so the two numbers stay comparable the way the ram
/// row's `11.4/32G` is. Terabytes above a terabyte, because a 3.6 TiB disk
/// written out as `3564/3667G` is four digits nobody reads.
///
/// **Binary, not decimal.** `SysDto`'s `*_gb` are GiB, so a `T` here has to be
/// 1024 of them or the row prints 3.7 where `df -h` prints 3.6 — and `df` is
/// exactly what someone reaches for to check it. `cap_pair` in
/// `crates/butai-client/src/chrome/mod.rs` is the same reading.
const TIB = 1024;
function capPair(usedGb: number, totalGb: number): string {
  if (totalGb >= TIB) return `${(usedGb / TIB).toFixed(1)}/${(totalGb / TIB).toFixed(1)}T`;
  // Whole gigabytes on both halves, unlike the ram row's `10.5/79G`. A disk is
  // three digits where memory is two, so the decimal is a tenth of a percent of
  // the reading and a character the mount beside it would rather have — and the
  // terminal draws `903/916G`, which is the number someone will be comparing
  // this against.
  return `${num(usedGb)}/${num(totalGb)}G`;
}

/// Mounts a column of gauges draws before it stops. The terminal's
/// `DISK_GAUGE_MAX`, and the same reason: a docker host's mount table is
/// dozens of image layers deep.
const DISK_MAX = 3;

/// Which mounts are worth a row: the real disks, largest first, capped.
///
/// This is the client's half of the daemon's bargain — it publishes every mount
/// and says what each one *is*, and each client decides which matter. Local
/// only: a tmpfs is RAM the ram row already counts, an overlay is the image
/// under a container rather than a disk that can fill, and a network mount's
/// capacity is a fact about a machine that has a column of its own. Largest
/// first is the daemon's own order, which is the order to cut from — an
/// installed snap is 100% full by construction, so a fullest-first cut would
/// spend the cap before naming a real disk.
///
/// `disk_mounts` in `crates/butai-client/src/chrome/mod.rs` is the same rule,
/// and the terminal has a `[ui] disks` key to override it. There is nowhere to
/// put such a key here yet, so this is the default and only the default.
export function railDisks(sys: SysDto | null | undefined): DiskDto[] {
  return (sys?.disks || []).filter((d) => d.kind === "local").slice(0, DISK_MAX);
}

/// One machine's telemetry as the rows a column of gauges draws — cpu, ram,
/// a line per GPU, then a line per disk.
///
/// Empty for a machine with no telemetry, so the caller's "there is nothing to
/// draw" test is `!gauges.length` rather than a second null check.
export function sysGauges(sys: SysDto | null | undefined): GaugeSpec[] {
  if (!sys) return [];
  const ram = sys.ram_total_gb ? (100 * sys.ram_used_gb) / sys.ram_total_gb : 0;
  const out: GaugeSpec[] = [
    {
      key: "cpu",
      label: "cpu",
      value: sys.cpu_pct,
      tone: loadTone(sys.cpu_pct),
      text: num(sys.cpu_pct) + "%" + (sys.cpu_temp != null ? " " + num(sys.cpu_temp) + "°" : ""),
    },
    {
      key: "ram",
      label: "ram",
      value: ram,
      tone: loadTone(ram),
      text: fmtGb(sys.ram_used_gb) + "/" + num(sys.ram_total_gb) + "G",
    },
  ];
  (sys.gpus || []).forEach((g, i) => {
    out.push({
      key: "gpu" + i,
      label: "gpu" + i,
      value: g.pct,
      tone: loadTone(g.pct),
      text: num(g.pct) + "% " + fmtGb(g.mem_used_gb) + "/" + num(g.mem_total_gb) + "G",
    });
  });
  // Disks last, below the things that move: they are the slowest reading here,
  // so they are what the eye passes on the way somewhere else. The mount is in
  // the label because it is the whole identity — two disks with their mounts
  // dropped are two identical rows, which is not true of `gpu0` and `gpu1`.
  railDisks(sys).forEach((d) => {
    const pct = d.total_gb ? (100 * d.used_gb) / d.total_gb : 0;
    out.push({
      key: "disk:" + d.mount,
      // A mount that missed the daemon's sweep says so rather than going
      // quiet: the row is still its last good reading, and a row that vanished
      // would read as a filesystem somebody unmounted.
      label: "dsk " + d.mount + (d.stale ? " (stale)" : ""),
      value: pct,
      // And it is drawn as a proportion rather than as a level: 99% full and a
      // minute out of date is news about the clock, not an alarm about the
      // disk.
      tone: d.stale ? "accent" : loadTone(pct),
      text: capPair(d.used_gb, d.total_gb),
    });
  });
  return out;
}

// ---------------------------------------------------------------------------
// Scrolling rails
// ---------------------------------------------------------------------------

/// What a scrolling **list** has to add to `ScrollArea`, until the kit's own
/// `ui/scroll-area.tsx` carries it.
///
/// Radix wraps a viewport's children in `<div style="min-width:100%;display:
/// table">` so that *horizontal* content can scroll. A table sizes to
/// `max-content`, so a list inside one is as wide as its widest row and the
/// rail scrolls sideways instead of truncating — measured in Chromium at 509px
/// of content inside a 319px CHANGES rail, and 385px inside a 299px AGENTS
/// rail. That is the audit's worst finding arriving by a new route: `Path`
/// elides in the middle only when its box is narrow, and in a box that is never
/// narrow nothing elides at all.
///
/// The `display` is an inline style of Radix's, so the override has to be
/// important. It belongs in `ui/scroll-area.tsx` — five pages now put a list in
/// one of these, and this string is in exactly one of them. See
/// `HANDOVER-work-home.md`.
export const SCROLLER = "[&_[data-slot=scroll-area-viewport]>div]:block!";

// ---------------------------------------------------------------------------
// Footers
// ---------------------------------------------------------------------------

/// `verbs.ts`'s table for a surface, packed into `HintBar` entries.
///
/// The packing is `fits`, at the terminal's own column count, and that is not a
/// layout decision made here — the browser's bar is elastic and would take
/// more. It is *which verbs are worth a column*, and the answer has to be the
/// terminal's or the two clients teach different keys. `press` receives the key
/// that was drawn, so clicking a hint and typing it are one dispatch.
export function hints(
  verbs: readonly Verb[],
  cols: number,
  rows: number,
  press?: ((key: string) => void) | undefined,
): Hint[] {
  return fits(verbs, cols, rows).map((v) => ({
    key: keyText(v.key),
    label: v.label,
    danger: v.danger,
    onSelect: press ? () => press(v.key) : undefined,
  }));
}
