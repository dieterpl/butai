// What the surface you are looking at can do *right now*, as data.
//
// The port of `crates/butai-client/src/verbs.rs`, and it carries the same rule
// the TUI *enforces* rather than intends:
//
//   > Nothing is reachable by pointer alone, and nothing is bound that cannot
//   > be found.
//
// Two halves, and both are checked rather than remembered:
//
// 1. **Every click target has a key.** [`TARGETS`] is the registry, and
//    [`click`] is the only way to put a handler on anything — it throws for a
//    target that is not declared, so a new button does not *run* until someone
//    has said which key reaches it. That is this file's answer to Rust's
//    `match` with no catch-all: JS has no exhaustiveness, so the exhaustive
//    thing is the constructor. `check.py`'s `invariant/every-click-target-has-
//    a-key` reads the source for the same rule without a browser.
// 2. **Every key is in a table.** One table per surface below, and they drive
//    four things at once: the footer text, the click hit-test, the key dispatch
//    and the `?` reference. Dispatch reads [`Verb.id`], so binding a key
//    without listing it is not possible.
//
// A verb that does not fit the footer is marked `quiet`: still bound, still in
// the reference, just not competing for a column. `docs/keys.md` is the whole
// list in prose; this is the same material as data.
//
// Nothing here touches the DOM, the network or the app — it is a table and the
// pure functions that lay it out, so `check.py` can run the lot under node.
// (TypeScript's half of that: nothing below imports a DTO, because nothing
// below crosses the wire. [`bind`] is the one exception to the DOM rule and it
// only touches an element the caller already has.)

// ---------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------

/// Every verb any surface can offer. One flat vocabulary rather than one per
/// surface: dispatch matches on it, so a verb offered by a surface that cannot
/// run it is a name that resolves nowhere and `check.py` says so.
///
/// Frozen because it is a spelling table: `VerbId.Kill` must be a typo that
/// throws, not `undefined` that silently matches nothing. Typed, it is a typo
/// that does not compile — [`ids`] hands back literal types, so the union
/// below is generated from the same list rather than written twice.
export const VerbId = Object.freeze(ids([
  // Spaces, projects, machines — the workbench layer.
  "SpaceWork", "SpaceFiles", "SpaceDocker", "SpaceGit", "SpaceDocs", "SpaceNext", "SpacePrev",
  // SETTINGS is a page and is deliberately *not* one of the spaces above, for
  // the reason `Page::Settings` gives: that list is the views of one workspace,
  // and this is about the client. It is entered and left rather than cycled.
  "SpaceSettings",
  // HOME is deliberately not one of the spaces above: the spaces are views *of
  // one workspace*, which is what makes them a list you can walk with one pair
  // of keys, and HOME spans daemons. It is a peer of the workspace chips and
  // lives on the tab bar beside them.
  "SpaceHome",
  "Workspace", "WorkspaceNext", "WorkspacePrev", "NewWorkspace", "CloseWorkspace",
  // Focus.
  "FocusAgents", "FocusProcs", "FocusChanges", "FocusStage", "FocusOff", "FocusCycle",
  // HOME's fleet — the one list that is not a view of the active workspace.
  "FocusFleet",
  // …and the one verb that acts on a fleet row: go to that agent's workspace,
  // on its own machine, and put it on the stage. Distinct from `Open`, which
  // every other list uses to mean "stage this row of the workspace I am in".
  "OpenAgent",
  // List navigation, listed in `?` so every rail documents itself.
  "Down", "Up", "First", "Last", "Open",
  // The left rail.
  "NewAgent", "PickAgent", "NewShell", "Restart", "Kill", "Ack",
  // The changes rail — file rows.
  "Stage", "Unstage", "Diff",
  // The changes rail — conflict rows. None of these has a key anywhere else.
  "ResolveOurs", "ResolveTheirs", "ResolveDone",
  // The changes rail — always.
  "Commit", "CommitAll", "Push", "Pull", "Fetch", "Branch", "Refresh",
  // The merge/rebase banner's two ways out.
  "SeqContinue", "SeqAbort",
  // The files page. `DeleteFile` is its own id rather than the rail's `Discard`
  // — one puts a file back to what git has, the other removes it.
  "Upload", "Download", "Edit", "Save", "CancelEdit", "ViewFile", "ViewDiff", "DeleteFile",
  // The docker page.
  "DockerLogs", "DockerShell", "DockerRestart", "DockerStop",
  // The GIT page. `Show` reads a commit or a stash into the body; `Scope`
  // points HISTORY at one ref. Neither writes anything — that rule is what
  // keeps this page from being a second CHANGES rail.
  "Scope", "ScopeAll", "GoToChanges", "Show", "CopySha",
  "Checkout", "Merge", "DeleteBranch", "TagDelete", "StashPop", "StashDrop",
  "OpenWorktree", "RemoveWorktree", "Revert", "CherryPick", "GitMenu", "CancelOp",
  // The SETTINGS page. Every one of them acts on the row the cursor is on, and
  // there is no Save: a change applies and is written when you make it.
  "SettingChange", "SettingChoose", "SettingKeep", "SettingToggle",
  "SettingBigger", "SettingSmaller", "SettingAuto", "CloseSettings",
  // Overlays.
  "Accept", "Cancel", "Clear", "ClearAll", "NewFolder",
  // The rest of the workbench.
  "Zen", "Help", "Alerts", "PasteImage", "FontBigger", "FontSmaller",
]));

/// One verb's name. The union of [`VerbId`]'s values, read off the table so the
/// two cannot drift.
export type VerbId = (typeof VerbId)[keyof typeof VerbId];

/// `{ Down: "Down", … }` from `["Down", …]`, with each name kept as its own
/// literal type — the `const` type parameter is `as const` at the call site, so
/// the tables below are written exactly as they were.
function ids<const N extends readonly string[]>(names: N): { readonly [K in N[number]]: K } {
  const out: Record<string, string> = {};
  for (const n of names) out[n] = n;
  return out as { readonly [K in N[number]]: K };
}

/// A verb as a surface offers it.
///
/// `key` is a **key name**, not a character: `"a"`, `"A"`, `"enter"`, `"esc"`,
/// `"tab"`, `"space"`. The TUI can use a `char` because `'\n'` is a character;
/// here the browser's `KeyboardEvent.key` is already a name for those, so
/// matching it is the thing that needs no translation.
export type Verb = Readonly<{ key: string; label: string; id: VerbId; danger: boolean; footer: boolean }>;

/// What [`mk`]'s last argument may override. Both have a default, and no verb
/// sets more than one of them.
type VerbExtra = { danger?: boolean; footer?: boolean };

function mk(key: string, label: string, id: VerbId, extra?: VerbExtra): Verb {
  return Object.freeze(Object.assign({ key, label, id, danger: false, footer: true }, extra));
}
const verb = (key: string, label: string, id: VerbId): Verb => mk(key, label, id);
const danger = (key: string, label: string, id: VerbId): Verb => mk(key, label, id, { danger: true });
/// Bound and documented, but never drawn in the footer.
const quiet = (key: string, label: string, id: VerbId): Verb => mk(key, label, id, { footer: false });

/// Separator between two verbs on the same footer row.
export const SEP = " · ";

/// The most footer rows any surface will give up to verbs.
export const MAX_ROWS = 3;

/// How the key is written down. Most are themselves; the named ones get their
/// names, because a footer reading `"  stage"` says nothing.
export function keyText(key: string): string {
  return key === " " ? "space" : key;
}

/// How wide `"<key> <label>"` draws.
function cellWidth(v: Verb): number {
  return keyText(v.key).length + 1 + v.label.length;
}

// ---------------------------------------------------------------------------
// Footer packing — the same greedy layout the TUI uses
// ---------------------------------------------------------------------------

/// Where one verb's `"<key> <label>"` lands: which row, and the columns it
/// covers. `key` is what the hit-test resolves a click back to.
export type Span = { row: number; start: number; end: number; key: string };

/// Pack `verbs` into at most `rows` lines of `width` columns, in order.
///
/// Ported rather than reinvented because the numbers have to agree with the
/// terminal's: the PROCESSES footer is `t new · r restart · x kill`, 26 columns
/// in 26, and a reworded verb that silently drops `x kill` off the only place
/// the key is written down is exactly what this catches. The browser's rail is
/// elastic, so nothing here decides pixels — it decides *which verbs are worth
/// a column*, which is the part that is a design decision and not a layout one.
export function layout(verbs: readonly Verb[], width: number, rows: number): Span[] {
  const spans: Span[] = [];
  let row = 0, x = 0;
  for (const v of verbs) {
    if (!v.footer) continue;
    if (row >= rows) break;
    const w = cellWidth(v);
    const sep = x === 0 ? 0 : SEP.length;
    if (x + sep + w > width) {
      // Doesn't fit here. Try a fresh row — unless it would not fit on an empty
      // row either, in which case no row will ever take it.
      if (w > width) continue;
      row += 1;
      x = 0;
      if (row >= rows) break;
    } else {
      x += sep;
    }
    spans.push({ row, start: x, end: x + w, key: v.key });
    x += w;
  }
  return spans;
}

