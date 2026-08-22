// The DOCS page's model: which of a project's files are its writing, and the
// built-in reference that sits above them.
//
// `Page::Docs` is "the Files page filtered to markdown: a project's own
// writing, without the code it is about", so there is no second tree here and
// there must not be one — `butai-files.js` draws both, and this is only the two
// things that make one of them DOCS: the filter, and the `reference` folder.
//
// ## The reference is generated, and that is why there is only one of it
//
// `docs/keys.md` says the in-app reference lives on this page. Stage 6 built a
// `?` that is *generated from the verb tables*, which is the property worth
// keeping: a surface cannot fall out of it while its keys keep working. So the
// reference here is not a second document, it is that same `reference()` laid
// out as markdown pages — one topic per section — and `?` opens this page on
// the first of them, exactly as the terminal's `ViewVerb::Help` opens
// `Flow::Reference(HELP_TOPIC)`.
//
// Two references that agree today are two references; one generator rendered in
// one place cannot disagree with itself.
//
// Nothing here touches the DOM or the network — the filter, the topics and the
// markdown reader are pure.

import { reference, type ReferenceSection } from "./verbs.ts";
import type { TreeEntry } from "../protocol/generated/protocol.ts";

/// The folder the topics live in, as the DOCS rail lists it.
///
/// A scheme, for the terminal's reason: no path in a workspace can contain
/// `://`, so this cannot collide with a directory however a project is laid
/// out, and `parentOf` knows it is not a path on disk.
export const REFERENCE_DIR = "butai://reference";
export const REFERENCE_NAME = "reference";

/// The topic `?` lands on: the two key layers, which is what the modal used to
/// hold and what people press `?` for. The rest is one rail row away.
export const HELP_TOPIC = "butai://keys";

// `isDoc` used to live here — every directory but the two nobody means, and
// markdown or a README by any spelling. It is `is_doc` in the protocol crate
// now and the daemon applies it, because it also decides the `changed` markers
// and the two have to be one decision. This client asks for it by name:
// `api.tree(id, path, "docs")`.

/// Up from here, or null at the root.
export function parentOf(dir: string | null | undefined): string | null {
  if (!dir) return null;
  // The reference is a folder in this rail without being a folder on disk, so
  // up from it is the root — not what splitting its sentinel on the last slash
  // would produce, which is the middle of a URL scheme.
  if (dir === REFERENCE_DIR) return "";
  const i = dir.lastIndexOf("/");
  return i < 0 ? "" : dir.slice(0, i);
}

/// A stable slug per reference section, so a topic has a name a link could use.
///
/// Written out rather than derived from the title, because the titles are
/// surface names that read as shouting in a file list (`AGENTS`) and because
/// `keys` has to stay `keys` — [`HELP_TOPIC`] names it, and a retitled section
/// must not silently move where `?` lands.
const SLUGS: Readonly<Record<string, string>> = Object.freeze({
  "The two layers": "keys",
  "The pointer's alone": "the-pointer",
});

export function slugFor(title: string): string {
  return SLUGS[title] || String(title).toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
}

/// One page of the reference: a file with no file behind it.
export interface Topic {
  slug: string;
  name: string;
  path: string;
  title: string;
  body: string;
}

/// One page of the reference, as a file the DOCS rail can list and the reading
/// column can open.
///
/// `path` is the sentinel the row carries; `title` is what the reading column
/// shows, with a `.md` suffix because it *is* markdown and a `reference/`
/// prefix so the title says which of the two things in this rail you are
/// looking at.
export function topics(prefix?: string | null): Topic[] {
  return reference().map((sec) => {
    const slug = slugFor(sec.title);
    return {
      slug,
      name: slug + ".md",
      path: "butai://" + slug,
      title: "reference/" + slug + ".md",
      body: topicBody(sec, prefix || "C-b"),
    };
  });
}

export function topicFor(path: string | null | undefined, prefix?: string | null): Topic | null {
  const slug = String(path || "").startsWith("butai://") ? String(path).slice(8) : null;
  if (!slug) return null;
  return topics(prefix).find((t) => t.slug === slug) || null;
}

/// A reference section as markdown.
///
/// The keys go in a table because that is what they are, and the section's own
/// note goes above it as prose — both come straight off `reference()`, so the
/// only thing written here is the layout.
function topicBody(sec: ReferenceSection, prefix: string): string {
  const out = ["# " + sec.title, ""];
  if (sec.note) out.push(sec.note.replace(/C-b/g, prefix), "");
  if (sec.rows.length) {
    out.push("| key | | |", "|---|---|---|");
    for (const r of sec.rows) {
      out.push("| `" + r.keys.replace(/C-b/g, prefix) + "` | " + r.label + " | "
        + (r.note || "").replace(/C-b/g, prefix) + " |");
    }
  }
  return out.join("\n");
}

