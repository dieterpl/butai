// This client's own configuration, and the DOCS page's model.
//
// Ported from `check.py`'s `check_settings` / `SETTINGS_JS` and `check_docs` /
// `DOCS_JS`. The Python copied `settings.js` and `docs.js` to temp `.mjs` files,
// wrote a probe that imported them and printed JSON, ran node, and asserted on
// the parsed result in Python. All of that existed to get a value out of a
// JavaScript module and into a test — which is what an import is. So the probes
// are gone and the fixtures and their expectations are what survives.
//
// The expectations are lifted digit for digit. They are the accumulated
// knowledge of these two pages and re-deriving them would only re-derive
// today's behaviour, which is the one thing a regression test must not do.
//
// **Both groups are invisible to a live run, and they have to be.** Settings is
// the one surface with no wire under it at all — `Page::Settings`: "the daemon
// renders no chrome, so it has no palette and no keymap to hold" — so a palette
// missing a role, or a row naming the wrong storage key, renders a perfectly
// plausible screen and nothing anywhere reports a problem. DOCS is the Files
// page filtered to markdown, so the filter *is* the page: a filter that is
// slightly wrong renders a plausible tree with somebody's notes missing from
// it.

import { describe, expect, test } from "bun:test";
import {
  DEFAULTS,
  GroupId,
  KEY_W,
  LABEL_W,
  ROLES,
  RowId,
  RowKind,
  THEMES,
  VARS,
  clampCursor,
  groups,
  load,
  readPrefixSpelling,
  resolveTheme,
  stepSize,
  termColors,
  themeByName,
  themeNames,
  type Colors,
  type Facts,
  type SettingsStorage,
} from "../src/logic/settings.ts";
import {
  HELP_TOPIC,
  REFERENCE_DIR,
  docsRows,
  inline,
  isBuiltin,
  parentOf,
  readMarkdown,
  rendersAsMarkdown,
  topicFor,
  topics,
} from "../src/logic/docs.ts";
import { reference } from "../src/logic/verbs.ts";

// ===========================================================================
// settings
// ===========================================================================

// `key` is what the bridge's `/api/daemons` sends and what the fleet keys on;
// this module never reads it, and the fixture keeps it because that is the
// record the page is handed. Bound to a name first so it is not a fresh object
// literal — the excess property is the point.
const daemons = [
  { key: "a", label: "a", primary: true, socket: "/tmp/a.sock", error: null },
  { key: "b", label: "b", primary: false, socket: "/tmp/b.sock", error: "gone" },
];

const facts: Facts = {
  agents: ["claude", "codex"],
  daemons,
  prefix: "C-b",
  bindings: 30,
  fallThrough: ["b", "f", "d", "backspace"],
  clientVersion: "0.8.0",
  daemonVersion: "butai 0.8.0",
  origin: "http://localhost:8181",
};

const store = (v: string | null): SettingsStorage => ({ getItem: () => v, setItem: () => {} });

const grps = groups({}, facts);

// -- the default is the client this stage started from ----------------------
//
// `theme: "system"` is not a fallback, it is the instruction to keep following
// the OS — which is what `index.html` has always done. Anything else here would
// make an untouched browser look different after this stage, and that is a bug
// rather than a feature.
describe("the defaults", () => {
  test("settings/default-is-system", () => {
    // `system` means *follow the OS*, which is what index.html's
    // prefers-color-scheme block already did — so an untouched browser must
    // draw exactly what it drew before this page existed.
    expect(DEFAULTS.theme).toBe("system");
  });

  test("settings/defaults-are-absences", () => {
    // Each of these has to default to the *absence* of a setting (0 and ""),
    // because the CSS keeps its own minmax() and a named agent could be an
    // agent somebody is actually called.
    expect([DEFAULTS.leftRail, DEFAULTS.rightRail, DEFAULTS.defaultAgent]).toEqual([0, 0, ""]);
  });

  test("settings/system-is-first", () => {
    // `system` is the default and belongs first; it is also the only entry that
    // is not a palette, so it cannot be buried among them.
    expect(themeNames()[0]).toBe("system");
    expect(themeNames().length).toBeGreaterThan(1);
  });
});

