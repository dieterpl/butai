// The verb tables: what every surface binds, how it packs into a footer, and
// what a click can reach.
//
// Ported from `check.py`'s `check_verbs` / `VERBS_JS` and `check_git_menu` /
// `GITMENU_JS`. The Python copied `verbs.js` and `git-menu.js` to temp `.mjs`
// files, wrote a probe that imported them and printed JSON, ran node, and
// asserted on the parsed result in Python. All of that existed to get a value
// out of a JavaScript module and into a test — which is what an import is. So
// the probes are gone and the fixtures and their expectations are what survives.
//
// The expectations are lifted digit for digit. `t new · r restart · x kill` is
// 26 columns in 26; the CHANGES rail's lines are what it drew at `dd615a5`; the
// three rows that ask first are the three that destroy work you cannot get
// back. Re-deriving any of them from today's tables would only re-derive today's
// behaviour, which is the one thing a regression test must not do.
//
// This is the stage-6 half of "nothing is reachable by pointer alone, and
// nothing is bound that cannot be found". The TUI gets the first half from a
// `match` over `hit::Target` with no catch-all — a new clickable thing does not
// compile until someone says which key reaches it. JavaScript has no
// exhaustiveness, so the equivalent is a registry every click site must be
// declared in, and this reads it: the registry entry names a verb some table
// really binds.

import { describe, expect, test } from "bun:test";
import {
  ALT_MUST_FALL_THROUGH,
  ChangesRow,
  DockerRow,
  GLOBAL,
  GitRow,
  OverlayKind,
  SettingRow,
  TARGETS,
  agentsVerbs,
  allSurfaces,
  altKeyName,
  altVerb,
  changesFooter,
  click,
  dockerVerbs,
  gitFooter,
  homeVerbs,
  isPrefix,
  keyName,
  keyText,
  layout,
  lines,
  overlayVerbs,
  prefixVerb,
  procsVerbs,
  reference,
  settingsVerbs,
  targetKeys,
  type Verb,
} from "../src/logic/verbs.ts";
import {
  GROUPS,
  GitAction,
  ITEMS,
  groupsFor,
  itemsFor,
  needsConfirm,
  type GitActionId,
} from "../src/logic/git-menu.ts";

// 26 = LEFT_W - 2, 36 = RIGHT_W - 2, from crates/butai-client/src/chrome/model.rs.
const RAIL = 26;
const CHANGES_W = 36;
// 50 = git_list_width's upper clamp (52) minus the box border, and 2 rows, which
// is what `git_split` gives a footer. SETTINGS's body gives one the same 50, and
// two rows is what the size arm needs — the only arm that does not fit on one.
const PAGE_W = 50;

const footer = (verbs: readonly Verb[], w: number, rows: number): string[] => lines(verbs, w, rows);

/** A key bound twice on one surface, which leaves one of them unreachable. */
const dupes = (verbs: readonly Verb[]): string[] => {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const x of verbs) {
    if (seen.has(x.key)) out.push(x.key);
    seen.add(x.key);
  }
  return out;
};

/** `[drawn, offered]` — how many footer verbs earned a column, and how many
 *  asked for one. The one that falls off keeps working and loses the only place
 *  its key is written down. */
const drawn = (verbs: readonly Verb[], w: number, rows: number): [number, number] => [
  layout(verbs, w, rows).length,
  verbs.filter((x) => x.footer).length,
];

// ---------------------------------------------------------------------------
// The lifted fixtures
// ---------------------------------------------------------------------------

// The SETTINGS footer, per row kind, at the width its body gives one.
//
// `settings.rs`'s `verbs()` is the source: `j/k move` always, then the row's own
// verb, then `tab group` and `esc close` — except while a choice is open, where
// the arm returns early and `esc` means "keep the old one" instead.
const SETTINGS_FOOTERS: Record<SettingRow, string[]> = {
  Choice: ["enter change · tab group · esc close", ""],
  Open: ["enter choose · esc keep the old one", ""],
  Toggle: ["space toggle · tab group · esc close", ""],
  Size: ["- smaller · + bigger · 0 auto · tab group", "esc close"],
  Info: ["tab group · esc close", ""],
  None: ["tab group · esc close", ""],
};

