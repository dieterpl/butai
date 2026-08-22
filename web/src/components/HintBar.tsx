// The verb footer, **spanning the page**, written the way the TUI writes it:
//
//     a new... · x kill
//     t new · r restart · x kill
//     j/k move   enter change   tab group   esc close
//     c commit · g git · ? keys
//
// Those are runs off `docs/images/workbench.svg` and `settings.svg`. What they
// show, and what the previous pass got wrong: **the key is not a keycap.** It
// is the letter, a space, and the label — one string, one colour, in `--faint`,
// with entries joined by ` · `. Only a destructive verb changes pen, and it
// changes the *whole* entry to `--bad` (`x kill`), not just the letter. There
// are no borders, no tinted boxes and no buttons on this bar.
//
// The kit does not import `verbs.ts`. A hint is a key and a label, already
// spelled: pass `keyText(v.key)` from the caller and this stays a view
// component with nothing to know about the verb table. `logic/page.ts`'s
// `hintKeys` already hands back exactly this shape.
//
// Two entry forms, and the difference is not decoration:
//
//   ["enter", "open"]                          — documentation. Drawn as text.
//   { key: "x", label: "kill", danger: true,
//     onSelect: () => run(VerbId.Kill) }       — wired. Drawn as a button.
//
// They look identical, because in the terminal they are identical — the footer
// is a legend, and the only difference here is that a browser lets you click
// one. A wired entry is a real `<button>` so it is reachable by tab and by
// pointer; it brightens to `--fg` under the mouse and does nothing else. An
// entry with no `onSelect` must not respond to the pointer at all, because a
// footer entry that looks live and is not is worse than one that reads as a
// note. And a wired one has to dispatch the same verb the key does — that is
// the property that stops the two clients teaching different keys.

import * as React from "react";

import { cn } from "@/lib/utils";

/** A key, what it does, and — when the page can run it — how. */
export type Hint = {
  /** Spelled as the user reads it: `"enter"`, `"esc"`, `"space"`, `"a"`. */
  key: string;
  label: string;
  /** Kill, abort, discard. Drawn in `--bad`, whole entry, as the TUI draws it. */
  danger?: boolean | undefined;
  /** Runs the same verb the key runs. Absent means the page cannot run it here. */
  onSelect?: (() => void) | undefined;
};

/** The shorthand for a hint the page only documents. */
export type HintLike = Hint | readonly [key: string, label: string];

type HintBarProps = React.ComponentProps<"div"> & {
  keys?: readonly HintLike[] | undefined;
};

function toHint(hint: HintLike): Hint {
  return "label" in hint ? hint : { key: hint[0], label: hint[1] };
}

function HintBar({ className, keys, ...props }: HintBarProps) {
  const hints = (keys ?? []).map(toHint);
  return (
    <div
      data-slot="hint-bar"
      role="group"
      aria-label="keys"
      className={cn(
        "flex h-row w-full shrink-0 items-center overflow-hidden whitespace-nowrap",
        "border-t border-border bg-card px-2 text-13 text-faint",
        className,
      )}
      {...props}
    >
      {hints.map((hint, i) => {
        // `key` here is React's, and it needs the index: two surfaces can offer
        // the same letter for two labels on one bar.
        const id = `${hint.key}/${i}`;
        const tone = hint.danger ? "text-bad" : undefined;
        const body = (
          <>
            {hint.key} {hint.label}
          </>
        );
        return (
          <React.Fragment key={id}>
            {/* The separator is the TUI's, a middle dot with a space either
                side, and it belongs to the *gap* rather than to either entry —
                so it is never the first or last thing on the bar. */}
            {i > 0 ? <span aria-hidden="true" className="px-1 text-faint">·</span> : null}
            {hint.onSelect ? (
              <button
                type="button"
                title={hint.label}
                onClick={hint.onSelect}
                className={cn(
                  "shrink-0 cursor-pointer rounded-none bg-transparent outline-none",
                  "hover:text-foreground focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring",
                  tone,
                )}
              >
                {body}
              </button>
            ) : (
              <span className={cn("shrink-0", tone)}>{body}</span>
            )}
          </React.Fragment>
        );
      })}
    </div>
  );
}

export { HintBar };