// -- every palette carries every role ---------------------------------------
describe("the palettes", () => {
  for (const t of THEMES) {
    test(`settings/palette/${t.name}`, () => {
      // A role with no colour leaves its variable unset and the *previous*
      // palette's colour showing through, which looks deliberate.
      const missing = ROLES.filter((r) => !t.colors[r]);
      const malformed = ROLES.filter(
        (r) => !/^#[0-9a-f]{6}([0-9a-f]{2})?$/.test(String(t.colors[r] || "")),
      );
      const extra = Object.keys(t.colors).filter((r) => !(ROLES as readonly string[]).includes(r));
      expect([missing, malformed, extra]).toEqual([[], [], []]);
    });

    test(`settings/palette-scheme/${t.name}`, () => {
      // The browser paints scrollbars and form controls from colour-scheme and
      // reads no custom property.
      expect(["dark", "light"]).toContain(t.scheme);
    });
  }
});

// -- the two web palettes are index.html's, to the digit ---------------------
//
// This is the one that keeps `web-dark` honest. Pinning it must mean "stop
// following the OS", not "change colour" — so the values here and the values in
// the stylesheet have to be the same values, and nothing else would ever notice
// them drifting.
//
// **The stylesheet is the fixture now.** `check.py` read `web/index.html` off
// disk and compared the two live sources; this app has no index.html of its own
// yet, and the one it was comparing against belongs to the client being
// deleted. So the two `:root` blocks are lifted here verbatim, parsed with the
// same regex, and the assertion is unchanged — see HANDOVER-settings-docs.md.
const INDEX_HTML_DARK = `
    --bg:#0b0e13; --panel:#161b22; --panel2:#1b212b; --line:#262d38;
    --fg:#d7dde5; --dim:#8b949e; --faint:#6e7681; --accent:#58a6ff; --sel:#1f6feb22;
    --ok:#3fb950; --warn:#d29922; --bad:#f85149; --run:#58a6ff;
    --on-accent:#04070d; --focus:#1f6feb44;
    --status-bg:#161b22; --status-fg:#8b949e;
    --term-bg:#0e1116; --term-fg:#d7dde5;
    color-scheme: dark;
`;

const INDEX_HTML_LIGHT = `
      --bg:#ffffff; --panel:#f6f8fa; --panel2:#eaeef2; --line:#d0d7de;
      --fg:#1f2328; --dim:#656d76; --faint:#8c959f; --accent:#0969da; --sel:#0969da1a;
      --ok:#1a7f37; --warn:#9a6700; --bad:#cf222e; --run:#0969da;
      --on-accent:#ffffff; --focus:#0969da44;
      --status-bg:#f6f8fa; --status-fg:#656d76;
      --term-bg:#ffffff; --term-fg:#1f2328;
      color-scheme: light;
`;

const declared = (block: string): Record<string, string> => {
  const out: Record<string, string> = {};
  for (const m of block.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) out[m[1]!] = m[2]!.trim().toLowerCase();
  return out;
};

/// `[role, var, want, have]` for every role whose colour and whose stylesheet
/// declaration disagree. A var the stylesheet does not declare is not drift.
const drift = (colors: Colors, css: Record<string, string>): string[][] => {
  const bad: string[][] = [];
  for (const role of ROLES) {
    const want = String(colors[role] || "").toLowerCase();
    const have = css[VARS[role]];
    if (have !== undefined && want !== have) bad.push([role, VARS[role], want, have]);
  }
  return bad;
};

