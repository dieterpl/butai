// The link scanner, case for case with `crates/butai-client/src/links.rs`.
//
// Two clients that disagreed about what a URL is would be a bug report about
// whichever one you happen to be using, so these are the Rust tests transcribed
// rather than a fresh set written against this implementation.

import { describe, expect, test } from "bun:test";
import { ScreenLinks, scanText } from "../src/logic/links.ts";

const urls = (line: string) => scanText(line).map((s) => s.url);
const grid = (...rows: string[]) => rows.map((r) => [...r]);

describe("what counts as a link", () => {
  test("a run is found and ends where the URL does", () => {
    const spans = scanText("see https://example.com/a?b=1#c now");
    expect(spans.length).toBe(1);
    expect(spans[0]!.url).toBe("https://example.com/a?b=1#c");
    // Columns, not characters of the original string: the caller paints
    // `start..end`.
    expect([spans[0]!.start, spans[0]!.end]).toEqual([4, 31]);
  });

  test("every shipped scheme is recognised and nothing else is", () => {
    for (const scheme of ["https://", "http://", "file://", "ftp://", "ftps://",
                          "ssh://", "git://", "ws://", "wss://", "mailto:"]) {
      expect(urls(`${scheme}host/x`)).toEqual([`${scheme}host/x`]);
    }
    expect(urls("HTTPS://Example.COM/x")).toEqual(["HTTPS://Example.COM/x"]);
    expect(urls("javascript:alert(1)")).toEqual([]);
    expect(urls("data:text/html,<b>hi</b>")).toEqual([]);
  });

  test("a bare host gets a scheme and needs a dot to be one", () => {
    expect(urls("go to www.example.com today")).toEqual(["https://www.example.com"]);
    expect(urls("www.and then")).toEqual([]);
  });

  test("sentence punctuation is trimmed and balanced brackets are not", () => {
    expect(urls("open https://example.com/a.")).toEqual(["https://example.com/a"]);
    expect(urls("(see https://example.com/a)")).toEqual(["https://example.com/a"]);
    expect(urls("https://x/Foo_(bar)")).toEqual(["https://x/Foo_(bar)"]);
    expect(urls("https://x/a?b=1, and")).toEqual(["https://x/a?b=1"]);
    expect(urls("<https://x/a>")).toEqual(["https://x/a"]);
    expect(urls('"https://x/a"')).toEqual(["https://x/a"]);
  });

  test("a scheme inside a longer token does not start a link", () => {
    expect(urls("https://x/y/https://z")).toEqual(["https://x/y/https://z"]);
    expect(urls("x-mailto:me@example.com")).toEqual([]);
    expect(urls("nothttps://x/y")).toEqual([]);
  });

  test("a scheme with nothing after it is not a link", () => {
    expect(urls("https://")).toEqual([]);
    expect(urls("https://.")).toEqual([]);
    expect(urls("mailto:")).toEqual([]);
  });
});

