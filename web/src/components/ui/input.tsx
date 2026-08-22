// A field, framed the way the TUI frames one.
//
//     ╭────────────────────────────────────────────────╮
//     │ >                                              │
//     ╰────────────────────────────────────────────────╯
//
// That is the agent's prompt box on the stage in `docs/images/workbench.svg`,
// and `changes-diff.svg` has the commit field as `[ commit... ]`. Either way it
// is a 1px rule and the ground behind it — no radius, no inner shadow, no
// three-pixel focus halo. Focus swaps the rule to `--focus`, which is exactly
// what the TUI does to the box that has the keyboard.
//
// `h-row`, so a field in a header lines up with the list under it.

import * as React from "react"

import { cn } from "@/lib/utils"

function Input({ className, type, ...props }: React.ComponentProps<"input">) {
  return (
    <input
      type={type}
      data-slot="input"
      className={cn(
        "h-row w-full min-w-0 rounded-none border border-input bg-transparent px-2 text-13",
        "outline-none selection:bg-primary selection:text-primary-foreground",
        "file:inline-flex file:h-row-compact file:border-0 file:bg-transparent file:text-13 file:text-foreground",
        "placeholder:text-faint disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50",
        // One line, swapped — not a border *and* a ring, which would be two
        // statements of the same fact and a field that changes width on focus.
        "focus-visible:border-ring",
        "aria-invalid:border-destructive",
        className
      )}
      {...props}
    />
  )
}

export { Input }
