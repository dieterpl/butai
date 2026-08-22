// Meter — a bar that is a *measurement*, because it always has a track.
//
// The audit's SYSTEM/COMPUTE finding: the old bars stopped at whatever width
// the value gave them, so 30% and 90% ended in two different places with
// nothing behind them to measure against — a coloured rectangle you cannot
// read a proportion off. The track is the whole component. There is no variant
// that omits it.
//
// **Square, and it jumps.** The previous pass rounded the ends and animated the
// fill over 150ms; the TUI's telemetry rows are a braille sparkline that
// repaints on the tick, with nothing rounded and nothing in between. The bar is
// 4px — under half a cell — so a stack of gauges keeps the terminal's rhythm
// rather than opening up into a dashboard.
//
// The sparkline itself is the one thing here that is not reproduced, and the
// reason is data, not CSS: `⣀⣀⣀⣀` is a *history*, and a `Meter` is handed one
// scalar. See `HANDOVER-tui-style.md`.

import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "@/lib/utils";

// The fill. Five tones, and every one of them is a palette role: `accent` for a
// neutral proportion, the three load colours, and `run` for a process's own.
const meterVariants = cva("h-full rounded-none", {
  variants: {
    tone: {
      accent: "bg-primary",
      ok: "bg-ok",
      warn: "bg-warn",
      bad: "bg-bad",
      run: "bg-run",
    },
  },
  defaultVariants: { tone: "accent" },
});

/** The tones a `Meter` (and so a `Gauge`) can be drawn in. */
export type MeterTone = NonNullable<VariantProps<typeof meterVariants>["tone"]>;

type MeterProps = React.ComponentProps<"div"> &
  VariantProps<typeof meterVariants> & {
    /** Where the bar is. Clamped into the track, so bad telemetry cannot draw past it. */
    value?: number | undefined;
    /** What the track *is*. Percentages are the common case, hence the default. */
    max?: number | undefined;
  };

function Meter({ className, value = 0, max = 100, tone, ...props }: MeterProps) {
  // A `max` of zero is a division by nothing — a daemon reporting a disk of
  // size 0 must not blank the page — and a NaN from a partial DTO is a bar at
  // the left, not a bar of width NaN.
  const span = max > 0 ? max : 100;
  const now = Number.isFinite(value) ? value : 0;
  const pct = Math.max(0, Math.min(100, (now * 100) / span));
  return (
    <div
      data-slot="meter"
      role="progressbar"
      aria-valuenow={now}
      aria-valuemin={0}
      aria-valuemax={span}
      {...props}
      // The track is `--line`, not `--panel2`: a meter sits on a card, and
      // `--panel2` against `--panel` is a step you cannot see at eight pixels
      // tall. A track that is technically drawn and visually absent is the bug,
      // not the fix for it.
      className={cn("h-1 w-full min-w-0 overflow-hidden rounded-none bg-border", className)}
    >
      <div className={meterVariants({ tone })} style={{ width: `${pct}%` }} />
    </div>
  );
}

export { Meter, meterVariants };
