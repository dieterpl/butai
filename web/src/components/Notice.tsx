// A box about a *state*, not about content.
//
// A rebase stopped on a conflict. A machine that has gone away. A daemon one
// version behind this client. `Card` is the wrong shape for those — it is panel
// ground that content sits on, and this is a coloured edge around a sentence
// and the two buttons that end it.
//
// It exists because the WORK port spelled `rounded-md border border-bad` inline
// and `ui/pages-spell-no-radii` caught it. That check working is the whole
// argument for this directory: the shape was missing, so the kit grows it once
// rather than every page inventing its own.
//
// **A coloured frame and nothing behind it.** The previous pass gave this a
// tinted plane and a shadow so it would sit "in front of" the list; the TUI
// draws the same thing as a box in the failing colour — `docs/images/` has one
// framed in `#f7768e`, the danger role, with the panel's own ground inside it.
// A tint and an elevation are both ways of saying *above*, and a character grid
// has no above. The edge carries it.
//
// Square, therefore, and 1px: the frame is the same weight as every other rule
// on the page, because in the terminal it is literally the same character.
//
// Padding is built in. The old one left it to the caller, which meant
// `<Notice>` with bare text was unreadable until somebody remembered `p-2`, and
// two callers remembered differently. `cn()` is `tailwind-merge`, so a caller
// who wants a different box still wins — `className="p-3"` genuinely overrides.

import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "@/lib/utils";

const noticeVariants = cva(
  cn(
    "min-w-0 rounded-none border px-2 py-1 text-13 text-foreground",
    "[&_a]:text-primary [&_a]:underline [&_a]:underline-offset-2",
  ),
  {
    variants: {
      variant: {
        default: "border-border",
        // `ok` is here because the shell's message line is the one Notice that
        // reports a *success* — "commit ok", "push ok". Without it that line was
        // a `default`, indistinguishable from the failure it most needs to be
        // told apart from.
        // Only the edge takes the colour. `booth.svg` frames its NEEDS YOU
        // block in `danger` and leaves every line inside it in the ordinary
        // pen — the frame is the alarm, the contents are still contents.
        ok: "border-ok",
        warn: "border-warn",
        bad: "border-bad",
      },
    },
    defaultVariants: { variant: "default" },
  },
);

function Notice({
  className,
  variant,
  ...props
}: React.ComponentProps<"div"> & VariantProps<typeof noticeVariants>) {
  return (
    <div
      data-slot="notice"
      data-variant={variant ?? "default"}
      // Announced when it appears, without stealing focus or interrupting.
      // `role="alert"` is deliberately not the default even for `bad`: most of
      // these are drawn on load, and a page that shouts three times before the
      // user has done anything trains them to ignore it. Pass it explicitly for
      // the ones that follow an action.
      role="status"
      className={cn(noticeVariants({ variant }), className)}
      {...props}
    />
  );
}

export { Notice, noticeVariants };
