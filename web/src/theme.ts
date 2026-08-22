// The palette, applied to `<html>`. The only bridge between `settings.ts` and
// the stylesheet, and the only place this client learns a colour.
//
// **There are no colour literals under `src/`.** `settings.ts` already owns the
// palettes and the role-to-variable table; a second copy here would be drift
// between two statements of one decision, which is the exact thing this rewrite
// exists to remove. So the palettes are read at runtime rather than restated.
//
// ## The mechanism, unchanged from every client before this one
//
// A theme is `VARS` written onto `document.documentElement` as inline custom
// properties. `styles.css` names those same variables through `@theme`, so one
// write restyles Tailwind's whole output — including, now that v4 resolves
// colours through `color-mix()`, every translucent surface and every shadow.
//
// ## One deliberate difference, and it is forced
//
// The old client's `system` arm *removed* the variables so a
// `prefers-color-scheme` block in its stylesheet showed through. There is no
// such block here, and removing them would leave the page with no palette at
// all. `system` resolves through `resolveTheme(name, prefersDark())` instead,
// which answers "which of the two did the OS ask for" out of the same table.
// Same colours, same OS preference, reached by a function call rather than by a
// media query.

import { useEffect, useState } from "react";
import { SYSTEM, VARS, load, resolveTheme, sanitize, save, termColors } from "./logic/settings.ts";

type Palette = ReturnType<typeof resolveTheme>;

/**
 * The OS preference. Dark unless the browser says light — matching every other
 * butai client, where the default is the palette it shipped with.
 */
export function prefersDark(): boolean {
  if (typeof window === "undefined" || !window.matchMedia) return true;
  return !window.matchMedia("(prefers-color-scheme: light)").matches;
}

/**
 * `localStorage`, or nothing.
 *
 * A browser can *throw* outright on the property — Safari's private mode, a
 * blocked third-party context — and a client that will not boot because it could
 * not read a colour preference is a worse client than one that draws the
 * default.
 */
export function storage(): Storage | null {
  try {
    return window.localStorage ?? null;
  } catch {
    return null;
  }
}

/** The stored theme name, shared with every other client under the same key. */
export function storedTheme(): string {
  return load(storage()).theme || SYSTEM;
}

/** Persist a theme name into the same store the settings page uses. */
export function storeTheme(name: string): string {
  save(storage(), sanitize({ ...load(storage()), theme: name }));
  return name;
}

/**
 * Write a palette's roles onto `<html>` and return the palette that won.
 *
 * Inline style, so it beats anything a stylesheet says; `colorScheme` alongside
 * them because the scrollbars, the form controls and the canvas the browser
 * paints behind us all read that and none of them reads a custom property.
 */
export function applyTheme(name: string, root?: HTMLElement): Palette | null {
  const el = root ?? document.documentElement;
  const pal = resolveTheme(name, prefersDark());
  if (!pal) return null;
  // `VARS` is role -> custom-property name, and `pal.colors` is keyed by the
  // same roles; TypeScript cannot see that they are the same key set through
  // `Object.entries`, which widens to `string`.
  const colors = pal.colors as Record<string, string | undefined>;
  for (const [role, v] of Object.entries(VARS)) setVar(el, v as string, colors[role]);
  const term = termColors(pal);
  setVar(el, "--term-bg", term.bg);
  setVar(el, "--term-fg", term.fg);
  el.style.colorScheme = pal.scheme;
  return pal;
}

function setVar(el: HTMLElement, name: string, value: string | undefined): void {
  if (value) el.style.setProperty(name, value);
  else el.style.removeProperty(name);
}

/**
 * Apply the stored theme as early as the page can manage it.
 *
 * Called from `main.tsx` before React renders, so the palette is on `<html>`
 * before the first frame that has anything in it. It reaches only same-origin
 * code; nothing here waits on the network.
 */
export function bootTheme(): Palette | null {
  return applyTheme(storedTheme());
}

/**
 * `useTheme(name)` — paint `name` and hand back the palette it resolved to.
 *
 * `system` is a palette that *moves*, so this re-resolves when the OS flips
 * rather than only when React re-renders: without the listener, a page left open
 * across a sunset keeps the morning's colours.
 */
export function useTheme(name: string): Palette | null {
  const [pal, setPal] = useState<Palette | null>(() => resolveTheme(name || SYSTEM, prefersDark()));
  useEffect(() => {
    const paint = () => setPal(applyTheme(name || SYSTEM));
    paint();
    if (!window.matchMedia) return undefined;
    const mq = window.matchMedia("(prefers-color-scheme: light)");
    mq.addEventListener("change", paint);
    return () => mq.removeEventListener("change", paint);
  }, [name]);
  return pal;
}
