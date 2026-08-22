// This client's own configuration, as data.
//
// The port of `crates/butai-client/src/chrome/settings.rs`, and it carries that
// file's argument unchanged:
//
//   > Nothing on this page is the daemon's. A palette and a keymap belong to
//   > whatever is drawing, and the daemon draws no chrome — so there is no
//   > config route to call and none is invented here.
//
// Which settles where a browser client keeps its settings: **in the browser**.
// There is no config file on this side, the bridge deliberately has none
// (stage 5), and adding a daemon route for a palette the daemon cannot use
// would be inventing the thing the TUI's comment says not to invent. So the
// store is `localStorage`, and **every row names the key it writes** — the same
// rule the terminal's page follows with `[theme] name`, spelled the way you
// would type it into a devtools console.
//
// **There is no Save button**, for the terminal's reason: a change applies and
// is written when you make it, so this is not the one surface in the product
// where something you can see has not happened yet.
//
// Nothing here touches the DOM or the network — the store, the palettes and the
// row model are pure, so the tests run the lot under bun with no daemon, no
// bridge and no browser.

import { PREFIX_STORAGE_KEY } from "./verbs.ts";

const KEY = "butai.settings";

/// The prefix lives in its own key, because `keys.ts` has read it from there
/// since stage 6 and moving it would silently reset every browser that had one
/// set. Imported rather than repeated: the settings row and the dispatcher have
/// to be reading the same string, and two constants spelling one key is the
/// kind of thing that agrees until somebody changes one.

/// Every setting this client has.
///
/// Written out as a type rather than inferred from `DEFAULTS`, because the
/// defaults are one *value* of it and the store has to accept the others: the
/// theme is any name in `themeNames()`, not the literal `"system"`.
export interface Settings {
  theme: string;
  fontPx: number;
  leftRail: number;
  rightRail: number;
  zen: boolean;
  defaultAgent: string;
}

/// The two calls this module makes on `localStorage`, and nothing else.
///
/// Named rather than taking the DOM's `Storage`, because the point of passing
/// the store in is that a test can hand over an object — see `load`.
export interface SettingsStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/// Every setting this client has, and what it is when nobody has said.
///
/// `theme: "system"` is not a fallback, it is a palette: it means *follow the
/// OS*, which is exactly what `index.html`'s `prefers-color-scheme` block has
/// always done. So an untouched browser draws precisely what it drew before
/// this page existed, and choosing anything else is the only way to change it.
export const DEFAULTS: Readonly<Settings> = Object.freeze({
  theme: "system",
  fontPx: 15,
  /// 0 is `auto`: the CSS keeps its own `minmax()` and the rail breathes with
  /// the window. A number pins it, the way `[ui] left_rail` pins cells.
  leftRail: 0,
  rightRail: 0,
  zen: false,
  /// "" is "ask every time". A named constant would be an agent somebody could
  /// actually be called, which is why the terminal has `ASK_EVERY_TIME` for the
  /// *label* and `None` for the value — same split here.
  defaultAgent: "",
});

export const ASK_EVERY_TIME = "ask every time";
export const AUTO = "auto";

/// Rail widths, in px. The floor is what the CSS `minmax()` already refused to
/// go below, so a rail cannot be typed into a state it could not be dragged
/// into — the terminal's rule for `resize_rail`, in the units this client has.
export const RAIL_MIN = 180;
export const RAIL_MAX = 640;
export const RAIL_STEP = 20;
export const FONT_MIN = 8;
export const FONT_MAX = 40;

/// The prefixes offered. Three, and all three are real: `C-b` is tmux's and the
/// default, `C-a` is screen's, `C-x` is for anyone who lives in both. A free
/// text field here would let you set a prefix you cannot then press to get the
/// page back.
///
/// A tuple rather than a `string[]`, so `PREFIXES[0]` — the default, read on
/// every miss below — is a value rather than a `string | undefined`.
export const PREFIXES = Object.freeze(["C-b", "C-a", "C-x"] as const);

// ---------------------------------------------------------------------------
// Palettes
// ---------------------------------------------------------------------------

/// Every role the chrome spends a colour on, in the order the swatch grid shows
/// them.
///
/// The names are `docs/theming.md`'s, not invented ones: a theme written for
/// the terminal and a theme written for this client have to be the same
/// vocabulary, or "butai's palette" means two things.
export const ROLES = Object.freeze([
  "ground", "surface", "sunken", "selection", "ink", "muted", "faint",
  "rule", "rule_focus", "on_accent", "accent", "info", "ok", "attention",
  "danger", "status_bg", "status_fg",
] as const);