/// The footer as text, one string per row (`rows` of them, blank where empty).
export function lines(verbs: readonly Verb[], width: number, rows: number): string[] {
  const spans = layout(verbs, width, rows);
  const out: string[] = new Array<string>(rows).fill("");
  for (const span of spans) {
    const v = verbs.find((x) => x.key === span.key);
    if (!v) continue;
    // `layout` never returns a row past `rows` and the array is filled, so the
    // `?? ""` is `noUncheckedIndexedAccess` being satisfied, not a case.
    let line = out[span.row] ?? "";
    if (line) line += SEP;
    line += keyText(v.key) + " " + v.label;
    out[span.row] = line;
  }
  return out;
}

/// The verbs that actually earn a column at this width — what the browser's
/// footer draws, in order.
export function fits(verbs: readonly Verb[], width: number, rows: number = MAX_ROWS): Verb[] {
  const keys = layout(verbs, width, rows).map((s) => s.key);
  return verbs.filter((v) => keys.includes(v.key));
}

// ---------------------------------------------------------------------------
// The two layers
// ---------------------------------------------------------------------------

/// A verb on the workbench layer: one id, one label, and the two spellings.
export type GlobalVerb = Readonly<{
  id: VerbId;
  label: string;
  note: string;
  claim: string;
  alt?: string;
  prefix?: string;
}>;

/// The two spellings of one workbench verb. A verb may have either or both.
type Spell = { alt?: string; prefix?: string };

/// The workbench layer: reachable from anywhere, including from inside a
/// running program.
///
/// `alt` and `prefix` are the two spellings of the same verb, exactly as
/// `docs/keys.md` describes them — Alt for a workbench you are typing past, and
/// the prefix for **a terminal that eats Alt**. In a browser the thing that eats
/// Alt is the browser, which is the same problem with a different owner, and it
/// is why the prefix layer here is the complete one rather than a courtesy.
///
/// `note` explains a spelling; `claim` is the stronger thing — a key that
/// something *above us* may take before the page ever sees it. Both are fields
/// rather than a paragraph in a README so the reference cannot drift from the
/// table: `?` prints them beside the key, and `check.py` requires every verb
/// carrying a `claim` to have a prefix spelling to fall back on, because on a
/// browser that takes the key that spelling is the only way in.
function g(id: VerbId, label: string, spell: Spell, note?: string, claim?: string): GlobalVerb {
  return Object.freeze(Object.assign({ id, label, note: note || "", claim: claim || "" }, spell));
}

export const GLOBAL: readonly GlobalVerb[] = Object.freeze([
  // -- spaces --------------------------------------------------------------
  g(VerbId.SpaceFiles, "files", { alt: "o", prefix: "o" },
    "press it again for work — every space key toggles back"),
  g(VerbId.SpaceDocker, "docker", { alt: "c", prefix: "c" },
    "alt-d is the browser's address bar, so containers take c — the same letter the TUI gives them, for its own reason"),
  g(VerbId.SpaceGit, "git", { alt: "r", prefix: "r" },
    "the repository over time, not the CHANGES rail — alt-g is the rail, and they do not share a letter"),
  g(VerbId.SpaceDocs, "docs", { alt: "m", prefix: "m" },
    "the files page filtered to a project's writing — and where this reference lives, "
    + "as a `reference` folder at the top of the rail"),
  g(VerbId.SpaceWork, "work", { prefix: "w" }, "the stage, the rails, the terminal"),
  g(VerbId.SpaceNext, "next space", { alt: ".", prefix: "." }),
  g(VerbId.SpacePrev, "prev space", { alt: ",", prefix: "," }),
  // -- the one page that spans machines ------------------------------------
  // Beside the numbered projects because that is where its chip is: HOME is a
  // peer of the workspaces, not a view of one, so `alt-,` / `alt-.` walk the
  // spaces *past* it and this key and the chip are how you reach it.
  g(VerbId.SpaceHome, "home", { alt: "0", prefix: "0" },
    "every agent on every machine — a peer of the project chips, so the space keys walk past it. "
    + "The browsers that take alt-1..alt-9 do not document alt-0, but C-b 0 is here for the ones that do"),
  g(VerbId.FocusFleet, "the fleet (HOME)", { alt: "w", prefix: "W" },
    "goes to HOME and puts the cursor in the list; also the way back out of the preview, "
    + "because once the preview has the keyboard every unmodified key is the agent's"),
  // -- the page that is about this client rather than a project -------------
  // `C-b S` rather than `C-b s`, which is the stage. The shifted letter is what
  // every other pair here does (`C-b a` picks an agent, `C-b A` focuses the
  // rail), and the terminal spends `C-b S` on a system monitor this client does
  // not have — a key for a thing that is not there is the half of the rule that
  // leaves it free.
  g(VerbId.SpaceSettings, "settings", { alt: "s", prefix: "S" },
    "this client's own — its palette, its rails, the agent `a` spawns, the machines it dials. "
    + "Not a space: alt-, and alt-. walk past it, and esc puts back the page you came from"),
  // -- projects ------------------------------------------------------------
  g(VerbId.Workspace, "project 1-9", { alt: "1-9", prefix: "1-9" }, "",
    "Chrome and Firefox switch browser tabs on Alt+digit on Linux and Windows; C-b 1 always arrives"),
  g(VerbId.WorkspaceNext, "next project", { alt: ">", prefix: "]" }, "spans every machine"),
  g(VerbId.WorkspacePrev, "prev project", { alt: "<", prefix: "[" }, "spans every machine"),
  g(VerbId.NewWorkspace, "new project", { alt: "n", prefix: "n" },
    "on a Mac, Option-n is a dead key in a terminal; the browser still reports which key it was"),
  g(VerbId.CloseWorkspace, "close project", { alt: "x", prefix: "X" }, "asks first"),
  // -- focus ---------------------------------------------------------------
  g(VerbId.FocusAgents, "AGENTS", { alt: "a", prefix: "A" }),
  g(VerbId.FocusProcs, "PROCESSES", { alt: "p", prefix: "P" }),
  g(VerbId.FocusChanges, "CHANGES", { alt: "g", prefix: "G" },
    "the rail — what you changed and can commit. alt-r is the GIT space, which is the "
    + "repository over time; they are the two most easily confused things here"),
  g(VerbId.FocusStage, "the stage", { prefix: "s" }, "enter on a rail row too"),
  g(VerbId.FocusOff, "off the stage", { alt: "esc" }, "back to the rails, from inside the program"),
  // -- what fills them -----------------------------------------------------
  g(VerbId.PickAgent, "choose an agent", { alt: "enter", prefix: "a" }, "A off the stage too"),
  g(VerbId.NewShell, "a new shell", { alt: "t", prefix: "t" }, "",
    "Firefox opens its Tools menu on Alt+T when the menu bar is on"),
  // -- the rest ------------------------------------------------------------
  g(VerbId.Zen, "collapse the rails", { alt: "z", prefix: "z" }),
  g(VerbId.Alerts, "who needs you", { alt: "u", prefix: "u" },
    "the [! n] badge — a web affordance with no TUI counterpart, so it takes a letter the TUI leaves free"),
  g(VerbId.PasteImage, "paste an image", { alt: "v", prefix: "v" }, "",
    "Firefox opens its View menu on Alt+V when the menu bar is on"),
  g(VerbId.FontBigger, "bigger", { alt: "=", prefix: "+" }, "the terminal's font, not the browser's zoom"),
  g(VerbId.FontSmaller, "smaller", { alt: "-", prefix: "-" }),
  g(VerbId.Help, "this reference", { prefix: "?" }, "bare ? off the stage"),
]);

/// Alt keys the workbench must **never** bind, and why.
///
/// `docs/keys.md`: "An Alt key the workbench does not bind falls through, so
/// `alt-b` and `alt-f` still move by words in readline." That is a promise about
/// the *absence* of a binding, which nothing would otherwise notice being
/// broken — so it is written down here and asserted, and it is what makes the
/// fall-through a feature rather than an accident.
export const ALT_MUST_FALL_THROUGH: readonly string[] = Object.freeze([
  "b", "f",             // readline: backward-word, forward-word
  "d",                  // readline kill-word — and the browser's address bar
  "backspace",          // readline: backward-kill-word
  "arrowleft", "arrowright",   // the browser's back and forward
]);

