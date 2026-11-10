// The HELP page's keyboard: every key this client answers, laid on the keys
// you press.
//
// **Nothing here is a binding.** HELP's rule is that every topic is generated
// from the tables, so a reference cannot describe a key the client does not
// have, or miss one it does. The keyboard keeps that rule: each layer below is
// read off `verbs.ts` — `GLOBAL` for the two workbench layers, `allSurfaces()`
// for the rest, the same two `reference()` walks — and the only things written
// in this file are
//
//   * **the physical keyboard**, which is geometry: where `q` is, what `1`
//     reads with shift held. A table of key *positions*, never of meanings;
//   * **how a table's key name lands on it** — `A` is shift and `a`, `<` is
//     shift and `,`, `C-s` is ctrl and `s`, `1-9` is nine keys;
//   * **a category per verb**, for the colour on the cap. `Record<VerbId, …>`,
//     so a verb added to the vocabulary does not compile until it has one.
//
// Two kinds of cap are not a row in any table, and both are read from data
// `verbs.ts` already exports rather than written here: the arrows, which are
// `ARROW_ALIASES` of `j`/`k`/`h`/`l` on a surface whose table binds those, and
// the Alt keys that must fall through to the program, which are
// `ALT_MUST_FALL_THROUGH`.
//
// Pure, like `verbs.ts`: no DOM, so the test that says "every binding is on the
// keyboard and nothing else is" runs without a browser.

import {
  ALT_MUST_FALL_THROUGH,
  ARROW_ALIASES,
  GLOBAL,
  VerbId,
  allSurfaces,
  keyText,
  reference,
} from "./verbs.ts";
import { HELP_TOPIC, slugFor } from "./docs.ts";

