// The FILES page — a Finder-style trail of directories and a file — and DOCS,
// which is this same page over a second listing.
//
// The port of `web/ui/files.js`, itself the port of `<butai-files>`: a directory
// browser on the left (`GET .../tree`, one directory at a time), and on the
// right the file itself (`.../file`), its diff (`.../diff`), an editor over it,
// or rendered markdown.
//
// ## The browser is a trail of columns, not an expanding tree
//
// It was an indented tree: click a folder and its contents appear underneath,
// pushed right. That reads well two levels down and stops reading at four — the
// path you are on is a diagonal you have to trace by eye through everything you
// opened on the way, and every folder you ever expanded stays on screen
// competing with it.
//
// The Finder's answer is columns, and it is the one this page takes now.
// Every directory on the path from the workspace root to where you are is a
// column of its own, side by side, with the row you came through still marked in
// each. Where you are is the shape of the whole thing rather than an indent
// level you have to count, and the columns you are *not* in are still lists you
// can reach back into with one click.
//
// `←`/`→` walk it and `space` peeks at a file without leaving the browser — the
// same four keys the terminal binds, from the same table, because the two
// clients teaching different keys is the thing `verbs.ts` exists to stop.
//
// ## …and the DOCS page, which is this widget over a second listing
//
// `Page::Docs` is "the FILES page filtered to markdown: a project's own writing,
// without the code it is about", and the terminal implements it by drawing one
// widget over two `Files` structs. Here that is one component with
// `kind="docs"`, mounted twice: two instances have two trees, two selections and
// two scroll positions without anything holding a pair of them.
//
// So there is one tree in this client and one file viewer, and DOCS adds exactly
// three things to them: `docs.ts`'s filter, the built-in `reference` folder at
// the root, and a body that renders markdown instead of printing it.
//
// ## Markdown is data, never markup
//
// `docs.ts`'s `readMarkdown` returns **blocks** and `Prose` builds elements from
// them — text nodes, no `innerHTML`, nowhere in the path. That property is the
// reason the parser returns data at all, and it matters because the daemon hands
// this page whatever is in somebody's repository: a `<script>` in a README is a
// string on screen and can never be anything else.
//
// ## What this port changes on top of the vanilla one
//
// The rows are the kit's `Row` with its `indent` rather than a hand-computed
// `paddingLeft`, both headers are `SectionTitle` (the old page used a card
// header on one side and a section title on the other, which is two of the four
// styles the audit counted), the markdown is `Prose`, the editor is `Textarea`,
// and the diff is the same `Patch` the GIT page draws.
//
// One thing that is not cosmetic: a row carries its click **target** but not a
// second `onClick`. `Row` composes a caller's `onClick` with its own `onSelect`,
// so the vanilla pairing — `onSelect=${fn}` *and* `vclick(target, fn)` — would
// fire `fn` twice here, which on a directory is an expand and an immediate
// collapse. See `verbTarget` below.

import { useEffect, useMemo, useRef, useState } from "react";

import { Code } from "@/components/Code";
import { Empty } from "@/components/Empty";
import { HintBar, type Hint } from "@/components/HintBar";
import { Minimap } from "@/components/Minimap";
import { Patch } from "@/components/Patch";
import { Path } from "@/components/Path";
import { Prose } from "@/components/Prose";
import { Row } from "@/components/Row";
import { SectionTitle } from "@/components/SectionTitle";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Textarea } from "@/components/ui/textarea";

import { api } from "@/logic/api.ts";
import {
  REFERENCE_DIR,
  docsRows,
  isBuiltin,
  readMarkdown,
  rendersAsMarkdown,
  topicFor,
  type Block,
  type DocRow,
} from "@/logic/docs.ts";
import { RAIL_COLS } from "@/logic/dom.ts";
import type { QualifiedWorkspace } from "@/logic/events.ts";
import {
  ROOT,
  type Trail,
  here as hereOf,
  holds,
  into,
  left,
  point,
  rowIn,
  trim,
} from "@/logic/trail.ts";
import {
  MAX_ROWS,
  VerbId,
  click,
  filesVerbs,
  fits,
  keyName,
  keyText,
  type TargetId,
} from "@/logic/verbs.ts";

