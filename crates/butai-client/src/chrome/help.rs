//! The HELP page: butai's own reference, as a page you enter, read and leave.
//!
//! **It was the DOCS page, and that is what this fixes.** `?` and `[help]` used
//! to set `Page::Docs`, point its rail at a `butai://reference` folder and open
//! a topic in the file viewer — so pressing help rebuilt the *file* screen
//! around a listing that was not files, with a breadcrumb, a `..` row, a find
//! button and an editor that had to refuse to save. Three separate notes in
//! this tree describe patching that up; the shape was the problem. A press on
//! help is not a request to browse a project.
//!
//! So it is a page of its own, on the terms [`super::Settings`] already set:
//! not in [`Page::ORDER`], because that list is the views *of one workspace*
//! and this is about the program; entered and left rather than cycled, with the
//! page you came from remembered so `esc` puts you back; and drawn over the
//! whole band, because a reference squeezed between two rails is the modal
//! problem again in a different frame.
//!
//! **Nothing here is a file.** The topics are `&'static str` in
//! [`crate::reference`], laid out by [`read`] as they are drawn — so the page
//! opens with no daemon in the loop at all, which is why it reads the same over
//! ssh as it does locally, and why there is no path, no save and no editor.

use super::{ellipsize, put_str, Geom, LRect, Page, Pen, Theme, View};
use crate::reference::{Topic, TOPICS};
use ratatui::buffer::Buffer;

/// The page's own state: which topic is open, how far into it you have read,
/// and where to go back to.
///
/// Its own struct rather than fields on [`View`], for [`super::Settings`]'s
/// reason: it is about one page. It outlives leaving, though — walk away
/// mid-page and come back and you are where you stopped, which is what anything
/// you read in instalments has to do.
#[derive(Debug, Clone)]
pub struct Help {
    /// Index into [`TOPICS`].
    pub topic: usize,
    /// First line of the topic drawn — the reading position, counted in the
    /// lines [`read`] produced rather than in the source, so it means the same
    /// thing after a resize wrapped the prose differently.
    pub scroll: usize,
    /// The page this one was entered from, so `esc` puts it back rather than
    /// dropping you somewhere you never were.
    pub ret: Page,
}

impl Default for Help {
    fn default() -> Self {
        // Opens on the keys, which is what the modal this descends from held and
        // what `?` means to anyone who has used tmux. After that it is wherever
        // you left it.
        Self {
            topic: crate::reference::index_of(crate::reference::HELP_SLUG),
            scroll: 0,
            ret: Page::Agents,
        }
    }
}

impl Help {
    /// Move to another topic, from the top of it.
    ///
    /// The scroll is per page rather than per topic on purpose: arriving
    /// halfway down something you have not read yet is never what was meant,
    /// and a scroll position per topic is eleven numbers to keep true.
    pub fn go(&mut self, topic: usize) {
        self.topic = topic.min(TOPICS.len() - 1);
        self.scroll = 0;
    }
}

/// Columns the contents list takes. The longest row is `Mouse and clipboard` at
/// 19, and it sits behind a two-column gutter.
const LIST_W: u16 = 24;
/// The widest the reading column is drawn, however wide the terminal is.
///
/// The reference is prose, and prose set to 200 columns is unreadable — the eye
/// loses the line it is on coming back from the right edge. The topics are hard
/// wrapped at 74; this leaves room for that plus the gutter, and whatever is
/// left over is margin.
const BODY_MAX_W: u16 = 82;
/// Columns of gutter before the text, in both columns.
const GUTTER: u16 = 3;
/// Below this there is no point wrapping to the width — the words are wider
/// than the column. It clips instead, which at least keeps the left edge.
const MIN_TEXT_W: u16 = 20;

/// Where the page's two columns sit: the contents, and the page being read.
pub struct Columns {
    pub list: LRect,
    pub body: LRect,
}

pub fn columns(outer: LRect) -> Columns {
    let w = LIST_W.min(outer.width);
    Columns {
        list: LRect::new(outer.x, outer.y, w, outer.height),
        body: LRect::new(outer.x + w, outer.y, outer.width.saturating_sub(w), outer.height),
    }
}

/// Columns of text the reading column holds — what [`read`] wraps to.
pub fn text_width(body: LRect) -> u16 {
    body.width.min(BODY_MAX_W).saturating_sub(GUTTER + 1).max(MIN_TEXT_W)
}

