// Commit lanes: the column of glyphs down the left of the GIT page's history.
//
// Ported from `check.py`'s `check_graph` / `GRAPH_JS`. The glyph strings are
// lifted exactly — they are a picture, and a picture is the only readable
// assertion about a drawing.

import { expect, test } from "bun:test";
import { glyphs, graphRows, graphWidth, rowWidth } from "../src/logic/graph.ts";

/** `"c b a"` is commit c with parents b and a. Newest first, topological — what
 *  `git/log` guarantees with `--topo-order`. */
const walk = (spec: string[]) =>
  spec.map((line) => {
    const p = line.trim().split(/\s+/);
    return { id: p[0]!, parents: p.slice(1) };
  });

const draw = (spec: string[], max = 6): string[] => graphRows(walk(spec), max).map((r) => glyphs(r, max));

test("graph/linear — a straight line is a column of dots, and nothing else", () => {
  expect(draw(["c b", "b a", "a"])).toEqual(["●", "●", "●"]);
});

test("graph/merge — the node forks a lane right, the branch runs down it, the lane closes where they meet", () => {
  // m is the merge (◆) and opens lane 1 for its second parent; s runs down lane
  // 1; t takes lane 0; b is where the two meet and closes lane 1.
  expect(draw(["m t s", "s b", "t b", "b a", "a"])).toEqual(["◆╮", "│●", "●│", "●╯", "●"]);
});

test("graph/noparents — a daemon that sends no parents still draws a list", () => {
  // Anything before 0.8. Drawing nothing at all would make an older daemon look
  // like an empty repository.
  expect(draw(["c", "b", "a"])).toEqual(["●", "●", "●"]);
});

test("graph/roots — two disjoint histories in one page", () => {
  expect(draw(["b a", "a", "z"])).toEqual(["●", "●", "●"]);
});

test("graph/octopus — two branches merging into one commit from the same parent", () => {
  expect(draw(["m a b c", "a x", "b x", "c x", "x"])).toEqual(["◆╮╮", "●││", "│●│", "││●", "●╯╯"]);
});

test("graph/overflow — past the limit the lanes collapse into one column", () => {
  // Rather than squeezing the summary off the screen.
  const over = draw(["m a b c d e f", "a z", "b z", "c z", "d z", "e z", "f z", "z"], 3);
  expect(over.length).toBeGreaterThan(0);
  // graph/overflow-clamped: nothing wider than the cap, ellipsis excluded.
  for (const r of over) expect(r.replace(/…+$/, "").length).toBeLessThanOrEqual(3);
});

test("graph/widths — a row is as wide as its own lanes", () => {
  expect([
    rowWidth({ lane: 0, through: [], merged: [], forked: [] }),
    rowWidth({ lane: 0, through: [2], merged: [], forked: [] }),
    rowWidth({ lane: 1, through: [], merged: [], forked: [4] }),
  ]).toEqual([1, 3, 5]);
});

test("graph/colwidth — the column is as wide as the widest row, so the shas line up", () => {
  expect(graphWidth(graphRows(walk(["m t s", "s b", "t b", "b a", "a"]), 6), 6)).toBe(2);
});

test("graph/empty — no commits, no rows", () => {
  expect(draw([])).toEqual([]);
});
