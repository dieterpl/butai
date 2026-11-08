// The URLs on a drawn screen — the browser client's half of `links.rs`.
//
// Same problem, same answer, different pointer. A pane's cells are text and
// nothing more: the daemon holds the PTY and ships characters with colours, so
// whether a URL is clickable is the drawing client's question. The terminal
// client marks its cells up as OSC 8 hyperlinks and lets the terminal do the
// rest; a <canvas> has no such thing to hand off to, so <Screen> hit-tests this
// map on hover and opens the tab itself.
//
// **The rules are deliberately identical** to `crates/butai-client/src/links.rs`
// — the schemes, the character sets, the trimming, the row joining. Two clients
// that disagreed about what a link is would be a bug report about the one you
// happen to be using, so the tests in `web/test/links.test.ts` mirror the Rust
// ones case for case.

/** One clickable run, in the coordinates of whatever was scanned. */
export interface Span {
  /** Index of the first character, into the array that was scanned. */
  start: number;
  /** One past the last, so `end - start` is the width in cells. */
  end: number;
  /** What to open — not always what is on screen: a `www.` run carries the
   *  scheme this adds, and a trailing full stop is trimmed off. */
  url: string;
}

/**
 * What a run has to begin with to be a link.
 *
 * `javascript:` and `data:` are absent on purpose: everything here is something
 * a browser is expected to be handed, and a cell that lies about being
 * clickable is worse than a URL that merely is not underlined.
 */
const SCHEMES = [
  "https://", "http://", "file://", "ftps://", "ftp://",
  "ssh://", "git://", "wss://", "ws://", "mailto:",
] as const;

/** The one schemeless form worth catching, and the scheme it gets. */
const BARE_HOST = "www.";
const BARE_SCHEME = "https://";

/** A link would have to be malformed to be longer than this. */
const MAX_URL = 2048;

/** How far short of the widest row around it a row may stop and still read as
 *  one a program wrapped — one cell of tolerance, not a guess at where a
 *  sentence ended. */
const WRAP_SLACK = 1;

/** How many rows either side of a wrapped one are read to find the width that
 *  block of output was wrapped at. */
const PARAGRAPH_REACH = 6;