describe("the palettes against the stylesheet", () => {
  test("settings/matches-index-html/web-dark", () => {
    expect(drift(themeByName("web-dark")!.colors, declared(INDEX_HTML_DARK))).toEqual([]);
  });

  test("settings/matches-index-html/web-light", () => {
    expect(drift(themeByName("web-light")!.colors, declared(INDEX_HTML_LIGHT))).toEqual([]);
  });

  test("settings/system-resolves", () => {
    // `system` has to resolve to one of the two palettes this client shipped
    // with (so the swatches and the preview can draw *something* for it), an
    // unknown name has to fall back rather than throw, and a real name has to
    // win over the OS.
    expect([
      resolveTheme("system", true).name,
      resolveTheme("system", false).name,
      resolveTheme("nonsense", true).name,
      resolveTheme("gruvbox-dark", false).name,
    ]).toEqual(["web-dark", "web-light", "web-dark", "gruvbox-dark"]);
  });

  test("settings/term-colours", () => {
    // The two web palettes keep the slightly darker terminal ground this client
    // has always drawn, and every butai palette resolves a pane's `default` to
    // its own ground and ink.
    expect([termColors(themeByName("web-dark")!), termColors(themeByName("blueprint-dark")!)]).toEqual([
      { bg: "#0e1116", fg: "#d7dde5" },
      { bg: "#151a23", fg: "#dde4ef" },
    ]);
  });
});

// -- the store clamps, because a stored setting is user input ----------------
//
// Storage can hold anything — it survives a version, it can be edited by hand,
// and Safari's private mode *throws* on read — and a client that will not boot
// because it could not read a colour preference is worse than one that draws
// the default.
describe("the store", () => {
  test("settings/load/empty", () => {
    expect(load(store(null))).toEqual(DEFAULTS);
  });

  test("settings/load/garbage", () => {
    expect(load(store("{{{"))).toEqual(DEFAULTS);
  });

  test("settings/load/notObject", () => {
    expect(load(store("7"))).toEqual(DEFAULTS);
  });

  test("settings/load/throws", () => {
    // Safari's private mode.
    const throws = { getItem() { throw new Error("private mode"); } } as unknown as SettingsStorage;
    expect(load(throws)).toEqual(DEFAULTS);
  });

  test("settings/load/clamped", () => {
    // Every value has to be clamped to something this client can draw. One bad
    // number is a rail 99999px wide with the settings page that would fix it
    // pushed off the screen.
    const absurd = load(store(JSON.stringify({
      theme: "no-such-theme", fontPx: 9000, leftRail: 99999, rightRail: 3, zen: "yes",
      defaultAgent: 12,
    })));
    expect(absurd).toEqual({
      theme: "system", fontPx: 40, leftRail: 640, rightRail: 180, zen: true, defaultAgent: "",
    });
  });

  test("settings/load/partial", () => {
    // A stored object naming only one setting must keep it and default the rest.
    expect(load(store(JSON.stringify({ theme: "gruvbox-dark" }))).theme).toBe("gruvbox-dark");
  });

  test("settings/prefix-spelling", () => {
    // An offered prefix is kept, one that is not offered falls back to C-b, and
    // so does nothing at all. A prefix you cannot press is a workbench you
    // cannot reach from a program.
    expect([
      readPrefixSpelling(store("C-a")),
      readPrefixSpelling(store("C-q")),
      readPrefixSpelling(store(null)),
      readPrefixSpelling(store("  C-x  ")),
    ]).toEqual(["C-a", "C-b", "C-b", "C-x"]);
  });
});

