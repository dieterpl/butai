// The ONE section header — **the title let into the top rule**.
//
//     ┌ AGENTS ─────────[+ agent]┐
//     ┌ CHANGES (5) · berth-clearance ─────┐
//     ├ PROCESSES ───────[+ term]┤
//
// That is `docs/images/workbench.svg`, a real capture, and it is what this
// component draws. The previous pass made this a filled strip with caps in
// `--dim` and a hairline under it — a web app's section header. Reversed.
//
// ## How the rule is drawn
//
// Three pieces, and none of them is a box-drawing glyph:
//
//   1. `bg-card` across the whole line, which **masks the top 22px of the
//      `Card`'s frame** — its top edge and the first 22px of both sides. See
//      `ui/card.tsx` for why the frame is an inset shadow and therefore
//      maskable at all.
//   2. An absolutely-positioned box from the vertical centre to the bottom,
//      carrying `border-t border-x`. Its top edge is the `─────` run; its two
//      sides are the descending halves of `┌` and `┐`, which land exactly on
//      the frame's sides and continue them.
//   3. The label and the `action`, each on `bg-card`, which interrupt the rule
//      the way the characters do in the capture.
//
// Real `┌` and `┐` glyphs were the obvious first try and they do not line up:
// the glyph's vertical stroke sits at the centre of its 8.4px cell, so it lands
// four pixels inside a frame drawn at the box's edge. CSS corners are exact.
//
// ## The colour is the frame's, not a heading colour
//
// In the capture the whole run — corner, title, rule — is one pen. An unfocused
// panel draws it in `rule`; the focused one draws it in `rule_focus`, which is
// the accent; a panel reporting a failure draws it in `danger`. So `tone` here
// picks a *frame* colour and the title follows it, rather than the title having
// a colour of its own. It reads dimmer than a web heading would, and that is
// the terminal: the panel names recede and the rows are what you read.

import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";

import { cn } from "@/lib/utils";

// Which pen the frame is drawn in. The three the TUI uses, and no fourth.
const frameTone = cva("", {
  variants: {
    tone: {
      /** `rule` — a panel that does not have the cursor. */
      default: "border-border text-border",
      /** `rule_focus` — the panel the keyboard is in. */
      focus: "border-ring text-ring",
      /** `danger` — a panel whose subject has failed. */
      danger: "border-bad text-bad",
    },
  },
  defaultVariants: { tone: "default" },
});

type SectionTitleProps = React.ComponentProps<"div"> &
  VariantProps<typeof frameTone> & {
    /**
     * The right-hand slot, drawn *inside the rule* — `[+ agent]`, `[+ term]`, a
     * count. In the capture this is a bracket button sitting in the top rule
     * just before the corner, which is why it is a slot and not four headers.
     */
    action?: React.ReactNode;
  };

function SectionTitle({ className, action, tone, children, ...props }: SectionTitleProps) {
  const pen = frameTone({ tone });
  return (
    <div
      data-slot="section-title"
      data-tone={tone ?? "default"}
      className={cn("relative flex h-row shrink-0 items-center bg-card px-1", pen, className)}
      {...props}
    >
      {/* The rule, and the two corner stubs that carry it down into the Card's
          own frame. `pointer-events-none` so the action above it stays hittable. */}
      <span
        aria-hidden="true"
        className={cn("pointer-events-none absolute inset-x-0 top-1/2 bottom-0 border-x border-t", pen)}
      />
      {/* `relative` lifts the words above the rule; `bg-card` is what actually
          cuts the gap in it. */}
      <span className="relative min-w-0 truncate bg-card px-1 uppercase">{children}</span>
      {action ? <span className="relative ml-auto flex shrink-0 items-center bg-card">{action}</span> : null}
    </div>
  );
}

export { SectionTitle, frameTone };