/**
 * The one write this page has, and the two ways in.
 *
 * The reads below are the page's own — a directory listing, a file, a diff —
 * because the daemon serves them and inventing an aggregate route for one client
 * is the side channel this refactor exists to remove. The *write* is not: it
 * needs the toast and the world re-read that follow it, and those are the
 * shell's policy rather than this page's. The editor's Save and the tree's
 * upload are one verb here because they are one route on the daemon.
 *
 * `upload` answers whether the bytes landed, which is the one thing a
 * fire-and-forget action cannot say and this page needs: a save that failed must
 * leave the editor open, with the draft still in it.
 *
 * Required rather than defaulted: the vanilla `wire()` filled a missing action
 * in with a function that warned at runtime, and a type is the same guarantee
 * made at compile time instead.
 */
export interface FilesActions {
  /** Write `blob` to `path`, relative to the workspace's cwd. */
  upload(req: { path: string; blob: Blob }): Promise<boolean>;
  /**
   * Delete `path`. Owns its own question — confirmation is a property of the
   * operation, as `actions.ts` puts it — and answers whether the file actually
   * went, so a refusal does not clear the selection out from under the reader.
   */
  deleteFile(path: string): Promise<boolean>;
  /**
   * The page's own refusals — "this page is built into the client", "file too
   * large to edit". Not the outcome of a write: `upload` reports its own.
   */
  toast(message: string): void;
}

export interface FilesPageProps {
  /** The current workspace, already qualified, or null when none is open. */
  ws: QualifiedWorkspace | null;
  /** The *listing*, not the drawing — see the header. */
  kind?: "files" | "docs" | undefined;
  /**
   * The workbench prefix as the user spells it (`C-b`). The reference's own
   * pages are written with it, so a reader is told the keys *they* have.
   */
  prefix?: string | undefined;
  actions: FilesActions;
}

/** One directory's listing, or why it has none. */
interface Listing {
  entries?: readonly DocRow[] | undefined;
  error?: string | undefined;
}

/** Pixels one column of the trail takes. */
const COL_W = 208;

/** What the right-hand column is showing. */
type Body =
  | { kind: "loading" }
  | { kind: "error"; text: string }
  | { kind: "patch"; text: string }
  | { kind: "text"; text: string; truncated: boolean }
  | { kind: "md"; blocks: readonly Block[]; title?: string | undefined };

