// SETTINGS — this client's own configuration, as a page you enter, change and
// leave.
//
// The port of `web/ui/page-settings.js`, which is the port of
// `web/butai-settings.js`, which is the port of
// `crates/butai-client/src/chrome/settings.rs`. The argument survives all four
// and is what makes this a page rather than the modal it would obviously have
// been:
//
//   > **Why a page and not a modal.** A modal is right for one question whose
//   > whole answer fits on screen. Settings is six groups of them, and one of
//   > the six cannot be answered in a box at all: the only way to judge a
//   > palette is to see what it does to a screen.
//
//   > **Every row names the key it writes.** The label is for the reader and
//   > the faint mono text beside it is the actual storage key — the line you
//   > would edit by hand.
//
//   > **There is no Save button.** A change applies and is written when you
//   > make it, so this is not the one surface in the product where something
//   > you can see has not happened yet.
//
// Nothing here decides anything. The store, the sanitiser, the palettes and the
// group/row table are `logic/settings.ts` — imported, never restated — the keys
// come from `logic/verbs.ts`'s `settingsVerbs()`, and a palette reaches the page
// through `theme.ts`, which is the only bridge between the two. So the footer
// advertises exactly the keys this page binds, clicking one runs the verb the
// key does, and a palette lives in one place and is shown here rather than
// copied.
//
// ## Colour, and the one file allowed to know any
//
// **There are no colour literals here.** The swatch grid reads `ROLES` and the
// resolved palette's own values; the sample screen is painted from the same
// object. `composite()` parses the palette's hex into protocol cells, which is
// reading a colour rather than writing one.
//
// The palette under the cursor is the *applied* one, not the stored one:
// walking an open theme list themes the whole page as you go, and drawing the
// swatches from the setting instead would show you the old palette while every
// other pixel on screen is in the new one. `esc` puts the old one back, which
// is the feature a modal cannot have — the modal is covering the thing you are
// trying to look at.
//
// ## MACHINES can now add and remove one
//
// This is the one behavioural change, and it is the one `web/README.md` had
// already written the epitaph for: *"The bridge does now accept another machine
// at runtime (`POST /api/daemons`); what this page has not grown yet is a
// surface for it, so the group still points at the environment. The row that
// says `restart the bridge` is the one that goes when it does."* This is that
// client, so that row goes and the form below the list replaces it.
//
// **`source` is what decides whether a row offers a remove.** An `env` entry
// comes back on the next restart whatever happens here, so removing one would
// be a gesture that silently undoes itself — worse than a refusal, because it
// looks like it worked. `server/roster.ts` refuses it for exactly that reason
// and says so; the row does not offer it in the first place, which is the same
// fact one step earlier.
//
// ## What the port changed, which is otherwise only ever geometry
//
// `web/UI-REWRITE.md`'s audit found four bugs on this page and every one of
// them is a shape rather than a behaviour: 43px rows against 23 on the git
// page; two selection styles for one concept (a light-blue band in the sidebar,
// a blue bordered box in the detail pane); blue caps for its own headers where
// every other page used grey; and a hint bar scoped to the middle column. They
// are `Row`, `Row`, `SectionTitle` and a page-wide `HintBar`. The description
// keeps its own line under each row and *outside* the selection band, which is
// where the terminal draws it and why walking the list does not make it jump.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { ScrollArea } from "@/components/ui/scroll-area";
import { HintBar } from "@/components/HintBar";
import { Row } from "@/components/Row";
import { SectionTitle } from "@/components/SectionTitle";
import { cn } from "@/lib/utils";

import type { DaemonEntry, World } from "@/app/world.ts";
import type { QualifiedWorkspace } from "@/logic/events.ts";
import { CLIENT_VERSION } from "@/logic/protocol.ts";
import {
  ASK_EVERY_TIME,
  GroupId,
  KEY_W,
  LABEL_W,
  ROLES,
  RowId,
  RowKind,
  SYSTEM,
  clampCursor,
  groups,
  load,
  readPrefixSpelling,
  sanitize,
  save,
  stepSize,
  termColors,
  writePrefixSpelling,
  type Facts,
  type Row as SettingsRow,
  type Settings,
  type Theme,
} from "@/logic/settings.ts";
import {
  ALT_MUST_FALL_THROUGH,
  GLOBAL,
  SettingRow,
  VerbId,
  keyName,
  keyText,
  settingsVerbs,
} from "@/logic/verbs.ts";
import { storage, storeTheme, useTheme } from "@/theme.ts";
import { Screen } from "@/stage/Screen.ts";
import type { Cell, CellRun, Color, Mods } from "@/protocol/generated/protocol.ts";

// The two columns the terminal budgets, in the unit that makes them the same
// budget: `1ch` is one advance width, so `LABEL_W`ch and `KEY_W`ch are the
// terminal's two columns. Written as a style rather than as `w-[20ch]` for the
// reason `Row` writes its indent inline — Tailwind cannot generate a class for
// a width it never sees spelled out, and spelling it out is a second copy of
// `LABEL_W` that nothing would ever catch drifting.
//
// The budget is kept rather than let out, because a key clipped to
// `butai.settings · defau…` is one you cannot search your own storage for,
// which is the entire job that column has.
const LABEL_COL = { width: `${LABEL_W}ch` } as const;
const KEY_COL = { width: `${KEY_W}ch` } as const;