/// One row of the DOCS rail: an entry from the daemon's listing, or one of the
/// two rows this page adds itself.
///
/// `size` is on the listing's entries and not on the rows built here; `builtin`
/// is the other way round — a `TreeEntry` is a file on disk by definition.
export type DocRow = Omit<TreeEntry, "size"> & { size?: number; builtin?: boolean };

/// The rows the DOCS rail shows for a directory listing.
///
/// The port of `workbench.rs`'s `tree_rows`, and the order is its order: the
/// listing, then `..`, then the reference folder at the root only.
///
/// **The filter is no longer one of the steps.** It ran here, over an answer
/// whose `changed` markers had already been decided across the whole change
/// set, so a directory kept a `●` earned by a file this function then dropped —
/// and following one down the rail landed on an empty listing. `api.tree` asks
/// for `?filter=docs` and the rows arrive filtered, by the same rule that
/// decided their markers.
///
/// `..` is an ordinary directory row rather than a special case, so Enter takes
/// the path it already takes for a directory — descending into a folder read as
/// a one-way trip while nothing on screen said otherwise.
///
/// **`nested` is this client's one difference, and it is the widget's, not the
/// page's.** The terminal's rail lists one directory at a time, so it needs a
/// row to walk up out of it; `butai-files.js` draws an expanding tree where a
/// child sits under its own parent, and a `..` row there points at the folder
/// two lines above it. Every other rule — the filter, the reference folder at
/// the root, the topics standing in for a listing — is the same either way,
/// which is why this is a flag on one function rather than a second one.
export function docsRows(
  entries: readonly TreeEntry[] | null | undefined,
  dir: string | null | undefined,
  nested: boolean,
): DocRow[] {
  let rows: DocRow[];
  if (dir === REFERENCE_DIR) {
    // Inside the reference there is nothing on disk, whatever a listing of that
    // path would have answered — so the topics *are* the listing, not an
    // addition to one.
    rows = topics().map((t) => ({ name: t.name, path: t.path, is_dir: false, changed: false, builtin: true }));
  } else {
    rows = (entries || []).slice();
  }
  const up = nested ? null : parentOf(dir);
  if (up !== null) rows = ([{ name: "..", path: up, is_dir: true, changed: false, builtin: false }] as DocRow[]).concat(rows);
  if (!dir) {
    rows = ([{ name: REFERENCE_NAME, path: REFERENCE_DIR, is_dir: true, changed: false, builtin: true }] as DocRow[])
      .concat(rows);
  }
  return rows;
}

/// Is this path built in — a page with no file behind it?
///
/// Everything that would write to disk asks this first, because a reference
/// topic has no path to write to and "saved" is the worst possible answer.
export function isBuiltin(path: string | null | undefined): boolean {
  return String(path || "").startsWith("butai://");
}

// ---------------------------------------------------------------------------
// Markdown, as blocks
// ---------------------------------------------------------------------------

/// One run of inline markdown: the text, and whatever is true of it.
export interface Span {
  text: string;
  code?: boolean;
  strong?: boolean;
  em?: boolean;
  href?: string;
}

/// One block of a read document. **Data, not markup** — see [`readMarkdown`].
export type Block =
  | { kind: "p"; spans: Span[] }
  | { kind: "h"; level: number; spans: Span[] }
  | { kind: "code"; lang: string; text: string }
  | { kind: "rule" }
  | { kind: "table"; rows: Span[][][] }
  | { kind: "ul"; items: Span[][] }
  | { kind: "quote"; spans: Span[] };