/// The prefix as a key spelling: a modifier and a key name.
export type Prefix = Readonly<{ ctrl: boolean; key: string }>;

/// The prefix, as a `KeyboardEvent` shape. `C-b` by default, the same as the
/// TUI's `[general] prefix`; press it twice to send a literal one through.
///
/// Overridable from the browser's own storage, because this client has no
/// config file and the bridge deliberately has none either (stage 5). The
/// SETTINGS page is stage 9's, and this is the escape hatch until then.
export const DEFAULT_PREFIX: Prefix = Object.freeze({ ctrl: true, key: "b" });
export const PREFIX_STORAGE_KEY = "butai.prefix";

/// The parts of a `KeyboardEvent` the two layers read.
///
/// Structural rather than `KeyboardEvent` itself, for the same reason the rest
/// of this file is DOM-free: the checks hand these functions plain objects, and
/// a real event satisfies this shape. Named `KeyEventLike` because `KeyEvent`
/// is already the daemon's wire type in `protocol.ts`, and they are two
/// different things — this one is the browser's, on its way *to* that one.
export type KeyEventLike = {
  readonly key: string;
  readonly code?: string;
  readonly shiftKey?: boolean;
  readonly ctrlKey?: boolean;
  readonly altKey?: boolean;
  readonly metaKey?: boolean;
};

/// A `KeyboardEvent` as a key **name** this file's tables are written in.
///
/// `e.key` first, because that is the character the user's layout actually
/// produces and the tables are written in characters. `e.code` is the fallback
/// for the two cases where `e.key` is not a name at all:
///
/// - **macOS**, where Option is a compose key: Option-o reports `e.key === "ø"`.
///   `docs/keys.md` says butai "reads those characters back" in a terminal;
///   in a browser `e.code` says `KeyO` outright, so we read the key instead of
///   the character it composed.
/// - **Dead keys** — Option-e and Option-n on a Mac — which report
///   `e.key === "Dead"` and emit nothing until the next keystroke. The terminal
///   cannot recover those at all (`docs/keys.md` says to use `C-b n` instead);
///   the browser can, because `e.code` is still `KeyN`.
///
/// Named keys keep their lowercase protocol-ish spelling so the tables can say
/// `"enter"` and `"esc"` the way the footer draws them.
const NAMED: Readonly<Record<string, string>> = {
  Enter: "enter", Escape: "esc", Tab: "tab", " ": " ", Backspace: "backspace",
  ArrowDown: "arrowdown", ArrowUp: "arrowup", ArrowLeft: "arrowleft", ArrowRight: "arrowright",
  Home: "home", End: "end", PageUp: "pageup", PageDown: "pagedown",
};
const CODE_FALLBACK: Readonly<Record<string, string>> = {
  Minus: "-", Equal: "=", Comma: ",", Period: ".", Slash: "/", Semicolon: ";",
};

export function keyName(e: KeyEventLike): string {
  const k = e.key;
  const named = NAMED[k];
  if (named) return named;
  if (typeof k === "string" && k.length === 1) return k;
  return codeName(e) || String(k || "").toLowerCase();
}

/// The key name to look up on the Alt layer.
///
/// Shift is part of the character (`alt->` is Alt-Shift-.), so it is not read
/// separately — but a composed or dead character has to come back through
/// `e.code`, and there `>` and `.` are the same physical key. Shift decides
/// which, and it is the only place in this file that has to.
export function altKeyName(e: KeyEventLike): string {
  const k = e.key;
  const named = NAMED[k];
  if (named) return named;
  if (typeof k === "string" && k.length === 1 && !isComposed(k)) return k;
  const code = codeName(e);
  if (!code) return keyName(e);
  if (!e.shiftKey) return code;
  const shifted: Readonly<Record<string, string>> =
    { ",": "<", ".": ">", "-": "_", "=": "+", "/": "?", ";": ":" };
  return shifted[code] || code.toUpperCase();
}

/// A character the Alt layer must not trust. On a Mac, Option-o is `ø` and
/// Option-1 is `¡`; anything outside the ASCII range Option composed for us.
function isComposed(ch: string): boolean {
  return ch.charCodeAt(0) > 0x7e;
}

function codeName(e: KeyEventLike): string | null {
  const c = e.code || "";
  if (/^Key[A-Z]$/.test(c)) return c.slice(3).toLowerCase();
  if (/^Digit[0-9]$/.test(c)) return c.slice(5);
  return CODE_FALLBACK[c] || null;
}

/// Is this event the prefix? `{ctrl: true, key: "b"}` by default.
export function isPrefix(e: KeyEventLike, prefix?: Prefix | null): boolean {
  const p = prefix || DEFAULT_PREFIX;
  if (!e.ctrlKey !== !p.ctrl) return false;
  if (e.altKey || e.metaKey) return false;
  return keyName(e).toLowerCase() === p.key;
}

/// Look a key up on the Alt layer. Returns the verb, or null to **fall
/// through** — which is the whole point of the layer.
export function altVerb(name: string): GlobalVerb | null {
  if (name.length === 1 && name >= "1" && name <= "9") {
    return GLOBAL.find((v) => v.alt === "1-9") || null;
  }
  return GLOBAL.find((v) => v.alt === name) || null;
}

/// Look a key up on the prefix layer.
export function prefixVerb(name: string): GlobalVerb | null {
  if (name.length === 1 && name >= "1" && name <= "9") {
    return GLOBAL.find((v) => v.prefix === "1-9") || null;
  }
  return GLOBAL.find((v) => v.prefix === name) || null;
}

// ---------------------------------------------------------------------------
// HOME's fleet
// ---------------------------------------------------------------------------

/// What the fleet list can do.
///
/// **Four keys, and that is the whole table** — the same four the TUI's
/// `Focus::AllAgents` has. `handle_rail_key` there answers only AGENTS and
/// PROCESSES, so the fleet has no lettered verbs at all: it navigates and it
/// opens. A `x kill` here would be a key for a thing that is not there, which
/// is the other half of the rule this file exists to keep.
///
/// `enter` is [`VerbId.OpenAgent`] rather than [`VerbId.Open`], and the
/// difference is the page: every other list stages a pane of the workspace you
/// are already in, and this one *travels* — to that agent's workspace, on that
/// agent's machine. The two cannot be the same verb, because the fleet's row is
/// the only one whose daemon may not be the active tab's.
///
/// `tab` reaches the preview and nothing brings it back, which is deliberate:
/// once the middle column has the keyboard every unmodified keystroke is the
/// agent's — `esc` and `tab` included — so the way out has to be `alt-w` or
/// `alt-esc` on the Alt layer.
export function homeVerbs(): readonly Verb[] {
  return HOME;
}
const HOME: readonly Verb[] = Object.freeze([
  verb("enter", "open", VerbId.OpenAgent),
  quiet("j", "down", VerbId.Down),
  quiet("k", "up", VerbId.Up),
  quiet("tab", "the preview", VerbId.FocusCycle),
  quiet("?", "keys", VerbId.Help),
]);

// ---------------------------------------------------------------------------
// The left rail
// ---------------------------------------------------------------------------