// …and where the rows stop, which is `settings.rs`'s `BODY_MAX_W` — the cursor
// marker, the two columns, a value column and a margin. Values are set hard
// right, and the terminal capped this because without it a wide screen drew a
// row's label and its value with a hundred columns of nothing in between: two
// halves of one fact, too far apart to take in together. A browser window is
// wider than any terminal, so the cap matters more here, not less.
const BODY_MAX = { maxWidth: `${3 + LABEL_W + KEY_W + 55 + 3}ch` } as const;

/**
 * What this page can only be *told*, rather than read out of the store or off
 * the world.
 *
 * Two fields, and they are the two nothing else on the page can reach: the
 * daemon's agent types need `GET /agents` per machine, and the daemon's build
 * string arrives on the stage's `hello`. Everything else in `settings.ts`'s
 * `Facts` — the roster, the prefix, the bindings, the versions, the origin — is
 * derived below, because a page that took them as props would be a page whose
 * caller could hand it a roster the world disagreed with.
 */
export interface SettingsFacts {
  /** The daemon's configured agent types, unioned across machines. */
  agents?: readonly string[] | undefined;
  /** The build string off the stage's hello, not the protocol number. */
  daemonVersion?: string | null | undefined;
}

/**
 * The one field of an action's answer this page reads.
 *
 * `actions.ts` reports through a toast and returns — no method throws, so no
 * page needs a `try` — and every one of them answers a structural
 * `{ok?, running?, summary?}`, or `null` when the call threw or a confirmation
 * was declined. This page needs `ok` for exactly one thing: **a socket path
 * that was refused has to stay in the field.** 403 for a directory outside
 * `BUTAI_SOCKET_DIRS` and 404 for a forward that is not up yet are both fixed
 * by editing what you typed, and a field that empties itself on a refusal is a
 * path you retype from memory.
 */
export interface ActionAnswer {
  ok?: boolean | null | undefined;
}

/**
 * The slice of `src/app/actions.ts` this page calls.
 *
 * Declared structurally rather than imported, the way every other page declares
 * its own: the page names exactly what it needs and `Actions` satisfies it by
 * shape. Both routes are the *bridge's* own rather than a daemon's, which is
 * how a page whose whole point is that nothing on it is the daemon's can still
 * have two writes on it.
 */
export interface SettingsActions {
  /** `POST /api/daemons` — connect one more machine, by socket path. */
  addDaemon(socket: string, name?: string): Promise<ActionAnswer | null | undefined>;
  /** `DELETE /api/daemons/{key}` — drop one this bridge dialled at runtime. */
  removeDaemon(key: string): Promise<ActionAnswer | null | undefined>;
}

/** View-state changes. Nothing here reaches a daemon. */
export interface SettingsCallbacks {
  /** Leave. `esc`, and the `close` button in the header. */
  close(): void;
}

export interface SettingsPageProps {
  /**
   * Every daemon, so MACHINES cannot disagree with the tab bar. The workspaces
   * are unread: SETTINGS is about the client, which is exactly why
   * `Page::Settings` is not one of the spaces.
   */
  world: World;
  actions: SettingsActions;
  on: SettingsCallbacks;
  /** The two things this page can only be told. Absent draws the same page. */
  facts?: SettingsFacts | undefined;
  /** Taken and ignored, so the shell can render any page without knowing which. */
  ws?: QualifiedWorkspace | null | undefined;
  focus?: string | undefined;
}

/**
 * Which arm of `settingsVerbs()` the cursor is on.
 *
 * The row says what it is; the footer, the key dispatch and the drawing all
 * read this one fact, so a row cannot advertise a key that does nothing to it.
 */
function rowKindOf(row: SettingsRow | undefined, open: boolean): SettingRow {
  if (open) return SettingRow.Open;
  if (!row) return SettingRow.None;
  if (row.kind === RowKind.Choice) return SettingRow.Choice;
  if (row.kind === RowKind.Toggle) return SettingRow.Toggle;
  if (row.kind === RowKind.Size) return SettingRow.Size;
  return SettingRow.Info;
}

// The arrow keys are the same two verbs `j` and `k` are, spelled the way the
// browser spells them. `verbs.ts` names the pair once and this is the only
// place the second spelling exists — a table with both in it would advertise
// four keys for two verbs in the footer of every list in the product.
const ARROW: Readonly<Record<string, VerbId | undefined>> = {
  arrowdown: VerbId.Down,
  arrowup: VerbId.Up,
};

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

