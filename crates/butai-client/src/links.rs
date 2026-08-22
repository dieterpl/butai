//! The URLs on a drawn screen, and how to hand one to the desktop.
//!
//! A pane's cells are text and nothing more — the daemon holds the PTY, and a
//! program that prints `https://…` prints characters, not a link. So whether a
//! URL is clickable is entirely the drawing client's question, and this is this
//! client's answer to it: find the runs that read as URLs in the composed
//! screen, and then say so twice.
//!
//! - To the terminal butai is *drawn on*, as OSC 8 hyperlinks
//!   ([`crate::workbench`]'s painter), so the pointer works the way it does over
//!   any other program: hover, and cmd- or ctrl-click.
//! - To the keyboard, as the link picker — because a terminal that does not
//!   speak OSC 8 (tmux before 3.4 drops it, and it is the common case) would
//!   otherwise leave the feature reachable only by pointer, and because the
//!   workbench is driven from the keyboard anyway.
//!
//! Both read the same map, computed once per frame from the buffer that is
//! about to be painted, so what the terminal underlines and what the picker
//! lists cannot disagree.
//!
//! **Why the cells and not the pane's text.** The daemon could scan a pane's
//! rows and ship spans, and it would know one thing this cannot: which rows a
//! program *wrapped*. But it would also be a per-client rendering concern
//! crossing the wire, it would cover only PTY panes — not the diff, the rails,
//! the files page or anything else the client draws itself — and it would need
//! a protocol change for something every client can compute from cells it
//! already holds. See `docs/design.md`.

use std::collections::HashMap;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

/// What a run has to begin with to be a link.
///
/// Deliberately short. Every entry here is something a terminal is expected to
/// hand to a browser, so `javascript:` and `data:` are absent on purpose and
/// anything exotic (`vscode:`, `slack:`) is left to the user's own eyes: a
/// false positive is a cell that lies about being clickable, which is worse
/// than a URL that merely is not underlined.
const SCHEMES: &[&str] = &[
    "https://", "http://", "file://", "ftps://", "ftp://", "ssh://", "git://", "wss://", "ws://",
    "mailto:",
];

/// The one schemeless form worth catching. `www.` is how a URL is written in
/// prose, and a link that has to be retyped with `https://` in front of it is
/// not a link. Nothing else qualifies: `example.com` on its own is
/// indistinguishable from a sentence with no space after the full stop.
const BARE_HOST: &str = "www.";

/// The scheme a [`BARE_HOST`] match is opened with.
const BARE_SCHEME: &str = "https://";

/// A link would have to be malformed to be longer than this, and a run of
/// punctuation the width of a wide terminal is not one.
const MAX_URL: usize = 2048;

/// Enough cells to recognise any scheme in [`SCHEMES`] — `https://` is the
/// longest at eight. Read at the start of a row to decide whether it continues
/// the row above or begins a link of its own.
const SCHEME_CELLS: u16 = 8;

/// One clickable run, in the coordinates of whatever was scanned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// Index of the first character, into the slice that was scanned.
    pub start: usize,
    /// One past the last, so `end - start` is the width in cells.
    pub end: usize,
    /// What to open — which is not always what is on screen: a `www.` run
    /// carries the scheme this adds, and a trailing full stop is trimmed off.
    pub url: String,
}

/// Every URL on one line of text.
///
/// Characters rather than a `&str` because the caller's index *is* a column:
/// the screen is a cell grid, and a byte offset into a UTF-8 row would have to
/// be mapped back. One char per cell is a promise the caller keeps (see
/// [`ScreenLinks::of`]), not something this can check.
pub fn scan(line: &[char]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut i = 0;
    while i < line.len() {
        let Some(scheme_len) = starts_here(line, i) else {
            i += 1;
            continue;
        };
        // The run: every character a URL may contain, to the first that is not
        // one. Non-ASCII ends it — a URL can carry it, but a CJK glyph after
        // one is prose in every case that matters, and it is two cells wide,
        // which the caller's one-char-per-cell mapping cannot represent.
        let mut end = i + scheme_len;
        while end < line.len() && url_char(line[end]) {
            end += 1;
        }
        let raw: String = line[i..end].iter().collect();
        let trimmed = trim_tail(&raw);
        let end = i + trimmed.chars().count();
        if usable(trimmed, scheme_len) {
            let url = if trimmed.starts_with(BARE_HOST) {
                format!("{BARE_SCHEME}{trimmed}")
            } else {
                trimmed.to_string()
            };
            out.push(Span { start: i, end, url });
        }
        // Past the whole run either way. A rejected candidate is not a place to
        // look for a second link inside.
        i = end.max(i + 1);
    }
    out
}