/// What the AGENTS section can do.
///
/// No `a`/`A` split: this client asks which type every time, so — exactly like
/// the TUI's unpinned table — `a` opens the chooser `A` would and only one of
/// them is worth drawing. No `m menu` either, because this client has no row
/// menu to open; a key for a thing that is not there would be the other half of
/// the rule broken.
///
/// `c seen` is the ✓ button stage 2 added, and it has no TUI counterpart: the
/// daemon clears a bell when a client *looks* at a pane, and the TUI's answer to
/// "I saw it and I am not going" is to walk onto the row. In a browser the rail
/// is not a cursor you park, so the answer had to be a verb.
///
/// The word is one character from being dropped: `a new... · c seen · x kill`
/// is 26 columns in 26, the same margin the TUI's PROCESSES footer has, and
/// `every_left_rail_section_fits_its_verbs` in `check.py` is what says so when a
/// rewording pushes `x kill` off the only place that key is written down.
///
/// ## Two arms, because SETTINGS can pin one
///
/// `agents_verbs(pinned)` in the Rust, and its argument holds here: `a` and `A`
/// are the same verb until an agent is pinned, and until then a footer offering
/// both is offering the same thing twice under two names. Pinned, `a` starts
/// that agent with nothing in between and `A` is the only route to the others,
/// so both are worth a column.
///
/// **What yields to make room is `c seen`**, and it is the right one: 26
/// columns is the terminal's rail, `a new · A new... · x kill` is the
/// terminal's pinned footer exactly, and `c seen` is the one verb here with no
/// counterpart there. It stays bound, stays in `?` and keeps its ✓ button — it
/// simply stops competing for a column the pin has taken. 25 in 26.
export function agentsVerbs(pinned?: boolean): readonly Verb[] {
  return pinned ? AGENTS_PINNED : AGENTS;
}
const AGENTS_TAIL: Verb[] = [
  quiet("enter", "open", VerbId.Open),
  quiet("j", "down", VerbId.Down),
  quiet("k", "up", VerbId.Up),
  quiet("tab", "next rail", VerbId.FocusCycle),
  quiet("esc", "the stage", VerbId.FocusStage),
  quiet("?", "keys", VerbId.Help),
];
const AGENTS: readonly Verb[] = Object.freeze([
  verb("a", "new...", VerbId.NewAgent),
  verb("c", "seen", VerbId.Ack),
  danger("x", "kill", VerbId.Kill),
  quiet("A", "new...", VerbId.PickAgent),
].concat(AGENTS_TAIL));
const AGENTS_PINNED: readonly Verb[] = Object.freeze([
  verb("a", "new", VerbId.NewAgent),
  verb("A", "new...", VerbId.PickAgent),
  danger("x", "kill", VerbId.Kill),
  quiet("c", "seen", VerbId.Ack),
].concat(AGENTS_TAIL));

/// What the PROCESSES section can do.
///
/// `t` rather than `n`: it is the bare spelling of `alt-t`, and `n` is already
/// "open another project" everywhere else in the workbench.
export function procsVerbs(): readonly Verb[] {
  return PROCS;
}
const PROCS: readonly Verb[] = Object.freeze([
  verb("t", "new", VerbId.NewShell),
  verb("r", "restart", VerbId.Restart),
  danger("x", "kill", VerbId.Kill),
  quiet("enter", "open", VerbId.Open),
  quiet("j", "down", VerbId.Down),
  quiet("k", "up", VerbId.Up),
  quiet("tab", "next rail", VerbId.FocusCycle),
  quiet("esc", "the stage", VerbId.FocusStage),
  quiet("?", "keys", VerbId.Help),
]);

// ---------------------------------------------------------------------------
// The changes rail
// ---------------------------------------------------------------------------

/// Which kind of row the changes rail has selected.
export const ChangesRow = Object.freeze(ids(["Conflict", "Unstaged", "Staged", "Commit", "None"]));
export type ChangesRow = (typeof ChangesRow)[keyof typeof ChangesRow];

/// Verbs for the selected row. `d` is listed once, on the rows where a diff
/// means something; `enter` runs it too but is not worth a footer slot.
export function changesRowVerbs(row: ChangesRow): readonly Verb[] {
  switch (row) {
    case ChangesRow.Conflict: return CONFLICT;
    case ChangesRow.Unstaged: return UNSTAGED;
    case ChangesRow.Staged: return STAGED;
    case ChangesRow.Commit: return COMMIT_ROW;
    default: return EMPTY;
  }
}
// A conflicted file offers the three ways out and nothing that would commit
// half a merge.
const CONFLICT: readonly Verb[] = Object.freeze([
  verb("o", "ours", VerbId.ResolveOurs),
  verb("t", "theirs", VerbId.ResolveTheirs),
  verb("a", "resolved", VerbId.ResolveDone),
  quiet("d", "diff", VerbId.Diff),
]);
const UNSTAGED: readonly Verb[] = Object.freeze([
  verb("s", "stage", VerbId.Stage),
  verb("d", "diff", VerbId.Diff),
]);
const STAGED: readonly Verb[] = Object.freeze([
  verb("u", "unstage", VerbId.Unstage),
  verb("d", "diff", VerbId.Diff),
]);
const COMMIT_ROW: readonly Verb[] = Object.freeze([verb("d", "show", VerbId.Diff)]);
const EMPTY: readonly Verb[] = Object.freeze([]);

/// What `changesAlwaysVerbs` reads: how far ahead of the upstream the branch
/// is, and whether a merge or rebase is in progress.
export type ChangesOpts = { ahead?: number; sequence?: boolean };

/// Verbs that apply whatever is selected.
///
/// `p push` earns its slot only when there is something to push, the way the
/// TUI's does; `y`/`n` appear only while a merge or rebase is in progress,
/// which is the one time the banner they answer is on screen at all.
export function changesAlwaysVerbs(opts?: ChangesOpts | null): Verb[] {
  const { ahead = 0, sequence = false }: ChangesOpts = opts || {};
  const v: Verb[] = [];
  if (sequence) {
    v.push(verb("y", "continue", VerbId.SeqContinue));
    v.push(danger("n", "abort", VerbId.SeqAbort));
  }
  if (ahead > 0) v.push(verb("p", "push", VerbId.Push));
  v.push(verb("c", "commit", VerbId.Commit));
  v.push(verb("b", "branch", VerbId.Branch));
  // Bound, documented, and not worth a column.
  v.push(quiet("C", "stage all + commit", VerbId.CommitAll));
  v.push(quiet("f", "fetch", VerbId.Fetch));
  v.push(quiet("P", "pull", VerbId.Pull));
  if (ahead === 0) v.push(quiet("p", "push", VerbId.Push));
  v.push(quiet("r", "refresh", VerbId.Refresh));
  v.push(quiet("?", "keys", VerbId.Help));
  v.push(quiet("enter", "diff", VerbId.Diff));
  v.push(quiet("j", "down", VerbId.Down));
  v.push(quiet("k", "up", VerbId.Up));
  v.push(quiet("tab", "next rail", VerbId.FocusCycle));
  v.push(quiet("esc", "the stage", VerbId.FocusStage));
  return v;
}

/// The rail's footer: the selected row's verbs, then the always-available ones.
export function changesFooter(row: ChangesRow, opts?: ChangesOpts | null): Verb[] {
  return changesRowVerbs(row).concat(changesAlwaysVerbs(opts));
}

/// Everything the rail responds to, in `?` order: navigation, then the verbs
/// for each kind of row, then the ones that always apply. Generated, so the
/// reference cannot describe a key the rail does not have.
export function changesHelpVerbs(): Verb[] {
  const v: Verb[] = [];
  for (const row of [ChangesRow.Unstaged, ChangesRow.Staged, ChangesRow.Conflict, ChangesRow.Commit]) {
    for (const x of changesRowVerbs(row)) {
      if (!v.some((e) => e.key === x.key && e.label === x.label)) v.push(x);
    }
  }
  for (const x of changesAlwaysVerbs({ ahead: 1, sequence: true })) {
    if (!v.some((e) => e.key === x.key)) v.push(x);
  }
  return v;
}

// ---------------------------------------------------------------------------
// The files page
// ---------------------------------------------------------------------------

export function filesVerbs(editing: boolean): readonly Verb[] {
  return editing ? FILES_EDITING : FILES;
}
const FILES: readonly Verb[] = Object.freeze([
  verb("enter", "open", VerbId.Open),
  verb("e", "edit", VerbId.Edit),
  verb("d", "diff", VerbId.ViewDiff),
  verb("f", "file", VerbId.ViewFile),
  quiet("y", "download", VerbId.Download),
  quiet("u", "upload", VerbId.Upload),
  danger("x", "delete", VerbId.DeleteFile),
  quiet("j", "down", VerbId.Down),
  quiet("k", "up", VerbId.Up),
  quiet("tab", "next rail", VerbId.FocusCycle),
  quiet("?", "keys", VerbId.Help),
]);
// `C-s` is the one key here that is not a bare letter: it is what every editor
// on the machine uses, and the page is an editor while this list is live.
const FILES_EDITING: readonly Verb[] = Object.freeze([
  verb("C-s", "save", VerbId.Save),
  verb("esc", "cancel", VerbId.CancelEdit),
]);

// ---------------------------------------------------------------------------
// The docker page
// ---------------------------------------------------------------------------

/// Which kind of row the docker page has selected. A stack and a container
/// answer to the same letters and mean it about different things, which is
/// precisely why they are one table with two arms rather than two tables.
export const DockerRow = Object.freeze(ids(["Stack", "Container", "None"]));
export type DockerRow = (typeof DockerRow)[keyof typeof DockerRow];