/// Rows of text it holds. The last row is the verbs, and the one above it is
/// the gap that keeps the prose off them.
pub fn text_height(body: LRect) -> u16 {
    body.height.saturating_sub(2)
}

/// The furthest `scroll` may go: far enough to bring the last line onto the
/// last row, and not one line further. A page shorter than the column does not
/// scroll at all.
pub fn max_scroll(lines: usize, height: u16) -> usize {
    lines.saturating_sub(height as usize)
}

/// Which topic row `y` is over, if any.
pub fn topic_at(list: LRect, y: u16) -> Option<usize> {
    let first = list.y + 2;
    if y < first || y >= list.y + list.height {
        return None;
    }
    let i = (y - first) as usize;
    (i < TOPICS.len()).then_some(i)
}

/// The keys that work on this page, drawn under it.
///
/// Every one of them is also a key the rest of the workbench uses for the same
/// thing — the same rule the rail verbs follow.
pub fn verbs() -> [(&'static str, &'static str); 4] {
    [("j/k", "scroll"), ("tab", "next page"), ("home/end", "top · bottom"), ("esc", "close")]
}

// ---------------------------------------------------------------------------
// The topic, as lines
// ---------------------------------------------------------------------------

/// What a run of text on the page *is*, which is all the renderer needs to know
/// to colour it.
///
/// Not a markdown AST. The reference is written for this page and hard wrapped
/// for it, so the five things it actually contains are the five things here —
/// and a parser that handled more would be a parser with cases nothing in the
/// tree exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    /// The `# ` line every topic opens with.
    Title,
    /// A `## ` line.
    Head,
    /// Ordinary prose.
    Text,
    /// The left-hand column of an indented block: a key, or a command you type.
    Key,
    /// `backticked`, with the backticks taken off.
    Code,
    /// `**bold**`, with the stars taken off.
    Strong,
}

/// A run of text on one line, and what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub ink: Ink,
}

impl Span {
    fn new(text: impl Into<String>, ink: Ink) -> Self {
        Self { text: text.into(), ink }
    }
}

/// One drawn row. Empty for a blank line.
pub type Line = Vec<Span>;

/// A topic laid out for a column `width` wide, with the prefix key filled in.
///
/// Pure, and the only thing that decides how long a topic is — so the scroll
/// clamp, the "more below" mark and the paint all count the same lines, and a
/// resize cannot leave the page scrolled past its own end.
pub fn read(topic: &Topic, prefix: &str, width: u16) -> Vec<Line> {
    let width = width.max(MIN_TEXT_W) as usize;
    let body = topic.body.replace(crate::reference::PREFIX_MARK, prefix);
    let mut out: Vec<Line> = Vec::new();
    for src in body.lines() {
        if let Some(rest) = src.strip_prefix("## ") {
            out.push(vec![Span::new(rest.trim(), Ink::Head)]);
        } else if let Some(rest) = src.strip_prefix("# ") {
            out.push(vec![Span::new(rest.trim(), Ink::Title)]);
        } else if src.starts_with("    ") {
            out.extend(typed(src, width));
        } else if src.trim().is_empty() {
            out.push(Line::new());
        } else {
            out.extend(wrap(inline(src, Ink::Text), width));
        }
    }
    out
}

/// An indented line: the key or command on the left, and whatever explains it.
///
/// The two are separated by a run of two or more spaces, which is how every
/// table in the reference is written. A line without one is a command you type
/// whole (`butai agent kill $P`), and all of it is the left-hand column.
///
/// **The key column is never wrapped and the gloss is** — under the column it
/// started in, so a table narrower than its own text loses no words and still
/// reads as two columns. Wrapping the whole line instead would put the tail of
/// one row under the next row's key, where it reads as another key; clipping it
/// (which this did first) drops the end of every explanation at 80 columns,
/// which is a common enough terminal to design for.
///
/// A command with no gloss is clipped, because there is nothing to indent it
/// *to* and a command broken across two lines is a command you cannot copy.
fn typed(src: &str, width: usize) -> Vec<Line> {
    let indent = src.len() - src.trim_start().len();
    let rest = &src[indent..];
    let Some(cut) = gap(rest) else { return vec![vec![Span::new(src, Ink::Key)]] };
    let key = Span::new(&src[..indent + cut], Ink::Key);
    // Where the gloss starts, which is where any continuation of it goes.
    let col = indent + cut + (rest[cut..].len() - rest[cut..].trim_start().len());
    let room = width.saturating_sub(col).max(MIN_TEXT_W as usize);
    let gloss = rest[cut..].trim_start();
    let mut wrapped = wrap(inline(gloss, Ink::Text), room).into_iter();
    let Some(first) = wrapped.next() else { return vec![vec![key]] };

    let mut line = vec![key, Span::new(" ".repeat(col - (indent + cut)), Ink::Text)];
    line.extend(first);
    let mut out = vec![line];
    for more in wrapped {
        let mut cont = vec![Span::new(" ".repeat(col), Ink::Text)];
        cont.extend(more);
        out.push(cont);
    }
    out
}