/// Read markdown into a list of blocks the page can draw.
///
/// **Blocks, not markup.** A parser that returns an HTML string is one
/// `innerHTML` away from a README in somebody's repository being script on this
/// page, and this client renders whatever the daemon hands it. Returning data
/// means the renderer builds text nodes, so there is no path from a file's
/// contents to markup at all — and it means the reader is pure and testable
/// without a browser.
///
/// Deliberately small: headings, fenced code, lists, quotes, tables, rules and
/// paragraphs, with inline code, links and emphasis. A project's own writing is
/// what this has to render, not every extension.
export function readMarkdown(text: string | null | undefined): Block[] {
  const lines = String(text == null ? "" : text).split("\n");
  const out: Block[] = [];
  let i = 0;
  const para: string[] = [];
  const flush = () => {
    if (para.length) out.push({ kind: "p", spans: inline(para.join(" ")) });
    para.length = 0;
  };
  // `lines[i]` is `string | undefined` to the compiler and a string to the
  // loop, which never indexes past `lines.length`; `?? ""` is that fact, not a
  // second behaviour.
  while (i < lines.length) {
    const line = lines[i] ?? "";
    const fence = /^\s*```(.*)$/.exec(line);
    if (fence) {
      flush();
      const body: string[] = [];
      i++;
      while (i < lines.length && !/^\s*```/.test(lines[i] ?? "")) body.push(lines[i++] ?? "");
      i++;                                   // the closing fence, or the end
      out.push({ kind: "code", lang: (fence[1] ?? "").trim(), text: body.join("\n") });
      continue;
    }
    // An indented code block, which is how the terminal's own reference writes
    // its key tables — four spaces and the keys line up.
    if (/^ {4}\S/.test(line) && !para.length) {
      const body: string[] = [];
      while (i < lines.length && (/^ {4}/.test(lines[i] ?? "") || !(lines[i] ?? "").trim())) {
        if (!(lines[i] ?? "").trim() && !body.length) { i++; continue; }
        body.push((lines[i] ?? "").slice(4));
        i++;
      }
      while (body.length && !(body[body.length - 1] ?? "").trim()) body.pop();
      out.push({ kind: "code", lang: "", text: body.join("\n") });
      continue;
    }
    const head = /^(#{1,6})\s+(.*)$/.exec(line);
    if (head) {
      flush();
      out.push({ kind: "h", level: (head[1] ?? "").length, spans: inline((head[2] ?? "").trim()) });
      i++;
      continue;
    }
    if (/^\s*(?:[-*_]\s*){3,}$/.test(line)) {
      flush();
      out.push({ kind: "rule" });
      i++;
      continue;
    }
    const row = /^\s*\|(.*)\|\s*$/.exec(line);
    if (row) {
      flush();
      const rows: Span[][][] = [];
      while (i < lines.length) {
        const m = /^\s*\|(.*)\|\s*$/.exec(lines[i] ?? "");
        if (!m) break;
        i++;
        const cells = (m[1] ?? "").split("|").map((c) => c.trim());
        // The `|---|---|` separator is not a row, it is the thing that says the
        // row above it was a header.
        if (cells.every((c) => /^:?-{2,}:?$/.test(c))) continue;
        rows.push(cells.map(inline));
      }
      out.push({ kind: "table", rows });
      continue;
    }
    const item = /^\s*(?:[-*+]|\d+\.)\s+(.*)$/.exec(line);
    if (item) {
      flush();
      const items: Span[][] = [];
      while (i < lines.length) {
        const m = /^\s*(?:[-*+]|\d+\.)\s+(.*)$/.exec(lines[i] ?? "");
        if (!m) break;
        items.push(inline(m[1] ?? ""));
        i++;
      }
      out.push({ kind: "ul", items });
      continue;
    }
    const quote = /^\s*>\s?(.*)$/.exec(line);
    if (quote) {
      flush();
      const body: string[] = [];
      while (i < lines.length) {
        const m = /^\s*>\s?(.*)$/.exec(lines[i] ?? "");
        if (!m) break;
        body.push(m[1] ?? "");
        i++;
      }
      out.push({ kind: "quote", spans: inline(body.join(" ")) });
      continue;
    }
    if (!line.trim()) { flush(); i++; continue; }
    para.push(line.trim());
    i++;
  }
  flush();
  return out;
}

const INLINE = /(`[^`]+`)|(\[[^\]]+\]\([^)\s]+\))|(\*\*[^*]+\*\*)|(\*[^*]+\*)|(_[^_]+_)/;

/// One line of markdown as `{text, code, strong, em, href}` spans.
export function inline(text: string | null | undefined): Span[] {
  const src = String(text == null ? "" : text);
  const out: Span[] = [];
  let rest = src;
  while (rest) {
    const m = INLINE.exec(rest);
    if (!m) { out.push({ text: rest }); break; }
    if (m.index > 0) out.push({ text: rest.slice(0, m.index) });
    const tok = m[0];
    if (tok.startsWith("`")) out.push({ text: tok.slice(1, -1), code: true });
    else if (tok.startsWith("[")) {
      const cut = tok.indexOf("](");
      out.push({ text: tok.slice(1, cut), href: tok.slice(cut + 2, -1) });
    } else if (tok.startsWith("**")) out.push({ text: tok.slice(2, -2), strong: true });
    else out.push({ text: tok.slice(1, -1), em: true });
    rest = rest.slice(m.index + tok.length);
  }
  return out.length ? out : [{ text: "" }];
}

/// Is this a file the DOCS body should render rather than print?
///
/// A `README` with no extension is markdown by convention and by `isDoc`, so it
/// renders; anything else in the rail — a `LICENSE`, a `.txt` somebody's
/// `readme` filter let through — is printed as it is written.
export function rendersAsMarkdown(path: string | null | undefined): boolean {
  if (isBuiltin(path)) return true;
  const name = (String(path || "").split("/").pop() ?? "").toLowerCase();
  return name.endsWith(".md") || name.endsWith(".markdown") || name.startsWith("readme");
}