// -- the rows ---------------------------------------------------------------
describe("the rows", () => {
  test("settings/groups", () => {
    // They are the terminal's six, in its order. `Page::Settings` calls this
    // "seven groups of them" as the argument for a page over a modal, and a
    // page that quietly became three groups is a page that should have been a
    // modal.
    expect(grps.map((g) => g.label)).toEqual([
      "APPEARANCE", "AGENTS", "WORKBENCH", "MACHINES", "KEYS", "ABOUT",
    ]);
  });

  for (const g of grps) {
    test(`settings/group-has-rows/${g.label}`, () => {
      expect(g.rows.length).toBeGreaterThan(0);
    });

    for (const r of g.rows) {
      // Every row names the key it writes, drawn beside it: a settings page
      // that invents its own vocabulary for a store people already own leaves
      // them with two things to learn and no way to map a row onto the line
      // they would edit by hand.
      test(`settings/row/${g.label}/${r.label}`, () => {
        const problems: string[] = [];
        if (!r.desc.endsWith(".")) problems.push("its description is not a sentence");
        if (r.desc.length > 95) problems.push(`its description is ${r.desc.length} columns`);
        if (r.label.length > LABEL_W) problems.push(`its label is ${r.label.length} columns`);
        if (r.key.length > KEY_W) problems.push(`the key it writes is ${r.key.length} columns`);
        if (r.kind !== RowKind.Info && !r.key) problems.push("it changes something and names no key");
        expect(problems).toEqual([]);
      });
    }
  }

  test("settings/keys-are-storage", () => {
    // Every editable row's key is a real storage spelling — one you could type
    // into a console — rather than a label with a dot in it. The terminal's
    // page prints `[theme] name` because that is the line you would edit by
    // hand; the browser's equivalent is the localStorage key.
    const keyed = grps.flatMap((g) => g.rows.filter((r) => r.kind !== RowKind.Info).map((r) => [g.label, r.label, r.key]));
    expect(keyed.filter((k) => !(k[2]!.startsWith("butai.settings · ") || k[2] === "butai.prefix"))).toEqual([]);
  });

  test("settings/theme-options", () => {
    // Every palette, with `system` first.
    const options = grps[0]!.rows[0]!.options || [];
    expect(options[0]).toBe("system");
    expect(options).toContain("gruvbox-dark");
  });

  test("settings/agent-options", () => {
    // The way back to being asked is first, because unpinning is the question a
    // pin actually raises, and it is a *label* rather than a value so it cannot
    // collide with an agent somebody is called.
    expect(grps[1]!.rows[0]!.options).toEqual(["ask every time", "claude", "codex"]);
  });

  test("settings/machines-are-facts", () => {
    // It lists what the bridge dialled, marks the one that is not answering,
    // and says where the list comes from. Nothing here is editable and that is
    // the honest shape: the bridge reads its daemon list from BUTAI_SOCKETS at
    // startup and has no route that accepts another, so a browser holding its
    // own would be holding sockets it cannot open.
    const machines = grps.find((g) => g.id === GroupId.Machines)!.rows.map((r) => r.value);
    expect(machines.length).toBe(3);
    expect(machines.some((m) => m.includes("unreachable"))).toBe(true);
    expect(machines.some((m) => m.includes("restart the bridge"))).toBe(true);
  });

  test("settings/rows-without-facts", () => {
    // The page has to draw before /api/daemons and /api/agents have answered,
    // and a group with no rows is a group that reads as broken.
    const counts = groups(null, {}).map((g) => g.rows.length);
    expect(counts.every((n) => n > 0)).toBe(true);
    expect(counts.length).toBe(6);
  });
});

// -- the clamps -------------------------------------------------------------
describe("the clamps", () => {
  test("settings/step-clamps", () => {
    // `auto` steps into the middle of the range rather than to the floor, so
    // `+` on an automatic rail does not first make it narrower than it was; and
    // every step is clamped, so a rail cannot be typed into a state it could
    // not be dragged into.
    //
    // The reduces ignore the element and fold the accumulator: five `+` presses
    // from `auto`, ten `-` presses from 200, and so on.
    expect({
      railUp: [0, 1, 2, 3, 4].reduce((v) => stepSize(RowId.LeftRail, v, 1), 0),
      railDownFromAuto: stepSize(RowId.LeftRail, 0, -1),
      railFloor: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10].reduce((v) => stepSize(RowId.LeftRail, v, -1), 200),
      railCeil: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10].reduce((v) => stepSize(RowId.LeftRail, v, 1), 600),
      railAuto: stepSize(RowId.LeftRail, 340, 0),
      fontUp: stepSize(RowId.Font, 15, 1),
      fontFloor: [1, 2, 3].reduce((v) => stepSize(RowId.Font, v, -1), 9),
      fontCeil: [1, 2, 3].reduce((v) => stepSize(RowId.Font, v, 1), 39),
      fontAuto: stepSize(RowId.Font, 30, 0),
    }).toEqual({
      railUp: 400, railDownFromAuto: 260, railFloor: 180, railCeil: 640, railAuto: 0,
      fontUp: 16, fontFloor: 8, fontCeil: 40, fontAuto: 15,
    });
  });

  test("settings/cursor-clamps", () => {
    // The groups gain and lose rows as machines come and go, so a cursor past
    // the end has to land on the last row rather than index nothing.
    expect([clampCursor(grps, 99, 99), clampCursor(grps, -3, -3)]).toEqual([
      { group: 5, row: 2 },
      { group: 0, row: 0 },
    ]);
  });
});