export function SettingsPage({ world, actions, on, facts: told }: SettingsPageProps) {
  const [settings, setSettings] = useState<Settings>(() => load(storage()));
  const [prefix, setPrefix] = useState<string>(() => readPrefixSpelling(storage()));
  const [group, setGroup] = useState(0);
  const [row, setRow] = useState(0);
  /**
   * A choice row expanded in place, and which option is highlighted. In place
   * rather than in a modal: a page that opens a modal to answer one of its own
   * rows is a page that did not need to be a page.
   */
  const [open, setOpen] = useState<number | null>(null);
  /**
   * The theme the cursor is *on*, while a theme list is open. Not a setting
   * until it is chosen — which is what makes walking the list a preview and
   * `esc` an undo.
   */
  const [preview, setPreview] = useState<string | null>(null);

  const pal = useTheme(preview || settings.theme);

  // Everything the row table can only be told, assembled from what the page can
  // see. The roster is the world's, so MACHINES cannot disagree with the tab
  // bar; the rest are constants this client owns.
  const facts: Facts = useMemo(
    () => ({
      agents: told?.agents ?? [],
      daemons: world.daemons.map((d) => ({
        label: d.label,
        primary: d.primary,
        socket: d.socket,
        error: d.error,
      })),
      prefix,
      bindings: GLOBAL.length,
      fallThrough: ALT_MUST_FALL_THROUGH.slice(0, 4),
      clientVersion: CLIENT_VERSION,
      daemonVersion: told?.daemonVersion ?? null,
      origin: typeof window === "undefined" ? "this origin" : window.location.origin,
    }),
    [told?.agents, told?.daemonVersion, world.daemons, prefix],
  );

  const grps = useMemo(() => machineGroup(groups(settings, facts), world.daemons), [settings, facts, world.daemons]);
  const at = clampCursor(grps, group, row);
  // `groups()` returns six groups and `clampCursor` just clamped into them, so
  // this is a fact the compiler cannot see rather than an assumption.
  const grp = grps[at.group]!;
  const current = grp.rows[at.row];
  const appearance = grp.id === GroupId.Appearance;
  const machines = grp.id === GroupId.Machines ? world.daemons : null;
  const verbs = settingsVerbs(rowKindOf(current, open != null));

  // The cursor is also the browser's focus, once the keyboard has been used.
  // `Row` draws the same ring for `focus-visible` as it does for `selected`, so
  // this adds no second affordance — what it adds is the row scrolling itself
  // into view, which is what stops walking a nine-entry theme list off the
  // bottom of a short window. Not on load: a page that grabs focus and scrolls
  // before you have touched it is a page that moved on its own.
  const listEl = useRef<HTMLDivElement | null>(null);
  const walked = useRef(false);
  useEffect(() => {
    if (!walked.current || !listEl.current) return;
    const sel = open != null ? `[data-opt="${open}"]` : `[data-row="${at.row}"]`;
    const el = listEl.current.querySelector<HTMLElement>(sel);
    if (el && el !== document.activeElement) el.focus();
  }, [at.row, at.group, open]);

  // -- writing, which is the whole of "there is no Save button" --------------
  const write = useCallback((patch: Partial<Settings>) => {
    setSettings((prev) => {
      const next = sanitize({ ...prev, ...patch });
      save(storage(), next);
      return next;
    });
  }, []);

  const closeList = useCallback(() => {
    setOpen(null);
    setPreview(null);
  }, []);

  /**
   * Every path out of an open list that is not "choose" comes through here,
   * which is what makes the preview a preview: dropping `preview` re-resolves
   * the palette from the stored setting and `useTheme` repaints `<html>`.
   */
  const keepOld = useCallback(() => {
    closeList();
  }, [closeList]);

  const selectGroup = useCallback(
    (i: number) => {
      closeList();
      setGroup(i);
      setRow(0);
    },
    [closeList],
  );

  /**
   * `enter` on the row the cursor is on. Opens a choice in place, toggles a
   * toggle, and does nothing at all on a fact — which is why the `Info` arm of
   * the verb table does not offer it.
   */
  const press = useCallback(
    (i: number | null) => {
      const c = clampCursor(grps, group, i == null ? row : i);
      setRow(c.row);
      const r = grps[c.group]?.rows[c.row];
      if (!r) return;
      if (r.kind === RowKind.Choice) {
        const options = r.options ?? [];
        const found = options.indexOf(String(r.value));
        const start = found < 0 ? 0 : found;
        setOpen(start);
        if (r.id === RowId.Theme) setPreview(options[start] ?? null);
        return;
      }
      // A row that cannot open a list must not leave one open behind it.
      // Clicking a fact while a theme list is expanded used to leave `open` set
      // on a row with no options: nothing was drawn, but the footer read
      // `enter choose · esc keep the old one` and the keys went with it.
      closeList();
      if (r.kind === RowKind.Toggle && r.id === RowId.Rails) write({ zen: !settings.zen });
    },
    [grps, group, row, settings.zen, write, closeList],
  );

  /** Keep the highlighted option: write it, and stop previewing. */
  const choose = useCallback(
    (i: number | null) => {
      const options = current?.options ?? [];
      const pick = i == null ? open : i;
      const value = pick == null ? undefined : options[pick];
      if (value == null || !current) return;
      closeList();
      if (current.id === RowId.Theme) {
        // Through `theme.ts`, which is the only thing under `src/` that knows
        // what a palette is — and through the same `localStorage` key every
        // other client writes, so choosing a theme on either themes both.
        storeTheme(value);
        setSettings(load(storage()));
      } else if (current.id === RowId.DefaultAgent) {
        write({ defaultAgent: value === ASK_EVERY_TIME ? "" : value });
      } else if (current.id === RowId.Prefix) {
        // Its own key, because `keys.ts` has read it from there since stage 6
        // and moving it would silently reset every browser that had one set.
        writePrefixSpelling(storage(), value);
        setPrefix(value);
      }
    },
    [current, open, closeList, write],
  );

  /**
   * `-`, `+` and `0` on a size row. `0` is *auto*, which is a value and not a
   * reset: the CSS keeps its own `minmax()` and the rail breathes.
   */
  const size = useCallback(
    (delta: number) => {
      if (!current || current.kind !== RowKind.Size) return;
      if (current.id === RowId.Font) write({ fontPx: stepSize(RowId.Font, settings.fontPx, delta) });
      if (current.id === RowId.LeftRail) write({ leftRail: stepSize(RowId.LeftRail, settings.leftRail, delta) });
      if (current.id === RowId.RightRail) write({ rightRail: stepSize(RowId.RightRail, settings.rightRail, delta) });
    },
    [current, settings, write],
  );

  /**
   * The cursor moved. While a theme list is open that *is* the preview: the
   * palette on screen is a function of where the cursor is, which is the
   * feature a page has and a modal does not.
   */
  const move = useCallback(
    (delta: number) => {
      walked.current = true;
      if (open != null) {
        const options = current?.options ?? [];
        const next = Math.max(0, Math.min(options.length - 1, open + delta));
        setOpen(next);
        if (current?.id === RowId.Theme) setPreview(options[next] ?? null);
        return;
      }
      setRow(Math.max(0, Math.min(grp.rows.length - 1, at.row + delta)));
    },
    [open, current, grp, at.row],
  );

  const leave = useCallback(() => {
    keepOld();
    on.close();
  }, [keepOld, on]);

  // -- one verb table, for the keyboard and for the footer -------------------
  // A verb the footer draws and a verb the keyboard runs are the same entry in
  // `settingsVerbs()`, dispatched through one map. That is what stops the two
  // halves of a surface teaching different keys, and it is why `HintBar`'s
  // entries are buttons rather than labels.
  //
  // It answers whether the verb was ours, because a key this page does not act
  // on has to reach the page under it rather than being swallowed by a
  // `preventDefault` for nothing — `?` is bound here in the table and answered
  // by a help page this client has not ported yet.
  const run = useCallback(
    (id: VerbId): boolean => {
      switch (id) {
        case VerbId.SettingChange: press(null); return true;
        case VerbId.SettingChoose: choose(null); return true;
        case VerbId.SettingKeep: keepOld(); return true;
        case VerbId.SettingToggle: press(null); return true;
        case VerbId.SettingSmaller: size(-1); return true;
        case VerbId.SettingBigger: size(1); return true;
        case VerbId.SettingAuto: size(0); return true;
        case VerbId.FocusCycle: selectGroup((at.group + 1) % grps.length); return true;
        case VerbId.CloseSettings: leave(); return true;
        case VerbId.Down: move(1); return true;
        case VerbId.Up: move(-1); return true;
        default: return false;
      }
    },
    [press, choose, keepOld, size, selectGroup, at.group, grps.length, leave, move],
  );

  // No dependency array on purpose: the listener closes over `run`, and a
  // listener registered once would go on running the first render's cursor.
  // Re-binding a single keydown per render is cheaper than the class of bug
  // that gets you.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // A row and a button already handled it. `Row` calls `preventDefault()`
      // before its `onSelect`, so without this an `enter` on a focused row is
      // dispatched twice — once by the row, once here — and a toggle toggles
      // back.
      if (e.defaultPrevented || e.altKey || e.ctrlKey || e.metaKey) return;
      // In a field every key is the field's. The page this was ported from had
      // no text input on it and so did not need this; MACHINES has two, and
      // without the guard typing a socket path walks the list, steps the font
      // and toggles the rails on the way through. `keys.ts` reads the composed
      // path for the same reason and this is the same test.
      if (isTyping(e)) return;
      const name = keyName(e);
      const verb = verbs.find((v) => v.key === name);
      const id = verb ? verb.id : ARROW[name];
      if (id && run(id)) e.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  return (
    <div className="flex h-full min-h-0 flex-col bg-background">
      <div className="flex min-h-0 flex-1">
        <aside className="flex w-52 shrink-0 flex-col border-r border-border bg-card">
          <SectionTitle>settings</SectionTitle>
          <ScrollArea className="min-h-0 flex-1">
            <div role="listbox" aria-label="settings groups">
              {grps.map((g, i) => (
                <Row key={g.id} selected={i === at.group} onSelect={() => selectGroup(i)}>
                  <span className="min-w-0 flex-1 truncate">{g.label}</span>
                  <Badge variant="outline">{g.rows.length}</Badge>
                </Row>
              ))}
            </div>
          </ScrollArea>
          <div className="shrink-0 border-t border-border px-3 py-1 text-11 text-faint">
            <div className="truncate font-mono">butai.settings</div>
            <div className="truncate">saved on change</div>
          </div>
        </aside>

        <main className="flex min-w-0 flex-1 flex-col">
          <SectionTitle
            action={
              <Button size="sm" variant="outline" onClick={leave} title="Back where you came from (esc)">
                close
              </Button>
            }
          >
            {grp.label}
          </SectionTitle>
          <ScrollArea className="min-h-0 flex-1">
            <div ref={listEl} className="min-w-0 py-2" style={BODY_MAX}>
              <div role="listbox" aria-label={grp.label}>
                {grp.rows.map((r, i) => (
                  <Setting
                    key={`${r.id || "row"}/${i}`}
                    row={r}
                    index={i}
                    selected={i === at.row}
                    open={i === at.row ? open : null}
                    onSelect={() => press(i)}
                    onChoose={choose}
                    onKeep={keepOld}
                    onSize={size}
                    action={
                      machines?.[i] ? (
                        <MachineAction daemon={machines[i]!} onRemove={actions.removeDaemon} />
                      ) : null
                    }
                  />
                ))}
              </div>
              {machines ? <AddMachine onAdd={actions.addDaemon} /> : null}
            </div>
          </ScrollArea>
        </main>

        {appearance && pal ? (
          <aside className="hidden w-80 shrink-0 flex-col border-l border-border bg-card xl:flex">
            <SectionTitle>palette</SectionTitle>
            <div className="px-3 py-2 text-11 text-faint">every role the chrome spends a colour on</div>
            <div className="grid grid-cols-2 gap-x-3 px-3 pb-2">
              {ROLES.map((role) => (
                <div key={role} className="flex h-4 min-w-0 items-center gap-2 text-11 text-faint">
                  <Swatch colour={pal.colors[role]} />
                  <span className="min-w-0 truncate">{role}</span>
                </div>
              ))}
            </div>
            <PaletteSample className="min-h-0 flex-1 border-t border-border" palette={pal} fontPx={settings.fontPx} />
          </aside>
        ) : null}
      </div>

      <HintBar
        keys={verbs
          .filter((v) => v.footer)
          .map((v) => ({ key: keyText(v.key), label: v.label, danger: v.danger, onSelect: () => run(v.id) }))}
      />
    </div>
  );
}

