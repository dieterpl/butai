// Code — a file, printed.
//
// The same box as `Patch` without the *reading* of it: a leading `-` in prose
// is not a deletion, and a source file full of them is not a diff. That is the
// whole difference between the two components, and it is why they are two.
//
// It scrolls in both directions rather than wrapping. A wrapped line stops
// being alignable against the line above it, and alignment is the reason this
// text is monospaced at all — so the box takes the horizontal scroll and the
// page never does.

import * as React from "react";

import { cn } from "@/lib/utils";

// Type and rhythm, in one place: the gutter and the body both take it, because
// a number that does not sit on its own line is worse than no number at all.
// 14px on an 18px line — the TUI's cell, so a patch drawn here and the same
// patch drawn on the stage are the same grid. `leading-normal` was a ratio and
// a ratio is not a cell; this is the number off `docs/images/changes-diff.svg`.
const TYPE = "font-mono text-13 leading-[18px]";

/**
 * The box both this and [`Patch`] are drawn in.
 *
 * One constant rather than one per component: while two of them exist they have
 * to agree to the pixel — a diff and the file it is a diff *of* are read one
 * after the other, and a line that moves between the two views reads as the
 * file having changed.
 */
export const CODE_BOX = `${TYPE} m-0 min-w-0 whitespace-pre p-2 text-foreground [tab-size:4]`;

type CodeProps = Omit<React.ComponentProps<"div">, "children"> & {
  /** The file's contents, as the daemon read them. */
  text: string;
  /** Draw a line-number gutter. Off by default: most of these are one hunk. */
  lineNumbers?: boolean | undefined;
};

function Code({ className, text, lineNumbers = false, ...props }: CodeProps) {
  const lines = text.split("\n");
  // A file that ends in a newline is not a file with a blank last line, and
  // numbering the phantom one is how you tell a reader the file is one line
  // longer than it is.
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();

  return (
    <div data-slot="code" {...props} className={cn("min-w-0 overflow-auto", className)}>
      {/* The scroller is the box, but the row inside it is `w-max`: a block
          that only filled the viewport would drop its right-hand padding the
          moment you scrolled, and the gutter's rule would stop at the fold
          instead of running the height of the longest line. */}
      <div className="flex w-max min-w-full items-stretch">
        {lineNumbers ? (
          <div
            aria-hidden="true"
            // `sticky` so the numbers stay put under a horizontal scroll — they
            // are the one thing on this surface that is not part of the text.
            // `select-none` keeps them out of a copy: a pasted file with a
            // number welded to every line is a file you have to clean by hand.
            className={cn(
              TYPE,
              "sticky left-0 shrink-0 border-r border-border bg-muted py-2 pr-1 pl-2",
              "text-right tabular-nums whitespace-pre text-faint select-none",
            )}
          >
            {lines.map((_, i) => i + 1).join("\n")}
          </div>
        ) : null}
        <pre className={cn(CODE_BOX, lineNumbers && "pl-1")}>{text}</pre>
      </div>
    </div>
  );
}

export { Code };
