// The frames both renderers are fed.
//
// One frame per thing that can be drawn differently, because a differential
// test is only as good as the cases it puts through. The list is taken from
// what `Cell` and `Mods` can actually carry — every colour form, every
// attribute, the wide-glyph convention and every cursor shape — rather than
// from what looked worth checking.

export interface FixtureCell {
  ch: string;
  fg?: unknown;
  bg?: unknown;
  mods?: Record<string, boolean> | null;
}
export interface Fixture {
  name: string;
  cols: number;
  rows: number;
  frame: {
    full: boolean;
    cells: Array<{ x: number; y: number; cells: FixtureCell[] }>;
    cursor?: [number, number] | null;
    cursor_shape?: string;
  };
}

const run = (x: number, y: number, text: string, extra: Partial<FixtureCell> = {}) => ({
  x,
  y,
  cells: [...text].map((ch) => ({ ch, ...extra })),
});

export const FIXTURES: Fixture[] = [
  {
    name: "plain text on a full frame",
    cols: 40,
    rows: 4,
    frame: { full: true, cells: [run(0, 0, "hello butai"), run(2, 2, "second row, indented")], cursor: null },
  },
  {
    name: "the three colour forms",
    cols: 40,
    rows: 4,
    frame: {
      full: true,
      cells: [
        run(0, 0, "default", { fg: "default", bg: "default" }),
        // An indexed colour goes through the 16-entry table; an rgb triple does
        // not. They are different code paths in `resolveColor` and a port that
        // collapsed them would still look right on this row alone.
        run(0, 1, "indexed", { fg: { indexed: 4 }, bg: { indexed: 0 } }),
        run(0, 2, "rgbcolor", { fg: { rgb: [255, 128, 0] }, bg: { rgb: [16, 32, 48] } }),
        run(0, 3, "bright", { fg: { indexed: 12 }, bg: { indexed: 8 } }),
      ],
      cursor: null,
    },
  },
  {
    name: "every attribute",
    cols: 40,
    rows: 8,
    frame: {
      full: true,
      cells: [
        run(0, 0, "bold", { mods: { bold: true } }),
        run(0, 1, "italic", { mods: { italic: true } }),
        run(0, 2, "dim", { mods: { dim: true } }),
        run(0, 3, "underline", { mods: { underline: true } }),
        run(0, 4, "crossed", { mods: { crossed_out: true } }),
        // Reverse swaps fg and bg *at draw time*, so it is the one attribute
        // that changes both passes.
        run(0, 5, "reverse", { fg: { indexed: 2 }, bg: { indexed: 1 }, mods: { reverse: true } }),
        run(0, 6, "combined", { mods: { bold: true, italic: true, underline: true } }),
        // A space that is only visible because it is underlined — the early
        // `continue` in pass 2 has to let this one through.
        run(0, 7, "   ", { mods: { underline: true } }),
      ],
      cursor: null,
    },
  },
  {
    name: "wide glyphs and the empty trailing half",
    cols: 20,
    rows: 3,
    frame: {
      full: true,
      // `ch: ""` is the trailing half of a double-width glyph: the buffer keeps
      // it distinct from a space, because the glyph in the cell before it is
      // drawn spanning two columns.
      //
      // **No pixel currently depends on that distinction**, and saying otherwise
      // would be inventing coverage. Pass 2 skips `""` and `" "` with the same
      // early `continue`, so mutating `applyFrame` to `cell.ch || " "` leaves
      // every fixture here byte-identical. The distinction is still the honest
      // record of what the daemon sent — and the moment anything paints a
      // background per cell, or the cursor redraws the glyph beneath it for a
      // wide character, it starts to matter.
      cells: [
        { x: 0, y: 0, cells: [{ ch: "日" }, { ch: "" }, { ch: "本" }, { ch: "" }, { ch: "!" }] },
        { x: 0, y: 1, cells: [{ ch: "→" }, { ch: "★" }, { ch: "✓" }, { ch: "é" }] },
      ],
      cursor: null,
    },
  },
  {
    name: "a partial frame over a full one",
    cols: 30,
    rows: 3,
    // `full: false` is the damage-diff path and is what the daemon actually
    // sends between repaints. A port that treated every frame as full would
    // pass every other fixture here and blank the screen in production.
    frame: {
      full: false,
      cells: [run(5, 1, "patched")],
      cursor: null,
    },
  },
  {
    name: "cursor: block",
    cols: 20,
    rows: 3,
    frame: { full: true, cells: [run(0, 0, "cursor here")], cursor: [3, 0], cursor_shape: "block" },
  },
  {
    name: "cursor: bar",
    cols: 20,
    rows: 3,
    frame: { full: true, cells: [run(0, 0, "cursor here")], cursor: [3, 0], cursor_shape: "bar" },
  },
  {
    name: "cursor: underline",
    cols: 20,
    rows: 3,
    frame: { full: true, cells: [run(0, 0, "cursor here")], cursor: [3, 0], cursor_shape: "underline" },
  },
  {
    name: "a run that overflows the row",
    cols: 10,
    rows: 2,
    // The bounds check is `if (x < this.cols)` *inside* the loop with `x++`
    // outside it, so a run starting near the edge is clipped rather than
    // wrapped. Worth pinning: the obvious rewrite clips the whole run.
    frame: { full: true, cells: [run(7, 0, "overflowing")], cursor: null },
  },
  {
    name: "a run addressed past the last row",
    cols: 10,
    rows: 2,
    frame: { full: true, cells: [run(0, 0, "kept"), run(0, 9, "dropped")], cursor: null },
  },
];