/// Byte offset of the first run of two or more spaces, if there is one.
fn gap(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    (0..bytes.len().saturating_sub(1)).find(|&i| bytes[i] == b' ' && bytes[i + 1] == b' ')
}

/// One line of prose as spans, with `code`, **bold** and *emphasis* picked out.
///
/// An unpaired marker is text, not a bug to report: a page that swallowed the
/// rest of a line looking for a partner would be worse than one that prints a
/// star. `**` is tried before `*` at the same position, so bold is not read as
/// emphasis around an empty string.
///
/// Emphasis draws as bold rather than italic because [`Pen`] has no italic and
/// terminals disagree about it. Both marks mean the same thing in this text
/// anyway — the author leaning on a word.
fn inline(text: &str, base: Ink) -> Line {
    const MARKS: [(&str, Ink); 3] = [("**", Ink::Strong), ("*", Ink::Strong), ("`", Ink::Code)];
    let mut out = Line::new();
    let mut rest = text;
    while !rest.is_empty() {
        let found = MARKS
            .iter()
            .filter_map(|(mark, ink)| {
                let open = rest.find(mark)?;
                let after = open + mark.len();
                let close = rest[after..].find(mark)? + after;
                Some((open, *mark, *ink, close))
            })
            .min_by_key(|(open, mark, ..)| (*open, std::cmp::Reverse(mark.len())));
        let Some((open, mark, ink, close)) = found else {
            out.push(Span::new(rest, base));
            break;
        };
        if open > 0 {
            out.push(Span::new(&rest[..open], base));
        }
        out.push(Span::new(&rest[open + mark.len()..close], ink));
        rest = &rest[close + mark.len()..];
    }
    out
}