/// One of the roles above. Derived from the table rather than written twice, so
/// a palette that forgets a role and a `VARS` that forgets one are both compile
/// errors rather than an undefined colour on the page.
export type Role = (typeof ROLES)[number];

/// A palette: one colour per role.
export type Colors = Readonly<Record<Role, string>>;

export type Scheme = "dark" | "light";

/// Which CSS variable each role drives.
///
/// `index.html` names its variables for what they *are* on a web page
/// (`--panel`, `--line`); the theme names them for what they *mean*. This table
/// is the whole join between the two, so a palette is a butai palette and the
/// stylesheet stays a stylesheet.
export const VARS: Readonly<Record<Role, string>> = Object.freeze({
  ground: "--bg",
  surface: "--panel",
  sunken: "--panel2",
  rule: "--line",
  ink: "--fg",
  muted: "--dim",
  faint: "--faint",
  accent: "--accent",
  rule_focus: "--focus",
  on_accent: "--on-accent",
  selection: "--sel",
  ok: "--ok",
  attention: "--warn",
  danger: "--bad",
  info: "--run",
  status_bg: "--status-bg",
  status_fg: "--status-fg",
});

/// One palette, named and labelled.
///
/// `term_bg`/`term_fg` are absent on every theme that has no opinion, which is
/// what `termColors` falls back for.
export interface Theme {
  name: string;
  label: string;
  scheme: Scheme;
  colors: Colors;
  term_bg?: string;
  term_fg?: string;
}

/// The two variables that are not a role.
///
/// A pane's *default* foreground and background are the program's, not the
/// chrome's — `docs/theming.md`: "Pane content is never themed" — but a browser
/// has to resolve "default" to some pixel, so the palette names one. It is
/// `ground`/`ink` for every butai theme, and the two web palettes keep the
/// slightly darker terminal ground this client has always drawn, because
/// pinning `web dark` must mean *stop following the OS*, not *change colour*.
///
/// The pair is `palette.ts`'s `TermTheme`, by shape rather than by import:
/// that module imports nothing, deliberately.
export function termColors(pal: Theme): { bg: string; fg: string } {
  return { bg: pal.term_bg || pal.colors.ground, fg: pal.term_fg || pal.colors.ink };
}

function theme(
  name: string,
  label: string,
  scheme: Scheme,
  colors: Record<Role, string>,
  term?: { term_bg: string; term_fg: string },
): Theme {
  return Object.freeze(Object.assign(
    { name, label, scheme, colors: Object.freeze(colors) }, term || {}));
}