// The AGENTS rail's two arms. Both are 26 columns or fewer, which is the
// terminal's rail width, and the pinned one is the terminal's line exactly.
const AGENTS_FOOTERS: Record<"pinned" | "unpinned", string[]> = {
  unpinned: ["a new... · c seen · x kill"],
  pinned: ["a new · A new... · x kill"],
};

const CHANGES_FOOTERS: Record<string, string[]> = {
  Unstaged: ["s stage · d diff · p push · c commit", "b branch", ""],
  Staged: ["u unstage · d diff · p push", "c commit · b branch", ""],
  Conflict: ["o ours · t theirs · a resolved", "p push · c commit · b branch", ""],
  Commit: ["d show · p push · c commit", "b branch", ""],
  Sequence: ["s stage · d diff · y continue", "n abort · c commit · b branch", ""],
};

// The GIT page's footers, per row kind, in 50 columns and 2 rows. The row verbs
// are `git_row_verbs`'s, verb for verb; the tail is `git_always_verbs`.
const GIT_FOOTERS: Record<GitRow, string[]> = {
  WorkingTree: ["enter changes · g git · r refresh", ""],
  Branch: ["enter scope · c checkout · m merge · d delete", "g git · r refresh"],
  CurrentBranch: ["enter scope · g git · r refresh", ""],
  BranchElsewhere: ["enter scope · m merge · g git · r refresh", ""],
  RemoteBranch: ["enter scope · m merge · g git · r refresh", ""],
  Remote: ["f fetch · g git · r refresh", ""],
  Tag: ["enter scope · x delete · g git · r refresh", ""],
  Stash: ["enter show · p pop · x drop · g git · r refresh", ""],
  Worktree: ["enter open · x remove · g git · r refresh", ""],
  ThisWorktree: ["g git · r refresh", ""],
  Commit: ["enter diff · y sha · v revert · p pick · g git", "r refresh"],
  None: ["g git · r refresh", ""],
};

// The CHANGES rail's own footers, computed once. `ahead: 2` is what puts
// `p push` on the line; the Sequence row is the Unstaged table with a rebase in
// progress.
const changesFooters: Record<string, string[]> = {};
const changesDupes: Record<string, string[]> = {};
for (const row of ["Unstaged", "Staged", "Conflict", "Commit"] as const) {
  const f = changesFooter(ChangesRow[row], { ahead: 2, sequence: false });
  changesFooters[row] = footer(f, CHANGES_W, 3);
  changesDupes[row] = dupes(f);
}
{
  const f = changesFooter(ChangesRow.Unstaged, { ahead: 0, sequence: true });
  changesFooters.Sequence = footer(f, CHANGES_W, 3);
  changesDupes.Sequence = dupes(f);
}

// ---------------------------------------------------------------------------
// The click-target registry
// ---------------------------------------------------------------------------

describe("every click target has a key", () => {
  // Not "has a letter written next to it": the entry names a VerbId, and this
  // resolves that id against the tables. So a key that *moves* fails here,
  // rather than leaving a stale comment behind.
  for (const [name, entry] of Object.entries(TARGETS).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))) {
    test(`verbs/target/${name} — ${entry.where}`, () => {
      const rows = targetKeys(name);
      // A clickable thing whose key does not exist is exactly what this
      // registry is for; the ids listed are the ones that resolve to no key in
      // any table on that target's own surface.
      expect(entry.verbs.filter((_, i) => rows[i] === null)).toEqual([]);
      expect(rows.length).toBeGreaterThan(0);
    });
  }

  test("verbs/click-throws — an undeclared target throws when the element is built", () => {
    // click() is the only constructor for a handler, so it is the place a button
    // with no key has to fail — and it has to fail loudly, at the moment the
    // element is built, or the browser smoke test walks straight past it.
    expect(() => click("no.such.target", () => {})).toThrow(/no such click target/);
  });
});

// ---------------------------------------------------------------------------
// The footers say what is bound
// ---------------------------------------------------------------------------

