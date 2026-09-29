// The Finder trail: which directories are on screen, and where the cursor is.
//
// The FILES page's browser is a row of columns — every directory on the path
// from the workspace root to where you are, each still listed, with the row you
// came through still marked. This is that path as a value, and the four moves
// that change it.
//
// It lives here rather than in the page for the reason everything in `logic/`
// does: it is arithmetic, it has invariants worth pinning, and a component is a
// bad place to test either. It is also the closest thing this client has to a
// port of the terminal's `chrome::Files` — the same trail, the same four moves,
// the same rule about when a column is dropped — and two clients that browse
// differently is exactly the drift the tables and these functions exist to stop.
//
// ## The one rule
//
// **Moving the cursor drops the columns to its right.** They are what the *old*
// selection contained, so leaving them under a new one would draw a path that
// does not exist: `src` selected in the root column, with `docs/`'s listing
// still beside it. Walking between columns — `←` and `→` — deliberately does
// not move a cursor, which is what lets it walk back and forth over a trail
// nothing has to re-fetch.

/** The path from the workspace root to the deepest directory opened. */
export type Trail = Readonly<{
  /**
   * The columns, outermost first. **Never empty**: `dirs[0]` is the workspace
   * root (`""`), and every reader here is written against that rather than
   * against a null check.
   */
  dirs: readonly string[];
  /**
   * Which column has the keyboard. Independent of `dirs.length`, since `←`
   * walks back through columns without discarding them.
   */
  col: number;
  /**
   * A cursor per *directory*, not per column: walk out of a folder and back
   * into it and the row you were on is still the row you are on.
   */
  cursor: Readonly<Record<string, number>>;
}>;

/** A browser sitting at the workspace root with nothing opened. */
export const ROOT: Trail = Object.freeze({ dirs: [""], col: 0, cursor: {} });

/** The directory the cursor is in. */
export function here(t: Trail): string {
  return t.dirs[Math.min(t.col, t.dirs.length - 1)] ?? "";
}

/**
 * The cursor in `dir`, clamped into a listing `len` rows long.
 *
 * Clamped on read rather than on write because the listing arrives *after* the
 * move that opened it, and a cursor stored against a directory nobody has
 * fetched yet has nothing to be clamped against.
 */
export function rowIn(t: Trail, dir: string, len: number): number {
  return Math.min(t.cursor[dir] ?? 0, Math.max(0, len - 1));
}

/**
 * Put the cursor on row `row` of column `i`, dropping the trail to its right.
 *
 * The exception is the row that is already selected in the deepest column:
 * re-pointing at it changes nothing, so it must not throw a column away — which
 * is what a click on the row a click already selected would otherwise do.
 */
export function point(t: Trail, i: number, row: number): Trail {
  const dir = t.dirs[i] ?? "";
  const settled = i === t.dirs.length - 1 && (t.cursor[dir] ?? 0) === row;
  return {
    dirs: settled ? t.dirs : t.dirs.slice(0, i + 1),
    col: i,
    cursor: { ...t.cursor, [dir]: row },
  };
}

/**
 * Open `path` as the column after `i`, and step into it.
 *
 * When the trail already reaches there this is only a step, which is the whole
 * reason the columns to the right are kept: `←` then `→` is two local moves and
 * no round trip, and over a slow link that is the difference between browsing
 * and waiting.
 */
export function into(t: Trail, i: number, path: string): Trail {
  if (t.dirs[i + 1] === path) return { ...t, col: i + 1 };
  return { ...t, dirs: [...t.dirs.slice(0, i + 1), path], col: i + 1 };
}

/** A file was chosen in column `i`, and a file contains nothing. */
export function trim(t: Trail, i: number): Trail {
  return { ...t, dirs: t.dirs.slice(0, i + 1) };
}

/**
 * Step one column towards the root.
 *
 * At the root it is a no-op rather than an error: there is nowhere above the
 * workspace, and a browser that could leave it would be a file tree that is not
 * about this project.
 */
export function left(t: Trail): Trail {
  return t.col === 0 ? t : { ...t, col: t.col - 1 };
}

/** Whether `path`'s directory is still one of the columns on screen. */
export function holds(t: Trail, dir: string): boolean {
  return t.dirs.includes(dir);
}
