//! A pane's cell grid to protocol `CellRun`s: `ratatui::Buffer` ->
//! `Buffer::diff` -> `FrameUpdate`.
//!
//! All that is left of a file that used to compose a whole workbench. The
//! daemon draws exactly one thing now — a program's screen, reconstructed from
//! the bytes it wrote to a PTY — and every other surface crosses the wire as
//! JSON for a client to draw. There is no theme here for the same reason:
//! a terminal's cells carry the program's own colours, and the palette around
//! them belongs to whoever is painting the frame.

use butai_protocol::{Cell as PCell, CellRun, Color as PColor, FrameUpdate, Mods};
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr;

// ---------------------------------------------------------------------------
// Buffer -> protocol conversion
// ---------------------------------------------------------------------------

pub fn to_proto_color(c: Color) -> PColor {
    match c {
        Color::Reset => PColor::Default,
        Color::Black => PColor::Indexed(0),
        Color::Red => PColor::Indexed(1),
        Color::Green => PColor::Indexed(2),
        Color::Yellow => PColor::Indexed(3),
        Color::Blue => PColor::Indexed(4),
        Color::Magenta => PColor::Indexed(5),
        Color::Cyan => PColor::Indexed(6),
        Color::Gray => PColor::Indexed(7),
        Color::DarkGray => PColor::Indexed(8),
        Color::LightRed => PColor::Indexed(9),
        Color::LightGreen => PColor::Indexed(10),
        Color::LightYellow => PColor::Indexed(11),
        Color::LightBlue => PColor::Indexed(12),
        Color::LightMagenta => PColor::Indexed(13),
        Color::LightCyan => PColor::Indexed(14),
        Color::White => PColor::Indexed(15),
        Color::Indexed(i) => PColor::Indexed(i),
        Color::Rgb(r, g, b) => PColor::Rgb(r, g, b),
    }
}

fn to_proto_cell(cell: &ratatui::buffer::Cell) -> PCell {
    let m = cell.modifier;
    PCell {
        ch: cell.symbol().to_string(),
        fg: to_proto_color(cell.fg),
        bg: to_proto_color(cell.bg),
        mods: Mods {
            bold: m.contains(Modifier::BOLD),
            dim: m.contains(Modifier::DIM),
            italic: m.contains(Modifier::ITALIC),
            underline: m.contains(Modifier::UNDERLINED),
            reverse: m.contains(Modifier::REVERSED),
            crossed_out: m.contains(Modifier::CROSSED_OUT),
        },
    }
}