describe("the footers say what is bound", () => {
  // `t new · r restart · x kill` is 26 columns in 26. A reworded verb silently
  // drops the last one off the footer, which does not break the key — only the
  // one place the key is written down. The TUI has this exact assertion.
  test("verbs/procs-footer — the same line the TUI draws", () => {
    expect(footer(procsVerbs(), RAIL, 1)).toEqual(["t new · r restart · x kill"]);
  });

  // The TUI's fleet grew `x kill` and `m menu`; this client has neither, and the
  // reason is structural rather than a decision: its keys are dispatched by
  // clicking the element a verb names, and a HOME row carries no kill button and
  // no row menu to open. So the footer stays `enter open` until those exist — a
  // verb in the table with nothing to click is the "key for a thing that is not
  // there" this rule has always been about. Closing the gap means adding the
  // buttons first, then the verbs, and moving this assertion.
  test("verbs/home-footer — the fleet navigates and opens, and that is the whole table here", () => {
    expect(footer(homeVerbs(), RAIL, 1)).toEqual(["enter open"]);
  });

  const railSurfaces = [
    ["home", homeVerbs()],
    ["agents", agentsVerbs(false)],
    ["procs", procsVerbs()],
  ] as const;
  for (const [label, verbs] of railSurfaces) {
    test(`verbs/${label}-fits — every footer verb earns a column in 26`, () => {
      // The one that falls off keeps working and loses the only place it is
      // written down.
      const [drew, offered] = drawn(verbs, RAIL, 1);
      expect(drew).toBe(offered);
      expect((footer(verbs, RAIL, 1)[0] ?? "").length).toBeLessThanOrEqual(RAIL);
    });
  }

  for (const row of Object.keys(CHANGES_FOOTERS)) {
    test(`verbs/changes-footer/${row} — no line overflows its 36 columns`, () => {
      expect((changesFooters[row] ?? []).filter((l) => l.length > CHANGES_W)).toEqual([]);
    });
  }
});

describe("the CHANGES rail did not move", () => {
  // Stage 7's first rule is that this rail does not move, shrink or lose a verb,
  // and a rule nothing asserts is a rule the next stage breaks by accident. The
  // lines below are what the rail drew at `dd615a5`.
  for (const [row, want] of Object.entries(CHANGES_FOOTERS)) {
    test(`verbs/changes-rail-verbs/${row} — staging lives on this rail and stays there`, () => {
      // A GIT page that quietly took a verb off it would split muscle memory
      // across two screens.
      expect(changesFooters[row]).toEqual(want);
    });
  }
});

// ---------------------------------------------------------------------------
// The GIT page's footers
// ---------------------------------------------------------------------------

describe("the GIT page's footers", () => {
  for (const row of Object.values(GitRow)) {
    const f = gitFooter(row);

    test(`verbs/git-footer/${row} — the same list \`git_row_verbs\` gives the terminal`, () => {
      expect(footer(f, PAGE_W, 2)).toEqual(GIT_FOOTERS[row]);
    });

    test(`verbs/git-fits/${row} — every verb the row offers is drawn in 2 rows of 50`, () => {
      const [drew, offered] = drawn(f, PAGE_W, 2);
      expect(drew).toBe(offered);
    });

    test(`verbs/one-key-one-meaning/${row} — GIT`, () => {
      expect(dupes(f)).toEqual([]);
    });
  }
});

// ---------------------------------------------------------------------------
// SETTINGS's footer, per row kind
// ---------------------------------------------------------------------------

describe("SETTINGS's footer, per row kind", () => {
  // `settings.rs`'s `verbs()`, one arm at a time. The `Open` arm is the one that
  // matters: while a choice is expanded `esc` means "keep the old one", not
  // "close the page", and a page whose escape key sometimes leaves and sometimes
  // reverts with nothing saying which is the failure the terminal avoids by
  // returning early on that arm.
  for (const row of Object.values(SettingRow)) {
    const f = settingsVerbs(row);

    test(`verbs/settings-footer/${row} — a row that cannot be changed must not advertise Enter`, () => {
      expect(footer(f, PAGE_W, 2)).toEqual(SETTINGS_FOOTERS[row]);
    });

    test(`verbs/settings-fits/${row} — every verb the arm offers is drawn in 2 rows of 50`, () => {
      const [drew, offered] = drawn(f, PAGE_W, 2);
      expect(drew).toBe(offered);
    });

    test(`verbs/one-key-one-meaning/Settings${row} — a key bound twice leaves one unreachable`, () => {
      expect(dupes(f)).toEqual([]);
    });
  }
});

// ---------------------------------------------------------------------------
// The AGENTS rail has two arms, and a setting chooses between them
// ---------------------------------------------------------------------------

