// The tab bar, which the TUI writes across the top of the frame:
//
//        booth!  │ [ 1:repo ! [x] ]                    [agents v]  [+ host]
//
// In `docs/images/workbench.svg` the selected tab is a **band of `--accent`
// with ground-coloured bold text** — the `[ 1:repo ! [x] ]` run is drawn at
// `#151a23` on a `#7aa2f7` rectangle — and the unselected ones are ordinary
// text on the ordinary ground. That is the whole vocabulary: no pill track
// behind the list, no rounded segment, no sliding underline.
//
// So `TabsList` loses its `bg-muted` tray and its radius, and `TabsTrigger`'s
// active state becomes the band. The `line` variant survives as the same thing
// without the fill, which is what a rail of unemphasised tabs wants.

import * as React from "react"
import { cva, type VariantProps } from "class-variance-authority"
import { Tabs as TabsPrimitive } from "radix-ui"

import { cn } from "@/lib/utils"

function Tabs({
  className,
  orientation = "horizontal",
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Root>) {
  return (
    <TabsPrimitive.Root
      data-slot="tabs"
      data-orientation={orientation}
      orientation={orientation}
      className={cn(
        "group/tabs flex gap-1 data-[orientation=horizontal]:flex-col",
        className
      )}
      {...props}
    />
  )
}

const tabsListVariants = cva(
  cn(
    "group/tabs-list inline-flex w-fit items-center justify-center gap-1 rounded-none bg-transparent p-0",
    "group-data-[orientation=horizontal]/tabs:h-row",
    "group-data-[orientation=vertical]/tabs:h-fit group-data-[orientation=vertical]/tabs:flex-col",
  ),
  {
    variants: {
      variant: {
        default: "",
        line: "",
      },
    },
    defaultVariants: {
      variant: "default",
    },
  }
)

function TabsList({
  className,
  variant = "default",
  ...props
}: React.ComponentProps<typeof TabsPrimitive.List> &
  VariantProps<typeof tabsListVariants>) {
  return (
    <TabsPrimitive.List
      data-slot="tabs-list"
      data-variant={variant}
      className={cn(tabsListVariants({ variant }), className)}
      {...props}
    />
  )
}

function TabsTrigger({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Trigger>) {
  return (
    <TabsPrimitive.Trigger
      data-slot="tabs-trigger"
      className={cn(
        "relative inline-flex h-row shrink-0 items-center justify-center gap-1 whitespace-nowrap",
        "rounded-none border-0 bg-transparent px-2 text-13 text-dim outline-none",
        "group-data-[orientation=vertical]/tabs:w-full group-data-[orientation=vertical]/tabs:justify-start",
        "hover:bg-sel hover:text-foreground",
        "focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring",
        "disabled:pointer-events-none disabled:opacity-50",
        // The band. `font-semibold` because the capture's selected tab is bold,
        // which is the other half of how it separates from its neighbours.
        "data-[state=active]:bg-primary data-[state=active]:font-semibold data-[state=active]:text-primary-foreground",
        // `line` says the same thing without the fill: the accent moves to the
        // text instead of behind it.
        "group-data-[variant=line]/tabs-list:data-[state=active]:bg-transparent",
        "group-data-[variant=line]/tabs-list:data-[state=active]:text-primary",
        "[&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-3.5",
        className
      )}
      {...props}
    />
  )
}

function TabsContent({
  className,
  ...props
}: React.ComponentProps<typeof TabsPrimitive.Content>) {
  return (
    <TabsPrimitive.Content
      data-slot="tabs-content"
      className={cn("min-w-0 flex-1 outline-none", className)}
      {...props}
    />
  )
}

export { Tabs, TabsList, TabsTrigger, TabsContent, tabsListVariants }
