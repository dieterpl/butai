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

/// How far short of the widest row around it a row may stop and still read as
/// one a program wrapped. A hard wrapper fills the row to its width exactly, so
/// this is one cell of tolerance for a wrapper that reserves one — not a guess
/// at where a sentence ended.
const WRAP_SLACK: usize = 1;

/// How many rows either side of a wrapped one are read to find the width that
/// block of output was wrapped at. Enough to see the rest of a paragraph, few
/// enough that a status line further down the screen is not part of the answer.
const PARAGRAPH_REACH: usize = 6;

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
    /// joined before scanning: a pane is a program's own output, wrapped by the
    /// terminal at the pane's width or by the program at its own, so a URL too
    /// long for the row continues on the next one — and scanning the rows
    /// separately would offer a link to a truncated address. How a join is
    /// decided is [`continues`]. Everywhere else the client is the one that
    /// laid the text out, and it truncates rather than wraps — joining there
    /// would splice two unrelated rows together.
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
        let grid: Vec<Vec<char>> = (rect.y..rect.bottom())
            .map(|y| (rect.x..rect.right()).map(|x| cell_char(buf, x, y)).collect())
            .collect();
        for line in logical_lines(&grid, join) {
            for span in scan(&line.chars) {
                let link = match seen.get(&span.url) {
                    Some(i) => *i,
                    None => {
                        self.urls.push(span.url.clone());
                        seen.insert(span.url, self.urls.len() - 1);
                        self.urls.len() - 1
                    }
                };
                // Back to cells, breaking at every row change: a span that
                // crossed a join is one link drawn on two rows, and the
                // indentation the join stepped over belongs to neither.
                let mut i = span.start;
                while i < span.end {
                    let (x0, row) = line.at[i];
                    let mut x1 = x0;
                    while i < span.end && line.at[i].1 == row {
                        x1 = line.at[i].0 + 1;
                        i += 1;
                    }
                    let y = (rect.y as usize + row).saturating_sub(buf.area.y as usize);
                    if let Some(runs) = self.rows.get_mut(y) {
                        runs.push(Run { x0: rect.x + x0 as u16, x1: rect.x + x1 as u16, link });
                    }
                }
            }
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

/// One line of text as a program meant it, which may be several rows of the
/// screen: its characters, and for each the `(column, row)` of the grid it was
/// read from — so a span found in it can be painted back onto the cells.
#[derive(Debug, Default)]
struct Line {
    chars: Vec<char>,
    at: Vec<(usize, usize)>,
}

impl Line {
    fn push_row(&mut self, grid: &[Vec<char>], y: usize, from: usize) {
        for (x, c) in grid[y].iter().enumerate().skip(from) {
            self.chars.push(*c);
            self.at.push((x, y));
        }
    }

    fn truncate(&mut self, len: usize) {
        self.chars.truncate(len);
        self.at.truncate(len);
    }
}

/// How one row is laid out, measured once per frame.
#[derive(Debug, Clone, Copy)]
struct Shape {
    /// One past the last non-blank cell.
    end: usize,
    /// The same with a right-hand border taken off as well — a box's `│` and
    /// the blanks inside it — which is where the row's *text* stops.
    inner: usize,
    /// Whether the row has a letter or digit on it. A row without one is a
    /// rule, a border or nothing, and ends the paragraph rather than being in it.
    words: bool,
}

fn shape(row: &[char]) -> Shape {
    let end = row.iter().rposition(|c| *c != ' ').map_or(0, |i| i + 1);
    let inner = row[..end].iter().rposition(|c| *c != ' ' && !border(*c)).map_or(0, |i| i + 1);
    Shape { end, inner, words: row.iter().any(char::is_ascii_alphanumeric) }
}

/// Glyphs that frame text rather than being part of it: box drawing, the
/// eighth-blocks agents draw a quote bar with, and the bracket pieces — `⎿` is
/// how Claude Code hangs a tool's output off the call that made it.
fn frame(c: char) -> bool {
    matches!(c, '\u{2500}'..='\u{259f}' | '\u{23a2}'..='\u{23a5}' | '\u{23b8}' | '\u{23b9}' | '\u{23bf}')
}

/// What may close a row on the right: a frame glyph, or an ASCII box's `|`.
/// Only ever taken off when the row below has the same glyph in the same
/// column — `curl https://x/a |` ends in a pipe, not a box.
fn border(c: char) -> bool {
    frame(c) || c == '|'
}

/// Split a grid into the lines its program wrote.
///
/// Without `join` every row is a line. With it — the stage, where the text is
/// a program's own output — a row is continued onto the next when, and only
/// when, it ends in the middle of a link: see [`continues`] for the evidence
/// that takes. A continuation is added without its indentation or gutter, so
/// the address reads through the break as one run.
fn logical_lines(grid: &[Vec<char>], join: bool) -> Vec<Line> {
    let shapes: Vec<Shape> =
        if join { grid.iter().map(Vec::as_slice).map(shape).collect() } else { Vec::new() };
    let mut out = Vec::with_capacity(grid.len());
    let mut y = 0;
    while y < grid.len() {
        let mut line = Line::default();
        line.push_row(grid, y, 0);
        let mut last = y;
        let mut from = 0;
        while join && last + 1 < grid.len() {
            let Some(j) = continues(grid, &shapes, &line, last, from) else { break };
            line.truncate(j.keep);
            last += 1;
            from = j.from;
            line.push_row(grid, last, from);
        }
        out.push(line);
        y = last + 1;
    }
    out
}

/// How a row is joined to the line above it.
struct Join {
    /// How much of the line so far to keep: everything up to the end of the
    /// wrapped row's text, so its right margin and border are not spliced in.
    keep: usize,
    /// The column the continuation's text starts at, past its indentation.
    from: usize,
}

/// Whether row `y + 1` carries on the link that row `y` — the last row of
/// `line`, read from column `from` — ends in.
///
/// The screen does not say which rows wrapped, so this reads the evidence a
/// wrap leaves behind, and every clause is there because a real screen broke
/// without it:
///
/// 1. **The row ends inside a link.** The run of URL characters reaching its
///    last cell of text has to contain the start of one. A row ending in prose
///    is never joined, whatever follows it.
/// 2. **A terminal's own wrap**: the row is filled to its last column and the
///    next carries straight on from column 0. Joined unless the next row
///    starts a link of its own — `$ echo https://…` fills a narrow pane's row
///    exactly and the shell echoes the same URL underneath, and joining the two
///    offered that address written twice.
/// 3. **A program's own wrap** — an agent's TUI breaks a long URL at *its*
///    width, which need not be the pane's, and indents the rest under the text
///    it belongs to. That takes all of:
///    - the row reaching (within [`WRAP_SLACK`]) the right edge of the
///      paragraph around it, which is where that program wraps. A row that
///      stops short of its neighbours ended because the line did;
///    - the next row's first token, past blanks and gutter glyphs, being URL
///      characters with a letter or digit among them, and not a scheme;
///    - and that token *reading as a piece of an address* — a `/`, `.`, `-`,
///      `?`, `=` or the like in it — or being all that is left on its row.
///      This is what keeps prose apart: a URL that happens to end a line at
///      the wrap width is followed by a word and a space, and `and then` is as
///      made of URL characters as `ion.html` is.
fn continues(
    grid: &[Vec<char>],
    shapes: &[Shape],
    line: &Line,
    y: usize,
    from: usize,
) -> Option<Join> {
    let row = &grid[y];
    let next = &grid[y + 1];
    let s = shapes[y];
    // A box: the same border glyph in the same column below, so the text of
    // this row stops inside it.
    let boxed = s.inner < s.end && next.get(s.end - 1) == Some(&row[s.end - 1]);
    let end = if boxed { s.inner } else { s.end };
    if end <= from {
        return None;
    }
    let keep = line.chars.len() - (row.len() - end);
    if !ends_in_link(&line.chars[..keep]) {
        return None;
    }
    if end == row.len() && next.first().is_some_and(|c| *c != ' ' && !border(*c)) {
        return starts_here(next, 0).is_none().then_some(Join { keep, from: 0 });
    }
    if end + WRAP_SLACK < paragraph_width(shapes, y) {
        return None;
    }
    let start = indent(next, row);
    let rest = &next[start..];
    let len = rest.iter().take_while(|c| url_char(**c)).count();
    let token = &rest[..len];
    if !token.iter().any(char::is_ascii_alphanumeric) || starts_here(token, 0).is_some() {
        return None;
    }
    let alone = rest[len..].iter().all(|c| *c == ' ' || border(*c));
    (alone || addressy(token)).then_some(Join { keep, from: start })
}

/// Whether a line so far ends in the middle of a link — the run of URL
/// characters at its end contains a scheme or a `www.`, at a place [`scan`]
/// would have started one. Read without [`usable`], because a row can break
/// straight after `https://` and the address is on the next one.
fn ends_in_link(chars: &[char]) -> bool {
    let t0 = chars.iter().rposition(|c| !url_char(*c)).map_or(0, |i| i + 1);
    (t0..chars.len()).any(|i| starts_here(chars, i).is_some())
}

/// The right edge of the paragraph around row `y`: the furthest any row of it
/// reaches, the paragraph being the rows with words on them either side of it.
/// That is the width the program wrapped this block of output at, measured
/// rather than assumed to be the pane's.
fn paragraph_width(shapes: &[Shape], y: usize) -> usize {
    let lo = y.saturating_sub(PARAGRAPH_REACH);
    let hi = (y + PARAGRAPH_REACH).min(shapes.len() - 1);
    let up = (lo..y).rev().take_while(|k| shapes[*k].words);
    let down = (y + 1..=hi).take_while(|k| shapes[*k].words);
    up.chain(down).map(|k| shapes[k].inner).fold(shapes[y].inner, usize::max)
}

/// Where a continuation's text starts: past blanks, frame glyphs, and the ASCII
/// gutters that sit in the same column on the row above — a quote's `>`, a
/// table's `|`, and a diff's `+` or `-` when the text runs straight on from it
/// (`+e newest…`, as Claude Code wraps an added line). A `-` followed by a
/// space is a list item, which is a new line and not a gutter.
fn indent(next: &[char], above: &[char]) -> usize {
    let mut x = 0;
    while let Some(&c) = next.get(x) {
        let same = above.get(x) == Some(&c);
        let marker = matches!(c, '+' | '-') && next.get(x + 1).is_some_and(|n| *n != ' ');
        if !(c == ' ' || frame(c) || (same && (matches!(c, '|' | '>') || marker))) {
            break;
        }
        x += 1;
    }
    x
}

/// Whether a token reads as a piece of an address rather than a word: it keeps
/// one of the characters that join paths, hosts and queries once the sentence's
/// own punctuation is taken off its end.
fn addressy(token: &[char]) -> bool {
    let token: String = token.iter().collect();
    trim_tail(&token).chars().any(|c| {
        matches!(c, '/' | '.' | '-' | '_' | '?' | '=' | '&' | '%' | '#' | '~' | ':' | '@' | '+')
    })
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
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let url: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
        // The URL is data to the Windows shell API, never a command string.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                std::ptr::null(),
                url.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        } as isize;
        if result <= 32 {
            return Err(format!("could not open link (Windows error {result})"));
        }
        Ok(())
    }
    #[cfg(unix)]
    {
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
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let set = |k| std::env::var_os(k).is_some_and(|v| !v.is_empty());
        (set("DISPLAY") || set("WAYLAND_DISPLAY")).then_some("xdg-open")
    }
    #[cfg(windows)]
    {
        Some("Windows Shell")
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

    /// A pane `width` cells wide showing `rows`, scanned as the stage.
    fn pane(width: u16, rows: &[String]) -> ScreenLinks {
        let mut b = Buffer::empty(Rect::new(0, 0, width, rows.len() as u16));
        for (y, r) in rows.iter().enumerate() {
            assert!(r.chars().count() <= width as usize, "row {y} is wider than the pane: {r:?}");
            b.set_string(0, y as u16, r, ratatui::style::Style::default());
        }
        ScreenLinks::of(&b, Some(b.area))
    }

    fn rows(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|r| r.to_string()).collect()
    }

    /// Wrapped output as agents and shells actually draw it, and what each
    /// screen must come out as. Every row the joiner is meant to read as
    /// wrapped is exactly as wide as its paragraph's widest — which is what a
    /// hard wrapper leaves — and the cases that must *not* join are the ones
    /// that look most like the cases that must.
    #[test]
    fn wrapped_urls_are_joined_and_prose_is_not() {
        let box_row = |s: &str| format!("│ {s:<36}│");
        let long = format!("https://example.com/{}{}cccc/d.html", "a".repeat(13), "b".repeat(40));
        let cases: Vec<(&str, u16, Vec<String>, Vec<String>)> = vec![
            (
                "Claude Code wraps a command at the pane's edge and indents the rest",
                39,
                rows(&[
                    "● Bash(install the client)",
                    "  ⎿  $ curl -fsSL https://raw.githubuse",
                    "     rcontent.com/o/r/install.sh | sh",
                ]),
                rows(&["https://raw.githubusercontent.com/o/r/install.sh"]),
            ),
            (
                "an agent wraps at its own width, short of the pane's, and the prose after it stays out",
                60,
                rows(&[
                    "● The release notes are at https://github",
                    "  .com/dieterpl/butai/releases/tag/v1.5.0",
                    "  and the installer picks it up from it.",
                ]),
                rows(&["https://github.com/dieterpl/butai/releases/tag/v1.5.0"]),
            ),
            (
                "an indented markdown list item",
                60,
                rows(&[
                    "  1. Follow https://docs.example.com/guide/inst",
                    "     all-on-linux.html and then come back.",
                ]),
                rows(&["https://docs.example.com/guide/install-on-linux.html"]),
            ),
            (
                "a URL that ends a line of prose is not spliced onto the next line",
                60,
                rows(&[
                    "I put the notes at https://example.com/notes",
                    "and the code is next to them in the repo.",
                ]),
                rows(&["https://example.com/notes"]),
            ),
            (
                "the same, filled to the pane's edge, with the next line indented",
                44,
                rows(&[
                    "I put the notes at https://example.com/notes",
                    "  and the code is next to them in the repo.",
                ]),
                rows(&["https://example.com/notes"]),
            ),
            (
                "a row that stops short of its paragraph ended there, even before a path",
                60,
                rows(&[
                    "The spec lives at https://example.com/spec",
                    "src/spec.rs implements it, and the tests in",
                    "tests/spec.rs check it against the examples.",
                ]),
                rows(&["https://example.com/spec"]),
            ),
            (
                "two URLs on consecutive rows, the first filling the row",
                41,
                rows(&["https://example.com/first/link/that/is/lo", "https://example.com/second"]),
                rows(&["https://example.com/first/link/that/is/lo", "https://example.com/second"]),
            ),
            (
                "a list of links is a list, not one long link",
                60,
                rows(&[
                    "  - https://github.com/dieterpl/butai/pull/1",
                    "  - https://github.com/dieterpl/butai/pull/2",
                ]),
                rows(&[
                    "https://github.com/dieterpl/butai/pull/1",
                    "https://github.com/dieterpl/butai/pull/2",
                ]),
            ),
            (
                "a list item after a link does not continue it, even when it is a path",
                60,
                rows(&["  - https://example.com/a/b/c", "  - src/main.rs"]),
                rows(&["https://example.com/a/b/c"]),
            ),
            (
                "inside a box: the border is not part of either half",
                39,
                vec![box_row("https://example.com/a/very/long/pat"), box_row("h/to/the/thing")],
                rows(&["https://example.com/a/very/long/path/to/the/thing"]),
            ),
            (
                "behind a quote bar",
                60,
                rows(&["▎ Claim them at https://claude.ai/settings/bi", "▎ lling/credits"]),
                rows(&["https://claude.ai/settings/billing/credits"]),
            ),
            (
                "behind a markdown quote",
                60,
                rows(&["> see https://example.com/a/b/c/d/e/f", "> /g/h for the rest"]),
                rows(&["https://example.com/a/b/c/d/e/f/g/h"]),
            ),
            (
                "an added line in Claude Code's diff, wrapped behind its `+`",
                42,
                rows(&[
                    "      12 +  curl -fsSL https://raw.githubu",
                    "         +sercontent.com/o/r/install.sh",
                ]),
                rows(&["https://raw.githubusercontent.com/o/r/install.sh"]),
            ),
            (
                "broken straight after the scheme",
                60,
                rows(&["see the dashboard at https://", "   grafana.example.com/d/abc"]),
                rows(&["https://grafana.example.com/d/abc"]),
            ),
            (
                "three rows, the middle one nothing but address",
                60,
                vec![
                    format!("● Opened {}", &long[..33]),
                    format!("  {}", &long[33..73]),
                    format!("  {}", &long[73..]),
                ],
                vec![long],
            ),
            (
                "a pipe at the end of a command is not a box",
                40,
                rows(&["  $ curl https://example.com/a |", "    .data/x.json"]),
                rows(&["https://example.com/a"]),
            ),
            (
                "a blank row ends it",
                40,
                rows(&["https://example.com/aaaaaaaaaaaaaaaaaaaa", "", "bbb/c.html"]),
                rows(&["https://example.com/aaaaaaaaaaaaaaaaaaaa"]),
            ),
            (
                "a terminal's own wrap carries on from column 0, word or not",
                30,
                rows(&["https://example.com/documentat", "ion and more"]),
                rows(&["https://example.com/documentation"]),
            ),
            (
                "known loss: a program-wrapped tail of bare letters followed by prose",
                60,
                rows(&["  open https://example.com/documentat", "  ion and then read the rest of it."]),
                rows(&["https://example.com/documentat"]),
            ),
        ];
        for (name, width, screen, want) in cases {
            let links = pane(width, &screen);
            assert_eq!(links.urls(), want.as_slice(), "{name}");
        }
    }

    /// Every fragment is the one link, and none of the indentation is.
    #[test]
    fn a_joined_url_covers_its_cells_and_not_the_indent() {
        let links = pane(
            39,
            &rows(&[
                "● Bash(install the client)",
                "  ⎿  $ curl -fsSL https://raw.githubuse",
                "     rcontent.com/o/r/install.sh | sh",
            ]),
        );
        let url = "https://raw.githubusercontent.com/o/r/install.sh";
        let id = id_of(url);
        assert_eq!(links.at(18, 1), Some((id, url)), "the first half");
        assert_eq!(links.at(38, 1), Some((id, url)), "to the last column");
        assert_eq!(links.at(4, 2), None, "the indent is not linked");
        assert_eq!(links.at(5, 2), Some((id, url)), "the second half");
        assert_eq!(links.at(31, 2), Some((id, url)), "to its last character");
        assert_eq!(links.at(32, 2), None, "and no further");

        let boxed = pane(
            39,
            &[
                format!("│ {:<36}│", "https://example.com/a/very/long/pat"),
                format!("│ {:<36}│", "h/to/the/thing"),
            ],
        );
        assert!(boxed.at(38, 0).is_none(), "the right border");
        assert!(boxed.at(0, 1).is_none(), "the left border");
        assert!(boxed.at(2, 1).is_some(), "the text inside it");
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