/** The reference section the two workbench layers are written up in. */
const KEYS_SLUG = HELP_TOPIC.replace(/^butai:\/\//, "");

// ---------------------------------------------------------------------------
// The physical keyboard
// ---------------------------------------------------------------------------

/**
 * One key on the board.
 *
 * `id` is the key's name **as the tables spell it** wherever the tables name
 * it at all — `a`, `1`, `[`, `enter`, `esc`, `backspace`, `arrowup` — so that
 * looking a table key up is, for most keys, finding the cap with that id. The
 * modifiers and the keys no table binds get names of their own.
 *
 * `code` is `KeyboardEvent.code`: the physical key, whatever the layout prints
 * on it, which is what pressing a key on your own keyboard is looked up by.
 */
export interface Cap {
  id: string;
  code: string;
  glyph: string;
  /** What the key types with shift held, where that is a different character. */
  shift?: string;
  /** Width in key units; 1 when absent. */
  w?: number;
  /** A modifier or a whitespace key: its glyph is a word, drawn smaller. */
  mod?: boolean;
}

/** A gap in a row, in key units. */
export interface Gap {
  gap: number;
}

export type Slot = Cap | Gap;

export function isGap(s: Slot): s is Gap {
  return "gap" in s;
}

const k = (id: string, code: string, glyph: string, shift?: string, w?: number): Cap => ({
  id, code, glyph, ...(shift ? { shift } : {}), ...(w ? { w } : {}),
});
const m = (id: string, code: string, glyph: string, w: number): Cap => ({ id, code, glyph, w, mod: true });
const gap = (n: number): Gap => ({ gap: n });
const letter = (c: string): Cap => k(c, "Key" + c.toUpperCase(), c);
const fn = (n: number): Cap => k("f" + n, "F" + n, "F" + n);

/** The main block: an ANSI board, F-row to space bar. Rows are 15 units wide. */
export const MAIN: readonly (readonly Slot[])[] = [
  [m("esc", "Escape", "esc", 1), gap(1), fn(1), fn(2), fn(3), fn(4), gap(0.5), fn(5), fn(6), fn(7), fn(8),
    gap(0.5), fn(9), fn(10), fn(11), fn(12)],
  [k("`", "Backquote", "`", "~"), k("1", "Digit1", "1", "!"), k("2", "Digit2", "2", "@"),
    k("3", "Digit3", "3", "#"), k("4", "Digit4", "4", "$"), k("5", "Digit5", "5", "%"),
    k("6", "Digit6", "6", "^"), k("7", "Digit7", "7", "&"), k("8", "Digit8", "8", "*"),
    k("9", "Digit9", "9", "("), k("0", "Digit0", "0", ")"), k("-", "Minus", "-", "_"),
    k("=", "Equal", "=", "+"), m("backspace", "Backspace", "⌫ backspace", 2)],
  [m("tab", "Tab", "⇥ tab", 1.5), ..."qwertyuiop".split("").map(letter),
    k("[", "BracketLeft", "[", "{"), k("]", "BracketRight", "]", "}"), k("\\", "Backslash", "\\", "|", 1.5)],
  [m("capslock", "CapsLock", "caps", 1.75), ..."asdfghjkl".split("").map(letter),
    k(";", "Semicolon", ";", ":"), k("'", "Quote", "'", "\""), m("enter", "Enter", "⏎ enter", 2.25)],
  [m("shiftleft", "ShiftLeft", "⇧ shift", 2.25), ..."zxcvbnm".split("").map(letter),
    k(",", "Comma", ",", "<"), k(".", "Period", ".", ">"), k("/", "Slash", "/", "?"),
    m("shiftright", "ShiftRight", "⇧ shift", 2.75)],
  [m("controlleft", "ControlLeft", "ctrl", 1.5), m("metaleft", "MetaLeft", "⌘ / ❖", 1.25),
    m("altleft", "AltLeft", "alt ⌥", 1.25), m("space", "Space", "space", 6.25),
    m("altright", "AltRight", "alt ⌥", 1.25), m("metaright", "MetaRight", "⌘ / ❖", 1.25),
    m("controlright", "ControlRight", "ctrl", 1.5)],
];

/** The navigation cluster, beside the main block and level with its number row. */
export const NAV: readonly (readonly Slot[])[] = [
  [k("insert", "Insert", "ins"), k("home", "Home", "home"), k("pageup", "PageUp", "pg up")],
  [k("delete", "Delete", "del"), k("end", "End", "end"), k("pagedown", "PageDown", "pg dn")],
  [],
  [gap(1), k("arrowup", "ArrowUp", "↑")],
  [k("arrowleft", "ArrowLeft", "←"), k("arrowdown", "ArrowDown", "↓"), k("arrowright", "ArrowRight", "→")],
];

/** Every cap on the board, in reading order. */
export const CAPS: readonly Cap[] = [...MAIN, ...NAV].flat().filter((s): s is Cap => !isGap(s));

const BY_ID: ReadonlyMap<string, Cap> = new Map(CAPS.map((c) => [c.id, c]));
const BY_CODE: ReadonlyMap<string, Cap> = new Map(CAPS.map((c) => [c.code, c]));
/** `?` -> `/`: the shifted characters, read back off the caps that print them. */
const BY_SHIFT: ReadonlyMap<string, Cap> = new Map(
  CAPS.filter((c) => c.shift).map((c) => [c.shift!, c]),
);

export function capById(id: string): Cap | null {
  return BY_ID.get(id) ?? null;
}

/** The cap a real key press landed on, by `KeyboardEvent.code`. */
export function capForCode(code: string): Cap | null {
  return BY_CODE.get(code) ?? null;
}

/** The modifier a layer is reached through, held while the key is pressed. */
export type Mod = "" | "shift" | "ctrl";

/** Where a table key lands on the board. */
export interface Place {
  cap: string;
  mod: Mod;
  /** The one key this place stands for, when the table key was a range. */
  one: string;
}

/**
 * Where a key, as a table spells it, is on the board — or null for a spelling
 * this keyboard cannot place, which the test treats as a failure: a key the
 * keyboard cannot draw is a key HELP has lost.
 *
 * One table key can be several places (`1-9`) but never none.
 */
export function placeKey(key: string): Place[] | null {
  const chord = /^C-(.+)$/.exec(key);
  if (chord) {
    const inner = placeKey(chord[1]!);
    if (!inner || inner.some((p) => p.mod)) return null;
    return inner.map((p) => ({ ...p, mod: "ctrl" as const }));
  }
  const range = /^(\d)-(\d)$/.exec(key);
  if (range) {
    const out: Place[] = [];
    for (let d = Number(range[1]); d <= Number(range[2]); d++) {
      const one = placeKey(String(d));
      if (!one) return null;
      out.push(...one);
    }
    return out;
  }
  if (key === " " || key === "space") return [{ cap: "space", mod: "", one: " " }];
  if (BY_ID.has(key)) return [{ cap: key, mod: "", one: key }];
  if (/^[A-Z]$/.test(key) && BY_ID.has(key.toLowerCase())) {
    return [{ cap: key.toLowerCase(), mod: "shift", one: key }];
  }
  const shifted = BY_SHIFT.get(key);
  if (shifted) return [{ cap: shifted.id, mod: "shift", one: key }];
  return null;
}

// ---------------------------------------------------------------------------
// Categories
// ---------------------------------------------------------------------------

export type Category = "view" | "agent" | "git" | "project" | "tool" | "danger" | "program";

/** Every category, in the order the legend and the per-layer list use. */
export const CATEGORIES: readonly { id: Category; label: string }[] = [
  { id: "view", label: "views & navigation" },
  { id: "agent", label: "agents & the stage" },
  { id: "git", label: "git, changes, diffs" },
  { id: "project", label: "projects & machines" },
  { id: "tool", label: "tools & this client" },
  { id: "danger", label: "close, kill, discard" },
  { id: "program", label: "left for the program" },
];

/**
 * The category of every verb — a colour on the cap, not a meaning.
 *
 * Keyed by the whole vocabulary on purpose: a verb added to `VerbId` without a
 * line here is a type error, which is cheaper than a grey cap someone has to
 * notice. A table verb marked `danger` is drawn as one whatever this says.
 */
const CATEGORY: Readonly<Record<VerbId, Category>> = {
  SpaceWork: "view", SpaceFiles: "view", SpaceDocker: "view", SpaceDocs: "view",
  SpaceNext: "view", SpacePrev: "view", Views: "view",
  SpaceGit: "git",
  SpaceSettings: "tool",
  SpaceHome: "project", FocusFleet: "project",
  Workspace: "project", WorkspaceNext: "project", WorkspacePrev: "project", NewWorkspace: "project",
  CloseWorkspace: "danger",
  FocusAgents: "agent", FocusProcs: "agent", FocusStage: "agent", FocusOff: "agent",
  FocusChanges: "git",
  FocusCycle: "view",
  OpenAgent: "agent",
  Fold: "view", FoldAll: "view",
  Down: "view", Up: "view", First: "view", Last: "view", Open: "view",
  NewAgent: "agent", PickAgent: "agent", NewShell: "agent", Restart: "agent", Ack: "agent",
  Kill: "danger",
  Stage: "git", Unstage: "git", Diff: "git",
  ResolveOurs: "git", ResolveTheirs: "git", ResolveDone: "git",
  Commit: "git", CommitAll: "git", Push: "git", Pull: "git", Fetch: "git", Branch: "git",
  Refresh: "tool",
  SeqContinue: "git", SeqAbort: "danger",
  Upload: "tool", Download: "tool", Edit: "tool", Save: "tool", CancelEdit: "tool",
  ViewFile: "tool", ViewDiff: "git", DeleteFile: "danger",
  TreeUp: "view", TreeInto: "view", Peek: "tool",
  DockerLogs: "tool", DockerShell: "tool", DockerRestart: "tool", DockerStop: "danger",
  Scope: "git", ScopeAll: "git", GoToChanges: "git", Show: "git", CopySha: "git",
  Checkout: "git", Merge: "git", DeleteBranch: "danger", TagDelete: "danger",
  StashPop: "git", StashDrop: "danger", OpenWorktree: "git", RemoveWorktree: "danger",
  Revert: "git", CherryPick: "git", GitMenu: "git", CancelOp: "danger",
  SettingChange: "tool", SettingChoose: "tool", SettingKeep: "tool", SettingToggle: "tool",
  SettingBigger: "tool", SettingSmaller: "tool", SettingAuto: "tool", CloseSettings: "tool",
  Accept: "tool", Cancel: "tool", Clear: "tool", ClearAll: "tool", NewFolder: "tool",
  Layout: "tool", Zen: "tool", Help: "view", Alerts: "tool", PasteImage: "tool",
  FontBigger: "tool", FontSmaller: "tool",
};

export function categoryOf(id: VerbId, danger = false): Category {
  return danger ? "danger" : CATEGORY[id];
}

// ---------------------------------------------------------------------------
// The layers
// ---------------------------------------------------------------------------

/**
 * One key's meaning in one layer.
 *
 * `source` says which data it came from: a `table` row, an `alias` (an arrow
 * standing for the table's `j`/`k`/`h`/`l`), or a `fallthrough` — an Alt key the
 * workbench promises never to take.
 */
export interface Binding {
  layer: string;
  /** The key exactly as the table spells it — `1-9`, `A`, `C-s`, `enter`. */
  key: string;
  cap: string;
  mod: Mod;
  /** How to write the whole combination: `alt-3`, `C-b A`, `enter`. */
  spelled: string;
  label: string;
  /** What the table says beside the key — a `note`, or a `claim` marked ⚠. */
  note: string;
  category: Category;
  id: VerbId | null;
  source: "table" | "alias" | "fallthrough";
  /** A surface verb that is bound but never drawn in the footer. */
  quiet: boolean;
  /** For an alias: the table key it stands for. */
  aliasOf?: string;
}

export interface Layer {
  id: string;
  /** The reference section this layer is read from — and so its name in `?`. */
  title: string;
  /** How you reach it: `alt`, the prefix, or nothing for a surface's bare keys. */
  chord: string;
  /** That section's own note, as the reference prints it. */
  note: string;
  /** Caps lit while this layer is up: the modifier you hold. */
  held: string[];
  /** The prefix key itself, on the prefix layer: press it twice for a literal. */
  prefixCap: string | null;
  bindings: Binding[];
}

/** The layer id a surface's section is known by. */
export function layerIdFor(title: string): string {
  return title.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
}

/**
 * A key as a person reads it in `alt-…` or a list: the tables' `arrowleft` is
 * `←`. Only the names a table uses and a keycap prints differently; everything
 * else is `keyText`'s.
 */
const SAY: Readonly<Record<string, string>> = {
  arrowleft: "←", arrowright: "→", arrowup: "↑", arrowdown: "↓", pageup: "pgup", pagedown: "pgdn",
};
function say(one: string): string {
  return SAY[one] ?? keyText(one);
}

/** The label a range key gives one of its keys: `project 1-9` on `3` is `project 3`. */
function oneOf(label: string, key: string, one: string): string {
  return key === one ? label : label.split(key).join(one);
}

/**
 * Every layer, generated.
 *
 * `prefix` is the prefix as the user spells it (`C-b`, `C-a`), because the
 * prefix layer is reached by *their* chord — the same rewrite `docs.ts` makes
 * for the prose topics.
 */
export function layers(prefix = "C-b"): Layer[] {
  const sections = reference();
  const noteOf = (title: string) => (sections.find((s) => s.title === title)?.note ?? "").replace(/C-b/g, prefix);
  // By the slug `?` lands on rather than by position, for `HelpPage`'s reason:
  // two lists that happen to line up are a bug waiting for a reorder.
  const workbench = sections.find((s) => slugFor(s.title) === KEYS_SLUG)?.title ?? "";
  const out: Layer[] = [];

  const global = (layer: string, chord: string, pick: (g: (typeof GLOBAL)[number]) => string | undefined) => {
    const bindings: Binding[] = [];
    for (const g of GLOBAL) {
      const key = pick(g);
      if (!key) continue;
      for (const p of placeKey(key) ?? []) {
        bindings.push({
          layer,
          key,
          cap: p.cap,
          mod: p.mod,
          spelled: chord + (layer === "alt" ? "-" : " ") + say(p.one),
          label: oneOf(g.label, key, p.one),
          note: (g.claim ? "⚠ " + g.claim : g.note).replace(/C-b/g, prefix),
          category: categoryOf(g.id),
          id: g.id,
          source: "table",
          quiet: false,
        });
      }
    }
    return bindings;
  };

  // The workbench layer, both spellings. The Alt layer also carries the keys it
  // promises *not* to take, because "alt-b reaches readline" is a fact about
  // the b key on this layer and the place you would look for it.
  const alt = global("alt", "alt", (g) => g.alt);
  for (const key of ALT_MUST_FALL_THROUGH) {
    for (const p of placeKey(key) ?? []) {
      alt.push({
        layer: "alt", key, cap: p.cap, mod: p.mod, spelled: "alt-" + say(p.one),
        label: "→ program", note: "never bound here, so it reaches the program on the stage",
        category: "program", id: null, source: "fallthrough", quiet: false,
      });
    }
  }
  out.push({
    id: "alt", title: workbench, chord: "alt", note: noteOf(workbench),
    held: ["altleft", "altright"], prefixCap: null, bindings: alt,
  });

  const letter = /^C-(\S)$/.exec(prefix)?.[1]?.toLowerCase() ?? "b";
  out.push({
    id: "prefix", title: workbench, chord: prefix, note: noteOf(workbench),
    held: ["controlleft", "controlright"], prefixCap: placeKey(letter)?.[0]?.cap ?? null,
    bindings: global("prefix", prefix, (g) => g.prefix),
  });

  for (const [title, verbs] of allSurfaces()) {
    const id = layerIdFor(title);
    const bindings: Binding[] = [];
    // A surface table concatenated from several arms repeats its shared rows;
    // the same key, label and verb twice is one binding.
    const seen = new Set<string>();
    for (const v of verbs) {
      const sig = v.key + "\u0000" + v.label + "\u0000" + v.id;
      if (seen.has(sig)) continue;
      seen.add(sig);
      for (const p of placeKey(v.key) ?? []) {
        bindings.push({
          layer: id, key: v.key, cap: p.cap, mod: p.mod, spelled: say(p.one),
          label: v.label, note: "", category: categoryOf(v.id, v.danger), id: v.id,
          source: "table", quiet: !v.footer,
        });
      }
    }
    for (const [arrow, verbId] of Object.entries(ARROW_ALIASES)) {
      const v = verbs.find((x) => x.id === verbId);
      if (!v) continue;
      for (const p of placeKey(arrow) ?? []) {
        bindings.push({
          layer: id, key: arrow, cap: p.cap, mod: p.mod, spelled: say(p.one),
          label: v.label, note: "", category: categoryOf(v.id, v.danger), id: v.id,
          source: "alias", quiet: !v.footer, aliasOf: v.key,
        });
      }
    }
    out.push({
      id, title, chord: "", note: noteOf(title), held: [], prefixCap: null, bindings,
    });
  }
  return out;
}

/** The bindings on one cap in one layer, unshifted first. */
export function onCap(layer: Layer, cap: string): Binding[] {
  const order: Record<Mod, number> = { "": 0, shift: 1, ctrl: 2 };
  return layer.bindings.filter((b) => b.cap === cap).sort((a, b) => order[a.mod] - order[b.mod]);
}

/** How many distinct keys (a cap with a modifier) a layer binds. */
export function keyCount(layer: Layer, match?: (b: Binding) => boolean): number {
  return new Set(layer.bindings.filter((b) => !match || match(b)).map((b) => b.cap + "/" + b.mod)).size;
}

/** Does a binding answer a search? Over what is drawn and what is said about it. */
export function matches(b: Binding, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return [b.label, b.note, b.spelled, b.key].some((s) => s.toLowerCase().includes(q));
}
