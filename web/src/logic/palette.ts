// Color resolution for the cell grid. The daemon sends protocol `Color`s —
// "default", {indexed: 0..255}, or {rgb:[r,g,b]} — exactly what a terminal
// emits. We map them the way a terminal would, with a curated 16-color base so
// the mirror is faithful *and* looks good.

import type { Color } from "../protocol/generated/protocol.ts";

/// The two colours a `"default"` cell resolves to.
///
/// A pane's default foreground and background are the *program's*, not the
/// chrome's, so they arrive from whoever is drawing — `settings.ts`'s
/// `termColors()` builds exactly this pair out of a palette. Named here rather
/// than imported from there: `palette.js` imports nothing, and a cell renderer
/// that pulls in the settings store is a cell renderer that cannot be used
/// without one.
export interface TermTheme {
  fg: string;
  bg: string;
}

// A tasteful, high-contrast ANSI-16 (GitHub-dark family). Order is the standard
// ANSI order the daemon relies on: 1=red 2=green 3=yellow 4=blue 5=magenta
// 6=cyan 7=gray 8=darkgray 15=white (see render.rs to_proto_color).
const ANSI16 = [
  "#0e1116", // 0 black
  "#f85149", // 1 red
  "#3fb950", // 2 green
  "#d29922", // 3 yellow
  "#58a6ff", // 4 blue
  "#bc8cff", // 5 magenta
  "#39c5cf", // 6 cyan
  "#b1bac4", // 7 white (light gray)
  "#6e7681", // 8 bright black (dark gray)
  "#ff7b72", // 9 bright red
  "#56d364", // 10 bright green
  "#e3b341", // 11 bright yellow
  "#79c0ff", // 12 bright blue
  "#d2a8ff", // 13 bright magenta
  "#56d4dd", // 14 bright cyan
  "#f0f6fc", // 15 bright white
] as const;

// 6-level cube step values used by xterm's 256-color palette.
const CUBE = [0, 95, 135, 175, 215, 255] as const;

function hex2(n: number): string {
  return n.toString(16).padStart(2, "0");
}

// Full xterm-256 lookup for indices 16..255 (16..231 = 6x6x6 cube, 232..255 =
// 24-step grayscale ramp). Index 238, butai's selection background, lands here.
//
// The four `!`s are the same fact four times: `Color::Indexed` carries a `u8`,
// so `idx` is 0..255, and `% 6` cannot leave the cube. `noUncheckedIndexedAccess`
// cannot see either, and a `?? fallback` would invent a colour for an index that
// does not exist rather than keep the JS's answer.
function xterm256(idx: number): string {
  if (idx < 16) return ANSI16[idx]!;
  if (idx < 232) {
    const n = idx - 16;
    const r = CUBE[Math.floor(n / 36) % 6]!;
    const g = CUBE[Math.floor(n / 6) % 6]!;
    const b = CUBE[n % 6]!;
    return `#${hex2(r)}${hex2(g)}${hex2(b)}`;
  }
  const v = 8 + (idx - 232) * 10;
  return `#${hex2(v)}${hex2(v)}${hex2(v)}`;
}

// Resolve a protocol Color to a CSS color string.
//   color: "default" | {indexed:n} | {rgb:[r,g,b]}
//   isFg:  pick the theme's default fg vs bg when color is "default"
//   theme: {fg, bg} CSS strings for the "default" case
//
// `color` is optional on a `Cell`, so an absent one is a colour this has to
// answer for — hence the `null | undefined` in the signature rather than a
// caller-side check nobody would remember to write.
export function resolveColor(color: Color | null | undefined, isFg: boolean, theme: TermTheme): string {
  if (color == null || color === "default") return isFg ? theme.fg : theme.bg;
  if (typeof color === "object") {
    if ("indexed" in color) return xterm256(color.indexed);
    if ("rgb" in color) {
      const [r, g, b] = color.rgb;
      return `rgb(${r},${g},${b})`;
    }
  }
  return isFg ? theme.fg : theme.bg;
}
