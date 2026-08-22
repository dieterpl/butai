// A state, written as **a word in a pen** — not a pill.
//
//     │  gemini              WAIT│
//     │  codex              ⠇ 15s│
//     │> claude             done•│
//     │  shell                 ok│
//
// That is `docs/images/workbench.svg`. There is no shape around any of those:
// a terminal cannot draw a rounded capsule, so it says the same thing with
// colour and with case. `WAIT` is capitals because it is the one that wants
// you; `ok` and `done•` are lower case because they do not.
//
// So this keeps shadcn's API — the variants pages already pass — and drops the
// box. `variant` now picks a pen:
//
//   default      the accent
//   destructive  `--bad`, **and upper case**, which is the TUI's WAIT
//   outline      the ordinary pen; the commonest by far on this client
//   secondary    `--dim`
//   ghost/link   unchanged in intent, flat in fact
//
// Pages pass `className` with tones on it already — `border-ok/40 text-ok`,
// `TONE.warn` — and those keep working: the `border-*` half is now painting an
// edge that has no width, and the `text-*` half is the whole point.

import * as React from "react"
import { cva, type VariantProps } from "class-variance-authority"
import { Slot } from "radix-ui"

import { cn } from "@/lib/utils"

const badgeVariants = cva(
  cn(
    "inline-flex w-fit shrink-0 items-center justify-center gap-1 overflow-hidden",
    "rounded-none border-0 bg-transparent px-0 whitespace-nowrap",
    "focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring",
    "[&>svg]:pointer-events-none [&>svg]:size-3",
  ),
  {
    variants: {
      variant: {
        default: "text-primary",
        secondary: "text-dim",
        // Upper case is the signal, exactly as `WAIT` is in the capture: a
        // failing or waiting state is the one word on a row you should be able
        // to find without reading the row.
        destructive: "font-semibold text-bad uppercase",
        outline: "text-foreground",
        ghost: "text-faint",
        link: "text-primary underline underline-offset-2",
      },
    },
    defaultVariants: {
      variant: "default",
    },
  }
)

function Badge({
  className,
  variant = "default",
  asChild = false,
  ...props
}: React.ComponentProps<"span"> &
  VariantProps<typeof badgeVariants> & { asChild?: boolean }) {
  const Comp = asChild ? Slot.Root : "span"

  return (
    <Comp
      data-slot="badge"
      data-variant={variant}
      className={cn(badgeVariants({ variant }), className)}
      {...props}
    />
  )
}

export { Badge, badgeVariants }