// What a **runtime** machine's row names and says, instead of what
// `settings.ts` wrote for it.
//
// That module writes every machine row as an environment entry, and it is right
// to: `MachineFact` carries a label, a socket and an error, so it cannot know
// otherwise. A machine you connected two minutes ago claiming it was dialled
// from `BUTAI_SOCKETS` at startup is a row pointing you at a variable that does
// not mention it — the exact failure "every row names the key it writes" exists
// to prevent. The page has `source`, so the page supplies the two fields that
// depend on it, and this moves back into `settings.ts` the moment `MachineFact`
// grows one. See `HANDOVER-settings.md`.
const RUNTIME_KEY = "POST /api/daemons";
const RUNTIME_DESC = "Connected while the bridge was running, so it goes on the next restart.";

/**
 * `settings.ts`'s MACHINES group, with the roster's own facts folded back in.
 *
 * Two things happen here and both come from `source`, which the row table
 * cannot see.
 *
 * **The trailing row goes.** That group is one row per machine — in the order
 * the facts were handed to it — followed by one row reading
 * `adding one · restart the bridge`. The bridge grew `POST /api/daemons`, so
 * that sentence is now wrong, and it is the sentence `web/README.md` said would
 * go when a client grew the surface. The form under the list is the surface.
 *
 * **A runtime row is relabelled**, per `RUNTIME_KEY` above.
 *
 * The trim is on the *model* rather than on the drawing, so `j`/`k` do not walk
 * a row that is not on screen — and it is also what makes row `i` of this group
 * provably `world.daemons[i]`, which is the correspondence the remove button
 * hangs on. With an empty roster the group is one placeholder row
 * (`machines · none`) plus the trailing row, and only the trailing row goes.
 *
 * Everything else is `settings.ts`'s, untouched: the labels, the
 * `— unreachable` marker and the sockets are still written in one place.
 */
