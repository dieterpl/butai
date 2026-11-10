// HELP's keyboard is generated, so it can neither miss a key nor invent one.
//
// Both directions, and both are checked against the tables *directly* rather
// than against `layers()`'s own walk of them: a check generated from the thing
// it checks passes when a key falls out of both at once. So the expectation
// below is rebuilt from `GLOBAL`, `allSurfaces()`, `ARROW_ALIASES` and
// `ALT_MUST_FALL_THROUGH` with nothing of `keyboard.ts` in it but `placeKey`,
// and the keyboard has to equal it — every binding on a cap, and every cap with
// something on it bound.

import { describe, expect, test } from "bun:test";
import {
  ALT_MUST_FALL_THROUGH,
  ARROW_ALIASES,
  GLOBAL,
  LAYOUT_ARROWS,
  VerbId,
  allSurfaces,
  altVerb,
  prefixVerb,
} from "../src/logic/verbs.ts";
import { CAPS, capById, capForCode, categoryOf, layerIdFor, layers, placeKey } from "../src/logic/keyboard.ts";

/** `layer|cap|mod|verb` — one fact the keyboard must draw. */
type Fact = string;
const fact = (layer: string, cap: string, mod: string, id: string | null) => `${layer}|${cap}|${mod}|${id ?? "-"}`;

/** Where a table key goes, or a failure naming it. */
function places(key: string) {
  const p = placeKey(key);
  if (!p || !p.length) throw new Error(`the keyboard cannot place the table key ${JSON.stringify(key)}`);
  return p;
}

/** Every fact the tables state, written out from the tables alone. */
function expected(): Set<Fact> {
  const out = new Set<Fact>();
  for (const g of GLOBAL) {
    if (g.alt) for (const p of places(g.alt)) out.add(fact("alt", p.cap, p.mod, g.id));
    if (g.prefix) for (const p of places(g.prefix)) out.add(fact("prefix", p.cap, p.mod, g.id));
  }
  for (const key of ALT_MUST_FALL_THROUGH) for (const p of places(key)) out.add(fact("alt", p.cap, p.mod, null));
  for (const [name, verbs] of allSurfaces()) {
    const layer = layerIdFor(name);
    for (const v of verbs) for (const p of places(v.key)) out.add(fact(layer, p.cap, p.mod, v.id));
    if (name === "LAYOUT") {
      for (const [arrow, key] of Object.entries(LAYOUT_ARROWS)) {
        const v = verbs.find((x) => x.key === key);
        if (v) for (const p of places(arrow)) out.add(fact(layer, p.cap, p.mod, v.id));
      }
    } else {
      for (const [arrow, id] of Object.entries(ARROW_ALIASES)) {
        if (verbs.some((v) => v.id === id)) for (const p of places(arrow)) out.add(fact(layer, p.cap, p.mod, id));
      }
    }
  }
  return out;
}

/** Every fact the keyboard draws. */
function drawn(): Set<Fact> {
  const out = new Set<Fact>();
  for (const l of layers()) for (const b of l.bindings) out.add(fact(l.id, b.cap, b.mod, b.id));
  return out;
}

