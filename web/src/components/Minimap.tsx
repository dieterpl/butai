// Minimap — the whole file as a strip of texture beside it.
//
// A file is read through a viewport a few dozen lines tall, and a scrollbar's
// answer to "where am I" is a fraction. This answers with the shape of the code:
// where the blank lines are, where a comment block sits, where the deeply
// indented middle of a function is. A jump is then aimed at something you
// recognise rather than at a percentage.
//
// The TUI draws the same thing out of `chrome::minimap`, and the two agree about
// what a minimap *is* — density and indentation, the whole file at once, the
// viewport marked on it — while disagreeing about what they can draw with. A
// terminal has five shades of block and a palette of token colours, because the
// TUI highlights. This client prints files as plain text, so there are no tokens
// to colour by and the texture is one colour at varying weight instead. Inventing
// a second highlighter here to close that gap would be a lot of code for a
// picture sixty pixels wide.
//
// ## Why a canvas
//
// A line is a handful of rectangles, and a big file is tens of thousands of
// them. As DOM that is a node per run; as canvas it is one element and a loop
// that runs when the text or the size changes, and never on scroll — the
// viewport marker is a separate absolutely-positioned box so that dragging it
// costs no repaint of the texture underneath.

import * as React from "react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

import { cn } from "@/lib/utils";

/**
 * Document columns the strip's width stands for.
 *
 * Lines longer than this are clipped rather than squeezed, so the left edge is
 * at the same scale on every row — which is the point, since indentation is what
 * makes a file recognisable at this size. The same number the TUI uses, so a
 * file has the same silhouette in both clients.
 */
const SPAN = 96;

/**
 * Lines past which the texture is sampled rather than drawn line by line.
 *
 * Well above any file worth reading in a browser, and there so that opening a
 * generated 400k-line lockfile is a slow *minimap* rather than a frozen tab.
 */
const MAX_LINES = 20_000;

/** Weight of one line's ink. Overlapping lines accumulate, which is the shading. */
const INK = 0.5;

/** One run of non-space on a line: where it starts and where it ends. */
type Run = readonly [start: number, end: number];

/**
 * A line's runs of ink, in columns, clipped to [`SPAN`].
 *
 * Runs rather than one bar from the indent to the line's end: the gaps between
 * words are most of what makes prose look like prose and code look like code at
 * this scale, and a solid bar throws all of it away.
 */
export function runsOf(line: string): Run[] {
  const out: Run[] = [];
  let col = 0;
  let start = -1;
  for (const ch of line) {
    if (col >= SPAN) break;
    // Tabs advance four, as `Code`'s `[tab-size:4]` draws them. A tab worth one
    // column here would put every indent at a depth the text never had.
    const w = ch === "\t" ? 4 : 1;
    const blank = ch === "\t" || ch === " ";
    if (blank) {
      if (start >= 0) out.push([start, col]);
      start = -1;
    } else if (start < 0) {
      start = col;
    }
    col += w;
  }
  if (start >= 0) out.push([start, Math.min(col, SPAN)]);
  return out;
}

export type MinimapProps = Omit<React.ComponentProps<"div">, "onSelect"> & {
  /** The file's contents, exactly as the body is showing them. */
  text: string;
  /** The element the file scrolls in. */
  scroller: React.RefObject<HTMLElement | null>;
};

