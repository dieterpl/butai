// The FILES page's browser: the Finder trail, and the minimap's ink.
//
// The three pieces of that page that are arithmetic rather than markup, and so
// the three that can be wrong without anything on screen looking broken: what a
// column is called, which verb a key means, and where a line's ink sits.
//
// The keyboard test is the one that matters most, and it is not really about the
// arrows. `docs/keys.md` and the terminal's `handle_files_key` bind the same four
// gestures; this page resolves a key by asking `filesVerbs()` what it means
// rather than by matching a letter, and what is pinned here is that the lookup
// really goes through the table — a page that grew its own `case "h":` would
// pass every other test in this suite and quietly teach a key the footer does
// not draw.

import { describe, expect, test } from "bun:test";

import { columnLabel, filesVerb } from "../src/pages/FilesPage.tsx";
import { runsOf } from "../src/components/Minimap.tsx";
import { ROOT, here, holds, into, left, point, rowIn, trim } from "../src/logic/trail.ts";
import { VerbId, filesVerbs, keyText } from "../src/logic/verbs.ts";

// ---------------------------------------------------------------------------
// The trail
// ---------------------------------------------------------------------------

describe("the trail is the path from the root to where you are", () => {
  // Root → crates → crates/butai-client, with the cursor in the deepest column.
  const deep = into(into(ROOT, 0, "crates"), 1, "crates/butai-client");

  test("trail/root — a browser starts at the workspace root, in it", () => {
    expect([ROOT.dirs, ROOT.col, here(ROOT)]).toEqual([[""], 0, ""]);
  });

  test("trail/descend — every directory on the way is still a column", () => {
    expect([deep.dirs, deep.col, here(deep)]).toEqual([
      ["", "crates", "crates/butai-client"],
      2,
      "crates/butai-client",
    ]);
  });

  // The whole reason the columns to the right are kept. `←` then `→` has to be
  // two local moves, or browsing over a slow link is browsing and waiting.
  test("trail/walk-back-and-forth — and neither move re-fetches", () => {
    const up = left(left(deep));
    expect([up.col, up.dirs]).toEqual([0, ["", "crates", "crates/butai-client"]]);
    const back = into(into(up, 0, "crates"), 1, "crates/butai-client");
    expect([back.col, back.dirs]).toEqual([2, ["", "crates", "crates/butai-client"]]);
  });

  test("trail/root-is-the-floor — there is nowhere above the workspace", () => {
    expect(left(ROOT)).toEqual(ROOT);
  });

  // The one rule. Those columns are what the *old* selection contained.
  test("trail/point-drops-the-stale-columns — or the path drawn does not exist", () => {
    const moved = point(deep, 0, 3);
    expect([moved.dirs, moved.col, moved.cursor[""]]).toEqual([[""], 0, 3]);
  });

  test("trail/point-settled — re-pointing at the selected row keeps its column", () => {
    const settled = point(point(ROOT, 0, 2), 0, 2);
    expect(settled.dirs).toEqual([""]);
    // …and the same row two columns deep does not throw the deeper one away.
    const held = point(into(point(ROOT, 0, 1), 0, "crates"), 1, 0);
    expect(held.dirs).toEqual(["", "crates"]);
  });

  test("trail/cursor-per-directory — walk out and back and the row survives", () => {
    const t = point(into(point(ROOT, 0, 2), 0, "crates"), 1, 4);
    expect([rowIn(t, "", 9), rowIn(t, "crates", 9)]).toEqual([2, 4]);
    // Clamped into the listing that actually arrived, not the one that was
    // stored — the cursor is set before anything has been fetched.
    expect(rowIn(t, "crates", 3)).toBe(2);
    expect(rowIn(t, "crates", 0)).toBe(0);
  });

  test("trail/trim — a file contains nothing, so nothing is below it", () => {
    expect(trim(deep, 1).dirs).toEqual(["", "crates"]);
  });

  test("trail/holds — what is still on screen, so a body can be dropped when it is not", () => {
    expect([holds(deep, "crates"), holds(deep, "docs")]).toEqual([true, false]);
  });
});