describe("the HELP keyboard is the tables, laid on keys", () => {
  test("keyboard/every-binding-is-drawn — a key the tables bind and the keyboard lacks", () => {
    const have = drawn();
    expect([...expected()].filter((f) => !have.has(f)).sort()).toEqual([]);
  });

  test("keyboard/nothing-unbound-is-drawn — a key the keyboard shows and no table binds", () => {
    const want = expected();
    expect([...drawn()].filter((f) => !want.has(f)).sort()).toEqual([]);
  });

  test("keyboard/one-layer-per-surface — and the two workbench layers", () => {
    expect(layers().map((l) => l.id)).toEqual(["alt", "prefix", ...allSurfaces().map(([n]) => layerIdFor(n))]);
  });

  test("keyboard/every-table-key-has-a-cap — including chords and ranges", () => {
    const keys = new Set<string>();
    for (const g of GLOBAL) for (const k of [g.alt, g.prefix]) if (k) keys.add(k);
    for (const [, verbs] of allSurfaces()) for (const v of verbs) keys.add(v.key);
    for (const k of [...ALT_MUST_FALL_THROUGH, ...Object.keys(ARROW_ALIASES)]) keys.add(k);
    expect([...keys].filter((k) => !placeKey(k)?.every((p) => capById(p.cap))).sort()).toEqual([]);
  });

  test("keyboard/the-lookups-agree — what a cap says is what the dispatcher would run", () => {
    // `altVerb` and `prefixVerb` are what the shell calls with a key press; the
    // cap must name the verb that press would reach.
    const [alt, prefix] = layers();
    const wrong: string[] = [];
    for (const b of alt!.bindings) {
      if (b.source !== "table") continue;
      const one = b.spelled.slice("alt-".length);
      if (altVerb(one === "space" ? " " : one)?.id !== b.id) wrong.push(b.spelled);
    }
    for (const b of prefix!.bindings) {
      const one = b.spelled.slice("C-b ".length);
      if (prefixVerb(one === "space" ? " " : one)?.id !== b.id) wrong.push(b.spelled);
    }
    expect(wrong).toEqual([]);
  });

  test("keyboard/fall-through-is-unbound — the program's caps are keys alt does not take", () => {
    const alt = layers()[0]!;
    const caps = alt.bindings.filter((b) => b.source === "fallthrough").map((b) => b.key);
    expect(caps.filter((k) => altVerb(k) !== null)).toEqual([]);
    expect([...new Set(caps)].sort()).toEqual([...ALT_MUST_FALL_THROUGH].sort());
  });

  test("keyboard/no-cap-means-two-things-on-the-workbench — one verb per key per layer", () => {
    // A surface may answer one key two ways (CHANGES' `d` is diff on a file and
    // show on a commit); the workbench layers may not, because nothing there
    // depends on a row.
    for (const l of layers().slice(0, 2)) {
      const by = new Map<string, Set<string>>();
      for (const b of l.bindings) {
        const k = b.cap + "/" + b.mod;
        by.set(k, (by.get(k) ?? new Set()).add(String(b.id)));
      }
      expect([...by].filter(([, ids]) => ids.size > 1).map(([k]) => `${l.id} ${k}`)).toEqual([]);
    }
  });

  test("keyboard/placement — shifted letters, shifted symbols, chords, ranges", () => {
    expect([placeKey("a"), placeKey("A"), placeKey("?"), placeKey("<"), placeKey("+"), placeKey(" "), placeKey("C-s")])
      .toEqual([
        [{ cap: "a", mod: "", one: "a" }],
        [{ cap: "a", mod: "shift", one: "A" }],
        [{ cap: "/", mod: "shift", one: "?" }],
        [{ cap: ",", mod: "shift", one: "<" }],
        [{ cap: "=", mod: "shift", one: "+" }],
        [{ cap: "space", mod: "", one: " " }],
        [{ cap: "s", mod: "ctrl", one: "s" }],
      ]);
    expect(placeKey("1-9")?.map((p) => p.cap)).toEqual(["1", "2", "3", "4", "5", "6", "7", "8", "9"]);
    expect(placeKey("hyper")).toBeNull();
  });

  test("keyboard/range-labels — `project 1-9` on the 3 key reads `project 3`", () => {
    const alt = layers()[0]!;
    expect(alt.bindings.find((b) => b.cap === "3")?.label).toBe("project 3");
  });

  test("keyboard/prefix-follows-the-setting — C-a is lit as the prefix, and spelled", () => {
    const prefix = layers("C-a")[1]!;
    expect(prefix.prefixCap).toBe("a");
    expect(prefix.bindings.find((b) => b.id === VerbId.Help)?.spelled).toBe("C-a ?");
  });

  test("keyboard/caps-are-unique — one id and one code per physical key", () => {
    expect(new Set(CAPS.map((c) => c.id)).size).toBe(CAPS.length);
    expect(new Set(CAPS.map((c) => c.code)).size).toBe(CAPS.length);
    expect(capForCode("KeyQ")?.id).toBe("q");
    expect(capForCode("Slash")?.id).toBe("/");
  });

  test("keyboard/danger-is-drawn-as-danger — a table's `danger` wins over the verb's category", () => {
    for (const l of layers()) for (const b of l.bindings) {
      if (b.source !== "table" || l.chord) continue;
      const v = allSurfaces().find(([n]) => layerIdFor(n) === l.id)![1].find((x) => x.key === b.key && x.id === b.id)!;
      if (v.danger) expect(categoryOf(v.id, true)).toBe(b.category);
    }
  });
});
