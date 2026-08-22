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