/// A line broken to `width`, on spaces, with each word keeping its ink.
///
/// **Over spans rather than over the source text**, which is the whole reason
/// this is not two lines of `split_whitespace`. Wrapping first and reading the
/// markers afterwards splits a `code span` that straddles the break, and both
/// halves then print a bare backtick — which is what the reference did at the
/// one width where its longest line does not fit. Caught by a test over every
/// topic, and it would have shipped otherwise.
///
/// The topics are already wrapped to 74, so on any ordinary terminal this
/// returns the line it was given and the page reads exactly as it is written.
/// It is the narrow window that needs it — a paragraph clipped at the right
/// edge loses the end of every sentence in it.
fn wrap(spans: Line, width: usize) -> Vec<Line> {
    if spans.iter().map(|s| s.text.chars().count()).sum::<usize>() <= width {
        return vec![spans];
    }
    // (word, ink, whether a space separated it from the one before). A word
    // whose ink changes mid-way becomes two tokens with no space between them,
    // and the layout below never breaks there — `the `x`th` is one word.
    let mut toks: Vec<(String, Ink, bool)> = Vec::new();
    let mut cur: Option<(String, Ink, bool)> = None;
    let mut space = false;
    for span in spans {
        for ch in span.text.chars() {
            if ch == ' ' {
                toks.extend(cur.take());
                space = true;
                continue;
            }
            match cur.as_mut() {
                Some(t) if t.1 == span.ink => t.0.push(ch),
                _ => {
                    toks.extend(cur.take());
                    cur = Some((ch.to_string(), span.ink, std::mem::take(&mut space)));
                }
            }
        }
    }
    toks.extend(cur);

    let mut out: Vec<Line> = Vec::new();
    let mut line = Line::new();
    let mut w = 0usize;
    for (text, ink, space_before) in toks {
        let tw = text.chars().count();
        if space_before && !line.is_empty() {
            if w + 1 + tw > width {
                out.push(std::mem::take(&mut line));
                w = 0;
            } else {
                line.push(Span::new(" ", Ink::Text));
                w += 1;
            }
        }
        line.push(Span::new(text, ink));
        w += tw;
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

pub fn draw(buf: &mut Buffer, geom: &Geom, h: Option<&Help>, view: &View, theme: &Theme) {
    let Some(state) = h else { return };
    let area = columns(geom.stage_box);
    let topic = state.topic.min(TOPICS.len() - 1);
    draw_list(buf, area.list, topic, theme);
    draw_body(buf, area.body, &TOPICS[topic], state, view, theme);
}

fn draw_list(buf: &mut Buffer, area: LRect, topic: usize, theme: &Theme) {
    let bound = area.x + area.width;
    put_str(
        buf,
        area.x + 1,
        area.y,
        "HELP",
        bound,
        Pen { fg: theme.accent, bg: theme.ground, bold: true },
    );

    for (i, t) in TOPICS.iter().enumerate() {
        let y = area.y + 2 + i as u16;
        if y >= area.y + area.height {
            break;
        }
        let on = i == topic;
        let bg = theme.row_bg(on);
        for x in area.x..bound {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_bg(bg);
            }
        }
        if on {
            put_str(buf, area.x + 1, y, ">", bound, Pen::new(theme.accent, bg));
        }
        let fg = if on { theme.accent } else { theme.muted };
        let room = area.width.saturating_sub(GUTTER + 1) as usize;
        put_str(buf, area.x + GUTTER, y, &ellipsize(t.name, room), bound, Pen { fg, bg, bold: on });
    }

    // Which of them you are on, where SETTINGS says which file it writes: the
    // bottom of the contents column, out of the way of the reading.
    let bottom = area.y + area.height;
    if bottom > area.y + 3 {
        put_str(
            buf,
            area.x + 1,
            bottom - 1,
            &format!("page {} of {}", topic + 1, TOPICS.len()),
            bound,
            Pen::new(theme.faint, theme.ground),
        );
    }
}

fn draw_body(
    buf: &mut Buffer,
    area: LRect,
    topic: &Topic,
    state: &Help,
    view: &View,
    theme: &Theme,
) {
    let bound = area.x + area.width.min(BODY_MAX_W);
    let x = area.x + GUTTER;
    let lines = read(topic, &view.prefix, text_width(area));
    let height = text_height(area);
    let scroll = state.scroll.min(max_scroll(lines.len(), height));

    for (row, line) in lines.iter().skip(scroll).take(height as usize).enumerate() {
        let y = area.y + row as u16;
        let mut cx = x;
        for span in line {
            if cx >= bound {
                break;
            }
            let pen = match span.ink {
                Ink::Title => Pen { fg: theme.accent, bg: theme.ground, bold: true },
                Ink::Head => Pen { fg: theme.ink, bg: theme.ground, bold: true },
                Ink::Text => Pen::new(theme.ink, theme.ground),
                Ink::Key => Pen::new(theme.accent, theme.ground),
                Ink::Code => Pen::new(theme.info, theme.ground),
                Ink::Strong => Pen { fg: theme.ink, bg: theme.ground, bold: true },
            };
            put_str(buf, cx, y, &span.text, bound, pen);
            cx += span.text.chars().count() as u16;
        }
    }

    // The verbs, where every list in this client draws them, and — hard right —
    // whether there is more of this page under the fold. The reference lost a
    // reported feature to exactly that question once, as a modal that scrolled
    // without saying so.
    let vy = area.y + area.height.saturating_sub(1);
    let mut vx = x;
    for (key, label) in verbs() {
        let w = key.len() as u16 + label.len() as u16 + 3;
        if vx + w >= bound {
            break;
        }
        put_str(buf, vx, vy, key, bound, Pen { fg: theme.accent, bg: theme.ground, bold: true });
        vx += key.len() as u16 + 1;
        put_str(buf, vx, vy, label, bound, Pen::new(theme.faint, theme.ground));
        vx += label.len() as u16 + 3;
    }
    if scroll < max_scroll(lines.len(), height) {
        let more = "more below";
        let mx = bound.saturating_sub(more.len() as u16 + 2);
        if mx > vx {
            put_str(buf, mx, vy, more, bound, Pen::new(theme.muted, theme.ground));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::{Scene, SysDto};
    use ratatui::layout::Rect;

    const COLS: u16 = 120;
    const ROWS: u16 = 40;

    fn screen(state: &Help) -> Vec<String> {
        let mut buf = Buffer::empty(Rect::new(0, 0, COLS, ROWS));
        let sys = SysDto::default();
        let view = View { page: Page::Help, ..Default::default() };
        let scene = Scene { help: Some(state), ..Scene::new(&[], &sys) };
        super::super::draw(&mut buf, COLS, ROWS, &scene, &view, &Theme::default());
        (0..ROWS)
            .map(|y| {
                (0..COLS)
                    .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn geom() -> Geom {
        let view = View { page: Page::Help, ..Default::default() };
        crate::chrome::page_geom(COLS, ROWS, &view)
    }

    /// The whole point of the page: it is the reference, not a file tree.
    ///
    /// What went wrong twice was that help borrowed a surface built for
    /// something else — first a modal, then the DOCS page — and arrived wearing
    /// that surface's furniture. So this pins the absence of it: no breadcrumb,
    /// no `..`, no find button, and the contents are topics rather than paths.
    #[test]
    fn the_page_is_the_reference_and_not_a_file_screen() {
        let out = screen(&Help::default()).join("\n");
        assert!(out.contains("HELP"), "the page does not name itself:\n{out}");
        for t in TOPICS {
            assert!(out.contains(t.name), "`{}` is missing from the contents:\n{out}", t.name);
        }
        // The body is the topic `?` opens on, drawn as a page rather than as a
        // file called `reference/keys.md`.
        assert!(out.contains("# Keys") || out.contains("Keys"), "no body:\n{out}");
        // The file page's own furniture, by the exact strings it draws: the find
        // button on the tree box's border, the breadcrumb it titles that box
        // with, the sentinel paths the topics used to carry, and the name the
        // reading column gave them. (Loose needles do not work here — the Keys
        // page itself contains the words `files · docs · docker` and `alt-1..9`,
        // which is its own argument for why the reference is not a file tree.)
        for furniture in ["[find]", " docs · /", "butai://", "reference/keys.md"] {
            assert!(!out.contains(furniture), "the file screen's `{furniture}` came along:\n{out}");
        }
    }

    /// Every topic draws, at every width the page is likely to see. A topic
    /// that panics or comes out blank is one nobody would find until they
    /// pressed `tab` eleven times.
    #[test]
    fn every_topic_draws_something() {
        for (i, t) in TOPICS.iter().enumerate() {
            let out = screen(&Help { topic: i, scroll: 0, ret: Page::Agents }).join("\n");
            let title = t.body.lines().next().unwrap().trim_start_matches("# ");
            assert!(out.contains(title), "`{}` drew no title:\n{out}", t.slug);
        }
    }

    /// The reading position stops at the end of the page, and the page says so
    /// while there is more — the modal this descends from scrolled silently,
    /// and a clipped list read as the whole list.
    #[test]
    fn it_says_when_there_is_more_and_stops_at_the_end() {
        let area = columns(geom().stage_box);
        let long = TOPICS
            .iter()
            .position(|t| {
                read(t, "^B", text_width(area.body)).len() > text_height(area.body) as usize
            })
            .expect("some topic is longer than the screen");
        let top = screen(&Help { topic: long, scroll: 0, ret: Page::Agents }).join("\n");
        assert!(top.contains("more below"), "a page with more to read did not say so:\n{top}");

        let lines = read(&TOPICS[long], "^B", text_width(area.body));
        let end = max_scroll(lines.len(), text_height(area.body));
        let bottom = screen(&Help { topic: long, scroll: end, ret: Page::Agents }).join("\n");
        assert!(!bottom.contains("more below"), "the end of the page still promised more");
        // The last line of the topic is on screen at the end, and not one row
        // past it — the clamp and the paint count the same lines.
        let last = lines
            .iter()
            .rev()
            .find_map(|l| l.first().map(|s| s.text.trim().to_string()).filter(|s| !s.is_empty()))
            .expect("the topic ends with a line of text");
        assert!(bottom.contains(&last), "the last line never came into view:\n{bottom}");
    }

    /// A key table is a table: the key on the left, its gloss where it was
    /// written, and the spaces between them intact. Wrapping one would put the
    /// second half of a command under the next key.
    #[test]
    fn a_key_row_keeps_its_two_columns() {
        let topic = Topic { name: "t", slug: "t", body: "# T\n\n    alt-o       files\n" };
        let lines = read(&topic, "^B", 74);
        let row = lines.last().expect("the key row");
        assert_eq!(row[0], Span::new("    alt-o", Ink::Key));
        let gloss: String = row[1..].iter().map(|s| s.text.as_str()).collect();
        assert_eq!(gloss, "       files", "the second column moved");
        assert_eq!(row[0].text.len() + gloss.len(), "    alt-o       files".len());
    }

    /// A command with no gloss is all one thing, rather than split at whatever
    /// two spaces happen to be inside it.
    #[test]
    fn a_bare_command_is_not_split() {
        let topic = Topic { name: "t", slug: "t", body: "# T\n\n    butai agent ls\n" };
        let lines = read(&topic, "^B", 74);
        assert_eq!(lines.last().unwrap(), &vec![Span::new("    butai agent ls", Ink::Key)]);
    }

    /// Markers come off the text they mark. A reference that printed its own
    /// backticks would be a reference nobody wrote.
    #[test]
    fn inline_markers_are_read_rather_than_printed() {
        let topic = Topic {
            name: "t",
            slug: "t",
            body: "# T\n\nPress `a`, **not** *that*, and 2 * 3 stays.\n",
        };
        let line = read(&topic, "^B", 74).into_iter().nth(2).expect("the prose line");
        assert!(line.contains(&Span::new("a", Ink::Code)), "{line:?}");
        assert!(line.contains(&Span::new("not", Ink::Strong)), "{line:?}");
        assert!(line.contains(&Span::new("that", Ink::Strong)), "{line:?}");
        let text: String = line.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "Press a, not that, and 2 * 3 stays.", "an unpaired `*` was eaten");
    }

    /// No topic prints a marker it meant as formatting, at any width.
    ///
    /// The whole reference goes through this, because the failure mode is one
    /// section written in a style the reader was not taught — and it reads as a
    /// page nobody rendered. **Every width, because that is what caught the
    /// real bug**: wrapping split a code span across two lines and each half
    /// printed a bare backtick, at 74 and nowhere else.
    #[test]
    fn no_topic_shows_its_own_markup() {
        for t in TOPICS {
            for line in [30u16, 40, 52, 74, 80, 120].into_iter().flat_map(|w| read(t, "^B", w)) {
                let prose: String = line
                    .iter()
                    .filter(|s| s.ink == Ink::Text || s.ink == Ink::Title || s.ink == Ink::Head)
                    .map(|s| s.text.as_str())
                    .collect();
                assert!(!prose.contains('`'), "{}: a backtick reached the screen: {prose}", t.slug);
                assert!(!prose.contains("**"), "{}: bold markers reached the screen", t.slug);
            }
        }
    }

    /// The prefix key is configurable, and the reference is the one place that
    /// must not print a placeholder.
    #[test]
    fn the_prefix_is_filled_in_at_the_width_it_is_drawn() {
        let keys = &TOPICS[crate::reference::index_of("keys")];
        for width in [40u16, 74, 120] {
            let text: String = read(keys, "^A", width)
                .iter()
                .flat_map(|l| l.iter().map(|s| s.text.clone()))
                .collect();
            assert!(!text.contains(crate::reference::PREFIX_MARK), "the placeholder survived");
            assert!(text.contains("^A"), "the configured prefix is not in the page");
        }
    }

    /// Prose is wrapped to the column and the topics are already wrapped to 74,
    /// so an ordinary terminal draws them exactly as they are written.
    #[test]
    fn a_wide_column_changes_nothing_and_a_narrow_one_wraps() {
        let topic = Topic {
            name: "t",
            slug: "t",
            body: "# T\n\nThe workbench has a fixed frame, and nothing about it moves.\n",
        };
        assert_eq!(read(&topic, "^B", 74).len(), 3, "a wide column re-wrapped the prose");
        let narrow = read(&topic, "^B", 20);
        assert!(narrow.len() > 3, "a narrow column clipped instead of wrapping");
        for line in &narrow {
            let w: usize = line.iter().map(|s| s.text.chars().count()).sum();
            assert!(w <= 20, "`{line:?}` is {w} wide in a 20-column body");
        }
    }

    /// The contents column answers the pointer on its rows and nowhere else —
    /// including the row the heading is on, which is not a topic.
    #[test]
    fn the_contents_answer_the_pointer_on_their_own_rows() {
        let list = columns(geom().stage_box).list;
        assert_eq!(topic_at(list, list.y), None, "the HELP heading resolved to a topic");
        assert_eq!(topic_at(list, list.y + 1), None, "the blank under it did too");
        for (i, _) in TOPICS.iter().enumerate() {
            assert_eq!(topic_at(list, list.y + 2 + i as u16), Some(i), "row {i}");
        }
        assert_eq!(topic_at(list, list.y + 2 + TOPICS.len() as u16), None, "past the last topic");
    }
}