export function dockerVerbs(row: DockerRow): readonly Verb[] {
  switch (row) {
    case DockerRow.Stack: return DOCKER_STACK;
    case DockerRow.Container: return DOCKER_CONTAINER;
    default: return EMPTY;
  }
}
const DOCKER_STACK: readonly Verb[] = Object.freeze([
  verb("r", "restart", VerbId.DockerRestart),
  danger("x", "stop", VerbId.DockerStop),
  quiet("enter", "logs", VerbId.DockerLogs),
  quiet("j", "down", VerbId.Down),
  quiet("k", "up", VerbId.Up),
  quiet("tab", "next rail", VerbId.FocusCycle),
  quiet("esc", "the stage", VerbId.FocusStage),
  quiet("?", "keys", VerbId.Help),
]);
const DOCKER_CONTAINER: readonly Verb[] = Object.freeze([
  verb("enter", "logs", VerbId.DockerLogs),
  verb("s", "shell", VerbId.DockerShell),
  verb("r", "restart", VerbId.DockerRestart),
  quiet("j", "down", VerbId.Down),
  quiet("k", "up", VerbId.Up),
  quiet("tab", "next rail", VerbId.FocusCycle),
  quiet("esc", "the stage", VerbId.FocusStage),
  quiet("?", "keys", VerbId.Help),
]);

// ---------------------------------------------------------------------------
// The GIT page
// ---------------------------------------------------------------------------

/// Which kind of row one of the GIT page's two lists has selected.
///
/// A summary of the drawing's row types for the same reason [`ChangesRow`] is:
/// the drawing carries payloads this table has no use for, and the two meet
/// here. Ported from `crates/butai-client/src/verbs.rs`'s `GitRow`.
export const GitRow = Object.freeze(ids([
  "WorkingTree",
  /// A local branch that is not checked out here.
  "Branch",
  /// The checked-out branch: it cannot be switched to or deleted.
  "CurrentBranch",
  /// A local branch checked out in another worktree — git refuses to check it
  /// out twice, so the row says where it went instead of offering a verb that
  /// would fail.
  "BranchElsewhere",
  "RemoteBranch",
  "Remote",
  "Tag",
  "Stash",
  /// Another checkout of this repository.
  "Worktree",
  /// The checkout this page is looking at. There is nowhere to go.
  "ThisWorktree",
  "Commit",
  /// A heading, or a cursor past the end of a list that shrank.
  "None",
]));
export type GitRow = (typeof GitRow)[keyof typeof GitRow];

/// Verbs for the row the cursor is on.
///
/// Deliberately short. Git has about thirty operations and the `g` menu already
/// carries all of them; these are the handful that are *about this row*, which
/// is the only thing a footer can say that a menu cannot. `j`/`k` are
/// navigation here, so no verb may take them — and `g` is the menu, which is
/// why this page has no `g`-for-top.
///
/// **Nothing here mutates on `enter`.** `enter` scopes the graph or loads a
/// diff; checkout, merge, delete and drop are lettered verbs. That is the same
/// rule the Docker page already keeps — `enter follow` beside `r restart` — and
/// it is what stops this page being a second CHANGES rail.
export function gitRowVerbs(row: GitRow): readonly Verb[] {
  switch (row) {
    case GitRow.WorkingTree: return GIT_WORKING_TREE;
    case GitRow.Branch: return GIT_BRANCH;
    case GitRow.CurrentBranch: return GIT_CURRENT;
    case GitRow.BranchElsewhere: return GIT_ELSEWHERE;
    case GitRow.RemoteBranch: return GIT_REMOTE_BRANCH;
    case GitRow.Remote: return GIT_REMOTE;
    case GitRow.Tag: return GIT_TAG;
    case GitRow.Stash: return GIT_STASH;
    case GitRow.Worktree: return GIT_WORKTREE;
    case GitRow.Commit: return GIT_COMMIT;
    default: return EMPTY;   // ThisWorktree and None both offer nothing
  }
}
const GIT_WORKING_TREE: readonly Verb[] = Object.freeze([verb("enter", "changes", VerbId.GoToChanges)]);
const GIT_BRANCH: readonly Verb[] = Object.freeze([
  verb("enter", "scope", VerbId.Scope),
  verb("c", "checkout", VerbId.Checkout),
  verb("m", "merge", VerbId.Merge),
  danger("d", "delete", VerbId.DeleteBranch),
]);
// No checkout and no delete: you are standing on it.
const GIT_CURRENT: readonly Verb[] = Object.freeze([verb("enter", "scope", VerbId.Scope)]);
// Nor here — the other worktree holds it.
const GIT_ELSEWHERE: readonly Verb[] = Object.freeze([
  verb("enter", "scope", VerbId.Scope),
  verb("m", "merge", VerbId.Merge),
]);
// No `c checkout`. The value a row carries is its shorthand, and the daemon's
// checkout resolves `refs/heads/{name}` — so `origin/main` asks for a local
// branch of that name and always fails. Checking out a remote branch properly
// means creating a local one that tracks it, which is a route that does not
// exist yet; until it does, offering the verb would be advertising a failure,
// which is the rule `BranchElsewhere` already obeys. `m merge` works as-is —
// that resolves the ref, not a branch name.
const GIT_REMOTE_BRANCH: readonly Verb[] = Object.freeze([
  verb("enter", "scope", VerbId.Scope),
  verb("m", "merge", VerbId.Merge),
]);
const GIT_REMOTE: readonly Verb[] = Object.freeze([verb("f", "fetch", VerbId.Fetch)]);
const GIT_TAG: readonly Verb[] = Object.freeze([
  verb("enter", "scope", VerbId.Scope),
  danger("x", "delete", VerbId.TagDelete),
]);
const GIT_STASH: readonly Verb[] = Object.freeze([
  verb("enter", "show", VerbId.Show),
  verb("p", "pop", VerbId.StashPop),
  danger("x", "drop", VerbId.StashDrop),
]);
const GIT_WORKTREE: readonly Verb[] = Object.freeze([
  verb("enter", "open", VerbId.OpenWorktree),
  danger("x", "remove", VerbId.RemoveWorktree),
]);
const GIT_COMMIT: readonly Verb[] = Object.freeze([
  verb("enter", "diff", VerbId.Show),
  verb("y", "sha", VerbId.CopySha),
  verb("v", "revert", VerbId.Revert),
  verb("p", "pick", VerbId.CherryPick),
]);

/// Verbs that apply wherever the cursor is on this page.
export function gitAlwaysVerbs(): readonly Verb[] {
  return GIT_ALWAYS;
}
const GIT_ALWAYS: readonly Verb[] = Object.freeze([
  verb("g", "git", VerbId.GitMenu),
  verb("r", "refresh", VerbId.Refresh),
  quiet("?", "keys", VerbId.Help),
  // `docs/keys.md`'s GIT row: "esc widen the scope back". Not the stage, which
  // is what esc means on every rail — this page has no pane of its own, and
  // undoing a scope is the thing you want back.
  quiet("esc", "all refs", VerbId.ScopeAll),
  quiet("tab", "next column", VerbId.FocusCycle),
  quiet("j", "down", VerbId.Down),
  quiet("k", "up", VerbId.Up),
  // Only live while a git operation is running, which is why it is quiet: a
  // footer column for a verb that is usually dead is exactly what `quiet` is
  // for. `DELETE .../git/op` had no caller in any client before this one.
  quiet("X", "cancel the operation", VerbId.CancelOp),
]);

/// The footer for one of the page's lists: the row's verbs, then the shared
/// ones. One list so the drawing, the click hit-test and the key dispatch agree.
export function gitFooter(row: GitRow): Verb[] {
  return gitRowVerbs(row).concat(gitAlwaysVerbs());
}

/// Everything the page responds to, in `?` order. Generated by walking every
/// row kind, so a row type cannot gain a verb the reference has never heard of.
export function gitHelpVerbs(): Verb[] {
  const v: Verb[] = [];
  for (const row of [
    GitRow.WorkingTree, GitRow.Branch, GitRow.RemoteBranch, GitRow.Remote,
    GitRow.Tag, GitRow.Stash, GitRow.Worktree, GitRow.Commit,
  ]) {
    for (const x of gitRowVerbs(row)) {
      if (!v.some((e) => e.key === x.key && e.label === x.label)) v.push(x);
    }
  }
  for (const x of gitAlwaysVerbs()) if (!v.some((e) => e.key === x.key)) v.push(x);
  return v;
}

// ---------------------------------------------------------------------------
// The SETTINGS page
// ---------------------------------------------------------------------------