const ALNUM = /[0-9A-Za-z]/;
/** RFC 3986's unreserved and reserved sets, plus `%`. */
const URL_CHAR = /[0-9A-Za-z\-._~:/?#[\]@!$&'()*+,;=%]/;
/** What may sit immediately before a link. Narrower than "not a URL character":
 *  prose routinely wraps one in `(`, `<` or `"`, and only a run that reads as
 *  the *middle* of something disqualifies a start. */
const JOINER = /[0-9A-Za-z\-._~%+@:/]/;

/** Every URL on one line of text, one array entry per cell. */
export function scan(line: readonly string[]): Span[] {
  const out: Span[] = [];
  let i = 0;
  while (i < line.length) {
    const schemeLen = startsHere(line, i);
    if (schemeLen === null) { i++; continue; }
    let end = i + schemeLen;
    while (end < line.length && URL_CHAR.test(line[end] ?? "")) end++;
    const trimmed = trimTail(line.slice(i, end).join(""));
    end = i + trimmed.length;
    if (usable(trimmed, schemeLen)) {
      out.push({
        start: i,
        end,
        url: trimmed.startsWith(BARE_HOST) ? BARE_SCHEME + trimmed : trimmed,
      });
    }
    // Past the whole run either way: a rejected candidate is not a place to
    // look for a second link inside.
    i = Math.max(end, i + 1);
  }
  return out;
}

/** [`scan`] over a string, for callers that have one (and for tests). */
export function scanText(line: string): Span[] {
  return scan([...line]);
}

/** The length of the scheme starting at `i`, or null if no link starts there. */
function startsHere(line: readonly string[], i: number): number | null {
  if (i > 0 && JOINER.test(line[i - 1] ?? "")) return null;
  for (const scheme of SCHEMES) if (matchesAt(line, i, scheme)) return scheme.length;
  if (matchesAt(line, i, BARE_HOST)) return BARE_HOST.length;
  return null;
}

/** Case-insensitive: the scheme is the one part of a URL defined to be. */
function matchesAt(line: readonly string[], i: number, word: string): boolean {
  for (let k = 0; k < word.length; k++) {
    const got = line[i + k];
    if (got === undefined || got.toLowerCase() !== word[k]) return false;
  }
  return true;
}

/**
 * Drop the characters that ended the sentence rather than the URL: sentence
 * punctuation always, a closing bracket only when nothing opened it — so
 * `…/Foo_(bar)` keeps its parenthesis and `(see …/foo)` does not.
 */
function trimTail(url: string): string {
  for (;;) {
    const last = url.at(-1);
    if (last === undefined) return url;
    let cut = ".,:;!?'*(".includes(last);
    if (last === ")") cut = count(url, "(") < count(url, ")");
    if (last === "]") cut = count(url, "[") < count(url, "]");
    if (!cut) return url;
    url = url.slice(0, -1);
  }
}

function count(s: string, c: string): number {
  let n = 0;
  for (const ch of s) if (ch === c) n++;
  return n;
}

/** A scheme with nothing after it is not a link, `www.` needs a dot of its own
 *  to be a host rather than a word, and anything past `MAX_URL` is a run of
 *  punctuation that happened to start with one. */
function usable(url: string, schemeLen: number): boolean {
  if (url.length <= schemeLen || url.length > MAX_URL) return false;
  const rest = url.slice(schemeLen);
  if (!ALNUM.test(rest)) return false;
  if (url.startsWith(BARE_HOST) && !rest.includes(".")) return false;
  return true;
}

/** One link's run on one row. A wrapped URL has several. */
export interface LinkRun {
  x0: number;
  /** Exclusive. */
  x1: number;
  /** Index into `ScreenLinks.urls`. */
  link: number;
}

/** How one row is laid out: one past its last non-blank cell; the same with a
 *  right-hand border taken off too, which is where its text stops; and whether
 *  it has a letter or digit on it at all. */
interface Shape { end: number; inner: number; words: boolean }

/** Glyphs that frame text rather than being part of it: box drawing, the
 *  eighth-blocks of a quote bar, and the bracket pieces (`⎿`). */
const FRAME = /[─-▟⎢-⎥⎸⎹⎿]/;
/** What may close a row on the right — only taken off when the row below has
 *  the same glyph in the same column, because `curl https://x/a |` is a pipe. */
const isBorder = (c: string) => FRAME.test(c) || c === "|";
/** What a token keeps, once the sentence's punctuation is off its end, when it
 *  is a piece of an address rather than a word. */
const ADDRESSY = /[/.\-_?=&%#~:@+]/;

function shape(row: readonly string[]): Shape {
  let end = row.length;
  while (end > 0 && row[end - 1] === " ") end--;
  let inner = end;
  while (inner > 0 && (row[inner - 1] === " " || isBorder(row[inner - 1] ?? ""))) inner--;
  return { end, inner, words: row.some((c) => ALNUM.test(c)) };
}

/** Whether the run of URL characters ending a line holds the start of a link.
 *  Read without `usable`, because a row can break straight after `https://`. */
function endsInLink(chars: readonly string[]): boolean {
  let t0 = chars.length;
  while (t0 > 0 && URL_CHAR.test(chars[t0 - 1] ?? "")) t0--;
  for (let i = t0; i < chars.length; i++) if (startsHere(chars, i) !== null) return true;
  return false;
}

/** The furthest any row of the paragraph around `y` reaches — the width the
 *  program wrapped that block of output at. */
function paragraphWidth(shapes: readonly Shape[], y: number): number {
  let w = shapes[y]?.inner ?? 0;
  for (let k = y - 1; k >= Math.max(0, y - PARAGRAPH_REACH) && shapes[k]?.words; k--) {
    w = Math.max(w, shapes[k]?.inner ?? 0);
  }
  for (let k = y + 1; k <= Math.min(shapes.length - 1, y + PARAGRAPH_REACH) && shapes[k]?.words; k++) {
    w = Math.max(w, shapes[k]?.inner ?? 0);
  }
  return w;
}

/** Where a continuation's text starts: past blanks, frame glyphs, and the ASCII
 *  gutters in the same column on the row above — `>`, `|`, and a diff's `+` or
 *  `-` when the text runs straight on from it. */
function indent(next: readonly string[], above: readonly string[]): number {
  let x = 0;
  while (x < next.length) {
    const c = next[x] ?? " ";
    const same = above[x] === c;
    const marker = (c === "+" || c === "-") && next[x + 1] !== undefined && next[x + 1] !== " ";
    if (!(c === " " || FRAME.test(c) || (same && (c === "|" || c === ">" || marker)))) break;
    x++;
  }
  return x;
}

/** Whether row `y + 1` carries on the link row `y` ends in, and if so how much
 *  of the line to keep and which column the continuation starts at. The same
 *  three clauses as `continues` in `links.rs`, which documents them. */
function continues(
  grid: readonly (readonly string[])[], shapes: readonly Shape[],
  chars: readonly string[], y: number, from: number,
): { keep: number; from: number } | null {
  const row = grid[y] ?? [];
  const next = grid[y + 1] ?? [];
  const s = shapes[y];
  if (!s) return null;
  const boxed = s.inner < s.end && next[s.end - 1] === row[s.end - 1];
  const end = boxed ? s.inner : s.end;
  if (end <= from) return null;
  const keep = chars.length - (row.length - end);
  if (!endsInLink(chars.slice(0, keep))) return null;
  const first = next[0];
  if (end === row.length && first !== undefined && first !== " " && !isBorder(first)) {
    return startsHere(next, 0) === null ? { keep, from: 0 } : null;
  }
  if (end + WRAP_SLACK < paragraphWidth(shapes, y)) return null;
  const start = indent(next, row);
  const rest = next.slice(start);
  let len = 0;
  while (len < rest.length && URL_CHAR.test(rest[len] ?? "")) len++;
  const token = rest.slice(0, len);
  if (!token.some((c) => ALNUM.test(c)) || startsHere(token, 0) !== null) return null;
  const alone = rest.slice(len).every((c) => c === " " || isBorder(c));
  return alone || ADDRESSY.test(trimTail(token.join(""))) ? { keep, from: start } : null;
}

/**
 * Every URL on a pane's cell grid.
 *
 * **Rows are joined** when one ends in the middle of a link and the next reads
 * as the rest of it — the terminal's own wrap at the pane's edge, or a
 * program's at its own width with the rest indented underneath. The rules are
 * `continues` in `links.rs`, transcribed. The terminal client applies them to
 * the stage only, since the rest of its screen is chrome it laid out itself;
 * here the whole grid *is* the pane, so they apply throughout.
 */
export class ScreenLinks {
  private constructor(
    private readonly _rows: LinkRun[][],
    private readonly _urls: string[],
  ) {}

  static of(cells: readonly (readonly string[])[]): ScreenLinks {
    const rows: LinkRun[][] = cells.map(() => []);
    const urls: string[] = [];
    const seen = new Map<string, number>();
    // The trailing half of a wide glyph is an empty cell and becomes a space,
    // so that char *i* is column *i* — the promise `scan` reads by.
    const grid = cells.map((row) => row.map((c) => (c === "" ? " " : c)));
    const shapes = grid.map(shape);
    let y = 0;
    while (y < grid.length) {
      const chars: string[] = [];
      const at: [number, number][] = [];
      const push = (r: number, from: number) => {
        const row = grid[r] ?? [];
        for (let x = from; x < row.length; x++) {
          chars.push(row[x] ?? " ");
          at.push([x, r]);
        }
      };
      push(y, 0);
      let last = y;
      let from = 0;
      while (last + 1 < grid.length) {
        const j = continues(grid, shapes, chars, last, from);
        if (!j) break;
        chars.length = j.keep;
        at.length = j.keep;
        last++;
        from = j.from;
        push(last, from);
      }
      for (const span of scan(chars)) {
        let link = seen.get(span.url);
        if (link === undefined) {
          link = urls.length;
          urls.push(span.url);
          seen.set(span.url, link);
        }
        // Back to cells, breaking at every row change: a span that crossed a
        // join is one link drawn on two rows.
        let i = span.start;
        while (i < span.end) {
          const here = at[i];
          if (!here) break;
          const [x0, row] = here;
          let x1 = x0;
          while (i < span.end && at[i]?.[1] === row) {
            x1 = (at[i]?.[0] ?? x0) + 1;
            i++;
          }
          rows[row]?.push({ x0, x1, link });
        }
      }
      y = last + 1;
    }
    return new ScreenLinks(rows, urls);
  }

  /** The link under a cell, or null. */
  at(x: number, y: number): { link: number; url: string } | null {
    const run = this._rows[y]?.find((r) => x >= r.x0 && x < r.x1);
    if (!run) return null;
    const url = this._urls[run.link];
    return url === undefined ? null : { link: run.link, url };
  }

  /** The runs on one row, for drawing. */
  rowRuns(y: number): readonly LinkRun[] {
    return this._rows[y] ?? [];
  }

  /** Each distinct URL once, in the order first met reading down the grid. */
  get urls(): readonly string[] {
    return this._urls;
  }
}