// ===========================================================================
// docs
// ===========================================================================

// `size` is on `TreeEntry` and the probe's fixture predates it; nothing in
// `docsRows` reads it, so it is 0 here and carries no assertion.
const ent = (name: string, isDir = false) => ({ name, is_dir: isDir, path: name, changed: false, size: 0 });

// What `GET .../tree?filter=docs` answers: already filtered, because the
// daemon decides the rows and their `changed` markers together. This fixture
// used to carry `main.rs`, `target/` and the rest, back when `docsRows` did the
// filtering — see the pass-through test below for why it no longer does.
const listing = [
  ent("README.md"), ent("readme"), ent("NOTES.markdown"), ent("docs", true),
  ent("src", true),
];

const sub = docsRows(listing, "docs", false).map((r) => r.name);

describe("the filter", () => {
  test("docs/rows-are-not-filtered-here", () => {
    // The rule itself is `is_doc` in the protocol crate and runs in the daemon,
    // because the `\u25cf` markers are decided there: a filter applied to the rows
    // afterwards left directories marked for files it had just dropped, and
    // following one down the rail landed on an empty listing every time.
    //
    // So this function must pass a `.rs` straight through. If a filter ever
    // creeps back in here, the two rules can disagree again and this fails.
    const raw = [ent("main.rs"), ent("target", true), ent("README.md")];
    expect(docsRows(raw, "docs", true).map((r) => r.name)).toEqual([
      "main.rs", "target", "README.md",
    ]);
  });

  test("docs/parent-of", () => {
    // Up from the root is nowhere, and up from the reference is the root: it is
    // a folder in this rail without being a folder on disk, so splitting its
    // sentinel on the last slash would land in the middle of a URL scheme.
    expect([parentOf(""), parentOf("a"), parentOf("a/b/c"), parentOf(REFERENCE_DIR)]).toEqual([
      null, "", "a/b", "",
    ]);
  });

  test("docs/root", () => {
    // The reference first, then the project's own writing with its code
    // filtered out, and no `..` because there is nowhere above it.
    expect(docsRows(listing, "", false).map((r) => r.name)).toEqual([
      "reference", "README.md", "readme", "NOTES.markdown", "docs", "src",
    ]);
  });

  test("docs/nested-has-no-dotdot", () => {
    // This client draws an expanding tree, where a child sits under its own
    // parent, so a `..` row there would point at the folder two lines above it.
    // Every other rule is the same either way, which is why this is a flag and
    // not a second function.
    expect(docsRows(listing, "docs", true).map((r) => r.name)).toEqual(sub.filter((r) => r !== ".."));
  });

  test("docs/reference-is-root-only", () => {
    // The reference belongs at the root and nowhere else, and every other
    // directory offers the way back up.
    expect(sub[0]).toBe("..");
    expect(sub).not.toContain("reference");
  });
});