/// [`scan`] over a string, for callers that have one (and for tests).
pub fn scan_str(line: &str) -> Vec<Span> {
    scan(&line.chars().collect::<Vec<_>>())
}

/// The length of the scheme starting at `i`, or `None` if no link starts there.
fn starts_here(line: &[char], i: usize) -> Option<usize> {
    // Inside a longer token, so not a start: `x-mailto:` is not mail, and the
    // second half of `http://h/https://x` is a path. Brackets, quotes and
    // punctuation *are* boundaries — `(https://x)` is how a URL is usually
    // written in prose.
    if i > 0 && !left_boundary(line[i - 1]) {
        return None;
    }
    for scheme in SCHEMES {
        if matches_at(line, i, scheme) {
            return Some(scheme.len());
        }
    }
    if matches_at(line, i, BARE_HOST) {
        return Some(BARE_HOST.len());
    }
    None
}

/// Case-insensitive because `HTTPS://` is a URL that has been shouted, and the
/// scheme is the one part of one that is defined to be case-insensitive.
fn matches_at(line: &[char], i: usize, word: &str) -> bool {
    let mut w = word.chars();
    let mut k = i;
    loop {
        let Some(c) = w.next() else { return true };
        match line.get(k) {
            Some(got) if got.eq_ignore_ascii_case(&c) => k += 1,
            _ => return false,
        }
    }
}

/// Whether a character can be *inside* a URL.
///
/// RFC 3986's unreserved and reserved sets, plus `%`. Which means it includes
/// the brackets and quotes prose wraps a URL in — they are stripped afterwards
/// by [`trim_tail`], where the balance can be judged, rather than here where
/// only one character is in hand.
fn url_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '-' | '.'
                | '_'
                | '~'
                | ':'
                | '/'
                | '?'
                | '#'
                | '['
                | ']'
                | '@'
                | '!'
                | '$'
                | '&'
                | '\''
                | '('
                | ')'
                | '*'
                | '+'
                | ','
                | ';'
                | '='
                | '%'
        )
}