/// Which kind of row the settings cursor is on.
///
/// `Open` is a choice row *expanded in place* and it is its own kind rather
/// than a flag, because it is the one state where `esc` does not mean "close
/// the page": it means "keep the old one". A page whose escape key sometimes
/// leaves and sometimes reverts, with nothing saying which, is the failure the
/// terminal's `verbs()` avoids by returning early on this arm.
export const SettingRow = Object.freeze(ids(["Choice", "Open", "Toggle", "Size", "Info", "None"]));
export type SettingRow = (typeof SettingRow)[keyof typeof SettingRow];

/// The keys that work on the row the cursor is on, so a row that cannot be
/// changed does not advertise Enter. `settings.rs`'s `verbs()`, one for one.
export function settingsVerbs(row: SettingRow): readonly Verb[] {
  switch (row) {
    case SettingRow.Choice: return SETTINGS_CHOICE;
    case SettingRow.Open: return SETTINGS_OPEN;
    case SettingRow.Toggle: return SETTINGS_TOGGLE;
    case SettingRow.Size: return SETTINGS_SIZE;
    default: return SETTINGS_INFO;
  }
}
const SETTINGS_TAIL: Verb[] = [
  verb("tab", "group", VerbId.FocusCycle),
  verb("esc", "close", VerbId.CloseSettings),
  quiet("j", "down", VerbId.Down),
  quiet("k", "up", VerbId.Up),
  quiet("?", "keys", VerbId.Help),
];
const SETTINGS_INFO: readonly Verb[] = Object.freeze(SETTINGS_TAIL.slice());
const SETTINGS_CHOICE: readonly Verb[] = Object.freeze(
  [verb("enter", "change", VerbId.SettingChange)].concat(SETTINGS_TAIL));
// While the list is open the page is answering one question, so `tab` and the
// page's own `esc` are not offered: both spellings of "leave" would leave the
// preview applied without it having been chosen.
const SETTINGS_OPEN: readonly Verb[] = Object.freeze([
  verb("enter", "choose", VerbId.SettingChoose),
  verb("esc", "keep the old one", VerbId.SettingKeep),
  quiet("j", "down", VerbId.Down),
  quiet("k", "up", VerbId.Up),
]);
const SETTINGS_TOGGLE: readonly Verb[] = Object.freeze(
  [verb(" ", "toggle", VerbId.SettingToggle)].concat(SETTINGS_TAIL));
const SETTINGS_SIZE: readonly Verb[] = Object.freeze([
  verb("-", "smaller", VerbId.SettingSmaller),
  verb("+", "bigger", VerbId.SettingBigger),
  verb("0", "auto", VerbId.SettingAuto),
].concat(SETTINGS_TAIL));

/// Everything the page responds to, in `?` order. Walked over every row kind,
/// so a kind cannot gain a verb the reference has never heard of.
export function settingsHelpVerbs(): Verb[] {
  const v: Verb[] = [];
  for (const row of [
    SettingRow.Choice, SettingRow.Open, SettingRow.Toggle, SettingRow.Size, SettingRow.Info,
  ]) {
    for (const x of settingsVerbs(row)) {
      if (!v.some((e) => e.key === x.key && e.label === x.label)) v.push(x);
    }
  }
  return v;
}

// ---------------------------------------------------------------------------
// Overlays
// ---------------------------------------------------------------------------

/// What an overlay is asking. Contextual for the same reason the changes rail
/// is: `n` means "no" to a confirmation and "new folder" in a browser, and one
/// table with two arms is the only way both are true without either being a
/// letter that sometimes does the other thing.
export const OverlayKind = Object.freeze(ids(["Ask", "Picker", "List"]));
export type OverlayKind = (typeof OverlayKind)[keyof typeof OverlayKind];

/// Every modal answers `enter` and `esc`, which is what makes a modal a modal
/// rather than a page: `docs/keys.md`'s "Overlays" row, and the reason a picker
/// full of buttons needs no letter of its own.
export function overlayVerbs(kind: OverlayKind): readonly Verb[] {
  switch (kind) {
    case OverlayKind.Ask: return OVERLAY_ASK;
    case OverlayKind.List: return OVERLAY_LIST;
    default: return OVERLAY_PICKER;
  }
}
const OVERLAY_BASE: Verb[] = [
  verb("enter", "choose", VerbId.Accept),
  verb("esc", "dismiss", VerbId.Cancel),
  quiet("j", "down", VerbId.Down),
  quiet("k", "up", VerbId.Up),
];
const OVERLAY_ASK: readonly Verb[] = Object.freeze(OVERLAY_BASE.concat([
  quiet("y", "yes", VerbId.Accept),
  quiet("n", "no", VerbId.Cancel),
]));
const OVERLAY_PICKER: readonly Verb[] = Object.freeze(OVERLAY_BASE.concat([
  quiet("n", "new folder", VerbId.NewFolder),
]));
const OVERLAY_LIST: readonly Verb[] = Object.freeze(OVERLAY_BASE.concat([
  quiet("c", "dismiss one", VerbId.Clear),
  quiet("C", "dismiss all", VerbId.ClearAll),
]));

/// What each surface's section of the reference says about itself.
const SURFACE_NOTES: Readonly<Record<string, string>> = Object.freeze({
  HOME:
    "bare keys in the fleet list (alt-0 for the page, alt-w for the list). The list spans every " +
    "connected machine and each row says which one it is on; enter goes to that agent's project, " +
    "on its own machine, and puts it on the stage. The middle column is a real pane — click it or " +
    "tab to it and every key is the agent's, which is why alt-w and alt-esc are the way back out.",
  AGENTS: "bare keys, with the rail focused (alt-a)",
  PROCESSES: "bare keys, with the rail focused (alt-p)",
  CHANGES:
    "bare keys, with the rail focused (alt-g). The first few follow the row the cursor is on: " +
    "an unstaged file stages, a staged one unstages, a conflict offers the three ways out, a " +
    "commit shows. y and n appear only while a merge or rebase is in progress.",
  FILES:
    "bare keys on the files page (alt-o) and on the docs page (alt-m), which are one widget over " +
    "two listings — docs is the same tree filtered to a project's writing, with this reference as " +
    "a `reference` folder at the top of it. The last two are for while you are editing; a " +
    "reference page has no file behind it and refuses all three of edit, upload and download.",
  SETTINGS:
    "bare keys on the settings page (alt-s, or [settings] in the footer). Not a space — alt-, and " +
    "alt-. walk past it — because it is about this client rather than about a project. Moving the " +
    "cursor inside an open theme list applies that palette to the whole workbench as you walk it, " +
    "and esc there puts the old one back rather than closing the page. There is no Save: a change " +
    "applies and is written when you make it.",
  DOCKER: "bare keys on the docker page (alt-c); on a stack row, r and x act on every container in it",
  GIT:
    "bare keys on the git page (alt-r). tab walks REFS → HISTORY → the commit, and " +
    "enter only ever *reads*: it scopes the graph to a ref, loads a commit or a stash, or — on " +
    "the working-tree row — takes you to the CHANGES rail, which is where staging lives and " +
    "stays. Everything that writes the repository has a letter, and everything git can do that " +
    "is not about one row is behind g.",
  overlays:
    "any modal — a picker, a prompt, a confirmation. y/n answer a confirmation, n opens a new " +
    "folder in the file picker, and c/C dismiss one or all in the needs-you list.",
});

/// One surface's name and every verb it offers.
export type Surface = readonly [name: string, verbs: readonly Verb[]];

/// Every table on every surface, for the checks that walk all of them.
export function allSurfaces(): Surface[] {
  return [
    ["HOME", homeVerbs()],
    ["AGENTS", agentsVerbs()],
    ["PROCESSES", procsVerbs()],
    ["CHANGES", changesHelpVerbs()],
    ["FILES", filesVerbs(false).concat(filesVerbs(true))],
    ["DOCKER", dockerVerbs(DockerRow.Stack).concat(dockerVerbs(DockerRow.Container))],
    ["GIT", gitHelpVerbs()],
    ["SETTINGS", settingsHelpVerbs()],
    ["overlays", overlayVerbs(OverlayKind.Ask)
      .concat(overlayVerbs(OverlayKind.Picker))
      .concat(overlayVerbs(OverlayKind.List))],
  ];
}

// ---------------------------------------------------------------------------
// The click-target registry
// ---------------------------------------------------------------------------

/// One entry in [`TARGETS`]: the surface the thing is drawn on, and the verbs
/// that reach it.
export type Target = Readonly<{ where: string; verbs: readonly VerbId[] }>;