function dirOf(path: string): string {
  const i = path.lastIndexOf("/");
  return i >= 0 ? path.slice(0, i) : "";
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/**
 * The click registry, in React's spelling — for a control that *is* the click.
 *
 * `verbs.ts`'s `click()` is "the only way to put a click handler on anything",
 * and it throws for a target that is not declared in `TARGETS`, which is what
 * stops a button existing with no key that reaches it. Its own return is
 * `h()`'s spelling (`onclick`, which React drops with a warning), so the
 * assertion is what is kept and the props are rewritten.
 *
 * `data-verb` is not decoration: `keys.ts` dispatches a verb by finding the
 * element carrying its target and clicking it, so an element without one is a
 * verb the keyboard cannot reach.
 */
function verbClick(target: TargetId, run: () => void) {
  click(target, run);
  return { "data-verb": target, onClick: run };
}

/**
 * The same assertion for a `Row`, which already owns its activation.
 *
 * A `Row` composes a caller's `onClick` with its own `onSelect`, so handing it
 * both is two calls per click — and `keys.ts` reaches the row by clicking the
 * element, which goes through `onSelect` either way. So the row takes the
 * target and nothing else.
 */
function verbTarget(target: TargetId, run: () => void) {
  click(target, run);
  return { "data-verb": target };
}

/**
 * The name a column wears above it.
 *
 * The workspace root has no basename, so it is `/` — except on DOCS, where the
 * root of the trail *is* the docs listing and saying so is the one thing that
 * tells the two pages apart at a glance.
 */
export function columnLabel(dir: string, docs: boolean): string {
  if (!dir) return docs ? "docs" : "/";
  return dir.split("/").pop() || dir;
}

/**
 * The keyboard, resolved against the page's own verb table.
 *
 * Not one key letter: the table says which letter means what, and the arrows are
 * `j`/`k`/`h`/`l` under the names a keyboard gives them — which is exactly the
 * rule `logic/keys.ts` follows, written here because that dispatcher is the
 * vanilla client's and this page owns its own keydown.
 */
export function filesVerb(e: { key: string; ctrlKey?: boolean }, editing: boolean): VerbId | null {
  const arrows: Record<string, VerbId> = {
    arrowdown: VerbId.Down,
    arrowup: VerbId.Up,
    arrowleft: VerbId.TreeUp,
    arrowright: VerbId.TreeInto,
  };
  const table = filesVerbs(editing);
  const name = keyName(e);
  const arrow = arrows[name];
  // An arrow still has to be in the table, or it means nothing here.
  const bound = arrow ? table.find((v) => v.id === arrow) : undefined;
  const spelled = e.ctrlKey ? "C-" + name : name;
  return (bound ?? table.find((v) => v.key === spelled))?.id ?? null;
}

// ---------------------------------------------------------------------------
// The page
// ---------------------------------------------------------------------------

export function FilesPage({ ws, kind = "files", prefix, actions }: FilesPageProps) {
  const docs = kind === "docs";
  // The *id*, not the workspace, is what every effect below depends on. A
  // workspace object is replaced on every event the daemon pushes — a new
  // agent, a tick of telemetry — and an effect keyed on the object would throw
  // the tree away several times a second.
  const id = ws ? String(ws.id) : null;

  // One directory per key, loaded when it is opened and kept — the vanilla page
  // re-fetched every open directory on every click, including the ones nothing
  // had touched.
  const [dirs, setDirs] = useState<Record<string, Listing | undefined>>({});
  // Which directories are on screen and where the cursor is, as one value —
  // `logic/trail.ts` owns the moves, and this page owns what to fetch and draw
  // when one of them lands.
  const [trail, setTrail] = useState<Trail>(ROOT);
  const [sel, setSel] = useState<string | null>(null);
  const [mode, setMode] = useState<"file" | "diff">("file");
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [body, setBody] = useState<Body | null>(null);
  const [nonce, setNonce] = useState(0);
  const picker = useRef<HTMLInputElement | null>(null);
  const browser = useRef<HTMLDivElement | null>(null);
  const scroller = useRef<HTMLDivElement | null>(null);
  // Enter asked for the file to take the keyboard, and the file is not on screen
  // yet — it is a fetch away. Focusing at the keystroke focuses nothing; this
  // remembers the intent until there is something to focus. See the effect below.
  const wantFile = useRef(false);

  // Uploads land in the directory the cursor is in. A reference topic is not in
  // a directory at all, so it must not move the destination somewhere that is
  // not a place.
  const here = hereOf(trail);
  const curDir = isBuiltin(here) ? "" : here;

  // Which side to diff is not a preference, it is which list the file is in.
  const changes = ws?.changes ?? null;
  const { stagedSet, worktreeSet } = useMemo(() => {
    const staged = new Set((changes?.staged ?? []).map((c) => c.path));
    const worktree = new Set((changes?.unstaged ?? []).map((c) => c.path));
    // A conflicted file is in neither list on the daemon's side, and it is very
    // much a file with a worktree diff.
    for (const c of changes?.conflicted ?? []) worktree.add(c.path);
    return { stagedSet: staged, worktreeSet: worktree };
  }, [changes]);

  // A new workspace is a new tree. Nothing about the old one survives,
  // including the upload destination — a path that was a directory over there is
  // not necessarily anything here.
  useEffect(() => {
    setDirs({});
    setTrail(ROOT);
    setSel(null);
    setMode("file");
    setEditing(false);
    setBody(null);
  }, [id, kind]);

  /**
   * One directory's listing.
   *
   * On DOCS the *listing* is what changes, not the drawing: `docsRows` filters
   * the entries, stands the reference's topics in for a directory that is not on
   * disk, and puts the reference folder at the root. The reference costs no
   * request at all — which is also why it reads the same over a slow link.
   */
  const load = (path: string) => {
    if (!id) return;
    const done = (entries: readonly DocRow[]) => setDirs((d) => ({ ...d, [path]: { entries } }));
    const failed = (e: unknown) => setDirs((d) => ({ ...d, [path]: { error: message(e) } }));
    if (docs && path === REFERENCE_DIR) {
      done(docsRows([], path, true));
      return;
    }
    api
      .tree(id, path, docs ? "docs" : undefined)
      .then((r) => done(docs ? docsRows(r.entries, path, true) : r.entries), failed);
  };

  useEffect(() => {
    if (!id) return;
    for (const path of trail.dirs) load(path);
    // `trail` is not a dependency: a directory is loaded by the move that
    // descends into it. Re-running this on every step would re-fetch every
    // directory already on screen, which is the vanilla page's behaviour and the
    // thing this cache exists to stop.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, kind, nonce]);

  // The column the trail has panned to has to stay on screen, or walking deep
  // enough leaves the keyboard driving something off the right-hand edge.
  useEffect(() => {
    browser.current?.children[trail.col]?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [trail.col, trail.dirs.length]);

  const entriesOf = (dir: string): readonly DocRow[] => dirs[dir]?.entries ?? [];
  const rowOf = (dir: string) => rowIn(trail, dir, entriesOf(dir).length);

  /** Land a new trail, dropping a selection no column holds any more. */
  const walk = (next: Trail) => {
    setTrail(next);
    if (sel && !isBuiltin(sel) && !holds(next, dirOf(sel))) setSel(null);
  };

  const pickFile = (path: string) => {
    setSel(path);
    setMode("file");
    setEditing(false); // a fresh selection starts in read mode
  };

  /**
   * Open what the cursor in column `i` is on.
   *
   * A directory becomes the column to the right and takes the keyboard with it;
   * a file becomes the body. `focusBody` is the whole of what separates Enter
   * from Space: both read the file, and only one of them stops you walking.
   */
  const enter = (i: number, focusBody: boolean) => {
    const dir = trail.dirs[i] ?? "";
    const e = entriesOf(dir)[rowOf(dir)];
    if (!e) return;
    if (e.is_dir) {
      walk(into(trail, i, e.path));
      setSel(null);
      if (!dirs[e.path]) load(e.path);
      return;
    }
    setTrail(trim(trail, i));
    pickFile(e.path);
    wantFile.current = focusBody;
  };

  /**
   * Click a row: put the cursor on it, then open it — the Finder's single click.
   *
   * Pointed first even though opening moves again, because pointing is what
   * drops the columns the *old* selection owned. Opening straight from the click
   * would put the new folder's listing beside the stale ones.
   */
  const clickRow = (i: number, row: number) => {
    const e = entriesOf(trail.dirs[i] ?? "")[row];
    const pointed = point(trail, i, row);
    if (!e) {
      walk(pointed);
    } else if (e.is_dir) {
      walk(into(pointed, i, e.path));
      setSel(null);
      if (!dirs[e.path]) load(e.path);
    } else {
      walk(trim(pointed, i));
      pickFile(e.path);
    }
  };

  const builtin = sel != null && isBuiltin(sel);
  // A fully staged file has nothing against the worktree, and asking anyway
  // answers "(no differences)" for a file the rail is *showing you as changed*.
  // The vanilla page carried a flag for this and set it from the CHANGES rail's
  // staged section, so a file opened any other way got the wrong side.
  const staged = sel != null && stagedSet.has(sel) && !worktreeSet.has(sel);
  const canDiff = sel != null && !builtin && (stagedSet.has(sel) || worktreeSet.has(sel));

  // The body: the file, its diff, or a document. A reference page is already
  // here, so it opens with no daemon in the loop at all.
  useEffect(() => {
    if (!id || !sel) {
      setBody(null);
      return undefined;
    }
    const topic = topicFor(sel, prefix);
    if (topic) {
      setBody({ kind: "md", title: topic.title, blocks: readMarkdown(topic.body) });
      return undefined;
    }
    let live = true;
    setBody({ kind: "loading" });
    if (mode === "diff") {
      api.diff(id, sel, staged).then(
        (d) => {
          if (live) setBody({ kind: "patch", text: d.patch || "(no differences)" });
        },
        (e: unknown) => {
          if (live) setBody({ kind: "error", text: message(e) });
        },
      );
    } else {
      api.file(id, sel).then(
        (f) => {
          if (!live) return;
          // Rendered on DOCS, printed everywhere else. A `.md` on the FILES page
          // is a file you came to read *as source* — it sits beside the code and
          // `e edit` opens it — and a rendered one there would be an editor you
          // cannot see the input of.
          if (docs && !f.truncated && rendersAsMarkdown(sel)) {
            setBody({ kind: "md", blocks: readMarkdown(f.text) });
          } else {
            setBody({
              kind: "text",
              truncated: f.truncated,
              text: f.text + (f.truncated ? "\n\n… (truncated)" : ""),
            });
          }
        },
        (e: unknown) => {
          if (live) setBody({ kind: "error", text: message(e) });
        },
      );
    }
    return () => {
      live = false;
    };
  }, [id, kind, docs, prefix, sel, mode, staged, nonce]);

  // Hand the keyboard to the file once it has arrived, if that is what opened
  // it. `←` gives it back — see `FileBody`'s `onBack`.
  useEffect(() => {
    if (!wantFile.current || body?.kind !== "text") return;
    wantFile.current = false;
    scroller.current?.focus();
  }, [body]);

  // -- the writes ------------------------------------------------------------

  // Uploading is the tree's own verb: each picked file goes to the destination
  // the rail shows, and the listing is read again afterwards.
  const upload = async (picked: FileList | null) => {
    if (!id || !picked || !picked.length) return;
    // Copied out before the first `await`, because the caller clears the input
    // as soon as this returns and an emptied `<input type=file>` is an emptied
    // `FileList`.
    for (const f of Array.from(picked)) {
      // Each file is its own action, so a folder where one name is refused
      // still delivers the rest and says which one did not land.
      await actions.upload({ path: (curDir ? curDir + "/" : "") + f.name, blob: f });
    }
    setNonce((n) => n + 1);
  };

  // The editor loads the raw text fresh, so a save can never write back a stale
  // buffer or a rendered one.
  const startEdit = async () => {
    if (!id || !sel) return;
    if (isBuiltin(sel)) {
      actions.toast("this page is built into the client");
      return;
    }
    let f;
    try {
      f = await api.file(id, sel);
    } catch (e) {
      actions.toast(message(e));
      return;
    }
    // Saving a truncated buffer would clobber the rest of the file.
    if (f.truncated) {
      actions.toast("file too large to edit");
      return;
    }
    setDraft(f.text);
    setEditing(true);
  };

  // Written back through the upload route, which overwrites the path. The rail
  // that owns staging is somewhere else and has to hear about the edit, which is
  // what the action's own refresh is.
  const save = async () => {
    if (!id || !sel) return;
    // A save that did not land leaves the editor open with the draft still in
    // it. The alternative — closing on the attempt — throws away the only copy
    // of the text, which is the one unrecoverable thing this page can do.
    if (!(await actions.upload({ path: sel, blob: new Blob([draft], { type: "text/plain" }) }))) return;
    setEditing(false);
    setNonce((n) => n + 1);
  };

  // The daemon replies with a Content-Disposition, so a plain navigation saves
  // the file rather than showing it.
  const download = () => {
    if (!id || !sel) return;
    if (isBuiltin(sel)) {
      actions.toast("this page is built into the client");
      return;
    }
    const a = document.createElement("a");
    a.href = api.downloadUrl(id, sel);
    a.download = "";
    a.style.display = "none";
    document.body.append(a);
    a.click();
    a.remove();
  };

  // The selected file, gone. The selection goes with it — a path that is not
  // there any more is a body that can only load as an error — and the listing is
  // read again, which is also what repaints the `changed` markers around it.
  const remove = async () => {
    if (!id || !sel) return;
    if (isBuiltin(sel)) {
      actions.toast("this page is built into the client");
      return;
    }
    if (!(await actions.deleteFile(sel))) return;
    setSel(null);
    setEditing(false);
    setNonce((n) => n + 1);
  };

  // -- the verbs -------------------------------------------------------------

  const canEdit = sel != null && !builtin && mode === "file" && !(body?.kind === "text" && body.truncated);
  const openPicker = () => picker.current?.click();

  // One table, read twice: the toolbar draws these and the `HintBar` binds the
  // same functions to the keys `verbs.ts` spells them with. A verb with no entry
  // is one this page cannot run *here*, and it is drawn as documentation rather
  // than as a button that would look live and do nothing.
  const runs: Partial<Record<VerbId, () => void>> = editing
    ? { [VerbId.Save]: () => void save(), [VerbId.CancelEdit]: () => setEditing(false) }
    : {
        [VerbId.Upload]: openPicker,
        // Walking the trail. These are in the same table the footer is drawn
        // from, so the key and the entry that documents it dispatch one
        // function — the property that keeps a footer from lying.
        [VerbId.Down]: () =>
          walk(point(trail, trail.col, Math.min(rowOf(here) + 1, entriesOf(here).length - 1))),
        [VerbId.Up]: () => walk(point(trail, trail.col, Math.max(rowOf(here) - 1, 0))),
        [VerbId.TreeUp]: () => setTrail(left(trail)),
        [VerbId.TreeInto]: () => enter(trail.col, false),
        [VerbId.Open]: () => enter(trail.col, true),
        // Quick Look: read the file the cursor is on without handing it the
        // keyboard, so the next `j` walks to the next name and shows you that
        // one instead. A directory has nothing to peek at — its contents are
        // what `→` shows — so it is left alone rather than made a second
        // descend key.
        [VerbId.Peek]: () => {
          const e = entriesOf(here)[rowOf(here)];
          if (e && !e.is_dir) pickFile(e.path);
        },
        ...(canDiff ? { [VerbId.ViewFile]: () => setMode("file"), [VerbId.ViewDiff]: () => setMode("diff") } : {}),
        ...(canEdit ? { [VerbId.Edit]: () => void startEdit() } : {}),
        ...(sel != null && !builtin
          ? { [VerbId.Download]: download, [VerbId.DeleteFile]: () => void remove() }
          : {}),
      };

  // The browser's keyboard. Every key is looked up in the table above rather
  // than matched here, so this cannot bind a letter the footer does not draw.
  const onKey = (e: React.KeyboardEvent) => {
    if (e.metaKey || e.altKey) return;
    const id = filesVerb(e, editing);
    const run = id ? runs[id] : undefined;
    if (!run) return;
    e.preventDefault();
    e.stopPropagation();
    run();
  };

  // Packed at the terminal's own column count, which reads like a layout
  // decision and is not one: it decides *which verbs are worth writing down*,
  // and that answer has to be the terminal's or the two clients teach different
  // keys. Where the list goes is the kit's change — `HintBar` spans the page.
  const keys: Hint[] = fits(filesVerbs(editing), RAIL_COLS, MAX_ROWS).map((v) => ({
    key: keyText(v.key),
    label: v.label,
    danger: v.danger,
    onSelect: runs[v.id],
  }));

  const root = dirs[""];
  const title = body?.kind === "md" && body.title ? body.title : sel;
  const label = docs ? "docs" : "files";

  return (
    <div className="flex h-full min-h-0 flex-col">
      {/* The browser grows a column at a time and stops at half the page: a trail
          six deep on a wide screen would otherwise leave the file whatever strip
          it had not claimed, and the deeper you went the less of the file you
          could read — a sidebar eating the page it is a sidebar of.

          The width is a *grid track*, not a width on the card. As a percentage
          on the card it would have resolved against a track sized `auto` — which
          is sized from the card — and a circular percentage silently becomes
          half of whatever the browser guessed, leaving a dead column down the
          middle of the page. Here the `50%` is half the grid, which is the thing
          the sentence above is actually about. */}
      <div
        style={
          {
            // Rounded down to whole columns, so the trail never ends in a
            // half-drawn one — a column cut off mid-name reads as a listing that
            // failed rather than as a listing that scrolls.
            // The `+ 2px` is the card's own inset rule, one pixel down each
            // side. Without it the last column's chevrons are drawn under it.
            "--browser":
              `calc(min(round(down, 50%, ${COL_W}px), ${trail.dirs.length * COL_W}px) + 2px)`,
          } as React.CSSProperties
        }
        className={
          "flex min-h-0 flex-1 flex-col gap-3 p-3 md:grid " +
          "md:[grid-template-columns:var(--browser)_minmax(0,1fr)] md:[grid-template-rows:minmax(0,1fr)]"
        }
      >
        <Card className="flex min-h-0 min-w-0 flex-1 flex-col gap-0 overflow-hidden p-0">
          <SectionTitle
            action={
              <Button
                size="sm"
                variant="ghost"
                title={"Upload into " + (curDir ? curDir + "/" : "/") + " (u)"}
                {...verbClick("files.upload", openPicker)}
              >
                ↑ upload
              </Button>
            }
          >
            {label}
          </SectionTitle>
          {!ws ? (
            <Empty>no workspace open</Empty>
          ) : root?.error ? (
            <Empty>{root.error}</Empty>
          ) : !root ? (
            <Empty>loading…</Empty>
          ) : (
            // One focusable element for the whole browser rather than one per
            // row: the cursor is the page's state, so the rows are drawn from
            // it and never hold the focus themselves — which is what stops tab
            // from walking a thousand filenames.
            <div
              ref={browser}
              role="tree"
              aria-label={label}
              tabIndex={0}
              onKeyDown={onKey}
              className="flex min-h-0 flex-1 overflow-x-auto outline-none"
            >
              {trail.dirs.map((dir, i) => {
                const listing = dirs[dir];
                const rows = listing?.entries;
                return (
                  <div
                    key={dir || "/"}
                    className="flex min-h-0 shrink-0 flex-col border-r border-border last:border-r-0"
                    style={{ width: COL_W }}
                  >
                    {/* The column's own name, over it. Dim unless it is the
                        one the keyboard is in, so which column `j`/`k` is
                        about to move is something you can see rather than
                        something you remember. */}
                    <div
                      className={
                        "h-row shrink-0 truncate border-b border-border px-2 text-12 leading-[--spacing(row)] " +
                        (i === trail.col ? "text-foreground" : "text-faint")
                      }
                      title={dir || "/"}
                    >
                      {columnLabel(dir, docs)}
                    </div>
                    {/* A plain scroller, not `ScrollArea`: that one's viewport
                        is content-sized, so a row asked to fill it fills the
                        longest filename instead of the column and pushes its
                        chevron out past the edge. The scrollbars are themed in
                        `styles.css` either way. */}
                    <div className="min-h-0 flex-1 overflow-y-auto">
                      {listing?.error ? (
                        <Empty compact>{listing.error}</Empty>
                      ) : !rows ? (
                        <Empty compact>loading…</Empty>
                      ) : !rows.length ? (
                        <Empty compact>{docs ? "nothing to read here" : "empty"}</Empty>
                      ) : (
                        <div role="group" className="flex flex-col">
                          {rows.map((entry, row) => (
                            <ColumnRow
                              key={entry.path}
                              entry={entry}
                              /* Three states, not two. The column with the
                                 keyboard marks its row the way every list in
                                 this client does; the columns behind it mark
                                 the row you came *through*, which is what
                                 makes the trail a path rather than several
                                 directories that happen to be adjacent. */
                              selected={row === rowOf(dir) && i === trail.col}
                              trail={row === rowOf(dir) && i !== trail.col}
                              onSelect={() => clickRow(i, row)}
                            />
                          ))}
                        </div>
                      )}
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </Card>

        <Card className="flex min-h-0 min-w-0 flex-1 flex-col gap-0 overflow-hidden p-0">
          <SectionTitle
            action={
              editing ? (
                <>
                  <Button size="sm" variant="default" title="Save (C-s)" {...verbClick("files.save", () => void save())}>
                    save
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    title="Discard changes (esc)"
                    {...verbClick("files.cancel", () => setEditing(false))}
                  >
                    cancel
                  </Button>
                </>
              ) : (
                <>
                  {builtin ? <Badge variant="outline">built in</Badge> : null}
                  {canDiff ? (
                    <>
                      <Button
                        size="sm"
                        variant={mode === "file" ? "secondary" : "ghost"}
                        title="The file itself (f)"
                        {...verbClick("files.view.file", () => setMode("file"))}
                      >
                        file
                      </Button>
                      <Button
                        size="sm"
                        variant={mode === "diff" ? "secondary" : "ghost"}
                        title={(staged ? "Diff of the staged copy" : "Diff of the working tree") + " (d)"}
                        {...verbClick("files.view.diff", () => setMode("diff"))}
                      >
                        {staged ? "diff (staged)" : "diff"}
                      </Button>
                    </>
                  ) : null}
                  {canEdit ? (
                    <Button
                      size="sm"
                      variant="ghost"
                      title="Edit this file (e)"
                      {...verbClick("files.edit", () => void startEdit())}
                    >
                      edit
                    </Button>
                  ) : null}
                  {sel != null && !builtin ? (
                    <>
                      <Button size="sm" variant="ghost" title="Download this file (y)" {...verbClick("files.download", download)}>
                        download
                      </Button>
                      {/* Last in the row and drawn in `--bad`, which is where
                          this client puts a verb you cannot take back. */}
                      <Button
                        size="sm"
                        variant="ghost"
                        className="text-bad hover:text-bad"
                        title="Delete this file (x)"
                        {...verbClick("files.delete", () => void remove())}
                      >
                        delete
                      </Button>
                    </>
                  ) : null}
                </>
              )
            }
          >
            {/* A path, in a header whose voice is caps. `Path` elides in the
                middle rather than at the end, which is the whole reason the
                filename survives a narrow column — and the overrides here are
                the header's *voice* (caps, semibold, dim), not its geometry: a
                path read in capitals is a different path. */}
            {title ? (
              <Path path={title} className="text-12 font-normal tracking-normal text-foreground normal-case" />
            ) : (
              label
            )}
          </SectionTitle>
          <FileBody
            body={body}
            sel={sel}
            docs={docs}
            editing={editing}
            draft={draft}
            onDraft={setDraft}
            onSave={() => void save()}
            onCancel={() => setEditing(false)}
            scroller={scroller}
            onBack={() => browser.current?.focus()}
          />
        </Card>
      </div>

      <HintBar keys={keys} />

      {/* The one element on this page that is not drawn: the picker the upload
          verb opens. `multiple`, because a rail that took one file at a time
          would be a rail you use once. */}
      <input
        ref={picker}
        type="file"
        multiple
        hidden
        onChange={(e) => {
          const input = e.target;
          void upload(input.files);
          input.value = "";
        }}
      />
    </div>
  );
}

// ---------------------------------------------------------------------------
// The trail
// ---------------------------------------------------------------------------

interface ColumnRowProps {
  entry: DocRow;
  /** The cursor, in the column that has the keyboard. */
  selected: boolean;
  /** The row a *deeper* column came through — the path, drawn behind the cursor. */
  trail: boolean;
  onSelect: () => void;
}

function ColumnRow({ entry, selected, trail, onSelect }: ColumnRowProps) {
  // Four states, and each one is a palette role rather than a colour: a folder,
  // a page that is built into the client, a file git has something to say about,
  // and everything else.
  const tone = entry.is_dir
    ? "text-primary"
    : entry.builtin
      ? "text-run"
      : entry.changed
        ? "text-warn"
        : "text-foreground";
  return (
    <Row
      compact
      selected={selected}
      // `w-full` is load-bearing: a `Row` is `shrink-0`, so in a fixed-width
      // column it takes its *content's* width and a long filename pushes the
      // chevron out past the column's edge instead of eliding.
      className={"w-full" + (trail ? " bg-muted" : "")}
      onSelect={onSelect}
      title={entry.path}
      {...verbTarget("files.row", onSelect)}
    >
      {/* Mono, and truncated here where the tree let names run: a column is a
          fixed width on purpose — four directories on screen at once is the
          whole point — so a name longer than one has to give way rather than
          push the trail sideways. The full path is on the row's `title`. */}
      <span className={"min-w-0 flex-1 truncate font-mono " + tone}>{entry.name}</span>
      {entry.changed ? (
        <span className="shrink-0 text-warn" title="changed">
          ●
        </span>
      ) : null}
      {/* The chevron the next column opens from, on the edge it opens towards.
          It says what the tree's `▸`/`▾` said and does not have to change to
          say it — a folder either has more in it or it does not, and which
          column is showing that is the trail's business, not the row's. */}
      <span aria-hidden="true" className="w-3 shrink-0 text-right text-dim">
        {entry.is_dir ? "›" : ""}
      </span>
    </Row>
  );
}

// ---------------------------------------------------------------------------
// The body
// ---------------------------------------------------------------------------

interface FileBodyProps {
  body: Body | null;
  sel: string | null;
  docs: boolean;
  editing: boolean;
  draft: string;
  onDraft: (text: string) => void;
  onSave: () => void;
  onCancel: () => void;
  /** The file's scroller, so the minimap can aim it and the keyboard can reach it. */
  scroller: React.RefObject<HTMLDivElement | null>;
  /** Hand the keyboard back to the browser — what `←` means from inside a file. */
  onBack: () => void;
}

function FileBody({
  body,
  sel,
  docs,
  editing,
  draft,
  onDraft,
  onSave,
  onCancel,
  scroller,
  onBack,
}: FileBodyProps) {
  if (editing) {
    return (
      <Textarea
        className="min-h-0 flex-1 resize-none rounded-none border-0 font-mono text-12 shadow-none focus-visible:ring-0"
        value={draft}
        spellCheck={false}
        onChange={(e) => onDraft(e.target.value)}
        onKeyDown={(e) => {
          // `C-s` is the one key on this page that is not a bare letter, because
          // it is what every editor on the machine uses and this is an editor
          // while it is open. `esc` is the way out, which is what `verbs.ts`
          // writes down.
          if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
            e.preventDefault();
            onSave();
          }
          if (e.key === "Escape") {
            e.preventDefault();
            onCancel();
          }
        }}
      />
    );
  }
  if (!body) return <Empty>{sel ? "loading…" : docs ? "Select something to read." : "Select a file."}</Empty>;
  if (body.kind === "loading") return <Empty>loading…</Empty>;
  if (body.kind === "error") return <Empty>{body.text}</Empty>;
  if (body.kind === "patch") return <Patch text={body.text} className="min-h-0 flex-1" />;
  if (body.kind === "md") {
    return (
      <ScrollArea type="auto" className="min-h-0 flex-1">
        <Prose blocks={body.blocks} />
      </ScrollArea>
    );
  }
  // The file, with the whole of it beside it. `tabIndex` so Enter on a row can
  // hand the keyboard here and the file pages with the keys every scroller
  // answers to — which is the difference Space is refusing to make.
  return (
    <div className="flex min-h-0 min-w-0 flex-1">
      <Code
        ref={scroller}
        tabIndex={0}
        text={body.text}
        lineNumbers
        className="min-h-0 min-w-0 flex-1 outline-none"
        // The one key the file claims. Everything else — the arrows, page
        // up and down, Home — belongs to the scroller, which is the whole
        // reason Enter hands the keyboard over here in the first place.
        onKeyDown={(e) => {
          if (filesVerb(e, false) !== VerbId.TreeUp) return;
          e.preventDefault();
          onBack();
        }}
      />
      <Minimap text={body.text} scroller={scroller} />
    </div>
  );
}
