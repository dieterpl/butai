// Toasts. shadcn's wrapper, with the two things it assumes about a Next.js app
// taken out.
//
// **No `next-themes`.** The generated source reads the theme from that package;
// butai's theme is custom properties on `<html>` written by `theme.ts`, so there
// is nothing to read and nothing to install. Sonner is told `theme="system"`
// once and then styled entirely through the tokens below, which means a theme
// change repaints it along with everything else for free.
//
// **The tokens are ours.** shadcn writes `var(--popover)`; under our `@theme`
// the variable is `--color-popover`. A name that resolves to nothing is not an
// error, it is an unstyled toast — so this is the sort of mismatch that has to
// be fixed by looking rather than by compiling.

import {
  CircleCheckIcon,
  InfoIcon,
  Loader2Icon,
  OctagonXIcon,
  TriangleAlertIcon,
} from "lucide-react";
import { Toaster as Sonner, type ToasterProps } from "sonner";

function Toaster(props: ToasterProps) {
  return (
    <Sonner
      theme="system"
      className="toaster group"
      icons={{
        success: <CircleCheckIcon className="size-4 text-ok" />,
        info: <InfoIcon className="size-4 text-primary" />,
        warning: <TriangleAlertIcon className="size-4 text-warn" />,
        error: <OctagonXIcon className="size-4 text-bad" />,
        loading: <Loader2Icon className="size-4 animate-spin" />,
      }}
      style={
        {
          "--normal-bg": "var(--color-popover)",
          "--normal-text": "var(--color-popover-foreground)",
          "--normal-border": "var(--color-border)",
          // Square. The radius namespace is cleared in `styles.css`, so
          // `var(--radius-lg)` here would resolve to nothing — and a toast
          // whose corners depend on a variable *not* existing is a corner that
          // comes back the day somebody re-adds one. Say zero.
          "--border-radius": "0",
        } as React.CSSProperties
      }
      {...props}
    />
  );
}

export { Toaster };
