// "loading…", "no commits", "not a git repository".
//
// Every list and every body on this client has these three states, and in the
// old one every list spelled them differently — a `<div>` here, a padded `<p>`
// there, one of them a `Row` that could take the cursor and open nothing. So:
// one line, on the row scale, in `--faint`, so it reads as *the absence of
// rows* rather than as a row.
//
// Not italic any more. A terminal's placeholder is the same cells in a fainter
// pen — italics in a monospace stack are a synthesised slant that breaks the
// grid the rows either side of it sit on, which is the opposite of what this
// line is for. `--faint` is a step under `--dim`, and it is the step the TUI
// uses for text that is present but not yours to act on.
//
// Not interactive, ever. That is the whole difference from `Row` and it is why
// this is its own component rather than a variant.

import * as React from "react";

import { INDENT_STEP } from "@/components/Row";
import { cn } from "@/lib/utils";

type EmptyProps = React.ComponentProps<"div"> & {
  /**
   * Matches `Row`'s, and for the same reason: a directory that is still
   * answering is an absence *at a depth*. A `loading…` back at the margin reads
   * as a sibling of the root rather than as the contents of the folder you just
   * opened.
   */
  indent?: number | undefined;
  /** 20px, to sit in a dense list without opening it up. */
  compact?: boolean | undefined;
};

function Empty({ className, indent, compact, children, ...props }: EmptyProps) {
  return (
    <div
      data-slot="empty"
      className={cn(
        "flex min-w-0 shrink-0 items-center px-3 text-faint",
        compact ? "h-row-compact" : "h-row",
        className,
      )}
      {...props}
    >
      {indent ? <span aria-hidden="true" className="shrink-0" style={{ width: indent * INDENT_STEP }} /> : null}
      {children}
    </div>
  );
}

export { Empty };
