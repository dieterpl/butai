// A dialog, drawn as **a framed box** — which is the only kind a terminal has.
//
// The TUI's overlays are `draw_box`: a 1px rule all the way round, the title
// let into the top of it, and the panel ground inside. So the shadcn source's
// `rounded-lg`, `shadow-lg` and `zoom-in-95` all go — a character grid has no
// radius, no elevation and nothing that scales into view — and the header
// becomes the same rule `SectionTitle` draws, for the same reason and by the
// same three pieces.
//
// The overlay behind it stays, because the TUI does dim the workbench under a
// modal. It is `--bg` at 60% rather than black, so it is the palette dimming
// the page and not a grey sheet over it.

import * as React from "react"
import { XIcon } from "lucide-react"
import { Dialog as DialogPrimitive } from "radix-ui"

import { cn } from "@/lib/utils"
import { Button } from "@/components/ui/button"

function Dialog({
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Root>) {
  return <DialogPrimitive.Root data-slot="dialog" {...props} />
}

function DialogTrigger({
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Trigger>) {
  return <DialogPrimitive.Trigger data-slot="dialog-trigger" {...props} />
}

function DialogPortal({
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Portal>) {
  return <DialogPrimitive.Portal data-slot="dialog-portal" {...props} />
}

function DialogClose({
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Close>) {
  return <DialogPrimitive.Close data-slot="dialog-close" {...props} />
}

function DialogOverlay({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Overlay>) {
  return (
    <DialogPrimitive.Overlay
      data-slot="dialog-overlay"
      className={cn(
        "fixed inset-0 z-50 bg-background/60",
        className
      )}
      {...props}
    />
  )
}

function DialogContent({
  className,
  children,
  showCloseButton = true,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Content> & {
  showCloseButton?: boolean
}) {
  return (
    <DialogPortal data-slot="dialog-portal">
      <DialogOverlay />
      <DialogPrimitive.Content
        data-slot="dialog-content"
        className={cn(
          "fixed top-[50%] left-[50%] z-50 grid w-full max-w-[calc(100%-2rem)] translate-x-[-50%] translate-y-[-50%]",
          // The frame is `--focus`, not `--rule`: a dialog is by definition the
          // thing with the keyboard, and that is the pen the TUI draws the
          // focused box in. `px-2` is the body gutter — the header and the
          // footer undo it with `-mx-2` so their rules reach the frame.
          //
          // An inset ring rather than a `border`, for the reason `ui/card.tsx`
          // sets out at length: the header has to *mask* the frame where the
          // title goes, and `-mx-2` reaches the padding edge — which is where an
          // inset shadow is drawn and is one pixel short of a border.
          "gap-2 rounded-none bg-card px-2 pt-0 pb-2 outline-none sm:max-w-lg",
          "shadow-[inset_0_0_0_1px_var(--color-ring)]",
          className
        )}
        {...props}
      >
        {children}
        {showCloseButton && (
          <DialogPrimitive.Close
            data-slot="dialog-close"
            // In the rule, at the corner, where the TUI puts `[x]`.
            className="absolute top-0 right-1 z-10 flex h-row items-center bg-card px-1 text-faint rounded-none outline-none hover:text-foreground focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring disabled:pointer-events-none [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-3.5"
          >
            <XIcon />
            <span className="sr-only">Close</span>
          </DialogPrimitive.Close>
        )}
      </DialogPrimitive.Content>
    </DialogPortal>
  )
}

function DialogHeader({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="dialog-header"
      // The title is let into the top rule, so the header is that rule: a line
      // of `bg-card` masking the frame, with the rule redrawn at its centre and
      // cut where the words are. Same three pieces as `SectionTitle`.
      className={cn(
        "relative -mx-2 flex h-row shrink-0 items-center bg-card px-1 text-left text-ring",
        // `before:`, not `after:`. Both the rule and the title are positioned,
        // so DOM order decides which is on top — and `::after` is generated as
        // the *last* child, which drew the rule straight through the words.
        // Measured, not reasoned about: the strike-through is visible in a
        // screenshot and invisible in the class string.
        "before:pointer-events-none before:absolute before:inset-x-0 before:top-1/2 before:bottom-0",
        "before:border-x before:border-t before:border-ring",
        className
      )}
      {...props}
    />
  )
}

function DialogFooter({
  className,
  showCloseButton = false,
  children,
  ...props
}: React.ComponentProps<"div"> & {
  showCloseButton?: boolean
}) {
  return (
    <div
      data-slot="dialog-footer"
      className={cn(
        "-mx-2 -mb-2 flex flex-col-reverse gap-1 border-t border-border px-2 py-1 sm:flex-row sm:justify-end",
        className
      )}
      {...props}
    >
      {children}
      {showCloseButton && (
        <DialogPrimitive.Close asChild>
          <Button variant="outline">Close</Button>
        </DialogPrimitive.Close>
      )}
    </div>
  )
}

function DialogTitle({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Title>) {
  return (
    <DialogPrimitive.Title
      data-slot="dialog-title"
      // `relative` and `bg-card` are what cut the gap in the rule behind it —
      // the same three pieces as `SectionTitle`, and they live here rather than
      // as a descendant selector on the header so the title carries its own
      // mask wherever it is put.
      className={cn("relative min-w-0 truncate bg-card px-1 text-13 uppercase", className)}
      {...props}
    />
  )
}

function DialogDescription({
  className,
  ...props
}: React.ComponentProps<typeof DialogPrimitive.Description>) {
  return (
    <DialogPrimitive.Description
      data-slot="dialog-description"
      className={cn("text-13 text-dim", className)}
      {...props}
    />
  )
}

export {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogOverlay,
  DialogPortal,
  DialogTitle,
  DialogTrigger,
}
