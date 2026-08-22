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

/** Enough cells to recognise any scheme above — `https://` is the longest at
 *  eight. Read at the start of a row to decide whether it continues the row
 *  above or begins a link of its own. */
const SCHEME_CELLS = 8;

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

/**
 * Every URL on a pane's cell grid.
 *
 * **Rows are joined** when the last cell of one is not blank, because that is
 * what a program's own wrapping leaves behind — an address too long for the row
 * continues at the start of the next, and scanning the rows separately would
 * offer a link to a truncated address. The terminal client applies this rule to
 * the stage only, since the rest of its screen is chrome it laid out itself;
 * here the whole grid *is* the pane, so it applies throughout.
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
    let y = 0;
    while (y < cells.length) {
      const chars: string[] = [];
      const at: [number, number][] = [];
      let last = y;
      for (;;) {
        const row = cells[last] ?? [];
        for (let x = 0; x < row.length; x++) {
          // The trailing half of a wide glyph is an empty cell and becomes a
          // space, so that char *i* is column *i* — the promise `scan` reads by.
          chars.push(row[x] === "" ? " " : (row[x] ?? " "));
          at.push([x, last]);
        }
        const full = chars.length > 0 && chars[chars.length - 1] !== " ";
        // ...unless the next row *starts a link of its own*, which is not a
        // continuation of anything. A shell is the case that proves it: `$ echo
        // https://…` fills the row exactly and the echoed URL lands underneath,
        // so the two joined into one address that was the URL written twice.
        const next = cells[last + 1];
        const continues = next !== undefined &&
          startsHere(next.slice(0, SCHEME_CELLS).map((c) => (c === "" ? " " : c)), 0) === null;
        if (full && continues) { last++; continue; }
        break;
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