describe("the AGENTS rail's two arms", () => {
  // The terminal's `agents_verbs(pinned)`: `a` and `A` are the same verb until
  // one is set, and until then a footer offering both is offering the same thing
  // twice under two names.
  const arms = [
    ["pinned", agentsVerbs(true)],
    ["unpinned", agentsVerbs(false)],
  ] as const;

  for (const [arm, verbs] of arms) {
    test(`verbs/agents-footer/${arm} — what yields to make room is \`c seen\``, () => {
      // Pinned, `a` starts that agent and `A` is the only route to the others,
      // so both are worth a column; `c seen` is the one verb here with no
      // counterpart in the terminal.
      expect(footer(verbs, RAIL, 1)).toEqual(AGENTS_FOOTERS[arm]);
    });

    test(`verbs/agents-arm-fits/${arm} — 26 is the terminal's rail`, () => {
      const [drew, offered] = drawn(verbs, RAIL, 1);
      expect(drew).toBe(offered);
      expect((footer(verbs, RAIL, 1)[0] ?? "").length).toBeLessThanOrEqual(RAIL);
    });

    test(`verbs/one-key-one-meaning/Agents${arm} — the arm binds nothing twice`, () => {
      expect(dupes(verbs)).toEqual([]);
    });
  }
});

// ---------------------------------------------------------------------------
// One key, one meaning, on one surface
// ---------------------------------------------------------------------------

describe("one key, one meaning, on one surface", () => {
  test("verbs/one-key-one-meaning/HOME — the fleet binds nothing twice", () => {
    expect(dupes(homeVerbs())).toEqual([]);
  });

  for (const [row, dup] of Object.entries(changesDupes)) {
    test(`verbs/one-key-one-meaning/${row} — CHANGES`, () => {
      expect(dup).toEqual([]);
    });
  }

  for (const row of ["Stack", "Container"] as const) {
    test(`verbs/one-key-one-meaning/${row} — DOCKER`, () => {
      expect(dupes(dockerVerbs(DockerRow[row]))).toEqual([]);
    });
  }

  for (const kind of ["Ask", "Picker", "List"] as const) {
    test(`verbs/one-key-one-meaning/${kind} — an overlay`, () => {
      expect(dupes(overlayVerbs(OverlayKind[kind]))).toEqual([]);
    });
  }
});

// ---------------------------------------------------------------------------
// The Alt layer's promise about what it does NOT bind
// ---------------------------------------------------------------------------

describe("the Alt layer's promise about what it does not bind", () => {
  // `docs/keys.md`: "An Alt key the workbench does not bind falls through, so
  // `alt-b` and `alt-f` still move by words in readline." Nothing else would
  // ever notice that promise being broken — the key would simply start doing
  // something, and the shell would stop.
  for (const key of ALT_MUST_FALL_THROUGH) {
    test(`verbs/falls-through/${key} — it must reach the program`, () => {
      // readline's word motions and the browser's own navigation live there, and
      // a client that swallows them has taken keys out of every shell on the
      // stage.
      expect(altVerb(key)).toBeNull();
    });
  }
});

// ---------------------------------------------------------------------------
// The two spellings
// ---------------------------------------------------------------------------

