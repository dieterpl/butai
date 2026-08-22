// The FILES page — a lazy tree and a file — and DOCS, which is this same page
// over a second listing.
//
// The port of `web/ui/files.js`, itself the port of `<butai-files>`: a directory
// tree on the left (`GET .../tree`, one directory at a time), and on the right
// the file itself (`.../file`), its diff (`.../diff`), an editor over it, or
// rendered markdown.
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
import { MAX_ROWS, VerbId, click, filesVerbs, fits, keyText, type TargetId } from "@/logic/verbs.ts";

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

/**
 * One line of the flattened tree: an entry, or the note a directory shows while
 * it has no entries to show.
 *
 * The vanilla page pushed a *fake entry* for those two states — a `path` with a
 * `#loading` suffix and a `pending` flag — which meant every consumer of a row
 * had to know that some rows are not files. A second arm says it in the type.
 */
type Line =
  | { kind: "entry"; key: string; depth: number; entry: DocRow }
  | { kind: "note"; key: string; depth: number; text: string };

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
 * The open tree, flattened depth-first — one pass over the cache, so a child
 * sits under its own parent and the list the cursor walks is the list on screen.
 *
 * A directory that is open but not yet answered contributes a `loading…` line
 * of its own rather than nothing, because a tree that swallows the click that
 * opened it reads as a tree that is broken.
 */
function treeRows(
  dirs: Readonly<Record<string, Listing | undefined>>,
  open: ReadonlySet<string>,
  path: string,
  depth = 0,
  out: Line[] = [],
): Line[] {
  const dir = dirs[path];
  if (!dir?.entries) return out;
  for (const entry of dir.entries) {
    out.push({ kind: "entry", key: entry.path, depth, entry });
    if (!entry.is_dir || !open.has(entry.path)) continue;
    const sub = dirs[entry.path];
    if (!sub) out.push({ kind: "note", key: entry.path + "#loading", depth: depth + 1, text: "loading…" });
    else if (sub.error) out.push({ kind: "note", key: entry.path + "#error", depth: depth + 1, text: sub.error });
    else treeRows(dirs, open, entry.path, depth + 1, out);
  }
  return out;
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
  const [open, setOpen] = useState<ReadonlySet<string>>(() => new Set<string>());
  const [sel, setSel] = useState<string | null>(null);
  const [mode, setMode] = useState<"file" | "diff">("file");
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [curDir, setCurDir] = useState("");
  const [body, setBody] = useState<Body | null>(null);
  const [nonce, setNonce] = useState(0);
  const picker = useRef<HTMLInputElement | null>(null);

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
    setOpen(new Set<string>());
    setSel(null);
    setMode("file");
    setEditing(false);
    setCurDir("");
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
    load("");
    for (const path of open) load(path);
    // `open` is not a dependency: a directory is loaded when it is opened, by
    // the click that opened it. Re-running this on every expansion would
    // re-fetch every directory already on screen, which is the vanilla page's
    // behaviour and the thing this cache exists to stop.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, kind, nonce]);

  const toggleDir = (path: string) => {
    setOpen((o) => {
      const next = new Set(o);
      if (next.has(path)) next.delete(path);
      else {
        next.add(path);
        if (!dirs[path]) load(path);
      }
      return next;
    });
    // Uploads land in the directory you just entered.
    setCurDir(path);
  };

  const pickFile = (path: string) => {
    setSel(path);
    setMode("file");
    setEditing(false); // a fresh selection starts in read mode
    // Uploads land alongside the file you selected — but a reference page is not
    // in a directory, so it must not move the destination somewhere that is not
    // a place.
    if (!isBuiltin(path)) setCurDir(dirOf(path));
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
        ...(canDiff ? { [VerbId.ViewFile]: () => setMode("file"), [VerbId.ViewDiff]: () => setMode("diff") } : {}),
        ...(canEdit ? { [VerbId.Edit]: () => void startEdit() } : {}),
        ...(sel != null && !builtin
          ? { [VerbId.Download]: download, [VerbId.DeleteFile]: () => void remove() }
          : {}),
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

  const lines = treeRows(dirs, open, "");
  const root = dirs[""];
  const title = body?.kind === "md" && body.title ? body.title : sel;
  const label = docs ? "docs" : "files";

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div
        className={
          "flex min-h-0 flex-1 flex-col gap-3 p-3 md:grid " +
          "md:[grid-template-columns:minmax(200px,300px)_1fr] md:[grid-template-rows:minmax(0,1fr)]"
        }
      >
        <Card className="flex min-h-0 flex-1 flex-col gap-0 overflow-hidden p-0">
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
          {/* Both axes: a deep tree is wider than the rail, and a path that
              silently loses its left-hand end is the audit's worst finding
              wearing a different hat. `w-max min-w-full` on the list is what
              gives the viewport something to scroll. */}
          <ScrollArea type="auto" className="min-h-0 flex-1">
            {!ws ? (
              <Empty>no workspace open</Empty>
            ) : root?.error ? (
              <Empty>{root.error}</Empty>
            ) : !root ? (
              <Empty>loading…</Empty>
            ) : !lines.length ? (
              <Empty>{docs ? "nothing to read here" : "empty"}</Empty>
            ) : (
              <div role="listbox" aria-label={label} className="flex w-max min-w-full flex-col">
                {lines.map((line) =>
                  line.kind === "note" ? (
                    <Empty key={line.key} indent={line.depth} compact>
                      {line.text}
                    </Empty>
                  ) : (
                    <TreeRow
                      key={line.key}
                      entry={line.entry}
                      depth={line.depth}
                      open={open.has(line.entry.path)}
                      selected={sel === line.entry.path}
                      onSelect={() =>
                        line.entry.is_dir ? toggleDir(line.entry.path) : pickFile(line.entry.path)
                      }
                    />
                  ),
                )}
              </div>
            )}
          </ScrollArea>
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
// The tree
// ---------------------------------------------------------------------------

interface TreeRowProps {
  entry: DocRow;
  depth: number;
  open: boolean;
  selected: boolean;
  onSelect: () => void;
}

function TreeRow({ entry, depth, open, selected, onSelect }: TreeRowProps) {
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
      indent={depth}
      selected={selected}
      onSelect={onSelect}
      title={entry.path}
      {...verbTarget("files.row", onSelect)}
    >
      <span aria-hidden="true" className="w-3 shrink-0 text-dim">
        {entry.is_dir ? (open ? "▾" : "▸") : ""}
      </span>
      {/* Mono, and never truncated: this is a filename, the list scrolls
          sideways rather than eliding it, and two names that differ in one
          character have to be diffable by eye. */}
      <span className={"shrink-0 whitespace-nowrap font-mono " + tone}>{entry.name}</span>
      {entry.changed ? (
        <span className="shrink-0 text-warn" title="changed">
          ●
        </span>
      ) : null}
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
}

function FileBody({ body, sel, docs, editing, draft, onDraft, onSave, onCancel }: FileBodyProps) {
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
  return <Code text={body.text} className="min-h-0 flex-1" />;
}