/// Diff `next` against what the client last saw and produce a frame update.
/// `prev = None` (or a size change) forces a full repaint.
pub fn diff_to_frame(
    prev: Option<&Buffer>,
    next: &Buffer,
    cursor: Option<(u16, u16)>,
) -> FrameUpdate {
    let full = match prev {
        Some(p) => p.area != next.area,
        None => true,
    };
    let mut runs: Vec<CellRun> = Vec::new();
    let mut push_cell =
        |x: u16, y: u16, cell: &ratatui::buffer::Cell, expected: &mut (u16, u16)| {
            let (ex, ey) = *expected;
            let pc = to_proto_cell(cell);
            if y == ey && x == ex {
                if let Some(run) = runs.last_mut() {
                    run.cells.push(pc);
                    *expected = (x + cell.symbol().width().max(1) as u16, y);
                    return;
                }
            }
            *expected = (x + cell.symbol().width().max(1) as u16, y);
            runs.push(CellRun { x, y, cells: vec![pc] });
        };

    let mut expected = (u16::MAX, u16::MAX);
    if full {
        let area = next.area;
        for y in area.y..area.bottom() {
            let mut x = area.x;
            while x < area.right() {
                let cell = &next[(x, y)];
                let w = cell.symbol().width().max(1) as u16;
                push_cell(x, y, cell, &mut expected);
                x += w;
            }
        }
    } else if let Some(p) = prev {
        for (x, y, cell) in p.diff(next) {
            push_cell(x, y, cell, &mut expected);
        }
    }
    FrameUpdate {
        full,
        cells: runs,
        cursor,
        cursor_shape: Default::default(),
        // Set by the caller that knows which pane this is: `diff_to_frame` sees
        // a cell grid, not a program.
        wants_mouse: false,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::*;

    // `elapsed_is_seconds_then_mmss` moved with the function it tested, to
    // `butai_core::chrome`'s `elapsed_switches_from_seconds_to_minutes_at_a_minute`.

    /// A grid with some text written into it, for diffing against another.
    fn buf_with(area: Rect, texts: &[(u16, u16, &str)]) -> Buffer {
        let mut b = Buffer::empty(area);
        for (x, y, s) in texts {
            for (i, ch) in s.chars().enumerate() {
                if let Some(cell) = b.cell_mut((x + i as u16, *y)) {
                    cell.set_char(ch);
                }
            }
        }
        b
    }

    #[test]
    fn full_frame_covers_area() {
        let area = Rect::new(0, 0, 4, 2);
        let next = buf_with(area, &[(0, 0, "ab")]);
        let fu = diff_to_frame(None, &next, Some((1, 0)));
        assert!(fu.full);
        assert_eq!(fu.cells.len(), 2);
        assert_eq!(fu.cells[0].cells.len(), 4);
        assert_eq!(fu.cells[0].cells[0].ch, "a");
        assert_eq!(fu.cursor, Some((1, 0)));
    }

    #[test]
    fn diff_produces_minimal_runs() {
        let area = Rect::new(0, 0, 10, 2);
        let prev = buf_with(area, &[(0, 0, "hello")]);
        let next = buf_with(area, &[(0, 0, "hellO"), (0, 1, "x")]);
        let fu = diff_to_frame(Some(&prev), &next, None);
        assert!(!fu.full);
        assert_eq!(fu.cells.len(), 2);
        assert_eq!(fu.cells[0].x, 4);
        assert_eq!(fu.cells[0].cells[0].ch, "O");
        assert_eq!(fu.cells[1].y, 1);
    }

    #[test]
    fn size_change_forces_full() {
        let prev = Buffer::empty(Rect::new(0, 0, 4, 2));
        let next = Buffer::empty(Rect::new(0, 0, 5, 2));
        assert!(diff_to_frame(Some(&prev), &next, None).full);
    }

    /// Wire colours, which are a contract: a client reads `default` as "your
    /// own", an index as one of the terminal's sixteen (or 256), and RGB as an
    /// exact value it must not substitute.
    ///
    /// These used to be asserted through the daemon's `Theme`, which is gone —
    /// palettes are the client's now. The *mapping* is not: it is what
    /// `to_proto_cell` puts on the wire for every pane the daemon streams.
    #[test]
    fn wire_colours_keep_their_meanings() {
        assert_eq!(to_proto_color(Color::Reset), PColor::Default);
        // The sixteen named colours keep the indices the pre-theme chrome
        // emitted, so a client written against either reads the same screen.
        for (color, want) in [
            (Color::Black, 0),
            (Color::Red, 1),
            (Color::Green, 2),
            (Color::Yellow, 3),
            (Color::Blue, 4),
            (Color::Magenta, 5),
            (Color::Cyan, 6),
            (Color::Gray, 7),
            (Color::DarkGray, 8),
            (Color::White, 15),
        ] {
            assert_eq!(to_proto_color(color), PColor::Indexed(want), "{color:?}");
        }
        assert_eq!(to_proto_color(Color::Indexed(238)), PColor::Indexed(238));
        // An exact colour stays exact: a client must not round it to a palette
        // entry, which is the whole reason a pinned theme is worth pinning.
        assert_eq!(to_proto_color(Color::Rgb(1, 2, 3)), PColor::Rgb(1, 2, 3));
    }
}