function machineGroup(grps: ReturnType<typeof groups>, daemons: readonly DaemonEntry[]): ReturnType<typeof groups> {
  return grps.map((g) => {
    if (g.id !== GroupId.Machines) return g;
    const rows = g.rows.slice(0, Math.max(1, daemons.length)).map((r, i) => {
      const d = daemons[i];
      return d?.source === "runtime" ? { ...r, key: RUNTIME_KEY, desc: RUNTIME_DESC } : r;
    });
    return { ...g, rows };
  });
}

/** Whether the keystroke belongs to a text field rather than to the page. */
function isTyping(e: KeyboardEvent): boolean {
  const path = e.composedPath ? e.composedPath() : [];
  return path.some((n) => n instanceof Element && (n.tagName === "INPUT" || n.tagName === "TEXTAREA"));
}

// ---------------------------------------------------------------------------
// One setting
// ---------------------------------------------------------------------------

/**
 * A row, its key, its value — and the description on its own line under it,
 * outside the band, which is where the terminal draws it and why walking the
 * list does not make it jump.
 */
function Setting({
  row,
  index,
  selected,
  open,
  onSelect,
  onChoose,
  onKeep,
  onSize,
  action,
}: {
  row: SettingsRow;
  index: number;
  selected: boolean;
  open: number | null;
  onSelect: () => void;
  onChoose: (i: number) => void;
  onKeep: () => void;
  onSize: (delta: number) => void;
  /** The right-hand slot. A machine's `remove`, and nothing else so far. */
  action?: React.ReactNode;
}) {
  const editable = row.kind !== RowKind.Info;
  return (
    <div className="min-w-0">
      <Row selected={selected} onSelect={editable ? onSelect : undefined} data-row={index}>
        <span
          style={LABEL_COL}
          className={cn("shrink-0 truncate", editable ? "text-foreground" : "text-faint")}
        >
          {row.label}
        </span>
        {/* Mono, because it is a storage key you would type character for
            character into a console — the kit's test for the face. */}
        <span style={KEY_COL} className="hidden shrink-0 truncate font-mono text-11 text-faint lg:block">
          {row.key}
        </span>
        <Value row={row} onSize={onSize} />
        {action}
        {row.kind === RowKind.Choice ? <span className="shrink-0 text-primary">›</span> : null}
      </Row>
      <div className="truncate px-3 pb-1 text-11 text-faint">{row.desc}</div>
      {open != null && row.kind === RowKind.Choice ? (
        <div className="pl-6" role="listbox" aria-label={row.label}>
          {(row.options ?? []).map((opt, i) => (
            <Row key={opt} selected={i === open} onSelect={() => onChoose(i)} data-opt={i}>
              <span className="min-w-0 truncate">{opt}</span>
              {String(opt) === String(row.value) ? <Badge variant="outline">current</Badge> : null}
              {opt === SYSTEM ? <span className="shrink-0 text-11 text-faint">follow the OS</span> : null}
            </Row>
          ))}
          <Row onSelect={onKeep}>
            <span className="text-11 text-faint">esc — keep the old one</span>
          </Row>
        </div>
      ) : null}
    </div>
  );
}