/// The palettes, and where each of them came from.
///
/// `web-dark` and `web-light` are the two this client has always drawn, lifted
/// out of `index.html` unchanged — so pinning one is "stop following the OS",
/// not "change the colours". The rest are butai's own, copied role for role
/// from `crates/butai-client/src/theme.rs` and the files in `examples/themes/`, which
/// is what makes them the *same* themes rather than five new ones with
/// familiar names.
export const THEMES: readonly Theme[] = Object.freeze([
  theme("web-dark", "web dark", "dark", {
    ground: "#0b0e13", surface: "#161b22", sunken: "#1b212b", selection: "#1f6feb22",
    ink: "#d7dde5", muted: "#8b949e", faint: "#6e7681", rule: "#262d38",
    rule_focus: "#1f6feb44", on_accent: "#04070d", accent: "#58a6ff", info: "#58a6ff",
    ok: "#3fb950", attention: "#d29922", danger: "#f85149",
    status_bg: "#161b22", status_fg: "#8b949e",
  }, { term_bg: "#0e1116", term_fg: "#d7dde5" }),
  theme("web-light", "web light", "light", {
    ground: "#ffffff", surface: "#f6f8fa", sunken: "#eaeef2", selection: "#0969da1a",
    ink: "#1f2328", muted: "#656d76", faint: "#8c959f", rule: "#d0d7de",
    rule_focus: "#0969da44", on_accent: "#ffffff", accent: "#0969da", info: "#0969da",
    ok: "#1a7f37", attention: "#9a6700", danger: "#cf222e",
    status_bg: "#f6f8fa", status_fg: "#656d76",
  }, { term_bg: "#ffffff", term_fg: "#1f2328" }),
  theme("blueprint-dark", "blueprint dark", "dark", {
    ground: "#151a23", surface: "#1b2230", sunken: "#10151d", selection: "#1f2535",
    ink: "#dde4ef", muted: "#8d9aae", faint: "#66738a", rule: "#2b3547",
    rule_focus: "#7aa2f7", on_accent: "#151a23", accent: "#7aa2f7", info: "#7aa2f7",
    ok: "#9ece6a", attention: "#e0af68", danger: "#f7768e",
    status_bg: "#1b2230", status_fg: "#8d9aae",
  }),
  theme("blueprint-light", "blueprint light", "light", {
    ground: "#e9edf3", surface: "#f7f9fc", sunken: "#dfe5ee", selection: "#dbe2ee",
    ink: "#1b2331", muted: "#5c6980", faint: "#8a95a8", rule: "#c6cfdd",
    rule_focus: "#2f56b8", on_accent: "#f7f9fc", accent: "#2f56b8", info: "#2f56b8",
    ok: "#4a7c2a", attention: "#9c6407", danger: "#b3261e",
    status_bg: "#dfe5ee", status_fg: "#5c6980",
  }),
  theme("tokyonight", "tokyonight", "dark", {
    ground: "#1a1b26", surface: "#1f2335", sunken: "#16161e", selection: "#292e42",
    ink: "#c0caf5", muted: "#a9b1d6", faint: "#565f89", rule: "#3b4261",
    rule_focus: "#7aa2f7", on_accent: "#1a1b26", accent: "#7aa2f7", info: "#7dcfff",
    ok: "#9ece6a", attention: "#e0af68", danger: "#f7768e",
    status_bg: "#1f2335", status_fg: "#c0caf5",
  }),
  theme("gruvbox-dark", "gruvbox dark", "dark", {
    ground: "#282828", surface: "#32302f", sunken: "#1d2021", selection: "#504945",
    ink: "#ebdbb2", muted: "#bdae93", faint: "#928374", rule: "#504945",
    rule_focus: "#83a598", on_accent: "#282828", accent: "#83a598", info: "#8ec07c",
    ok: "#b8bb26", attention: "#fabd2f", danger: "#fb4934",
    status_bg: "#3c3836", status_fg: "#ebdbb2",
  }),
  theme("catppuccin-mocha", "catppuccin mocha", "dark", {
    ground: "#1e1e2e", surface: "#313244", sunken: "#181825", selection: "#45475a",
    ink: "#cdd6f4", muted: "#a6adc8", faint: "#6c7086", rule: "#45475a",
    rule_focus: "#89b4fa", on_accent: "#1e1e2e", accent: "#89b4fa", info: "#89dceb",
    ok: "#a6e3a1", attention: "#f9e2af", danger: "#f38ba8",
    status_bg: "#181825", status_fg: "#a6adc8",
  }),
  theme("solarized-light", "solarized light", "light", {
    ground: "#fdf6e3", surface: "#fdf6e3", sunken: "#eee8d5", selection: "#eee8d5",
    ink: "#586e75", muted: "#657b83", faint: "#93a1a1", rule: "#93a1a1",
    rule_focus: "#268bd2", on_accent: "#fdf6e3", accent: "#268bd2", info: "#2aa198",
    ok: "#859900", attention: "#b58900", danger: "#dc322f",
    status_bg: "#eee8d5", status_fg: "#657b83",
  }),
]);

/// `system` first, because it is the default and because it is the only entry
/// that is not a palette — it is the instruction to keep following the OS.
export const SYSTEM = "system";

export function themeNames(): string[] {
  return ([SYSTEM] as string[]).concat(THEMES.map((t) => t.name));
}

export function themeByName(name: string): Theme | null {
  return THEMES.find((t) => t.name === name) || null;
}

