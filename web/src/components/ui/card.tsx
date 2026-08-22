// The panel, which in this client is a **box-drawing frame**.
//
//     ┌ AGENTS ─────────[+ agent]┐
//     │  gemini              WAIT│
//     └──────────────────────────┘
//
// `docs/images/workbench.svg` is the reference and it is a real capture, so the
// geometry below is measured rather than chosen: a 1px rule all the way round,
// square corners, no fill beyond `--panel`, no elevation.
//
// ## Why the frame is an inset ring and not a `border`
//
// The title has to be *let into* the top rule, which means something has to
// paint over the frame where the words go. Every list card on this client sets
// `overflow-hidden` (`LIST_CARD` in GitPage, both cards in FilesPage, the rail
// blocks in HomePage and UsagePage), and `overflow` clips descendants to the
// **padding** box — so a child can never reach a real `border`, at any margin.
//
// An `inset` box-shadow is drawn inside the padding box instead, which puts it
// where children can cover it: painting order is background, then inset
// shadows, then descendants. So `SectionTitle` masks the top of the frame with
// `bg-card` and draws its own rule at its centre, and the two meet exactly.
//
// It also composes with the one thing pages already do to a `Card`: GitPage
// marks its focused column with `ring-1 ring-inset ring-ring`, and Tailwind
// lists the ring shadow *before* `--tw-shadow`, so the accent ring paints over
// this frame rather than fighting it. A real border would have been a second
// line beside it.
//
// ## The padding
//
// Zero, on both axes. shadcn ships `py-6` and `gap-6`, which is a card on a
// marketing page; every page here already had to spell `py-0 gap-0` to undo it,
// and a panel's first row starting one pixel under the rule is what the capture
// shows. `CardHeader`/`CardContent` carry the gutter for the cards that want
// one.

import * as React from "react"

import { cn } from "@/lib/utils"

function Card({ className, children, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card"
      className={cn(
        "relative flex min-w-0 flex-col gap-0 bg-card py-0 text-card-foreground",
        // The frame. `--color-border` by name, not by value — see `styles.css`.
        "shadow-[inset_0_0_0_1px_var(--color-border)]",
        className
      )}
      {...props}
    >
      {children}
    </div>
  )
}

function CardHeader({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-header"
      className={cn(
        "@container/card-header grid auto-rows-min grid-rows-[auto_auto] items-start gap-0 px-3 py-1",
        "has-data-[slot=card-action]:grid-cols-[1fr_auto] [.border-b]:pb-1",
        className
      )}
      {...props}
    />
  )
}

function CardTitle({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-title"
      className={cn("min-w-0 truncate text-13 leading-[18px] font-semibold", className)}
      {...props}
    />
  )
}

function CardDescription({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-description"
      className={cn("text-12 text-muted-foreground", className)}
      {...props}
    />
  )
}

function CardAction({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-action"
      className={cn(
        "col-start-2 row-span-2 row-start-1 self-start justify-self-end",
        className
      )}
      {...props}
    />
  )
}

function CardContent({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-content"
      className={cn("min-w-0 px-3", className)}
      {...props}
    />
  )
}

function CardFooter({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="card-footer"
      className={cn("flex items-center px-3 [.border-t]:pt-1", className)}
      {...props}
    />
  )
}

export {
  Card,
  CardHeader,
  CardFooter,
  CardTitle,
  CardAction,
  CardDescription,
  CardContent,
}
