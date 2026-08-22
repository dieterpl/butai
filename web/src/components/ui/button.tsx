// The only button, and in this client it is written **`[+ agent]`**.
//
//     [+ agent]   [+ host]   [+ new]   [+ term]   [open]   [agents v]
//     [layout] [detach] [help] [settings]
//
// Every one of those is off `docs/images/workbench.svg` and `settings.svg`,
// which are real captures. Square brackets, no fill, no border, no radius, no
// shadow, no lift, no transition. The previous pass gave this six filled and
// outlined variants with a shadow that rose on hover and a one-pixel press;
// that is the web-app direction, and it has been reversed.
//
// ## What the variants mean now
//
// In the TUI a button is a *word in brackets* and the only thing that varies is
// the pen it is drawn in — so the six shadcn variants collapse onto the five
// pens the captures actually use:
//
//   default      the active one. An accent band with ground-coloured bold text,
//                which is how `[help]`, `[settings]`, `[work]` and the current
//                tab are drawn when they are the thing you are looking at.
//   outline      an ordinary button: `--fg`, hover takes the selection band.
//   ghost        `[+ agent]` — `--faint`, hover brightens to `--fg`.
//   secondary    the status bar's `[layout] [detach]` — `--dim`.
//   destructive  `x kill` — `--bad`.
//   link         unchanged; the one shape a terminal has no answer for.
//
// ## The brackets are elements, not `::before`
//
// Pseudo-elements are flex items, so `gap` lands *inside* the brackets and you
// get `[ + agent ]`. Two spans with no gap around them give the capture's
// spacing exactly. They are `aria-hidden`, so the accessible name is still the
// label alone.
//
// `asChild` skips them: `Slot` takes exactly one child, and a button rendered
// as somebody else's element is not this shape anyway. Pass `bracket={false}`
// for the same reason anywhere the brackets would be wrong.

import * as React from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { Slot } from "radix-ui";

import { cn } from "@/lib/utils";

const BASE = cn(
  // `gap-0`: the brackets have to hug the label — `[+ agent]`, never
  // `[ + agent ]`. The gap that separates an icon from its text lives on the
  // inner span instead, which is exactly the span the brackets sit outside of.
  "inline-flex shrink-0 select-none items-center justify-center gap-0 whitespace-nowrap",
  // No radius, no shadow, no transform, no transition. A terminal repaints.
  "rounded-none",
  // Keyboard-only focus. An inset ring, because a ring drawn outside a 22px
  // control in a 22px row is drawn over the rows either side of it.
  "outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring",
  "disabled:pointer-events-none disabled:opacity-50",
  // Icons size themselves to the cell unless the caller says otherwise.
  "[&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-3.5",
);

const buttonVariants = cva(BASE, {
  variants: {
    variant: {
      default: "bg-primary font-semibold text-primary-foreground",
      destructive: "bg-transparent text-bad hover:bg-sel",
      secondary: "bg-transparent text-dim hover:bg-sel hover:text-foreground",
      outline: "bg-transparent text-foreground hover:bg-sel",
      ghost: "bg-transparent text-faint hover:bg-sel hover:text-foreground",
      link: "bg-transparent text-primary underline-offset-2 hover:underline",
    },
    size: {
      // `row` is the default because a button most often sits beside a list —
      // or in the rule above one, which is the same 22px line.
      default: "h-row px-1 text-13",
      sm: "h-row-compact px-1 text-13",
      lg: "h-row-lg px-1 text-13",
      // The icon sizes set a height and let the width follow, rather than
      // being square: `[×]` is three cells wide and one tall, and a square box
      // would put the brackets on top of the glyph.
      icon: "h-row px-1",
      "icon-sm": "h-row-compact px-1 [&_svg:not([class*='size-'])]:size-3",
      "icon-lg": "h-row-lg px-1",
    },
  },
  defaultVariants: { variant: "default", size: "default" },
});

function Button({
  className,
  variant,
  size,
  asChild = false,
  bracket = true,
  children,
  ...props
}: React.ComponentProps<"button"> &
  VariantProps<typeof buttonVariants> & {
    /** Render the child element instead of a `<button>`, keeping the styling. */
    asChild?: boolean;
    /** Draw the `[` `]`. On by default — see the note above for when it is not. */
    bracket?: boolean;
  }) {
  const Comp = asChild ? Slot.Root : "button";
  const wrapped = asChild || !bracket;
  return (
    <Comp
      data-slot="button"
      type={asChild ? undefined : ((props as React.ComponentProps<"button">).type ?? "button")}
      className={cn(buttonVariants({ variant, size }), className)}
      {...props}
    >
      {wrapped ? (
        children
      ) : (
        <>
          <span aria-hidden="true">[</span>
          <span className="inline-flex min-w-0 items-center gap-1">{children}</span>
          <span aria-hidden="true">]</span>
        </>
      )}
    </Comp>
  );
}

export { Button, buttonVariants };
