// The ONE selection style: **a background band**, which is what the terminal
// draws.
//
// `UI-REWRITE.md` counted **three selection styles for one concept**, and all
// three were answers to the same flinch: a selected row that grows a border is
// a row that is two pixels taller than its neighbours, so a list you walk with
// `j`/`k` reflows under the cursor. The TUI has never had that problem, because
// a cursor there is `sel` painted across the cells of a line — `#2b3547`
// against `#151a23` in `docs/images/settings.svg`, which is a real capture.
// **Never a bordered box.**
//
// The band is the whole of it now. The previous pass added a 1px inset ring
// alongside it, which is a web app's way of saying the same thing twice; the
// ring is gone and only the focus ring survives, because a browser has a
// keyboard focus that a terminal does not.
//
// Two smaller fixes the old kit did not have:
//
// **Hover no longer eats the selection.** `hover:bg-muted` is a class *and* a
// pseudo-class, so it outranks `bg-sel` on specificity: pointing at the selected
// row used to un-draw the band. The hover tint is only applied when the row is
// not selected.
//
// **The keyboard handler cannot be clobbered.** A caller's `onKeyDown` composes
// with this one rather than replacing it, so a row that takes the pointer keeps
// `Enter` / `Space` — the property that stops "clickable but unreachable" from
// coming back in one careless page.

import * as React from "react";

import { cn } from "@/lib/utils";

/** One level of tree depth. The kit owns the step; see `indent`. */
const INDENT_STEP = 12;

type RowProps = Omit<React.ComponentProps<"div">, "onSelect"> & {
  /** Draw the selection band and the ring. Presentation only — the page owns the cursor. */
  selected?: boolean | undefined;
  /**
   * Make the row interactive. It takes the pointer, `Enter` / `Space` and a
   * focus ring *together*, so a row is never reachable by one and not the
   * other. Without it the row is inert text.
   */
  onSelect?: ((event: React.MouseEvent<HTMLDivElement> | React.KeyboardEvent<HTMLDivElement>) => void) | undefined;
  /** 20px instead of 22px, for a dense list — a diff, a file tree. */
  compact?: boolean | undefined;
  /**
   * Depth in a tree, one 12px step per level, drawn as a spacer **inside** the
   * row rather than as padding on it. Two reasons, and both are why the kit
   * owns the step rather than each tree picking its own: a child's name lands
   * under its parent's, and the selection band still starts in the same place
   * on every line whatever the depth.
   */
  indent?: number | undefined;
};

function Row({
  className,
  selected,
  onSelect,
  compact,
  indent,
  children,
  onClick,
  onKeyDown,
  ...props
}: RowProps) {
  const interactive = onSelect != null;

  return (
    <div
      data-slot="row"
      // `option` wants a `listbox` around it: the list is the page's, so the
      // page puts `role="listbox"` on the container it already renders.
      role={interactive ? "option" : undefined}
      aria-selected={interactive ? !!selected : undefined}
      data-selected={selected ? "" : undefined}
      tabIndex={interactive ? 0 : undefined}
      onClick={
        interactive || onClick
          ? (e) => {
              onClick?.(e);
              if (!e.defaultPrevented) onSelect?.(e);
            }
          : undefined
      }
      onKeyDown={
        interactive || onKeyDown
          ? (e) => {
              onKeyDown?.(e);
              if (e.defaultPrevented || !onSelect) return;
              if (e.key !== "Enter" && e.key !== " ") return;
              // Otherwise `Space` scrolls the list out from under the row it
              // was about to open.
              e.preventDefault();
              onSelect(e);
            }
          : undefined
      }
      className={cn(
        // 22px, and no transition: the terminal's line is 18px at 14px type,
        // and 22 rather than 18 is the one deliberate concession to the
        // pointer — enough to click at, short of a web app's 32px control.
        "group flex min-w-0 shrink-0 items-center gap-2 px-3 text-foreground",
        compact ? "h-row-compact" : "h-row",
        interactive && "cursor-pointer",
        // Hover stays a *fainter* plane than the band. A terminal has no
        // pointer and so no third state; giving hover the selection colour
        // would make the row under the mouse indistinguishable from the row
        // under the cursor, which is the one thing this band has to say.
        selected ? "bg-sel" : interactive && "hover:bg-muted",
        // The one thing here the TUI has no equivalent for. Inset, because a
        // ring that grew outward would be drawn over the rows either side of a
        // 22px line; 1px, because the band is already carrying the selection.
        "outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring",
        className,
      )}
      {...props}
    >
      {/* The one place geometry is inline rather than a class: depth is a
          number at runtime, and Tailwind cannot generate a class for a width it
          never sees in the source. */}
      {indent ? <span aria-hidden="true" className="shrink-0" style={{ width: indent * INDENT_STEP }} /> : null}
      {children}
    </div>
  );
}

export { Row, INDENT_STEP };