/// Whether a character can sit immediately before the start of a link.
///
/// Narrower than "not a [`url_char`]": a URL in prose is routinely preceded by
/// `(`, `<`, `"` or `,`, and treating those as part of the token before it
/// would lose the link. What disqualifies a start is the run reading as the
/// *middle* of something — a word character, or the punctuation that joins
/// paths, hosts and versions.
fn left_boundary(c: char) -> bool {
    !(c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~' | '%' | '+' | '@' | ':' | '/'))
}

/// Drop the characters that ended the sentence rather than the URL.
///
/// Two rules, in a loop because a URL at the end of a parenthesis at the end of
/// a sentence has both: sentence punctuation goes, and a closing bracket goes
/// only when nothing opened it — so `…/Foo_(bar)` keeps its parenthesis and
/// `(see …/foo)` does not.
fn trim_tail(url: &str) -> &str {
    let mut url = url;
    loop {
        let Some(last) = url.chars().last() else { return url };
        let cut = match last {
            '.' | ',' | ':' | ';' | '!' | '?' | '\'' | '*' | '(' => true,
            ')' => count(url, '(') < count(url, ')'),
            ']' => count(url, '[') < count(url, ']'),
            _ => false,
        };
        if !cut {
            return url;
        }
        url = &url[..url.len() - last.len_utf8()];
    }
}

fn count(s: &str, c: char) -> usize {
    s.chars().filter(|x| *x == c).count()
}

/// Whether what is left after trimming is worth offering.
///
/// A scheme with nothing after it is not a link, `www.` needs a dot of its own
/// to be a host rather than a word, and anything past [`MAX_URL`] is a run of
/// punctuation that happens to have started with one.
fn usable(url: &str, scheme_len: usize) -> bool {
    if url.len() <= scheme_len || url.len() > MAX_URL {
        return false;
    }
    let rest = &url[scheme_len..];
    if !rest.chars().any(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    if url.starts_with(BARE_HOST) && !rest.contains('.') {
        return false;
    }
    true
}

/// One link's run on one row of the screen. A wrapped URL has several.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Run {
    x0: u16,
    /// Exclusive.
    x1: u16,
    /// Index into [`ScreenLinks::urls`].
    link: usize,
}

/// Every URL on a composed screen, by cell.
///
/// Built from the buffer that is about to be painted rather than from the panes
/// behind it, so a URL is found wherever it is drawn — a shell's output, an
/// agent's answer, a diff, a file, the git rail — and found once.
#[derive(Debug, Default, Clone)]
pub struct ScreenLinks {
    /// By row, in column order.
    rows: Vec<Vec<Run>>,
    /// Each distinct URL once, in the order first met reading down the screen.
    urls: Vec<String>,
}

impl ScreenLinks {
    /// Scan a composed screen.
    ///
    /// `stage` is where the streamed pane is, when one is showing
    /// ([`crate::chrome::stage_rect`]). It is the only region whose rows are
    /// joined before scanning: a pane is a program's own output wrapped at the
    /// pane's width, so a URL too long for the row continues on the next one,
    /// and scanning the rows separately would offer a link to a truncated
    /// address. Everywhere else the client is the one that laid the text out,
    /// and it truncates rather than wraps — joining there would splice two
    /// unrelated rows together.
    pub fn of(buf: &Buffer, stage: Option<Rect>) -> Self {
        let area = buf.area;
        let mut me = Self { rows: vec![Vec::new(); area.height as usize], urls: Vec::new() };
        let mut seen: HashMap<String, usize> = HashMap::new();
        // The stage first so its rows are joined, then the chrome to either
        // side of it: every cell is scanned exactly once, in reading order, and
        // the order the picker's list comes out in follows from that.
        let stage = stage.map(|s| s.intersection(area)).filter(|s| s.width > 0 && s.height > 0);
        if let Some(s) = stage {
            me.block(buf, s, true, &mut seen);
        }
        for y in area.y..area.bottom() {
            let row = Rect::new(area.x, y, area.width, 1);
            match stage.filter(|s| y >= s.y && y < s.bottom()) {
                None => me.block(buf, row, false, &mut seen),
                Some(s) => {
                    let left = Rect::new(area.x, y, s.x.saturating_sub(area.x), 1);
                    let right = Rect::new(s.right(), y, area.right().saturating_sub(s.right()), 1);
                    for part in [left, right].into_iter().filter(|r| r.width > 0) {
                        me.block(buf, part, false, &mut seen);
                    }
                }
            }
        }
        for runs in &mut me.rows {
            runs.sort_by_key(|r| r.x0);
        }
        me
    }

    /// Scan one rectangle, joining its rows when `join` is set.
    fn block(&mut self, buf: &Buffer, rect: Rect, join: bool, seen: &mut HashMap<String, usize>) {
        let mut chars: Vec<char> = Vec::with_capacity(rect.width as usize);
        let mut at: Vec<(u16, u16)> = Vec::with_capacity(rect.width as usize);
        let mut y = rect.y;
        while y < rect.bottom() {
            chars.clear();
            at.clear();
            let mut last = y;
            loop {
                for x in rect.x..rect.right() {
                    chars.push(cell_char(buf, x, last));
                    at.push((x, last));
                }
                // A row filled to its last column is a row a program ran out
                // of: the text continues on the next one. A row with a blank
                // there ended on its own, and joining it to what follows would
                // invent an address out of two lines of prose.
                //
                // Unless what follows *starts a link of its own*, which is not
                // a continuation of anything. A shell is the case that proves
                // it: `$ echo https://…` fills the row exactly and the echoed
                // URL lands underneath, so the two joined into one address that
                // was the URL written twice — and the picker offered it.
                let full = chars.last().is_some_and(|c| *c != ' ');
                let next = last + 1;
                if join && full && next < rect.bottom() && !row_starts_a_link(buf, rect, next) {
                    last = next;
                    continue;
                }
                break;
            }
            for span in scan(&chars) {
                let link = match seen.get(&span.url) {
                    Some(i) => *i,
                    None => {
                        self.urls.push(span.url.clone());
                        seen.insert(span.url, self.urls.len() - 1);
                        self.urls.len() - 1
                    }
                };
                // Back to cells, breaking at every row change: a span that
                // crossed a join is one link drawn on two rows.
                let mut i = span.start;
                while i < span.end {
                    let (x0, row) = at[i];
                    let mut x1 = x0;
                    while i < span.end && at[i].1 == row {
                        x1 = at[i].0 + 1;
                        i += 1;
                    }
                    if let Some(runs) = self.rows.get_mut(row.saturating_sub(buf.area.y) as usize) {
                        runs.push(Run { x0, x1, link });
                    }
                }
            }
            y = last + 1;
        }
    }

    /// The link under a cell: its id and what it points at.
    ///
    /// The id is the URL's own hash rather than a counter, so the same address
    /// keeps the same id from frame to frame and across the rows a wrapped one
    /// covers. That is what tells a terminal the cells are one link — OSC 8's
    /// `id=` parameter exists for exactly this — and a counter would renumber
    /// them on every repaint, leaving a hover highlighting whichever half was
    /// drawn last.
    pub fn at(&self, x: u16, y: u16) -> Option<(u64, &str)> {
        let runs = self.rows.get(y as usize)?;
        let run = runs.iter().find(|r| x >= r.x0 && x < r.x1)?;
        let url = self.urls.get(run.link)?;
        Some((id_of(url), url.as_str()))
    }

    /// Every distinct URL on screen, in reading order.
    pub fn urls(&self) -> &[String] {
        &self.urls
    }

    pub fn is_empty(&self) -> bool {
        self.urls.is_empty()
    }
}

/// Whether a row begins a link rather than continuing the one above it.
///
/// Only the first cells are read — enough for the longest scheme — because that
/// is the whole question: a wrapped URL continues in the middle of a path, and
/// a path does not begin with `https://`.
fn row_starts_a_link(buf: &Buffer, rect: Rect, y: u16) -> bool {
    let n = rect.width.min(SCHEME_CELLS);
    let head: Vec<char> = (0..n).map(|i| cell_char(buf, rect.x + i, y)).collect();
    starts_here(&head, 0).is_some()
}

/// One cell, one character.
///
/// The cell grid is what makes a column an index: the trailing half of a wide
/// glyph is an empty symbol and becomes a space, and a multi-character grapheme
/// (a flag, a combining mark) becomes one replacement character. Neither can be
/// part of a URL, so flattening them is free — and it keeps the promise
/// [`scan`] relies on, which is that char *i* is column *i*.
fn cell_char(buf: &Buffer, x: u16, y: u16) -> char {
    let Some(cell) = buf.cell((x, y)) else { return ' ' };
    let mut chars = cell.symbol().chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => c,
        (None, _) => ' ',
        (Some(_), Some(_)) => '\u{fffd}',
    }
}