/**
 * The value, hard right and `tabular-nums`, so a group of them is a column.
 *
 * A size row gets the three keys as buttons beside it, because `-` `+` `0` are
 * keys you have to be told about and a pointer has to be able to reach every
 * verb — the two halves of "nothing is reachable by pointer alone, and nothing
 * is bound that cannot be found".
 */
function Value({ row, onSize }: { row: SettingsRow; onSize: (delta: number) => void }) {
  if (row.kind === RowKind.Size) {
    const step = (delta: number) => (e: React.MouseEvent) => {
      // The row is the cursor and the button is the verb. Without this the
      // click reaches both and the row under the cursor moves as you press `+`.
      e.stopPropagation();
      onSize(delta);
    };
    return (
      <span className="ml-auto flex shrink-0 items-center gap-1">
        <span className="tabular-nums text-foreground">{row.value}</span>
        <Button size="sm" variant="outline" onClick={step(-1)} title="smaller (-)">
          −
        </Button>
        <Button size="sm" variant="outline" onClick={step(1)} title="bigger (+)">
          +
        </Button>
        <Button size="sm" variant="outline" onClick={step(0)} title="auto (0)">
          auto
        </Button>
      </span>
    );
  }
  const on = row.kind === RowKind.Toggle && row.value === "on";
  return (
    <span
      className={cn(
        "ml-auto min-w-0 truncate whitespace-nowrap tabular-nums",
        on ? "text-ok" : row.kind === RowKind.Info ? "text-dim" : "text-foreground",
      )}
    >
      {row.value}
    </span>
  );
}

// ---------------------------------------------------------------------------
// MACHINES
// ---------------------------------------------------------------------------

/**
 * The right-hand slot on a machine row: a remove, or the reason there is none.
 *
 * **`source` is the whole of it.** An entry the environment configured comes
 * back on the next restart whatever happens here, so `Roster.remove` refuses it
 * with that sentence — and a button that exists only to be refused is a button
 * that teaches you the rule by failing. The row says it instead.
 */
function MachineAction({ daemon, onRemove }: { daemon: DaemonEntry; onRemove: SettingsActions["removeDaemon"] }) {
  const [busy, setBusy] = useState(false);
  if (daemon.source !== "runtime") {
    return (
      <span
        className="shrink-0 text-11 text-faint"
        title={`${daemon.key} is in this bridge's environment, so removing it here would come back on the next restart — change BUTAI_SOCKET/BUTAI_SOCKETS instead`}
      >
        from the environment
      </span>
    );
  }
  return (
    <Button
      size="sm"
      variant="ghost"
      className="shrink-0 text-bad hover:text-bad"
      disabled={busy}
      title={`Drop ${daemon.key} from this bridge. Its panes keep running; nothing on the machine is touched.`}
      onClick={(e) => {
        // The row is the cursor and the button is the verb.
        e.stopPropagation();
        setBusy(true);
        void onRemove(daemon.key).then(() => setBusy(false));
      }}
    >
      remove
    </Button>
  );
}

/**
 * Connect one more machine, by socket path.
 *
 * A socket path and not a host, because **the bridge does not run ssh**: a far
 * daemon becomes a local path through `ssh -N -L <local>:<remote-socket> host`,
 * after which nothing can tell it from a second daemon on this box. That is the
 * same sentence `server/index.ts` refuses a `host` field with, said before the
 * refusal rather than after it.
 *
 * The fields are a `<form>`, so `enter` submits and `tab` reaches every part of
 * it — which is how this surface satisfies "nothing is reachable by pointer
 * alone" without a verb. `verbs.ts` has no entry for adding a machine and this
 * page cannot add one; see `HANDOVER-settings.md`.
 */
