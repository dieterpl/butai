// Gauge — a labelled `Meter`, with its readout right-aligned in a fixed column.
//
// The other half of the SYSTEM/COMPUTE finding, and the half that was invisible
// until you measured it: HOME's compute bars ended at 1112px while the numbers
// they belonged to right-aligned at 1268px, so the bar and its value were two
// measurements of nothing in particular. Here the label line and the bar share
// one `px-3` gutter, so the track's right edge *is* the readout's right edge by
// construction rather than by both being roughly right.
//
// The readout is mono and tabular and lives in a fixed column, so a stack of
// gauges reads down as a column whatever the labels do — that is the same rule
// `DiffStat` follows, for the same reason.
//
// The value is only tinted at `warn` and `bad`. A column of green numbers is
// eight rows reporting eight alarms: on this client a machine that is fine is
// the normal case, and the normal case is not news.

import * as React from "react";

import { cn } from "@/lib/utils";
import { Meter, type MeterTone } from "@/components/Meter";

type GaugeProps = Omit<React.ComponentProps<"div">, "children"> & {
  /** What is being measured — `cpu`, `ram`, `gpu0`. Truncates; the number does not. */
  label: string;
  /** The measurement itself, in whatever unit `max` is in. */
  value: number;
  /** Full scale. Percentages are the common case, hence the default. */
  max?: number | undefined;
  /** Appended to the formatted value — `"%"`, `" GB"`. */
  suffix?: string | undefined;
  /**
   * The readout, written out. For the composite ones a number and a suffix
   * cannot say: `"0% 12.0/24G"` is a GPU's utilisation *and* its memory, and
   * the bar underneath is only the first of them.
   */
  text?: string | undefined;
  tone?: MeterTone | undefined;
};

function Gauge({ className, label, value, max = 100, suffix, text, tone, ...props }: GaugeProps) {
  // React's own id, because the bar needs a name and the label is already on
  // screen — a second, invisible copy in an `aria-label` is a second string to
  // keep in step with the first.
  const id = React.useId();
  const shown = text ?? `${Math.round((Number.isFinite(value) ? value : 0) * 10) / 10}${suffix ?? ""}`;
  return (
    <div data-slot="gauge" {...props} className={cn("flex min-w-0 flex-col", className)}>
      {/* The label line and the sparkline under it: `CPU Ryzen 7 5700  16% 56°`
          over its own row, which is exactly the shape of the TUI's SYSTEM
          panel. `h-row-compact`, because two of these stacked is one reading
          and it should read as one block rather than as two rows. */}
      <div className="flex h-row-compact min-w-0 items-center gap-3 px-3 text-13">
        <span id={id} className="min-w-0 truncate text-dim">
          {label}
        </span>
        <span
          data-numeric
          className={cn(
            "ml-auto min-w-16 shrink-0 whitespace-nowrap text-right font-mono",
            tone === "warn" ? "text-warn" : tone === "bad" ? "text-bad" : "text-foreground",
          )}
        >
          {shown}
        </span>
      </div>
      <div className="px-3 pb-1">
        <Meter value={value} max={max} tone={tone} aria-labelledby={id} aria-valuetext={shown} />
      </div>
    </div>
  );
}

export { Gauge };