describe("the grid", () => {
  test("a wrapped URL is one link over two rows", () => {
    const links = ScreenLinks.of(grid(
      "https://example.com/",
      "a/long/path?x=1     ",
      "done                ",
    ));
    expect(links.urls).toEqual(["https://example.com/a/long/path?x=1"]);
    expect(links.at(0, 0)?.link).toBe(links.at(0, 1)!.link);
    expect(links.at(0, 1)?.url).toBe("https://example.com/a/long/path?x=1");
    expect(links.at(0, 2)).toBeNull();
  });

  test("a row that begins a link is not a continuation", () => {
    // The case that shipped broken in the terminal client: `$ echo https://…`
    // fills a narrow row exactly and the echoed address lands underneath, so
    // "this row is full, it must have wrapped" produced the URL written twice.
    const links = ScreenLinks.of(grid(
      "$ echo https://x/a?b=1",
      "https://x/a?b=1       ",
      "$                     ",
    ));
    expect(links.urls).toEqual(["https://x/a?b=1"]);
    expect(links.at(7, 0)?.url).toBe("https://x/a?b=1");
    expect(links.at(0, 1)?.url).toBe("https://x/a?b=1");
  });

  test("a row that ends short of the edge is not joined", () => {
    const links = ScreenLinks.of(grid("https://example.com ", "and then some text  "));
    expect(links.urls).toEqual(["https://example.com"]);
  });

  test("the same URL twice is one entry", () => {
    const links = ScreenLinks.of(grid("https://x/a  https://x/a"));
    expect(links.urls.length).toBe(1);
    expect(links.at(0, 0)?.link).toBe(links.at(13, 0)!.link);
  });

  test("a wide glyph's trailing cell does not shift the columns", () => {
    // The daemon sends the second half of a wide glyph as an empty cell; the
    // grid keeps it as a column, and the scanner reads it as a space.
    const links = ScreenLinks.of([["日", "", " ", ...[..."https://x/a"]]]);
    expect(links.urls).toEqual(["https://x/a"]);
    expect(links.at(2, 0)).toBeNull();
    expect(links.at(3, 0)?.url).toBe("https://x/a");
  });

  test("the runs of a wrapped link cover both rows, for the hover underline", () => {
    const links = ScreenLinks.of(grid("https://example.com/", "a/long/path?x=1     "));
    expect(links.rowRuns(0)).toEqual([{ x0: 0, x1: 20, link: 0 }]);
    expect(links.rowRuns(1)).toEqual([{ x0: 0, x1: 15, link: 0 }]);
  });
});

/** A pane `width` cells wide showing `rows`, every row padded to the width. */
const pane = (width: number, rows: string[]) =>
  rows.map((r) => {
    const cells = [...r];
    if (cells.length > width) throw new Error(`row is wider than the pane: ${r}`);
    return [...cells, ...Array<string>(width - cells.length).fill(" ")];
  });