/// The palette to *draw* for a setting, given what the OS is asking for.
///
/// `system` resolves to one of the two this client shipped with, so the swatch
/// grid and the preview can show real colours for it rather than a blank —
/// "follow the OS" is a choice about which of two palettes, not an absence of
/// one.
///
/// Which is also why this answers a `Theme` and never `null`: the two `!`s are
/// `web-light` and `web-dark` being in `THEMES` above, and the second is the
/// `themeByName(name)` the branch above just proved.
export function resolveTheme(name: string, prefersDark?: boolean): Theme {
  if (name === SYSTEM || !themeByName(name)) {
    return themeByName(prefersDark === false ? "web-light" : "web-dark")!;
  }
  return themeByName(name)!;
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// Read the settings out of a storage-shaped object.
///
/// Takes the storage rather than reaching for `window.localStorage`, so the
/// whole store is testable under bun — and so a browser that throws outright
/// on `localStorage` (Safari's private mode does) falls back to the defaults
/// instead of taking the client down with it.
export function load(storage: SettingsStorage | null | undefined): Settings {
  const out: Settings = Object.assign({}, DEFAULTS);
  let raw: string | null | undefined = null;
  try { raw = storage && storage.getItem(KEY); } catch { return out; }
  if (!raw) return out;
  let obj: unknown = null;
  try { obj = JSON.parse(raw); } catch { return out; }
  if (!obj || typeof obj !== "object") return out;
  // The cast is a claim about the *shape* of what was stored, not its values,
  // and `sanitize` on the next line is what makes it true.
  return sanitize(Object.assign(out, obj as Partial<Settings>));
}

/// Every value clamped to something this client can actually draw.
///
/// A stored setting is *user input* — it survives a version, it can be edited
/// by hand, and one bad number is a rail 20000px wide with no way back to the
/// page that would fix it. So the read is where the clamping lives, not the
/// write. The types say `Partial<Settings>` and the runtime checks stay anyway:
/// the type describes what a *caller* may pass, and this function's real domain
/// is whatever JSON was in `localStorage`.
export function sanitize(s: Partial<Settings> | null | undefined): Settings {
  const out: Settings = Object.assign({}, DEFAULTS, s || {});
  if (!themeNames().includes(out.theme)) out.theme = DEFAULTS.theme;
  out.fontPx = clampInt(out.fontPx, FONT_MIN, FONT_MAX, DEFAULTS.fontPx);
  out.leftRail = railWidth(out.leftRail);
  out.rightRail = railWidth(out.rightRail);
  out.zen = !!out.zen;
  out.defaultAgent = typeof out.defaultAgent === "string" ? out.defaultAgent : "";
  return out;
}

function railWidth(v: unknown): number {
  const n = Math.round(Number(v) || 0);
  if (!n) return 0;
  return Math.max(RAIL_MIN, Math.min(RAIL_MAX, n));
}

function clampInt(v: unknown, lo: number, hi: number, dflt: number): number {
  const n = Math.round(Number(v));
  if (!isFinite(n)) return dflt;
  return Math.max(lo, Math.min(hi, n));
}

export function save(storage: SettingsStorage | null | undefined, s: Settings): Settings {
  try { storage && storage.setItem(KEY, JSON.stringify(sanitize(s))); } catch { /* full or refused */ }
  return s;
}

export function readPrefixSpelling(storage: SettingsStorage | null | undefined): string {
  let raw: string | null | undefined = null;
  try { raw = storage && storage.getItem(PREFIX_STORAGE_KEY); } catch { return PREFIXES[0]; }
  const spell = String(raw || "").trim();
  return (PREFIXES as readonly string[]).includes(spell) ? spell : PREFIXES[0];
}

export function writePrefixSpelling(storage: SettingsStorage | null | undefined, spell: string): void {
  try { storage && storage.setItem(PREFIX_STORAGE_KEY, spell); } catch { /* refused */ }
}

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

/// What a row does when it is pressed. The terminal's `Kind`, one for one.
export const RowKind = Object.freeze({
  Choice: "Choice", Toggle: "Toggle", Size: "Size", Info: "Info",
} as const);
export type RowKind = (typeof RowKind)[keyof typeof RowKind];

/// Which setting a row is.
///
/// The page matches on this rather than on "row 0 of the APPEARANCE group", so
/// inserting a setting above another cannot silently reassign what Enter does
/// to it — the terminal's `RowId`, and its reason.
export const RowId = Object.freeze({
  Theme: "Theme", Font: "Font", DefaultAgent: "DefaultAgent", Prefix: "Prefix",
  LeftRail: "LeftRail", RightRail: "RightRail", Rails: "Rails",
  /// Read-only. Several rows share it because none of them acts.
  Fact: "Fact",
} as const);
export type RowId = (typeof RowId)[keyof typeof RowId];

/// One group of settings — a row in the list down the left.
export const GroupId = Object.freeze({
  Appearance: "Appearance", Agents: "Agents", Workbench: "Workbench",
  Machines: "Machines", Keys: "Keys", About: "About",
} as const);
export type GroupId = (typeof GroupId)[keyof typeof GroupId];

/// One row: a setting, or a fact about the machine you are on.
export interface Row {
  id: RowId;
  label: string;
  key: string;
  desc: string;
  value: string;
  kind: RowKind;
  /// Only a `Choice` has them.
  options?: string[];
}

export interface Group {
  id: GroupId;
  label: string;
  rows: Row[];
}

/// One machine, as this page is *told* about it.
///
/// A subset of the bridge's `DaemonDto` on purpose: the client assembles these
/// rows out of `/api/daemons` and the live snapshot's error for the same key
/// (see `settingsFacts` in the view layer), so what arrives here is the four
/// fields the MACHINES group draws and not the whole record.
export interface MachineFact {
  label: string;
  primary?: boolean;
  socket?: string;
  error?: string | null;
}

/// Everything this page can only be told, rather than read from the store.
export interface Facts {
  /// The daemon's configured agent types, unioned across machines.
  agents?: readonly string[];
  daemons?: readonly MachineFact[];
  prefix?: string;
  bindings?: number;
  fallThrough?: readonly string[];
  clientVersion?: string;
  daemonVersion?: string | null;
  origin?: string;
}

function info(label: string, key: string, value: string, desc: string): Row {
  return { id: RowId.Fact, label, key, desc, value, kind: RowKind.Info };
}

/// Columns the label and the key column hold, from the terminal's `LABEL_W` and
/// `KEY_W`.
///
/// The browser's columns are elastic and would happily take more; the point of
/// keeping the budget is that a key clipped to `butai.settings · defau…` is one
/// you cannot search your own storage for, which is the entire job that column
/// has, and a page that lets its labels grow past the terminal's is a page that
/// has quietly stopped being the same page.
export const LABEL_W = 20;
export const KEY_W = 29;

/// Every group and row, built fresh from the live state.
///
/// Built rather than cached, exactly as the terminal builds it: the font also
/// moves from `alt-=`, the rails also move from `alt-z`, and a page holding its
/// own copy of either would show a number the rest of the client had moved on
/// from.
///
/// `facts` is everything this page can only be *told*: the daemon's agent
/// types, the roster the bridge dialled, the versions. None of it is a setting
/// — it is drawn as a fact, which is what the terminal does with `GET
/// /v1/agents` for the same reason.
export function groups(s: Partial<Settings> | null | undefined, facts: Facts | null | undefined): Group[] {
  const f = facts || {};
  const st = sanitize(s);
  const px = (n: number): string => (n ? n + " px" : AUTO);
  const machines = f.daemons || [];

  return [
    {
      id: GroupId.Appearance,
      label: "APPEARANCE",
      rows: [
        {
          id: RowId.Theme,
          label: "theme",
          key: "butai.settings · theme",
          desc: "The palette every part of the chrome draws from.",
          value: st.theme,
          kind: RowKind.Choice,
          options: themeNames(),
        },
        {
          id: RowId.Font,
          label: "terminal font",
          key: "butai.settings · fontPx",
          desc: "Cell size on the stage. alt-= and alt-- move the same number.",
          value: st.fontPx + " px",
          kind: RowKind.Size,
        },
        info(
          "palettes",
          "",
          THEMES.length + " built in",
          "This client's own: the daemon renders no chrome, so it has none to give.",
        ),
      ],
    },
    {
      id: GroupId.Agents,
      label: "AGENTS",
      rows: [
        {
          id: RowId.DefaultAgent,
          label: "default agent",
          key: "butai.settings · defaultAgent",
          desc: "What `a` and [+] spawn with nothing in between. `A` still picks.",
          value: st.defaultAgent || ASK_EVERY_TIME,
          kind: RowKind.Choice,
          options: ([ASK_EVERY_TIME] as string[]).concat(f.agents || []),
        },
        info(
          "available agents",
          "[[agents]]",
          (f.agents || []).length ? (f.agents || []).join(", ") : "none configured",
          "The daemon's own, not this client's. Its config file defines them.",
        ),
      ],
    },
    {
      id: GroupId.Workbench,
      label: "WORKBENCH",
      rows: [
        {
          id: RowId.LeftRail,
          label: "left rail",
          key: "butai.settings · leftRail",
          desc: "Agents, processes and the gauges. `auto` lets it breathe with the window.",
          value: px(st.leftRail),
          kind: RowKind.Size,
        },
        {
          id: RowId.RightRail,
          label: "right rail",
          key: "butai.settings · rightRail",
          desc: "The CHANGES rail.",
          value: px(st.rightRail),
          kind: RowKind.Size,
        },
        {
          id: RowId.Rails,
          label: "start collapsed",
          key: "butai.settings · zen",
          desc: "Open with both rails folded away, which is what alt-z does now.",
          value: st.zen ? "on" : "off",
          kind: RowKind.Toggle,
        },
      ],
    },
    {
      id: GroupId.Machines,
      label: "MACHINES",
      // Facts, every one of them, and that is the honest shape rather than a
      // shortfall. The bridge reads its daemon list from `BUTAI_SOCKETS` at
      // startup and has no route that accepts another; a browser holding its
      // own list would be holding sockets it cannot open — it never touches a
      // daemon socket, the bridge does. So this group says what was dialled and
      // where to change it, and offers nothing it could not honour.
      rows: (machines.length
        ? machines.map((d) => info(
          d.primary ? "machine (primary)" : "machine",
          "BUTAI_SOCKETS",
          d.label + (d.error ? " — unreachable" : "") + (d.socket ? "  " + d.socket : ""),
          "Dialled by the bridge at startup, so the machine is in the bar every morning.",
        ))
        : [info("machines", "BUTAI_SOCKETS", "none", "The bridge has not answered with a roster yet.")]
      ).concat([
        info(
          "adding one",
          "",
          "restart the bridge",
          "The list is the bridge's environment; this client has no socket of its own.",
        ),
      ]),
    },
    {
      id: GroupId.Keys,
      label: "KEYS",
      rows: [
        {
          id: RowId.Prefix,
          label: "prefix",
          key: "butai.prefix",
          desc: "The key that opens a prefix binding, tmux-style. Twice sends a literal one.",
          value: f.prefix || PREFIXES[0],
          kind: RowKind.Choice,
          options: PREFIXES.slice(),
        },
        info(
          "bindings",
          // Left spelling `.js` on purpose: it is a string on the page, and
          // changing what a row *reads* is a behavioural change. `web/verbs.js`
          // is also still there — the two older clients import it.
          "web/verbs.js",
          (f.bindings || 0) + " bound, none from a config",
          "The same tables `?` is generated from. This client has no keymap file.",
        ),
        info(
          "never taken",
          "",
          (f.fallThrough || []).map((k) => "alt-" + k).join(" "),
          "Alt keys left for the program, so readline's word motions still work.",
        ),
      ],
    },
    {
      id: GroupId.About,
      label: "ABOUT",
      rows: [
        info(
          "client",
          "",
          "butai " + (f.clientVersion || "?"),
          "Client and daemon agree a protocol version at the handshake.",
        ),
        info(
          "daemon",
          "",
          f.daemonVersion || "not yet — nothing has been staged",
          "The build string off the stage's hello, not the protocol number.",
        ),
        info(
          "settings",
          "butai.settings",
          "localStorage · " + (f.origin || "this origin"),
          "This browser, this origin. Every row above writes one key in it.",
        ),
      ],
    },
  ];
}

/// Where the cursor is, once the lists have moved underneath it.
export interface Cursor {
  group: number;
  row: number;
}

/// Which group and row the cursor is really on, after the lists have moved
/// underneath it. Clamping in one place, so nothing downstream indexes past the
/// end of a group that lost a machine.
export function clampCursor(grps: readonly Group[], group: number, row: number): Cursor {
  const g = Math.max(0, Math.min(grps.length - 1, group | 0));
  const rows = grps[g]?.rows || [];
  return { group: g, row: Math.max(0, Math.min(rows.length - 1, row | 0)) };
}

/// The next value a `-`/`+`/`0` press produces, for a size row.
///
/// A function rather than three call sites, because the clamping is the whole
/// content of the rule: `auto` steps into the middle of the range rather than
/// to the floor, so pressing `+` on an automatic rail does not first make it
/// narrower than it was.
export function stepSize(id: RowId, value: number, delta: number): number {
  if (id === RowId.Font) {
    if (delta === 0) return DEFAULTS.fontPx;
    return clampInt(value + delta, FONT_MIN, FONT_MAX, DEFAULTS.fontPx);
  }
  if (delta === 0) return 0;
  if (!value) return delta > 0 ? 320 : 260;
  return Math.max(RAIL_MIN, Math.min(RAIL_MAX, value + delta * RAIL_STEP));
}