describe("the two spellings", () => {
  // Stronger than the `claim` rule below, and for a sharper reason: a verb you
  // cannot reach is a verb, and a page you cannot reach is a page. The Alt layer
  // is the one something above us may take — the browser, the OS, a terminal in
  // between — and on a browser that takes it the prefix is the only way in.
  // Every Space verb has both spellings today; this is what says so when the
  // next page is added with one.
  for (const g of GLOBAL) {
    if (!g.id.startsWith("Space")) continue;
    test(`verbs/space-has-a-prefix/${g.id} — a page reachable only through Alt is a page you may not reach`, () => {
      expect(g.prefix ?? null).not.toBeNull();
    });
  }

  for (const g of GLOBAL) {
    if (!g.claim) continue;
    test(`verbs/contested-has-a-prefix/${g.id} — something above us may take alt-${g.alt}`, () => {
      // On a browser that claims the key there is otherwise no way in at all.
      expect(g.prefix ?? null).not.toBeNull();
    });
  }

  test("verbs/key-names — including the two cases a terminal cannot recover", () => {
    // The second is a Mac's Option-o, which arrives as ø; the third is Option-n,
    // a *dead* key that emits nothing until the next keystroke. `docs/keys.md`
    // says a terminal cannot get those back, and a browser can, because e.code
    // still says which key it was.
    expect([
      altKeyName({ key: "o", code: "KeyO" }),
      altKeyName({ key: "ø", code: "KeyO" }),
      altKeyName({ key: "Dead", code: "KeyN" }),
      altKeyName({ key: ">", code: "Period", shiftKey: true }),
      altKeyName({ key: "Dead", code: "Period", shiftKey: true }),
      altKeyName({ key: "¡", code: "Digit1" }),
      keyName({ key: "Enter" }),
      keyName({ key: "Escape" }),
      keyName({ key: "Tab" }),
    ]).toEqual(["o", "o", "n", ">", ">", "1", "enter", "esc", "tab"]);
  });

  test("verbs/prefix — C-b is the prefix, C-M-b is not, a bare b is not, and a configured C-a is", () => {
    // C-M-b belongs to the Alt layer.
    expect([
      isPrefix({ ctrlKey: true, key: "b" }, null),
      isPrefix({ ctrlKey: true, key: "b", altKey: true }, null),
      isPrefix({ ctrlKey: false, key: "b" }, null),
      isPrefix({ ctrlKey: true, key: "a" }, { ctrl: true, key: "a" }),
    ]).toEqual([true, false, false, true]);
  });

  test("verbs/alt-lookup — o is the files space, any digit is the one project verb, b is unbound", () => {
    // b must stay unbound, and esc leaves the stage.
    expect([
      altVerb("o")?.id ?? null,
      altVerb("3")?.id ?? null,
      altVerb("b")?.id ?? null,
      altVerb("esc")?.id ?? null,
    ]).toEqual(["SpaceFiles", "Workspace", null, "FocusOff"]);
  });

  test("verbs/prefix-lookup — the prefix layer resolves the same verbs by their other spelling", () => {
    expect([
      prefixVerb("A")?.id ?? null,
      prefixVerb("7")?.id ?? null,
      prefixVerb("?")?.id ?? null,
      prefixVerb("q")?.id ?? null,
    ]).toEqual(["FocusAgents", "Workspace", "Help", null]);
  });
});

// ---------------------------------------------------------------------------
// The reference is generated, so it cannot omit a key
// ---------------------------------------------------------------------------

// Every key any surface binds, plus both spellings of every workbench verb.
const allKeys = (() => {
  const keys = new Set<string>();
  for (const [, verbs] of allSurfaces()) for (const x of verbs) keys.add(x.key);
  for (const g of GLOBAL) {
    if (g.alt) keys.add(g.alt);
    if (g.prefix) keys.add(g.prefix);
  }
  return [...keys].sort();
})();

// Every VerbId any surface table offers, so the lint below can require the
// dispatcher to have somewhere to send each of them.
const offered = (() => {
  const ids = new Set<string>();
  for (const [, verbs] of allSurfaces()) for (const x of verbs) ids.add(x.id);
  return [...ids].sort();
})();

describe("the reference is generated, so it cannot omit a key", () => {
  test("verbs/reference-is-complete — a key `?` cannot name is a key that fell out of the generation", () => {
    const ref = JSON.stringify(reference());
    expect(allKeys.filter((k) => !ref.includes(JSON.stringify(k).slice(1, -1)))).toEqual([]);
  });

  // Every verb a surface offers has somewhere for the dispatcher to send it. A
  // verb in a table with no entry in `keys.ts`'s map is a key that is drawn in a
  // footer, listed in `?`, and does nothing — the mirror of a key in no table.
  //
  // Read out of the source rather than imported, exactly as `check.py` reads
  // `keys.js`: what is being asserted is that the dispatcher *mentions* the id,
  // and only the text can say that.
  test("verbs/dispatchable — keys.ts sends every offered verb somewhere", async () => {
    const kts = await Bun.file(new URL("../src/logic/keys.ts", import.meta.url)).text();
    expect(offered.filter((v) => !kts.includes("VerbId." + v))).toEqual([]);
  });

  // Every surface has a section, from a list written *here* rather than from the
  // same `allSurfaces()` the reference is generated from. A check generated from
  // the thing it checks passes when a surface is deleted from both at once,
  // which is exactly how a whole page falls out of `?`.
  for (const name of ["HOME", "AGENTS", "PROCESSES", "CHANGES", "FILES", "DOCKER", "GIT", "SETTINGS", "overlays"]) {
    test(`verbs/reference-has-surface/${name} — kept by hand, so a dropped surface is noticed`, () => {
      expect(reference().map((s) => s.title)).toContain(name);
    });
  }

  // Per surface, not just "every key appears somewhere": most letters are bound
  // on several surfaces, so a whole section can vanish without a single key
  // going missing from the text. That is mutation 10.
  for (const [name, verbs] of allSurfaces()) {
    test(`verbs/reference-by-subject/${name} — "the same material split by subject"`, () => {
      // A surface that falls out of the reference keeps every key it had and
      // loses the only place they are written down.
      const sec = reference().find((s) => s.title === name);
      const have = sec ? sec.rows.map((r) => r.keys) : [];
      expect(verbs.map((x) => keyText(x.key)).filter((k) => !have.includes(k))).toEqual([]);
    });
  }
});