function AddMachine({ onAdd }: { onAdd: SettingsActions["addDaemon"] }) {
  const [open, setOpen] = useState(false);
  const [socket, setSocket] = useState("");
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    const path = socket.trim();
    const label = name.trim();
    if (!path || busy) return;
    setBusy(true);
    void onAdd(path, label || undefined).then((r) => {
      setBusy(false);
      // Only a machine that is actually on the roster clears the form. A
      // refusal is a path you are about to edit, not one to retype — and the
      // refusal itself is already on screen as a toast.
      if (!r || r.ok === false) return;
      setSocket("");
      setName("");
      setOpen(false);
    });
  };

  if (!open) {
    return (
      <div className="flex min-w-0 items-center gap-3 px-3 pt-2">
        <Button size="sm" variant="outline" onClick={() => setOpen(true)}>
          + connect a machine
        </Button>
        <span className="min-w-0 truncate text-11 text-faint">
          A socket already reachable on this host — forward a far one first.
        </span>
      </div>
    );
  }

  return (
    <form className="flex min-w-0 flex-col gap-2 px-3 pt-2" onSubmit={submit}>
      <div className="flex min-w-0 items-center gap-2">
        <label style={LABEL_COL} className="shrink-0 truncate text-13" htmlFor="machine-socket">
          socket
        </label>
        <Input
          id="machine-socket"
          className="h-row min-w-0 flex-1 font-mono text-12"
          placeholder="/run/forwards/gpu-box.sock"
          autoFocus
          value={socket}
          onChange={(e) => setSocket(e.target.value)}
        />
      </div>
      <div className="flex min-w-0 items-center gap-2">
        <label style={LABEL_COL} className="shrink-0 truncate text-13" htmlFor="machine-name">
          name
        </label>
        <Input
          id="machine-name"
          className="h-row min-w-0 flex-1 text-12"
          placeholder="derived from the path"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
      </div>
      <div className="flex items-center gap-2">
        <span style={LABEL_COL} className="shrink-0" aria-hidden="true" />
        <Button size="sm" type="submit" disabled={busy || !socket.trim()}>
          {busy ? "connecting…" : "connect"}
        </Button>
        <Button size="sm" variant="ghost" onClick={() => setOpen(false)}>
          cancel
        </Button>
        <span className="min-w-0 truncate text-11 text-faint">
          Forward a far daemon with <code className="font-mono">ssh -N -L</code> and pass that path.
        </span>
      </div>
    </form>
  );
}

// ---------------------------------------------------------------------------
// The palette, shown rather than described
// ---------------------------------------------------------------------------

/**
 * One role's colour.
 *
 * **This belongs in `src/components/`** — `KIT.md` says a shape a page needs is
 * added there with its reason and never inlined, and the old kit had a `Swatch`
 * for exactly this. It is here because this pass may not write to that
 * directory; see `HANDOVER-settings.md`.
 *
 * The colour is an inline style because it is a value at runtime, which is the
 * same reason `Row` writes its indent inline: Tailwind cannot generate a class
 * for a colour it never sees. It is still not a literal — it is the palette's
 * own value, read back.
 */
function Swatch({ colour }: { colour?: string | undefined }) {
  return (
    <span
      aria-hidden="true"
      className="size-3 shrink-0 rounded-sm border border-border"
      style={colour ? { background: colour } : undefined}
    />
  );
}

/**
 * The palette sample: not a picture of the theme.
 *
 * The same `Screen` the stage draws with, the same cell grid, the same
 * `resolveColor`. Half of it is written in explicit colours — the roles a
 * screen would otherwise never show you — and half in `default`, which is what
 * a program's own output uses, so the two halves answer the two different
 * questions a palette raises.
 *
 * `preview: true` is what makes it safe to put beside a list you are walking:
 * no tab stop, no grab for the keyboard, nothing sent. A sample terminal that
 * takes the caret out of the list is worse than no sample at all.
 *
 * It also follows `terminal font`, which the page it was ported from did not:
 * that setting is two rows above this panel and its whole effect is the cell
 * size, so showing it here is the same argument the palette makes.
 */