/// Every clickable thing in this client, and the verb that reaches it.
///
/// This is the half of the rule that has no natural home in JavaScript. The TUI
/// gets it from a `match` over `hit::Target` with no catch-all — a new
/// clickable thing does not *compile* until someone says which key reaches it.
/// Here the equivalent is that [`click`] is the only constructor for a handler
/// and it throws on an id that is not in this table, so a new clickable thing
/// does not *run*. `check.py` reads the same rule out of the source.
///
/// `verbs` names [`VerbId`]s, never letters. A key that moves therefore fails
/// the check rather than leaving a stale comment behind — the property
/// `docs/keys.md` claims for the TUI's assertions, kept here.
export const TARGETS = Object.freeze({
  // -- the tab bar and the header -----------------------------------------
  // The HOME chip is leftmost on the bar and is the only pointer route back to
  // the page, because the space buttons deliberately do not carry it.
  "tab.home": t("tab bar", [VerbId.SpaceHome]),
  "tab.select": t("tab bar", [VerbId.Workspace, VerbId.WorkspaceNext, VerbId.WorkspacePrev]),
  "tab.close": t("tab bar", [VerbId.CloseWorkspace]),
  "tab.new": t("tab bar", [VerbId.NewWorkspace]),
  "space.work": t("header", [VerbId.SpaceWork, VerbId.SpaceNext, VerbId.SpacePrev]),
  "space.files": t("header", [VerbId.SpaceFiles]),
  "space.docker": t("header", [VerbId.SpaceDocker]),
  "space.git": t("header", [VerbId.SpaceGit]),
  "space.docs": t("header", [VerbId.SpaceDocs]),
  "header.alerts": t("header", [VerbId.Alerts]),
  "header.font.bigger": t("header", [VerbId.FontBigger]),
  "header.font.smaller": t("header", [VerbId.FontSmaller]),
  "header.rails": t("header", [VerbId.Zen]),
  "work.scrim": t("the mobile drawer", [VerbId.Zen]),
  "footer.help": t("footer", [VerbId.Help]),
  // `Page::Settings`: "reached from `[settings]` in the footer or `alt-s` — a
  // peer of the workbench's own controls rather than an entry in a menu of
  // views". So it is here, beside `? keys`, and deliberately not among the
  // space buttons.
  "footer.settings": t("footer", [VerbId.SpaceSettings]),

  // -- HOME ----------------------------------------------------------------
  // A machine header and a project header are not click targets: the cursor
  // steps over them, and clicking a machine's name is not a request to open
  // somebody's agent. The tray's rows are *copies* of rows in the list below,
  // so clicking one moves the one cursor rather than being a second thing you
  // can select — which is why its verbs are the walking pair and not `open`.
  "home.row": t("HOME", [VerbId.FocusFleet, VerbId.Down, VerbId.Up]),
  "home.tray": t("HOME", [VerbId.Down, VerbId.Up]),
  "home.open": t("HOME", [VerbId.OpenAgent]),

  // -- AGENTS --------------------------------------------------------------
  // Two buttons only while an agent is pinned, because that is the only state
  // in which `a` and `A` are two verbs: unpinned, `a` opens the very chooser
  // `A` would, and a second button for a thing there is already a button for
  // is what `agents_verbs`'s comment calls out.
  "agents.new": t("AGENTS", [VerbId.NewAgent]),
  "agents.pick": t("AGENTS", [VerbId.PickAgent]),
  "agents.row": t("AGENTS", [VerbId.FocusAgents, VerbId.Down, VerbId.Up, VerbId.Open]),
  "agents.ack": t("AGENTS", [VerbId.Ack]),
  "agents.kill": t("AGENTS", [VerbId.Kill]),

  // -- PROCESSES -----------------------------------------------------------
  "procs.new": t("PROCESSES", [VerbId.NewShell]),
  "procs.row": t("PROCESSES", [VerbId.FocusProcs, VerbId.Down, VerbId.Up, VerbId.Open]),
  "procs.restart": t("PROCESSES", [VerbId.Restart]),
  "procs.kill": t("PROCESSES", [VerbId.Kill]),

  // -- CHANGES -------------------------------------------------------------
  "changes.branch": t("CHANGES", [VerbId.Branch]),
  "changes.seq.continue": t("CHANGES", [VerbId.SeqContinue]),
  "changes.seq.abort": t("CHANGES", [VerbId.SeqAbort]),
  "changes.row": t("CHANGES", [VerbId.FocusChanges, VerbId.Down, VerbId.Up, VerbId.Diff]),
  "changes.ours": t("CHANGES", [VerbId.ResolveOurs]),
  "changes.theirs": t("CHANGES", [VerbId.ResolveTheirs]),
  "changes.resolved": t("CHANGES", [VerbId.ResolveDone]),
  "changes.stage": t("CHANGES", [VerbId.Stage]),
  "changes.unstage": t("CHANGES", [VerbId.Unstage]),
  "changes.fetch": t("CHANGES", [VerbId.Fetch]),
  "changes.pull": t("CHANGES", [VerbId.Pull]),
  "changes.push": t("CHANGES", [VerbId.Push]),
  "changes.commit": t("CHANGES", [VerbId.Commit]),
  "changes.commitAll": t("CHANGES", [VerbId.CommitAll]),

  // -- the rail footers ----------------------------------------------------
  // A footer word *is* a key: the click resolves to the letter that was drawn
  // and goes through the same dispatch the keystroke does, which is what stops
  // the two from ever disagreeing.
  "rail.verb": t("a rail footer", [VerbId.Help]),

  // -- FILES ---------------------------------------------------------------
  "files.upload": t("FILES", [VerbId.Upload]),
  "files.row": t("FILES", [VerbId.Down, VerbId.Up, VerbId.Open]),
  "files.edit": t("FILES", [VerbId.Edit]),
  "files.save": t("FILES", [VerbId.Save]),
  "files.cancel": t("FILES", [VerbId.CancelEdit]),
  "files.view.file": t("FILES", [VerbId.ViewFile]),
  "files.view.diff": t("FILES", [VerbId.ViewDiff]),
  "files.download": t("FILES", [VerbId.Download]),
  "files.delete": t("FILES", [VerbId.DeleteFile]),

  // -- DOCKER --------------------------------------------------------------
  "docker.stack": t("DOCKER", [VerbId.Down, VerbId.Up, VerbId.DockerLogs]),
  "docker.container": t("DOCKER", [VerbId.Down, VerbId.Up, VerbId.DockerLogs]),
  "docker.restart": t("DOCKER", [VerbId.DockerRestart]),
  "docker.stop": t("DOCKER", [VerbId.DockerStop]),
  "docker.shell": t("DOCKER", [VerbId.DockerShell]),

  // -- GIT -----------------------------------------------------------------
  // Two lists, so two row targets. The verbs on a row target are the `enter`
  // behaviours *of that list*, because clicking a row is what `enter` does —
  // and on REFS `enter` means five different reads depending on what the row
  // is, which is precisely why they all live on one entry.
  "git.ref": t("GIT", [VerbId.Down, VerbId.Up, VerbId.Scope, VerbId.GoToChanges,
    VerbId.Show, VerbId.OpenWorktree]),
  "git.commit": t("GIT", [VerbId.Down, VerbId.Up, VerbId.Show]),
  "git.checkout": t("GIT", [VerbId.Checkout]),
  "git.merge": t("GIT", [VerbId.Merge]),
  "git.branch.delete": t("GIT", [VerbId.DeleteBranch]),
  "git.fetch": t("GIT", [VerbId.Fetch]),
  "git.tag.delete": t("GIT", [VerbId.TagDelete]),
  "git.stash.pop": t("GIT", [VerbId.StashPop]),
  "git.stash.drop": t("GIT", [VerbId.StashDrop]),
  "git.worktree.remove": t("GIT", [VerbId.RemoveWorktree]),
  "git.sha": t("GIT", [VerbId.CopySha]),
  "git.revert": t("GIT", [VerbId.Revert]),
  "git.pick": t("GIT", [VerbId.CherryPick]),
  "git.menu": t("GIT", [VerbId.GitMenu]),
  "git.refresh": t("GIT", [VerbId.Refresh]),
  // The HISTORY box's title says what the graph is a history *of*, so it is
  // also the way back out of a scope.
  "git.scope.all": t("GIT", [VerbId.ScopeAll]),
  "git.op.cancel": t("GIT", [VerbId.CancelOp]),

  // -- SETTINGS ------------------------------------------------------------
  // A group heading is a click target here and a section heading is not one on
  // the GIT page, and the difference is real: choosing a group is what `tab`
  // does, so the row stands for a verb. A row that cannot be changed still
  // takes the cursor — it is a fact you may want to read — but it offers
  // nothing beyond the walking pair, which is why `settings.row` names the four
  // acting verbs and the `Info` arm of the table binds none of them.
  "settings.group": t("SETTINGS", [VerbId.FocusCycle]),
  "settings.row": t("SETTINGS", [VerbId.Down, VerbId.Up, VerbId.SettingChange, VerbId.SettingToggle]),
  "settings.option": t("SETTINGS", [VerbId.Down, VerbId.Up, VerbId.SettingChoose]),
  "settings.keep": t("SETTINGS", [VerbId.SettingKeep]),
  "settings.bigger": t("SETTINGS", [VerbId.SettingBigger]),
  "settings.smaller": t("SETTINGS", [VerbId.SettingSmaller]),
  "settings.auto": t("SETTINGS", [VerbId.SettingAuto]),
  "settings.close": t("SETTINGS", [VerbId.CloseSettings]),

  // -- overlays ------------------------------------------------------------
  "overlay.accept": t("an overlay", [VerbId.Accept]),
  "overlay.cancel": t("an overlay", [VerbId.Cancel]),
  "overlay.row": t("an overlay", [VerbId.Down, VerbId.Up, VerbId.Accept]),
  "overlay.clear": t("an overlay", [VerbId.Clear]),
  "overlay.clearAll": t("an overlay", [VerbId.ClearAll]),
  "overlay.newFolder": t("an overlay", [VerbId.NewFolder]),
  // The new-folder row's own cancel. A second `overlay.cancel` would be found
  // ahead of the picker's, and `esc` would close the row you are not in.
  "overlay.closeRow": t("an overlay", [VerbId.Cancel]),
});