function Minimap({ className, text, scroller, ...props }: MinimapProps) {
  const canvas = useRef<HTMLCanvasElement | null>(null);
  const host = useRef<HTMLDivElement | null>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  // The viewport marker, as fractions of the document. Kept apart from the
  // texture so a scroll moves a box and does not repaint the file.
  const [view, setView] = useState({ top: 0, height: 1 });

  // -- the viewport ----------------------------------------------------------

  const measure = useCallback(() => {
    const el = scroller.current;
    if (!el) return;
    const total = el.scrollHeight || 1;
    setView({ top: el.scrollTop / total, height: Math.min(1, el.clientHeight / total) });
  }, [scroller]);

  useEffect(() => {
    const el = scroller.current;
    if (!el) return undefined;
    measure();
    el.addEventListener("scroll", measure, { passive: true });
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => {
      el.removeEventListener("scroll", measure);
      ro.disconnect();
    };
    // `text` is in here because a new file is a new scroll height, and the
    // marker is a fraction of it.
  }, [measure, scroller, text]);

  // -- the texture -----------------------------------------------------------

  // Layout effect, not effect: the first paint otherwise lands with a zero-sized
  // canvas and the strip flashes empty on every file you open.
  useLayoutEffect(() => {
    const el = host.current;
    if (!el) return undefined;
    const ro = new ResizeObserver(() => {
      const r = el.getBoundingClientRect();
      setSize({ w: Math.round(r.width), h: Math.round(r.height) });
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useEffect(() => {
    const c = canvas.current;
    if (!c || !size.w || !size.h) return;
    // Device pixels for the backing store, CSS pixels for the drawing, so a
    // one-pixel line is one *device* pixel on a retina screen rather than a
    // blurry two.
    const dpr = window.devicePixelRatio || 1;
    c.width = Math.round(size.w * dpr);
    c.height = Math.round(size.h * dpr);
    const ctx = c.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, size.w, size.h);

    const lines = text.split("\n");
    if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
    // The palette, resolved: the canvas wears `text-foreground`, so this is
    // whatever `--fg` is right now. Read at draw time rather than held, which is
    // also what makes a theme change come out right on the next resize.
    ctx.fillStyle = getComputedStyle(c).color;
    ctx.globalAlpha = INK;

    // The file fills the strip, whatever its length: a pixel is not a line, so
    // a two-hundred-line file drawn one line to the pixel would be a texture in
    // the top third of an empty column and a viewport marker — which *is* a
    // fraction of the strip — floating somewhere below it.
    //
    // Short file: each line is a fat bar. Long one: `lh` falls under a pixel,
    // lines land on the same row, and the alpha accumulates — which is the
    // shading, and it comes out of the compositor rather than out of a count.
    const lh = size.h / Math.max(1, lines.length);
    const step = Math.max(1, Math.ceil(lines.length / MAX_LINES));
    const bar = Math.max(1, lh * step);
    const scale = size.w / SPAN;
    for (let i = 0; i < lines.length; i += step) {
      const y = i * lh;
      for (const [from, to] of runsOf(lines[i]!)) {
        ctx.fillRect(from * scale, y, Math.max(1, (to - from) * scale), bar);
      }
    }
  }, [text, size.w, size.h]);

  // -- aiming ----------------------------------------------------------------

  // A press puts the point under the pointer in the *middle* of the viewport,
  // not at its top: you clicked a shape in order to read what is around it, and
  // a shape pinned to the top row has its context cut off.
  const aim = useCallback(
    (clientY: number) => {
      const el = scroller.current;
      const box = host.current;
      if (!el || !box) return;
      const r = box.getBoundingClientRect();
      const at = r.height ? (clientY - r.top) / r.height : 0;
      const max = el.scrollHeight - el.clientHeight;
      el.scrollTop = Math.max(0, Math.min(max, at * el.scrollHeight - el.clientHeight / 2));
    },
    [scroller],
  );

  const drag = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    e.currentTarget.setPointerCapture(e.pointerId);
    aim(e.clientY);
  };

  return (
    <div
      ref={host}
      data-slot="minimap"
      // `text-foreground` is not decoration here: the canvas reads its own
      // computed colour off this class, so the strip follows the theme.
      className={cn(
        "relative w-16 shrink-0 cursor-pointer border-l border-border bg-card text-foreground",
        className,
      )}
      onPointerDown={drag}
      onPointerMove={(e) => {
        if (e.buttons & 1) aim(e.clientY);
      }}
      {...props}
    >
      <canvas ref={canvas} className="block h-full w-full" style={{ color: "inherit" }} />
      {/* The viewport, over the texture. `pointer-events-none` so a press on the
          marker aims like a press anywhere else — a marker you have to grab by
          its edge is a marker that swallows half the clicks aimed at it. */}
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-x-0 border-y border-border/70 bg-foreground/10"
        style={{ top: `${view.top * 100}%`, height: `${Math.max(0.02, view.height) * 100}%` }}
      />
    </div>
  );
}

export { Minimap };