/// FNV-1a, because the id only has to be stable and distinct, and pulling in a
/// hasher for eight bytes of hex would be the larger decision.
pub fn id_of(url: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in url.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Hand a URL to the desktop this client is running on.
///
/// The client, not the daemon: the daemon may be on another machine entirely,
/// and a browser opened there is a browser nobody can see. Even here it can
/// fail for a reason that is not a bug — a TUI's home is an ssh session, where
/// there is no display server to open anything on — so the caller is expected
/// to have something to fall back to, and the clipboard is it: OSC 52 reaches
/// the terminal emulator, which *is* on a desktop.
pub fn open(url: &str) -> Result<(), String> {
    // Never a shell, so nothing in a URL can be a command. The scheme check is
    // the second lock: this is only ever called with something [`scan`]
    // produced, and that list is an allowlist.
    if !SCHEMES.iter().any(|s| url.to_ascii_lowercase().starts_with(s)) {
        return Err(format!("not a link: {url}"));
    }
    let opener = opener().ok_or_else(|| "no desktop on this machine to open it".to_string())?;
    let child = std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("{opener}: {e}"));
    match child {
        Ok(mut child) => {
            // Reaped on a thread of its own: `xdg-open` can sit there for as
            // long as the browser takes to start, and the event loop is not
            // waiting for that. Not reaping it at all would leave a zombie per
            // link in a process that runs all day.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// The command that opens a URL here, or `None` when nothing can.
///
/// macOS always has `open` and no `$DISPLAY`, so the question is only asked on
/// Linux — the same split [`crate::clipboard`] makes, for the same reason.
fn opener() -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    {
        Some("open")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let set = |k| std::env::var_os(k).is_some_and(|v| !v.is_empty());
        (set("DISPLAY") || set("WAYLAND_DISPLAY")).then_some("xdg-open")
    }
}

/// Whether [`open`] has anywhere to open a link, so the picker can say what
/// Enter will do before it is pressed rather than after.
pub fn can_open() -> bool {
    opener().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(line: &str) -> Vec<String> {
        scan_str(line).into_iter().map(|s| s.url).collect()
    }

    #[test]
    fn a_run_is_found_and_ends_where_the_url_does() {
        let spans = scan_str("see https://example.com/a?b=1#c now");
        assert_eq!(spans.len(), 1, "{spans:?}");
        assert_eq!(spans[0].url, "https://example.com/a?b=1#c");
        // Columns, not bytes: the caller paints cell `start..end`.
        assert_eq!((spans[0].start, spans[0].end), (4, 31));
        assert_eq!(&"see https://example.com/a?b=1#c now"[4..31], spans[0].url);
    }

    #[test]
    fn every_shipped_scheme_is_recognised_and_nothing_else_is() {
        for scheme in SCHEMES {
            let line = format!("{scheme}host/x");
            assert_eq!(urls(&line), vec![line.clone()], "{scheme}");
        }
        // A shouted scheme is still a scheme.
        assert_eq!(urls("HTTPS://Example.COM/x"), vec!["HTTPS://Example.COM/x"]);
        // And the ones deliberately left out stay out.
        assert!(urls("javascript:alert(1)").is_empty());
        assert!(urls("data:text/html,<b>hi</b>").is_empty());
    }

    #[test]
    fn a_bare_host_gets_a_scheme_and_needs_a_dot_to_be_one() {
        assert_eq!(urls("go to www.example.com today"), vec!["https://www.example.com"]);
        // `www.` and a single word is not a host — it is a sentence that has
        // lost a space.
        assert!(urls("www.and then").is_empty());
    }

    /// The rule that decides whether a link works when it is clicked. Every one
    /// of these appears in ordinary agent output.
    #[test]
    fn sentence_punctuation_is_trimmed_and_balanced_brackets_are_not() {
        assert_eq!(urls("open https://example.com/a."), vec!["https://example.com/a"]);
        assert_eq!(urls("(see https://example.com/a)"), vec!["https://example.com/a"]);
        assert_eq!(urls("https://x/Foo_(bar)"), vec!["https://x/Foo_(bar)"]);
        assert_eq!(urls("https://x/a?b=1, and"), vec!["https://x/a?b=1"]);
        assert_eq!(urls("<https://x/a>"), vec!["https://x/a"]);
        assert_eq!(urls("\"https://x/a\""), vec!["https://x/a"]);
    }

    #[test]
    fn a_scheme_inside_a_longer_token_does_not_start_a_link() {
        // The host's own path, not a second link.
        assert_eq!(urls("https://x/y/https://z"), vec!["https://x/y/https://z"]);
        assert!(urls("x-mailto:me@example.com").is_empty());
        assert!(urls("nothttps://x/y").is_empty());
    }

    #[test]
    fn a_scheme_with_nothing_after_it_is_not_a_link() {
        assert!(urls("https://").is_empty());
        assert!(urls("https://.").is_empty());
        assert!(urls("mailto:").is_empty());
    }

    fn buf(lines: Vec<&str>) -> Buffer {
        Buffer::with_lines(lines)
    }

    #[test]
    fn a_wrapped_url_in_the_pane_is_one_link_over_two_rows() {
        // 20 columns. The URL fills the first row to the last cell and
        // continues on the next — which is what a pane does with a long one.
        let b = buf(vec!["https://example.com/", "a/long/path?x=1     ", "done                "]);
        let stage = Rect::new(0, 0, 20, 3);
        let links = ScreenLinks::of(&b, Some(stage));
        assert_eq!(links.urls(), ["https://example.com/a/long/path?x=1"]);
        // Both halves are the same link, so the terminal underlines all of it.
        let (top, _) = links.at(0, 0).expect("the first row is linked");
        let (bottom, url) = links.at(0, 1).expect("the continuation is linked");
        assert_eq!(top, bottom);
        assert_eq!(url, "https://example.com/a/long/path?x=1");
        assert!(links.at(0, 2).is_none(), "the row after it is not");
    }

    /// The case that shipped broken: a shell echoing a URL.
    ///
    /// `$ echo https://…` fills a narrow pane's row exactly, and the echoed
    /// address lands on the row below — so the "this row is full, it must have
    /// wrapped" rule joined them and produced the URL written twice, which the
    /// picker then offered as something to open.
    #[test]
    fn a_row_that_begins_a_link_is_not_a_continuation() {
        let b =
            buf(vec!["$ echo https://x/a?b=1", "https://x/a?b=1       ", "$                     "]);
        let links = ScreenLinks::of(&b, Some(Rect::new(0, 0, 22, 3)));
        assert_eq!(links.urls(), ["https://x/a?b=1"]);
        // Both rows carry it, and they are the same link because it is the same
        // address — not because one was spliced onto the other.
        assert_eq!(links.at(7, 0).map(|(_, u)| u), Some("https://x/a?b=1"));
        assert_eq!(links.at(0, 1).map(|(_, u)| u), Some("https://x/a?b=1"));
    }

    #[test]
    fn rows_the_client_laid_out_are_never_joined() {
        // The same two rows outside the stage: chrome truncates rather than
        // wraps, so splicing them would invent an address.
        let b = buf(vec!["https://example.com/", "a/long/path?x=1     "]);
        let links = ScreenLinks::of(&b, None);
        assert_eq!(links.urls(), ["https://example.com/"]);
    }

    #[test]
    fn a_row_that_ends_short_of_the_edge_is_not_joined_even_in_the_pane() {
        let b = buf(vec!["https://example.com ", "and then some text  "]);
        let links = ScreenLinks::of(&b, Some(Rect::new(0, 0, 20, 2)));
        assert_eq!(links.urls(), ["https://example.com"]);
    }

    #[test]
    fn the_rails_beside_the_stage_are_scanned_too() {
        // Left rail, stage, right rail on one row. The stage's own columns are
        // joined; the two sides are not, and none of the three is scanned twice.
        let b = buf(vec!["a.com https://s/1 www.r.io"]);
        let links = ScreenLinks::of(&b, Some(Rect::new(6, 0, 11, 1)));
        assert_eq!(links.urls(), ["https://s/1", "https://www.r.io"]);
        assert_eq!(links.at(6, 0).map(|(_, u)| u), Some("https://s/1"));
        assert_eq!(links.at(18, 0).map(|(_, u)| u), Some("https://www.r.io"));
    }

    #[test]
    fn the_same_url_twice_is_one_entry_and_one_id() {
        let b = buf(vec!["https://x/a  https://x/a"]);
        let links = ScreenLinks::of(&b, None);
        assert_eq!(links.urls().len(), 1);
        assert_eq!(links.at(0, 0).map(|(i, _)| i), links.at(13, 0).map(|(i, _)| i));
    }

    #[test]
    fn a_wide_glyph_before_a_link_does_not_shift_its_columns() {
        let mut b = Buffer::empty(Rect::new(0, 0, 14, 1));
        b.set_string(0, 0, "日 https://x/a", ratatui::style::Style::default());
        let links = ScreenLinks::of(&b, None);
        assert_eq!(links.urls(), ["https://x/a"]);
        // The wide glyph is two cells and the link starts after them.
        assert!(links.at(2, 0).is_none(), "the space is not part of it");
        assert_eq!(links.at(3, 0).map(|(_, u)| u), Some("https://x/a"));
    }

    #[test]
    fn an_id_is_the_urls_own_and_survives_a_rescan() {
        let a = ScreenLinks::of(&buf(vec!["https://x/a"]), None);
        let b = ScreenLinks::of(&buf(vec!["  https://x/a"]), None);
        assert_eq!(a.at(0, 0).map(|(i, _)| i), b.at(2, 0).map(|(i, _)| i));
        assert_ne!(id_of("https://x/a"), id_of("https://x/b"));
    }

    #[test]
    fn opening_refuses_anything_that_is_not_one_of_the_schemes() {
        assert!(open("javascript:alert(1)").is_err());
        assert!(open("; rm -rf /").is_err());
    }
}