function PaletteSample({
  palette,
  fontPx,
  className,
}: {
  palette: Theme;
  fontPx: number;
  className?: string | undefined;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const hostRef = useRef<HTMLDivElement | null>(null);
  const screenRef = useRef<Screen | null>(null);
  // The renderer outlives every render and reads the palette at call time, so a
  // new palette does not tear the canvas down — it repaints it.
  const live = useRef(palette);
  live.current = palette;

  const paint = useCallback(() => {
    const s = screenRef.current;
    if (!s || !s.cols || !s.rows) return;
    s.refreshTheme();
    s.applyFrame({ full: true, cells: sampleCells(live.current, s.cols, s.rows), cursor: null });
  }, []);

  useEffect(() => {
    const canvas = canvasRef.current;
    const host = hostRef.current;
    if (!canvas || !host) return undefined;
    const screen = new Screen(canvas, {
      onMessage: () => {},
      getTheme: () => termColors(live.current),
      preview: true,
      fontPx,
    });
    screenRef.current = screen;
    // `Screen` blanks its buffer when the grid changes, so a resize is a
    // repaint. Its own observer is registered first and therefore runs first,
    // which is what makes `cols`/`rows` current by the time this fires.
    const ro = new ResizeObserver(() => paint());
    ro.observe(host);
    paint();
    return () => {
      ro.disconnect();
      screen.destroy();
      screenRef.current = null;
    };
    // Mount only: `fontPx` is the renderer's starting size and the effect below
    // carries every later value, the same split `Stage` uses.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    screenRef.current?.setFontPx(fontPx);
    paint();
  }, [fontPx, paint]);

  // Every render, because the palette is a prop and a prop change is the only
  // signal this gets. Repainting a 40x20 grid costs less than the bookkeeping
  // to decide not to.
  useEffect(paint);

  return (
    <div className={cn("relative min-h-0 min-w-0", className)}>
      <div ref={hostRef} className="absolute inset-0">
        <canvas ref={canvasRef} className="absolute inset-0 block h-full w-full" />
      </div>
    </div>
  );
}

/** A CSS hex's three channels. */
function channels(hex: string): [number, number, number] {
  const s = hex.replace("#", "");
  const full = s.length === 3 ? [...s].map((ch) => ch + ch).join("") : s;
  const v = parseInt(full.slice(0, 6), 16) || 0;
  return [(v >> 16) & 255, (v >> 8) & 255, v & 255];
}

/**
 * A CSS hex as a protocol colour, composited over `over` when it carries alpha.
 *
 * Named for what it does rather than `rgb`, which reads to a colour-literal
 * scan as a CSS colour being written down here. It writes none: every value it
 * returns came out of the palette.
 *
 * `selection` carries an alpha channel in the two web palettes — a wash the
 * chrome lays over whatever is behind it, which CSS does for free and a cell
 * grid does not. Truncating to six digits drew the cursor row as a solid blue
 * bar four shades off anything on screen, so the alpha is composited here
 * against the palette's own ground: the same pixel the browser would have
 * produced.
 */
function composite(hex: string, over: string): Color {
  const s = hex.replace("#", "");
  const full = s.length === 3 ? [...s].map((ch) => ch + ch).join("") : s;
  const [r, g, b] = channels(full);
  if (full.length !== 8) return { rgb: [r, g, b] };
  const a = parseInt(full.slice(6, 8), 16) / 255;
  const [br, bg, bb] = channels(over);
  const mix = (x: number, y: number) => Math.round(x * a + y * (1 - a));
  return { rgb: [mix(r, br), mix(g, bg), mix(b, bb)] };
}

/**
 * The sample screen, as protocol cell runs.
 *
 * A miniature of the workbench rather than a colour chart: the roles are
 * semantic (`ok` means "fine", not "green"), so the only honest preview is one
 * that spends them on the things they are actually spent on — a waiting agent,
 * a modified file, a diff.
 */
function sampleCells(pal: Theme, cols: number, rows: number): CellRun[] {
  const c = pal.colors;
  const out: CellRun[] = [];
  const put = (y: number, x: number, text: string, fg?: string, bg?: string, mods?: Mods) => {
    if (y >= rows) return;
    const cells: Cell[] = [];
    for (const ch of text.slice(0, Math.max(0, cols - x))) {
      cells.push({
        ch,
        fg: fg ? composite(fg, c.ground) : "default",
        bg: bg ? composite(bg, c.ground) : "default",
        ...(mods ? { mods } : {}),
      });
    }
    if (cells.length) out.push({ x, y, cells });
  };
  const bar = (y: number, bg: string) => put(y, 0, " ".repeat(cols), undefined, bg);

  bar(0, c.sunken);
  put(0, 1, "butai", c.accent, c.sunken, { bold: true });
  put(0, 7, " alpha ", c.on_accent, c.accent, { bold: true });
  put(0, 15, " bravo ", c.muted, c.sunken);
  put(2, 1, "AGENTS", c.accent, undefined, { bold: true });
  put(3, 1, "[?] claude    waiting on you", c.danger);
  bar(4, c.selection);
  put(4, 1, "[~] codex     working", c.attention, c.selection);
  put(5, 1, "[ ] aider     idle", c.muted);
  put(7, 1, "CHANGES", c.accent, undefined, { bold: true });
  put(8, 1, " M src/main.rs", c.attention);
  put(9, 1, " A README.md", c.ok);
  put(10, 1, " ? notes.txt", c.danger);
  // Default fg and bg: what a program's own output looks like, which is the
  // half of the palette the chrome above cannot show.
  put(12, 1, "$ git diff");
  put(13, 1, "@@ -1,4 +1,4 @@", c.info);
  put(14, 1, "+ the line that arrived", c.ok);
  put(15, 1, "- the line that left", c.danger);
  put(17, 1, "the rest is your program's own colours, never themed", c.faint);
  bar(rows - 1, c.status_bg);
  put(rows - 1, 1, "alpha · 3 agents · 1 proc", c.status_fg, c.status_bg);
  return out;
}
