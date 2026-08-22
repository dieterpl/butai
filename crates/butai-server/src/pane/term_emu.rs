//! The VT-emulator abstraction. vt100 backs it today; the trait exists so
//! `alacritty_terminal` can slot in if vt100 correctness becomes a limit.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use crate::input::encode::EncodeModes;

/// How [`TermEmulator::text_rows`] renders each row. Kept here rather than
/// taking the protocol's `OutputFormat` so the emulator layer stays independent
/// of the wire types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowFormat {
    /// Plain characters, no escape sequences.
    Text,
    /// SGR-formatted, reproducing colors and attributes.
    Ansi,
}

pub trait TermEmulator: Send {
    fn feed(&mut self, bytes: &[u8]);
    fn resize(&mut self, rows: u16, cols: u16);
    /// Draw the current view (honoring any scrollback offset) into `buf`.
    fn render_into(&self, buf: &mut Buffer, area: Rect);
    /// Cursor position (col, row) relative to the pane, `None` when hidden
    /// or when scrolled back.
    fn cursor(&self) -> Option<(u16, u16)>;
    /// Raw cursor position (row, col), always reported — used to answer a
    /// cursor-position query (DSR) regardless of visibility or scrollback.
    fn cursor_pos(&self) -> (u16, u16);
    fn title(&self) -> String;
    fn modes(&self) -> EncodeModes;
    /// Adjust the scrollback view offset by `delta` lines (positive = older).
    fn scroll_view(&mut self, delta: i32);
    fn scroll_offset(&self) -> usize;
    /// Reset the scrollback view to the live screen.
    fn scroll_reset(&mut self);
    /// Total bell rings seen (used for attention detection).
    fn bell_count(&self) -> usize;
    /// Whether the inner application has enabled mouse reporting.
    fn mouse_active(&self) -> bool;
    /// Plain-text rows, oldest first, ending at the bottom of the live screen.
    ///
    /// `want` is a maximum — fewer come back when the pane has not produced that
    /// many lines yet — and the `bool` is `true` when older lines were left
    /// behind. Rows are right-trimmed, and a wide grapheme occupies one entry
    /// rather than two: the filler-cell rule that [`Self::render_into`] makes
    /// every client reimplement (see `docs/protocol.md`) is applied here
    /// instead, which is the whole point of a text read.
    ///
    /// Takes `&mut self` because vt100 exposes scrollback only through the view
    /// offset. The offset is saved and restored before returning, so no attached
    /// client ever observes a scroll — safe only because the core is a
    /// single-owner actor that does not yield in between. A future async
    /// refactor has to keep that true.
    fn text_rows(
        &mut self,
        want: usize,
        scrollback: bool,
        format: RowFormat,
    ) -> (Vec<String>, bool);
    /// Whether the inner application has switched to the alternate screen (vim,
    /// htop). vt100 gives the alternate grid *no* scrollback, so a scrollback
    /// read of such a pane can only ever return the visible screen.
    fn alternate_screen(&self) -> bool;
}

/// The two things vt100 used to keep on the screen and now reports as it
/// parses: the window title and the audible bell.
///
/// Both are state rather than events to everything above here — a pane's title
/// is whatever the program last set, and attention detection wants the running
/// count of rings rather than the individual ones — so the callbacks do the
/// only thing they can and hold the latest of each.
#[derive(Default)]
struct Reported {
    /// The last OSC 0/2 string. Agents rewrite this continuously to say what
    /// they are doing, which is what an agent row shows.
    title: String,
    /// Rings seen since the pane opened. Monotonic: callers compare it against
    /// the count they saw last, so it must never reset.
    bells: usize,
}

impl vt100::Callbacks for Reported {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bells += 1;
    }

    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        // A title is whatever the program sent, is not required to be UTF-8,
        // and is only ever shown — so a lossy read is the right kind of wrong.
        self.title = String::from_utf8_lossy(title).into_owned();
    }
}

pub struct Vt100Emulator {
    parser: vt100::Parser<Reported>,
}