// ---------------------------------------------------------------------------
// The `g` menu — mnemonics, groups, confirmations
//
// Ported from `check.py`'s `check_git_menu` / `GITMENU_JS`. Assigned to this
// file alongside the verb tables; `PORTING-TESTS.md`'s one-file-per-group rule
// would put them in `test/git-menu.test.ts`, and they are one contiguous block
// here so that move stays a single cut.
// ---------------------------------------------------------------------------

const quiet = { inSequence: false };
const stuck = { inSequence: true };

describe("the `g` menu", () => {
  test("gitmenu/mnemonics — a mnemonic matching two rows of one group leaves one unreachable", () => {
    const bad: Record<string, string[]> = {};
    for (const g of GROUPS) {
      const seen = new Set<string>();
      for (const i of ITEMS.filter((x) => x.group === g.id)) {
        if (seen.has(i.key)) (bad[g.id] ??= []).push(i.key);
        seen.add(i.key);
      }
    }
    expect(bad).toEqual({});
  });

  test("gitmenu/group-mnemonics — no two groups share a letter", () => {
    const gkeys = GROUPS.map((g) => g.key);
    expect(new Set(gkeys).size).toBe(gkeys.length);
  });

  test("gitmenu/group-size — no group has more than twelve rows", () => {
    // The terminal's shared list overlay clamps to sixteen lines, three of which
    // are chrome and one the `..`, and a menu that reads differently in the two
    // clients is two menus.
    const sizes = GROUPS.map((g) => [g.id, ITEMS.filter((i) => i.group === g.id).length] as const);
    expect(sizes.filter(([, n]) => n > 12).map(([g]) => g)).toEqual([]);
  });

  // A stuck repository must offer the way out and nothing that would make it
  // worse. The menu's one piece of real logic.
  test("gitmenu/mid-sequence — mid-merge the menu offers only the way out", () => {
    // git refuses most of the rest and the remainder would tangle the sequence
    // further.
    expect(groupsFor(stuck).map((g) => g.id)).toEqual(["Integrate"]);
  });

  test("gitmenu/mid-sequence-rows — and inside it, only continue, abort and skip", () => {
    expect(itemsFor("Integrate", stuck).map((i) => i.action).sort()).toEqual([
      "SequenceAbort",
      "SequenceContinue",
      "SequenceSkip",
    ]);
  });

  test("gitmenu/quiet-rows — with nothing in progress, a new merge and no `continue`", () => {
    // A `continue` for a sequence that is not running is a row that cannot work.
    const ids = itemsFor("Integrate", quiet).map((i) => i.action);
    expect(ids).toContain("Merge");
    expect(ids).not.toContain("SequenceContinue");
  });

  test("gitmenu/confirmed — exactly the three that destroy work you cannot get back", () => {
    // A force push, reset --hard, and abandoning a sequence.
    const confirmed = (Object.keys(GitAction) as GitActionId[])
      .filter((a) => needsConfirm(GitAction[a]))
      .sort();
    expect(confirmed).toEqual(["PushForce", "ResetHard", "SequenceAbort"]);
  });

  test("gitmenu/no-orphan-actions — every declared action has a row", () => {
    // A vocabulary that outlives its rows stops describing the menu.
    const orphans = (Object.keys(GitAction) as GitActionId[])
      .filter((a) => !ITEMS.some((i) => i.action === GitAction[a]))
      .sort();
    expect(orphans).toEqual([]);
  });
});