describe("wrapped output", () => {
  // `wrapped_urls_are_joined_and_prose_is_not` in links.rs, row for row.
  const boxRow = (s: string) => `│ ${s.padEnd(36)}│`;
  const long = `https://example.com/${"a".repeat(13)}${"b".repeat(40)}cccc/d.html`;
  const cases: [string, number, string[], string[]][] = [
    [
      "Claude Code wraps a command at the pane's edge and indents the rest", 39,
      ["● Bash(install the client)", "  ⎿  $ curl -fsSL https://raw.githubuse", "     rcontent.com/o/r/install.sh | sh"],
      ["https://raw.githubusercontent.com/o/r/install.sh"],
    ],
    [
      "an agent wraps at its own width, short of the pane's, and the prose after it stays out", 60,
      ["● The release notes are at https://github", "  .com/dieterpl/butai/releases/tag/v1.5.0", "  and the installer picks it up from it."],
      ["https://github.com/dieterpl/butai/releases/tag/v1.5.0"],
    ],
    [
      "an indented markdown list item", 60,
      ["  1. Follow https://docs.example.com/guide/inst", "     all-on-linux.html and then come back."],
      ["https://docs.example.com/guide/install-on-linux.html"],
    ],
    [
      "a URL that ends a line of prose is not spliced onto the next line", 60,
      ["I put the notes at https://example.com/notes", "and the code is next to them in the repo."],
      ["https://example.com/notes"],
    ],
    [
      "the same, filled to the pane's edge, with the next line indented", 44,
      ["I put the notes at https://example.com/notes", "  and the code is next to them in the repo."],
      ["https://example.com/notes"],
    ],
    [
      "a row that stops short of its paragraph ended there, even before a path", 60,
      ["The spec lives at https://example.com/spec", "src/spec.rs implements it, and the tests in", "tests/spec.rs check it against the examples."],
      ["https://example.com/spec"],
    ],
    [
      "two URLs on consecutive rows, the first filling the row", 41,
      ["https://example.com/first/link/that/is/lo", "https://example.com/second"],
      ["https://example.com/first/link/that/is/lo", "https://example.com/second"],
    ],
    [
      "a list of links is a list, not one long link", 60,
      ["  - https://github.com/dieterpl/butai/pull/1", "  - https://github.com/dieterpl/butai/pull/2"],
      ["https://github.com/dieterpl/butai/pull/1", "https://github.com/dieterpl/butai/pull/2"],
    ],
    [
      "a list item after a link does not continue it, even when it is a path", 60,
      ["  - https://example.com/a/b/c", "  - src/main.rs"],
      ["https://example.com/a/b/c"],
    ],
    [
      "inside a box: the border is not part of either half", 39,
      [boxRow("https://example.com/a/very/long/pat"), boxRow("h/to/the/thing")],
      ["https://example.com/a/very/long/path/to/the/thing"],
    ],
    [
      "behind a quote bar", 60,
      ["▎ Claim them at https://claude.ai/settings/bi", "▎ lling/credits"],
      ["https://claude.ai/settings/billing/credits"],
    ],
    [
      "behind a markdown quote", 60,
      ["> see https://example.com/a/b/c/d/e/f", "> /g/h for the rest"],
      ["https://example.com/a/b/c/d/e/f/g/h"],
    ],
    [
      "an added line in Claude Code's diff, wrapped behind its `+`", 42,
      ["      12 +  curl -fsSL https://raw.githubu", "         +sercontent.com/o/r/install.sh"],
      ["https://raw.githubusercontent.com/o/r/install.sh"],
    ],
    [
      "broken straight after the scheme", 60,
      ["see the dashboard at https://", "   grafana.example.com/d/abc"],
      ["https://grafana.example.com/d/abc"],
    ],
    [
      "three rows, the middle one nothing but address", 60,
      [`● Opened ${long.slice(0, 33)}`, `  ${long.slice(33, 73)}`, `  ${long.slice(73)}`],
      [long],
    ],
    [
      "a pipe at the end of a command is not a box", 40,
      ["  $ curl https://example.com/a |", "    .data/x.json"],
      ["https://example.com/a"],
    ],
    [
      "a blank row ends it", 40,
      ["https://example.com/aaaaaaaaaaaaaaaaaaaa", "", "bbb/c.html"],
      ["https://example.com/aaaaaaaaaaaaaaaaaaaa"],
    ],
    [
      "a terminal's own wrap carries on from column 0, word or not", 30,
      ["https://example.com/documentat", "ion and more"],
      ["https://example.com/documentation"],
    ],
    [
      "known loss: a program-wrapped tail of bare letters followed by prose", 60,
      ["  open https://example.com/documentat", "  ion and then read the rest of it."],
      ["https://example.com/documentat"],
    ],
  ];
  for (const [name, width, rows, want] of cases) {
    test(name, () => expect(ScreenLinks.of(pane(width, rows)).urls).toEqual(want));
  }

  test("a joined URL covers its cells and not the indent", () => {
    const links = ScreenLinks.of(pane(39, [
      "● Bash(install the client)",
      "  ⎿  $ curl -fsSL https://raw.githubuse",
      "     rcontent.com/o/r/install.sh | sh",
    ]));
    const url = "https://raw.githubusercontent.com/o/r/install.sh";
    expect(links.at(18, 1)?.url).toBe(url);
    expect(links.at(38, 1)?.url).toBe(url);
    expect(links.at(4, 2)).toBeNull();
    expect(links.at(5, 2)?.url).toBe(url);
    expect(links.at(31, 2)?.url).toBe(url);
    expect(links.at(32, 2)).toBeNull();
    expect(links.rowRuns(2)).toEqual([{ x0: 5, x1: 32, link: 0 }]);

    const boxed = ScreenLinks.of(pane(39, [
      boxRow("https://example.com/a/very/long/pat"), boxRow("h/to/the/thing"),
    ]));
    expect(boxed.at(38, 0)).toBeNull();
    expect(boxed.at(0, 1)).toBeNull();
    expect(boxed.at(2, 1)).not.toBeNull();
  });
});