impl Vt100Emulator {
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        Self {
            parser: vt100::Parser::new_with_callbacks(rows, cols, scrollback, Reported::default()),
        }
    }

    /// The current view as one string per row, at whatever scrollback offset is
    /// set. vt100's own extraction, so a wide grapheme occupies one entry and
    /// trailing blanks are already gone from each row.
    fn window(&self, cols: u16, format: RowFormat) -> Vec<String> {
        match format {
            RowFormat::Text => self.parser.screen().rows(0, cols).collect(),
            // SGR escapes are ASCII, so the lossy conversion cannot alter
            // anything vt100 emits here.
            RowFormat::Ansi => self
                .parser
                .screen()
                .rows_formatted(0, cols)
                .map(|r| String::from_utf8_lossy(&r).into_owned())
                .collect(),
        }
    }
}

fn conv_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

impl TermEmulator for Vt100Emulator {
    fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
    }

    fn render_into(&self, buf: &mut Buffer, area: Rect) {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        for row in 0..rows.min(area.height) {
            let mut col = 0;
            while col < cols.min(area.width) {
                let Some(cell) = screen.cell(row, col) else {
                    col += 1;
                    continue;
                };
                let x = area.x + col;
                let y = area.y + row;
                let Some(out) = buf.cell_mut((x, y)) else {
                    col += 1;
                    continue;
                };
                let contents = cell.contents();
                if contents.is_empty() {
                    out.set_symbol(" ");
                } else {
                    out.set_symbol(contents);
                }
                out.set_fg(conv_color(cell.fgcolor()));
                out.set_bg(conv_color(cell.bgcolor()));
                let mut m = Modifier::empty();
                if cell.bold() {
                    m |= Modifier::BOLD;
                }
                if cell.italic() {
                    m |= Modifier::ITALIC;
                }
                if cell.underline() {
                    m |= Modifier::UNDERLINED;
                }
                if cell.inverse() {
                    m |= Modifier::REVERSED;
                }
                out.modifier = m;
                col += if cell.is_wide() { 2 } else { 1 };
            }
        }
    }

    fn cursor(&self) -> Option<(u16, u16)> {
        let screen = self.parser.screen();
        if screen.hide_cursor() || screen.scrollback() > 0 {
            return None;
        }
        let (row, col) = screen.cursor_position();
        Some((col, row))
    }

    fn cursor_pos(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    fn title(&self) -> String {
        self.parser.callbacks().title.clone()
    }

    fn modes(&self) -> EncodeModes {
        let screen = self.parser.screen();
        EncodeModes {
            application_cursor_keys: screen.application_cursor(),
            bracketed_paste: screen.bracketed_paste(),
        }
    }

    fn scroll_view(&mut self, delta: i32) {
        let cur = self.parser.screen().scrollback() as i64;
        let next = (cur + delta as i64).max(0) as usize;
        self.parser.screen_mut().set_scrollback(next);
    }

    fn scroll_offset(&self) -> usize {
        self.parser.screen().scrollback()
    }

    fn scroll_reset(&mut self) {
        if self.parser.screen().scrollback() > 0 {
            self.parser.screen_mut().set_scrollback(0);
        }
    }

    fn bell_count(&self) -> usize {
        self.parser.callbacks().bells
    }

    fn mouse_active(&self) -> bool {
        self.parser.screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None
    }

    fn text_rows(
        &mut self,
        want: usize,
        scrollback: bool,
        format: RowFormat,
    ) -> (Vec<String>, bool) {
        let saved = self.parser.screen().scrollback();
        let (rows, cols) = self.parser.screen().size();
        let rows = usize::from(rows);

        // `set_scrollback` clamps to the real depth, so asking for more than
        // could exist both discovers the depth and parks the view at the top.
        self.parser.screen_mut().set_scrollback(usize::MAX);
        let depth = self.parser.screen().scrollback();
        // The window at any offset is one screen tall, so one screen back is as
        // far as a single read reaches, however deep `[general] scrollback`
        // runs; `more` says so rather than pretending the rest is not there.
        // Reaching further means walking the scrollback a screen at a time,
        // which is worth doing when something asks for it and nothing does yet.
        let reach = if scrollback { depth.min(rows) } else { 0 };

        let mut out: Vec<String> = Vec::with_capacity(reach + rows);
        if reach > 0 {
            // At this offset the window is the last `reach` scrollback rows
            // followed by the live screen's first `rows - reach`; only the
            // scrollback half is not already in the live window below.
            self.parser.screen_mut().set_scrollback(reach);
            out.extend(self.window(cols, format).into_iter().take(reach));
        }
        self.parser.screen_mut().set_scrollback(0);
        out.extend(self.window(cols, format));
        self.parser.screen_mut().set_scrollback(saved);

        if !scrollback {
            // The viewport verbatim, padding included: `screen` and `footer`
            // are about what the pane *looks like*, and a blank row is part of
            // that. Take from the bottom, so a short read is the newest rows.
            let drop = out.len().saturating_sub(want);
            return (out.split_off(drop), depth > 0);
        }

        // A grid is almost always taller than what has been written into it,
        // and those trailing blank rows are padding rather than output. Cutting
        // them here — before counting — is what makes `--lines 5` on a quiet
        // pane return its last five *lines* instead of five blank rows.
        while out.last().is_some_and(|l| l.trim().is_empty()) {
            out.pop();
        }
        let dropped = out.len().saturating_sub(want);
        out.drain(..dropped);
        (out, dropped > 0 || depth > reach)
    }

    fn alternate_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_to_string(emu: &dyn TermEmulator, cols: u16, rows: u16) -> String {
        let area = Rect::new(0, 0, cols, rows);
        let mut buf = Buffer::empty(area);
        emu.render_into(&mut buf, area);
        let mut out = String::new();
        for y in 0..rows {
            for x in 0..cols {
                out.push_str(buf.cell((x, y)).unwrap().symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn renders_plain_text() {
        let mut emu = Vt100Emulator::new(3, 10, 100);
        emu.feed(b"hi there");
        let s = render_to_string(&emu, 10, 3);
        assert!(s.starts_with("hi there"));
        assert_eq!(emu.cursor(), Some((8, 0)));
    }

    /// vt100 no longer keeps the title on the screen — it arrives as a callback
    /// during parsing and [`Reported`] holds the last one. Worth a test of its
    /// own because nothing else here would notice it going missing: an agent
    /// rewrites its title continuously to say what it is doing, and that string
    /// is what an AGENTS row shows, so a silently empty title would read as an
    /// agent that had stopped saying anything.
    #[test]
    fn the_osc_title_is_whatever_was_set_last() {
        let mut emu = Vt100Emulator::new(3, 20, 100);
        assert_eq!(emu.title(), "", "a pane that has set no title has none");

        emu.feed(b"\x1b]2;building\x07");
        assert_eq!(emu.title(), "building");

        // OSC 0 sets the icon name *and* the title; agents use either form.
        emu.feed(b"\x1b]0;running tests\x07");
        assert_eq!(emu.title(), "running tests");

        // OSC 1 is the icon name on its own, and must leave the title alone.
        emu.feed(b"\x1b]1;icon\x07");
        assert_eq!(emu.title(), "running tests", "an icon name overwrote the title");
    }

    #[test]
    fn handles_colors_and_cursor_movement() {
        let mut emu = Vt100Emulator::new(4, 20, 100);
        emu.feed(b"\x1b[2;3H\x1b[31mred\x1b[0m");
        let area = Rect::new(0, 0, 20, 4);
        let mut buf = Buffer::empty(area);
        emu.render_into(&mut buf, area);
        let cell = buf.cell((2u16, 1u16)).unwrap();
        assert_eq!(cell.symbol(), "r");
        assert_eq!(cell.fg, Color::Indexed(1));
        assert_eq!(emu.cursor(), Some((5, 1)));
    }

    #[test]
    fn scrollback_view_hides_cursor() {
        let mut emu = Vt100Emulator::new(2, 10, 100);
        for i in 0..10 {
            emu.feed(format!("line{i}\r\n").as_bytes());
        }
        assert!(emu.cursor().is_some());
        emu.scroll_view(3);
        assert_eq!(emu.scroll_offset(), 3);
        assert!(emu.cursor().is_none());
        emu.scroll_reset();
        assert_eq!(emu.scroll_offset(), 0);
    }

    fn feed_lines(emu: &mut Vt100Emulator, n: usize) {
        for i in 0..n {
            emu.feed(format!("line{i}\r\n").as_bytes());
        }
    }

    #[test]
    fn text_rows_reads_the_bottom_of_the_live_screen() {
        let mut emu = Vt100Emulator::new(3, 20, 100);
        emu.feed(b"alpha\r\nbeta\r\ngamma");
        let (lines, more) = emu.text_rows(3, false, RowFormat::Text);
        assert_eq!(lines, vec!["alpha", "beta", "gamma"]);
        assert!(!more);
        // A quiet pane is mostly blank grid; a scrollback read must count back
        // from the last line written, not from the bottom of that grid.
        let mut roomy = Vt100Emulator::new(20, 20, 100);
        roomy.feed(b"one\r\ntwo\r\nthree");
        let (lines, _) = roomy.text_rows(2, true, RowFormat::Text);
        assert_eq!(lines, vec!["two", "three"], "padding must not count as lines");
        // A short read is the tail, not the head — it ends at the live screen.
        let (tail, _) = emu.text_rows(1, false, RowFormat::Text);
        assert_eq!(tail, vec!["gamma"]);
    }

    #[test]
    fn text_rows_reaches_past_one_screen_into_scrollback() {
        let mut emu = Vt100Emulator::new(2, 20, 100);
        feed_lines(&mut emu, 10);

        let (all, more) = emu.text_rows(1000, true, RowFormat::Text);
        assert!(all.len() > 2, "a scrollback read must pass the live screen");
        // Ten lines went in and vt100 0.15 can only reach one screen back, so
        // the read is short of the full history and has to say so.
        assert!(more, "unreachable older lines must be reported");

        // The read ends at the newest line the pane actually wrote — not at
        // the bottom of the grid, which is blank padding below it.
        assert_eq!(all.last().map(String::as_str), Some("line9"));
        // A shorter scrollback read is that same tail, not the head.
        let (tail, more) = emu.text_rows(3, true, RowFormat::Text);
        assert!(more, "older lines were left behind");
        assert_eq!(&all[all.len() - 3..], &tail[..]);

        // Contiguous and in order, oldest first.
        let nums: Vec<usize> =
            all.iter().filter_map(|l| l.strip_prefix("line")?.parse().ok()).collect();
        assert!(nums.windows(2).all(|w| w[1] == w[0] + 1), "got {all:?}");
    }

    #[test]
    fn text_rows_keeps_one_entry_per_wide_grapheme() {
        let mut emu = Vt100Emulator::new(2, 10, 10);
        emu.feed("日本語".as_bytes());
        let (lines, _) = emu.text_rows(2, false, RowFormat::Text);
        // Not "日 本 語" with filler cells, and not six columns of nothing.
        assert_eq!(lines[0], "日本語");
    }

    #[test]
    fn text_rows_restores_the_scroll_offset() {
        let mut emu = Vt100Emulator::new(2, 10, 100);
        feed_lines(&mut emu, 10);
        emu.scroll_view(3);
        let before = emu.scroll_offset();
        let _ = emu.text_rows(1000, true, RowFormat::Text);
        assert_eq!(emu.scroll_offset(), before, "a read must not move the view");
    }

    #[test]
    fn the_alternate_screen_has_no_scrollback() {
        let mut emu = Vt100Emulator::new(2, 10, 100);
        feed_lines(&mut emu, 10);
        assert!(!emu.alternate_screen());
        emu.feed(b"\x1b[?1049hALT");
        assert!(emu.alternate_screen());
        // vt100 gives the alternate grid no scrollback at all, so however much
        // history the normal screen had, a read here sees only what is on it.
        let (lines, more) = emu.text_rows(1000, true, RowFormat::Text);
        assert_eq!(lines, vec!["ALT"], "the alternate grid is only ever one screen");
        assert!(!more);
    }

    #[test]
    fn tracks_modes() {
        let mut emu = Vt100Emulator::new(2, 10, 0);
        assert!(!emu.modes().application_cursor_keys);
        emu.feed(b"\x1b[?1h\x1b[?2004h");
        assert!(emu.modes().application_cursor_keys);
        assert!(emu.modes().bracketed_paste);
    }
}