describe("a column is named after the directory it lists", () => {
  test("files/column-label — the root is `/`, and on DOCS it says so", () => {
    expect([
      columnLabel("", false),
      columnLabel("", true),
      columnLabel("crates", false),
      columnLabel("crates/butai-client/src", false),
    ]).toEqual(["/", "docs", "crates", "src"]);
  });
});

// ---------------------------------------------------------------------------
// The keyboard
// ---------------------------------------------------------------------------

describe("the browser's keys come from the verb table", () => {
  const at = (key: string, ctrlKey = false) => filesVerb({ key, ctrlKey }, false);

  test("files/keys-finder — the four gestures the terminal binds", () => {
    expect([
      at("ArrowLeft"),
      at("ArrowRight"),
      at(" "),
      at("Enter"),
    ]).toEqual([VerbId.TreeUp, VerbId.TreeInto, VerbId.Peek, VerbId.Open]);
  });

  test("files/keys-letters — and the letters they are aliases for", () => {
    expect([at("h"), at("l"), at("j"), at("k")]).toEqual([
      VerbId.TreeUp,
      VerbId.TreeInto,
      VerbId.Down,
      VerbId.Up,
    ]);
  });

  // The horizontal pair is the trail's, the vertical pair is the column's. A
  // client that answered one with the other would walk the wrong axis, which is
  // why they are four ids and not two.
  test("files/keys-two-axes — walking columns is not walking rows", () => {
    expect(new Set([at("h"), at("l"), at("j"), at("k")]).size).toBe(4);
  });

  test("files/keys-editing — while editing, only the two ways out", () => {
    const editing = (key: string, ctrlKey = false) => filesVerb({ key, ctrlKey }, true);
    expect([editing("s", true), editing("Escape"), editing("h"), editing(" ")]).toEqual([
      VerbId.Save,
      VerbId.CancelEdit,
      null,
      null,
    ]);
  });

  // An arrow that is not in the surface's table means nothing: the alias only
  // exists because `h` and `l` are bound here, and it must not survive them.
  test("files/keys-arrows-are-aliases — not a second table", () => {
    const table = filesVerbs(false);
    for (const id of [VerbId.TreeUp, VerbId.TreeInto, VerbId.Down, VerbId.Up]) {
      expect(table.some((v) => v.id === id)).toBe(true);
    }
  });

  // The footer is drawn from the same table this reads, so a gesture worth
  // binding is a gesture that is written down. `peek` is the one that had to be
  // argued for: it is a whole mode of reading a directory, and a key nobody can
  // find is a key nobody uses.
  test("files/keys-peek-is-documented — space is in the footer, spelled `space`", () => {
    const peek = filesVerbs(false).find((v) => v.id === VerbId.Peek);
    expect([peek?.footer, keyText(peek?.key ?? "")]).toEqual([true, "space"]);
  });
});

// ---------------------------------------------------------------------------
// The minimap
// ---------------------------------------------------------------------------

describe("a line's ink is its runs, not one bar", () => {
  // The gaps between words are most of what makes prose look like prose and
  // code look like code at sixty pixels wide. One bar from the indent to the
  // end of the line throws all of it away.
  test("minimap/runs — the gaps are the texture", () => {
    expect(runsOf("ab cd")).toEqual([
      [0, 2],
      [3, 5],
    ]);
  });

  test("minimap/runs-indent — leading space is where the shape comes from", () => {
    expect(runsOf("    x")).toEqual([[4, 5]]);
  });

  // Tabs advance four, as `Code`'s `[tab-size:4]` draws them and as the
  // terminal's texture measures them. A tab worth one column here would put
  // every indent at a depth the text never had.
  test("minimap/runs-tabs — four columns, in both clients", () => {
    expect(runsOf("\t\tx")).toEqual([[8, 9]]);
  });

  test("minimap/runs-blank — a blank line has no ink at all", () => {
    expect([runsOf(""), runsOf("   ")]).toEqual([[], []]);
  });

  // Clipped rather than squeezed, so the left edge is at the same scale on
  // every row — which is the point, since indentation is what makes a file
  // recognisable at this size.
  test("minimap/runs-clip — a long line loses its tail, not its scale", () => {
    const [run] = runsOf("x".repeat(500));
    expect(run).toEqual([0, 96]);
  });
});