/// The name of a declared click target — every key of [`TARGETS`], as a type.
/// [`click`] and [`bind`] still take a `string` and still throw at runtime,
/// because the registry is checked at the moment the element is built; this is
/// here for a caller that would rather be told at compile time.
export type TargetId = keyof typeof TARGETS;

function t(where: string, verbs: VerbId[]): Target {
  return Object.freeze({ where, verbs: Object.freeze(verbs) });
}

/// The props [`click`] hands back — whatever the caller passed, plus the two
/// this file adds. `unknown` rather than `any`: the renderer is what knows what
/// an attribute means, and it is the one that reads them.
export type Props = Record<string, unknown>;

/// A click handler. Written for the event because most of them take it — a
/// handler that ignores its argument satisfies this too.
export type ClickHandler = (event: MouseEvent) => void;

/// The only way to put a click handler on anything.
///
/// Returns the props `h()` wants, so a call site reads
/// `h("button", click("agents.kill", fn), "✕")`. Throws for a target that is
/// not declared — loudly and at the moment the element is built, because a
/// button that quietly has no key is the exact failure this file exists to
/// prevent, and the browser smoke test would otherwise pass over it.
export function click(target: string, fn: ClickHandler, props?: Props): Props {
  assertTarget(target, "click");
  return Object.assign({}, props || {}, { "data-verb": target, onclick: fn });
}

/// The same rule for an element that already exists — the shell's static
/// markup, mostly. `bind(el, "footer.help", fn)`.
export function bind<T extends Element>(el: T | null, target: string, fn: ClickHandler): T | null {
  assertTarget(target, "bind");
  if (!el) return el;
  el.setAttribute("data-verb", target);
  // `Element`'s event map has no `click` — that one is on `HTMLElement` — so
  // this resolves to the `EventListener` overload. The cast is the widening
  // that always happens at a DOM boundary, not a claim about `fn`.
  el.addEventListener("click", fn as EventListener);
  return el;
}

/// Throws unless `target` is one of [`TARGETS`]'s own keys — an assertion
/// signature, so the lookups below need neither a cast nor a `!`.
function assertTarget(target: string, how: string): asserts target is TargetId {
  if (!Object.prototype.hasOwnProperty.call(TARGETS, target)) {
    throw new Error(
      how + '("' + target + '"): no such click target. Every clickable thing is declared in ' +
      "verbs.js's TARGETS with the verb that reaches it — nothing is reachable by pointer alone.",
    );
  }
}

/// The table a target's own surface offers, by the name the registry writes in
/// `where`. Anything that is not a surface — the tab bar, the header, the
/// footer — is reached from the workbench layer and has no bare keys of its own.
function surfaceTable(where: string): readonly Verb[] {
  switch (where) {
    case "HOME": return homeVerbs();
    case "AGENTS": return agentsVerbs();
    case "PROCESSES": return procsVerbs();
    case "CHANGES": return changesHelpVerbs();
    case "FILES": return filesVerbs(false).concat(filesVerbs(true));
    case "DOCKER": return dockerVerbs(DockerRow.Stack).concat(dockerVerbs(DockerRow.Container));
    case "GIT": return gitHelpVerbs();
    case "SETTINGS": return settingsHelpVerbs();
    case "an overlay":
      return overlayVerbs(OverlayKind.Ask)
        .concat(overlayVerbs(OverlayKind.Picker))
        .concat(overlayVerbs(OverlayKind.List));
    default: return [];
  }
}

/// One of a target's verbs and the keys that reach it.
export type TargetKey = { id: VerbId; keys: string[] };

/// Which verbs a target's keys are, resolved against the tables. Returns null
/// for a verb id its own surface does not offer, which is the drift this exists
/// to catch.
///
/// **Scoped to the surface**, not to the whole vocabulary: `Restart` is a key
/// on PROCESSES and nothing on AGENTS, so an AGENTS button pointed at it has no
/// key even though the id resolves somewhere. Searching every table was the
/// weaker check and it let exactly that through.
export function targetKeys(target: string): (TargetKey | null)[] {
  assertTarget(target, "targetKeys");
  const table = surfaceTable(TARGETS[target].where);
  return TARGETS[target].verbs.map((id) => {
    const global = GLOBAL.find((v) => v.id === id);
    if (global) {
      const spell: string[] = [];
      if (global.alt) spell.push("alt-" + global.alt);
      if (global.prefix) spell.push("C-b " + global.prefix);
      return { id, keys: spell };
    }
    const keys: string[] = [];
    for (const v of table) if (v.id === id && !keys.includes(v.key)) keys.push(v.key);
    return keys.length ? { id, keys } : null;
  });
}

// ---------------------------------------------------------------------------
// The reference
// ---------------------------------------------------------------------------

/// One line of the `?` reference: the key or keys, what they do, and whatever
/// the table had to say about the spelling.
export type ReferenceRow = { keys: string; label: string; note: string };

/// One block of the `?` reference — a surface, or one of the two prose ones.
export type ReferenceSection = { title: string; note: string; rows: ReferenceRow[] };

/// The `?` reference, generated from the tables above — "the same material
/// split by subject", which is what `docs/keys.md` says the in-app reference is.
///
/// Returns data, not markup: the caller draws it, and `check.py` reads it. A
/// hand-written panel is the thing this replaces; the old one was titled "keys
/// & layout" and listed no keys at all.
export function reference(): ReferenceSection[] {
  const sections: ReferenceSection[] = [];
  sections.push({
    title: "The two layers",
    note:
      "Alt works from inside a running program — an Alt key this client does not bind falls " +
      "through, so alt-b and alt-f still move by words in readline. The prefix (C-b) is for " +
      "when something above us has taken Alt: the browser, the OS, or a terminal in between. " +
      "Press C-b twice to send a literal one to the pane.",
    rows: GLOBAL.map((v) => ({
      keys: [v.alt ? "alt-" + v.alt : "", v.prefix ? "C-b " + v.prefix : ""].filter(Boolean).join("  ·  "),
      label: v.label,
      note: v.claim ? "⚠ " + v.claim : v.note,
    })),
  });
  const surface = (title: string, verbs: readonly Verb[], note?: string) => sections.push({
    title,
    note: note || "",
    rows: verbs.map((v) => ({ keys: keyText(v.key), label: v.label, note: v.footer ? "" : "not in the footer" })),
  });
  // Walked from `allSurfaces()` rather than written out, so a surface cannot
  // fall out of the reference while its keys keep working — which is the whole
  // failure mode a hand-written panel has.
  for (const [name, verbs] of allSurfaces()) surface(name, verbs, SURFACE_NOTES[name] || "");

  sections.push({
    title: "The pointer's alone",
    note:
      "Two gestures stand for no verb, so neither has a key: dragging to select text in the " +
      "terminal, and the wheel. Everything else on screen is in the table above — verbs.js's " +
      "TARGETS is the registry, and a button that is not in it throws rather than shipping " +
      "as something you can only click.",
    rows: [],
  });
  return sections;
}