// -- the reference: one topic per section, generated -------------------------
describe("the reference", () => {
  const sections = reference().map((s) => s.title);
  const tops = topics();

  test("docs/one-topic-per-section", () => {
    // `docs/keys.md` says the in-app reference lives on this page, and it is
    // generated from the verb tables — so a section with no topic is a surface
    // whose keys are documented nowhere the user can reach.
    expect(tops.length).toBe(sections.length);
    expect(tops.length).toBeGreaterThan(5);
  });

  test("docs/reference-listing", () => {
    // Inside it there is nothing on disk, so the topics *are* the listing
    // rather than an addition to one.
    expect(docsRows([], REFERENCE_DIR, false).map((r) => r.name)).toEqual([".."].concat(tops.map((t) => t.name)));
  });

  test("docs/topic-shape", () => {
    // The path is a scheme, because no path in a workspace can contain `://`
    // and this must not collide with a real directory; the title carries
    // `reference/` so it says which of the two things in this rail you are
    // looking at, and `.md` because it is markdown.
    expect(tops.filter((t) => !(t.path.startsWith("butai://") && t.title.startsWith("reference/")
      && t.name.endsWith(".md")))).toEqual([]);
  });

  test("docs/help-lands-somewhere", () => {
    // The terminal's `ViewVerb::Help` sets the page and then
    // `Flow::Reference(HELP_TOPIC)`; a landing page that does not resolve is a
    // help key that opens an empty document.
    expect(HELP_TOPIC).toBe("butai://keys");
    expect(topicFor(HELP_TOPIC)).toBeTruthy();
    expect(topicFor(HELP_TOPIC)!.body.split("\n")[0]).toBe("# The two layers");
  });

  test("docs/prefix-is-not-hard-coded", () => {
    // The prefix is a setting, and the reference is the one place that must not
    // print a key the user does not have.
    expect(topicFor(HELP_TOPIC, "C-a")!.body.indexOf("C-a o")).toBeGreaterThan(0);
  });

  test("docs/unknown-topic", () => {
    // A sentinel that is not a topic has to be nothing, or an unknown one opens
    // as a blank page rather than being fetched.
    expect(topicFor("butai://nope")).toBe(null);
    expect(topicFor("docs/design.md")).toBe(null);
  });

  test("docs/builtin-and-rendered", () => {
    // Everything that would write to disk asks the first (a reference page has
    // no path, and "saved" is the worst possible answer), and the second is
    // what decides a rendering from a printing.
    expect([isBuiltin("butai://keys"), isBuiltin("docs/x.md")]).toEqual([true, false]);
    expect([
      rendersAsMarkdown("a/README"), rendersAsMarkdown("a/x.md"),
      rendersAsMarkdown("a/x.rs"), rendersAsMarkdown("butai://keys"),
    ]).toEqual([true, true, false, true]);
  });
});

// -- the markdown reader -----------------------------------------------------
const md = `# Title

A paragraph with \`code\`, **bold**, *em* and [a link](http://example.com).

- one
- two

> quoted

\`\`\`sh
echo hello
\`\`\`

| a | b |
|---|---|
| 1 | 2 |

---
`;

describe("the markdown reader", () => {
  const blocks = readMarkdown(md);

  test("docs/markdown-blocks", () => {
    // A project's own writing is what this has to render.
    expect(blocks.map((b) => b.kind)).toEqual(["h", "p", "ul", "quote", "code", "table", "rule"]);
  });

  test("docs/markdown-shapes", () => {
    // The `|---|` separator is not a row, it is what says the row above it was
    // a header.
    expect(blocks.flatMap((b) => (b.kind === "code" ? [[b.lang, b.text]] : []))).toEqual([["sh", "echo hello"]]);
    expect(blocks.flatMap((b) => (b.kind === "table" ? [b.rows.length] : []))).toEqual([2]);
    expect(blocks.flatMap((b) => (b.kind === "ul" ? [b.items.length] : []))).toEqual([2]);
  });

  test("docs/markdown-inline", () => {
    // Code, emphasis and links, as *data*. That is the whole reason this
    // returns blocks rather than HTML: a renderer that returned a string would
    // put an innerHTML in the page, and the daemon hands us other people's
    // files.
    const para = blocks[1];
    expect(para && "spans" in para).toBe(true);
    const spans = para && "spans" in para ? para.spans : [];
    expect(spans.flatMap((s) => (["code", "strong", "em", "href"] as const).filter((k) => !!s[k])))
      .toEqual(["code", "strong", "em", "href"]);
  });

  test("docs/markdown-indented", () => {
    // Four spaces is how a key table is written in prose, and reflowing one
    // into a paragraph loses the alignment that made it a table.
    expect(readMarkdown(["intro", "", "    alt-o   files", "    alt-m   docs", ""].join("\n"))
      .map((b) => b.kind)).toEqual(["p", "code"]);
  });

  test("docs/markdown-empty", () => {
    // An empty file is an empty document, not a crash.
    expect([readMarkdown("").length, readMarkdown(null).length, inline("").length]).toEqual([0, 0, 1]);
  });
});
