// DiffStat — `+102 -0` as a **column**, not as a tail on the filename.
//
// The audit: the counts were text that started wherever the path stopped, so
// they landed in a different place on every row. You could not read down them
// to find the big change, which is the only question a diffstat answers.
//
// Two fixed cells, right-aligned, mono and tabular: the sign lines up under the
// sign and the digits under the digits. The size is fixed here rather than
// inherited because a 40px cell only holds a predictable number of glyphs if
// the glyphs are a known width — that is the whole point of the cell.

import * as React from "react";

import { cn } from "@/lib/utils";
import type { FileChange } from "@/protocol/generated/protocol.ts";

// 44px is five cells of the 14px grid (8.4px each), which is `+9999` — more
// added lines than any one file in a review has, and past that the cell grows
// rather than lies. It was `w-10` when the counts were set at 12px; the type
// collapsed onto one size in the TUI pass and the cell had to follow, or the
// fifth digit would have been cut off by the box that exists to hold it.
const CELL = "w-11 shrink-0 text-right tabular-nums";

type DiffStatProps = Omit<React.ComponentProps<"span">, "children"> & {
  /** Lines added, as [`FileChange`] counts them. `null` draws an empty cell. */
  added?: FileChange["added"] | null | undefined;
  /** Lines deleted. The DTO's spelling, so a call site is a straight copy. */
  deleted?: FileChange["deleted"] | null | undefined;
};

function DiffStat({ className, added, deleted, ...props }: DiffStatProps) {
  return (
    <span
      data-slot="diff-stat"
      {...props}
      className={cn("flex shrink-0 items-baseline gap-2 font-mono text-13", className)}
    >
      <span className={cn(CELL, "text-ok")}>{added == null ? "" : `+${added}`}</span>
      <span className={cn(CELL, "text-bad")}>{deleted == null ? "" : `-${deleted}`}</span>
    </span>
  );
}

export { DiffStat };
