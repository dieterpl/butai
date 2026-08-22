// A label and a **right-aligned numeric column**.
//
// A stack of these is a column, whatever the labels do: the value never wraps,
// is `tabular-nums` so the digits sit under the digits, and is pushed to the
// right edge rather than trailing the label. The label is what truncates —
// between "which metric" and "what it reads", the number is the part you came
// for, and the label is the part you can widen the panel to recover.
//
// `font-mono` and `tabular-nums` on the value are now belt and braces — every
// glyph on this client is a cell wide since the type collapsed to one family —
// but they stay, because they are the *claim* that this string is compared
// character by character, and that claim survives the family.
//
// Only `warn` and `bad` are worth tinting by default. A column of green numbers
// is `Badge`'s "eight rows, eight alarms" again — on this client a machine that
// is fine is the normal case, and the normal case is not news.

import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "@/lib/utils";

const statValue = cva("shrink-0 whitespace-nowrap font-mono tabular-nums", {
  variants: {
    tone: {
      default: "text-foreground",
      dim: "text-dim",
      accent: "text-primary",
      ok: "text-ok",
      warn: "text-warn",
      bad: "text-bad",
    },
  },
  defaultVariants: { tone: "default" },
});

type StatProps = React.ComponentProps<"div"> &
  VariantProps<typeof statValue> & {
    label: React.ReactNode;
    /** Pre-formatted. Units, precision and "—" for absent are the caller's call. */
    value: React.ReactNode;
    /** 20px, to sit in a dense list. */
    compact?: boolean | undefined;
  };

function Stat({ className, label, value, tone, compact, ...props }: StatProps) {
  return (
    <div
      data-slot="stat"
      className={cn(
        "flex min-w-0 shrink-0 items-center gap-3 px-3",
        compact ? "h-row-compact" : "h-row",
        className,
      )}
      {...props}
    >
      <span className="min-w-0 truncate text-dim">{label}</span>
      <span className={cn("ml-auto", statValue({ tone }))}>{value}</span>
    </div>
  );
}

export { Stat, statValue };
