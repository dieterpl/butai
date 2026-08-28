//! The editor's minimap: the whole file as a column of texture beside it.
//!
//! A file is read through a window a few dozen rows tall, and nothing on screen
//! said how big the thing behind that window was or where in it you were. The
//! scrollbar answer is "somewhere between the top and the bottom"; this answers
//! with the shape of the code — where the blank lines are, where a block comment
//! sits, where the deeply indented middle of a function is — so a jump is aimed
//! at something you recognise rather than at a fraction.
//!
//! ## Why a texture and not text
//!
//! Sixteen cells cannot hold a line of code, so this does not try. Each cell
//! stands for a rectangle of the file — some lines tall, some columns wide — and
//! is drawn as one shaded block whose density is how much ink is in that
//! rectangle and whose colour is what that ink mostly *was*: a comment block is
//! a muted slab, a run of strings is green, an indent is the blank left edge.
//! At that size indentation is the signal, which is exactly what makes a file
//! recognisable from across the room.
//!
//! ## Why the texture is cached on the buffer
//!
//! Painting walks only the rows on screen; a minimap by definition walks the
//! whole file. Doing that from [`Token`] runs — which are `String`s — would mean
//! re-reading every character of the file on every frame. So [`Editor`] holds a
//! byte per column per line ([`Editor::texture`]), rebuilt on the same edit that
//! rebuilds the highlighting, and this module reads bytes. It also samples: a
//! row standing for two hundred lines takes [`SAMPLES`] of them, because the
//! other hundred and ninety-six cannot change a five-level shade.

use super::{put_str, Editor, Pen, Theme};
use crate::layout::Rect as LRect;
use crate::syntax::Token;
use ratatui::buffer::Buffer;

/// Cells the minimap takes down the right of a file being read.
///
/// Sixteen because the thing it has to show is indentation, and indentation is
/// read from the *left edge* of each row: eight cells put four levels of nesting
/// into the same column and the shape goes flat, while thirty-two is a column of
/// texture as wide as a rail for a picture with no words in it.
pub const MINIMAP_W: u16 = 16;

/// Blank cells between the text and the texture.
///
/// The gap is load-bearing rather than decorative: a line long enough to fill
/// the file column runs right up to the minimap's edge, and a wall of code
/// against a wall of shaded blocks reads as one thing with a seam in it. One
/// cell is enough to make them two columns.
const GUTTER: u16 = 1;

/// Cells of code the minimap has to leave behind, or it is not drawn at all.
///
/// The same bargain [`super::DIFF_TEXT_MIN_W`] strikes for the diff's line
/// numbers: this is an orientation aid *over* the text, so a minimap that leaves
/// forty cells of code has taken more than it gave. On a narrow terminal — or
/// with the Finder columns walked several deep, which is the case that made this
/// necessary — the file column loses the minimap and keeps the file.
pub const MINIMAP_MIN_TEXT_W: u16 = 48;

/// Document columns the minimap's width stands for.
///
/// Lines longer than this are clipped rather than squeezed, so the scale of the
/// left edge is the same on every row — which is the whole point, since it is
/// the left edge that carries the indentation.
const SPAN: usize = 96;

/// Lines sampled per minimap row, at most.
///
/// A row that stands for two hundred lines is drawn in five shades; reading all
/// two hundred to choose between them is work nobody can see. Evenly spread
/// across the row's range and always including its first line.
const SAMPLES: usize = 4;

/// One byte of [`Editor::texture`]: blank, or the token that was there.
///
/// Zero is "no ink" rather than a token, so the common case — the indentation
/// and the ragged right edge, which is most of a file — is a byte the shading
/// loop can skip without a match.
pub const BLANK: u8 = 0;

/// Encode one character's token for the texture.
pub fn ink(token: Token) -> u8 {
    1 + match token {
        Token::Plain => 0,
        Token::Comment => 1,
        Token::Str => 2,
        Token::Number => 3,
        Token::Keyword => 4,
        Token::Type => 5,
    }
}

/// The texture for one file: a byte per column, per line, capped at [`SPAN`].
///
/// Built from the same highlighted runs the body draws from, so the minimap
/// cannot disagree with the code beside it about what is a comment. Tabs expand
/// to four, as they do in the body — a tab that advanced one column here would
/// put every indent on screen at a different depth from the one it draws.
pub fn texture(highlighted: &[Vec<(Token, String)>]) -> Vec<Vec<u8>> {
    highlighted
        .iter()
        .map(|runs| {
            let mut row: Vec<u8> = Vec::new();
            for (token, text) in runs {
                let mark = ink(*token);
                for ch in text.chars() {
                    if row.len() >= SPAN {
                        break;
                    }
                    match ch {
                        '\t' => row.resize((row.len() + 4).min(SPAN), BLANK),
                        c if c.is_whitespace() => row.push(BLANK),
                        _ => row.push(mark),
                    }
                }
            }
            // The ragged right of a line is blank by absence, so the trailing
            // run of nothing is not worth a byte each.
            while row.last() == Some(&BLANK) {
                row.pop();
            }
            row
        })
        .collect()
}

/// Width the minimap gets beside `body_w` cells of file column: all of it or
/// none.
///
/// Never a squeezed version of itself. Below the floor the scale would stop
/// meaning anything — four cells cannot separate an indent from a body — and a
/// minimap you cannot read is [`MINIMAP_W`] cells of code you no longer have.
pub fn width(body_w: u16) -> u16 {
    if body_w >= MINIMAP_W + MINIMAP_MIN_TEXT_W {
        MINIMAP_W
    } else {
        0
    }
}

/// The denominator both mappings scale by.
///
/// `lines` once the file is taller than the minimap, and `rows` while it is
/// shorter — which is what keeps a short file drawn at 1:1 down the top of the
/// column instead of stretched to fill it. A stretched thirty-line file would
/// claim the same height as a three-thousand-line one, which is the single most
/// misleading thing a minimap can do.
fn scale(lines: usize, rows: u16) -> usize {
    lines.max(rows as usize).max(1)
}

/// The first document line a minimap row stands for.
pub fn line_at(row: u16, rows: u16, lines: usize) -> usize {
    if rows == 0 {
        return 0;
    }
    row as usize * scale(lines, rows) / rows as usize
}

/// The minimap row a document line lands on.
///
/// The inverse of [`line_at`]'s partition rather than its arithmetic: the last
/// row whose range starts at or before `line`. Written as the obvious
/// `line * rows / n` it is off by a row wherever the two floors disagree, and a
/// viewport marker one row above the shape it is marking is worse than none.
pub fn row_of(line: usize, rows: u16, lines: usize) -> u16 {
    if rows == 0 {
        return 0;
    }
    let n = scale(lines, rows);
    (((line + 1) * rows as usize - 1) / n).min(rows as usize - 1) as u16
}

/// Where a click at minimap row `row` should leave the file: that point in the
/// middle of the window rather than at the top of it.
///
/// Centred because the click was aimed at a shape, and a shape put on the top
/// row is a shape with its context cut off — you jumped to it to read what is
/// around it.
pub fn scroll_to(row: u16, rows: u16, lines: usize, view_h: u16) -> usize {
    let line = line_at(row, rows, lines);
    line.saturating_sub(view_h as usize / 2).min(lines.saturating_sub(1))
}

/// The shade for `ink` inked cells out of `total` looked at.
fn shade(inked: usize, total: usize) -> char {
    if inked == 0 || total == 0 {
        return ' ';
    }
    match inked * 4 / total {
        0 => '░',
        1 => '▒',
        2 => '▓',
        _ => '█',
    }
}

/// Which document lines this row samples: evenly spread, first one always in.
fn samples(row: u16, rows: u16, lines: usize) -> impl Iterator<Item = usize> {
    let from = line_at(row, rows, lines);
    let to = line_at(row + 1, rows, lines).max(from + 1).min(lines);
    let span = to.saturating_sub(from);
    let step = span.div_ceil(SAMPLES).max(1);
    (from..to).step_by(step)
}

/// Draw the minimap into `area`, with the window `view_h` rows tall marked on
/// it.
///
/// `top` is the first line the file column is showing — `Editor::scroll` while
/// reading, and where the cursor is while editing, since there the widget owns
/// the scrolling and the cursor is the only anchor this side can see.
pub fn draw(buf: &mut Buffer, area: LRect, open: &Editor, view_h: u16, top: usize, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let lines = open.texture.len();
    let rows = area.height;
    let cells = area.width.saturating_sub(GUTTER) as usize;
    let from = area.x + GUTTER;
    if cells == 0 {
        return;
    }
    // Which rows the window covers, so the reader can see where they are as
    // well as what is around them. At least one row: on a file long enough that
    // the whole window rounds to nothing, "you are here" is the only thing this
    // marker is for.
    let first = row_of(top, rows, lines);
    let last = row_of(top + view_h as usize, rows, lines).max(first + 1);

    for r in 0..rows {
        let y = area.y + r;
        let inside = r >= first && r < last;
        let bg = theme.row_bg(inside);
        for x in area.x..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_bg(bg);
            }
        }
        if r as usize >= lines && lines >= rows as usize {
            continue;
        }
        // Tally each cell's rectangle of the file in one pass over the sampled
        // lines: how much ink it holds, and what that ink mostly was.
        let mut inked = vec![0usize; cells];
        let mut looked = vec![0usize; cells];
        let mut tally = vec![[0usize; 6]; cells];
        for line in samples(r, rows, lines) {
            let Some(row) = open.texture.get(line) else { continue };
            for c in 0..cells {
                let from = c * SPAN / cells;
                let to = (c + 1) * SPAN / cells;
                looked[c] += to - from;
                for &b in row.get(from..to.min(row.len())).unwrap_or(&[]) {
                    if b != BLANK {
                        inked[c] += 1;
                        tally[c][(b - 1) as usize] += 1;
                    }
                }
            }
        }
        for c in 0..cells {
            let glyph = shade(inked[c], looked[c]);
            if glyph == ' ' {
                continue;
            }
            let (which, _) = tally[c]
                .iter()
                .enumerate()
                .max_by_key(|(_, n)| **n)
                .expect("the tally is a fixed six wide");
            let fg = match which {
                1 => theme.muted,
                2 => theme.ok,
                3 => theme.accent,
                4 => theme.attention,
                5 => theme.info,
                _ => theme.ink,
            };
            put_str(buf, from + c as u16, y, &glyph.to_string(), area.right(), Pen::new(fg, bg));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(text: &str) -> Vec<Vec<(Token, String)>> {
        text.lines().map(|l| vec![(Token::Plain, l.to_string())]).collect()
    }

    /// The two mappings are each other's inverse at row boundaries, which is
    /// what lets a click land on the shape that was clicked. Checked across
    /// both regimes — file taller than the column, and shorter.
    #[test]
    fn row_and_line_agree() {
        for lines in [1usize, 7, 40, 41, 1000, 9999] {
            for rows in [1u16, 3, 40] {
                for r in 0..rows {
                    let line = line_at(r, rows, lines);
                    assert_eq!(
                        row_of(line, rows, lines),
                        r,
                        "row {r} of {rows} over {lines} lines did not come back"
                    );
                }
            }
        }
    }

    /// A file shorter than the column is drawn at 1:1 rather than stretched, so
    /// a short file *looks* short.
    #[test]
    fn short_files_are_not_stretched() {
        assert_eq!(line_at(5, 40, 10), 5, "ten lines over forty rows is one line a row");
        assert_eq!(row_of(9, 40, 10), 9);
        // And a long one is scaled.
        assert_eq!(line_at(1, 40, 400), 10, "four hundred lines over forty rows is ten a row");
    }

    /// The minimap is all of itself or none of it — never a squeezed version.
    #[test]
    fn width_is_all_or_nothing() {
        assert_eq!(width(MINIMAP_W + MINIMAP_MIN_TEXT_W), MINIMAP_W);
        assert_eq!(width(MINIMAP_W + MINIMAP_MIN_TEXT_W - 1), 0, "below the floor it is dropped");
        assert_eq!(width(0), 0);
    }

    /// Tabs expand to four, as the body draws them: an indent that measured one
    /// column here would be drawn at a depth the code beside it never had.
    #[test]
    fn tabs_are_four_columns() {
        let tex = texture(&lines_of("\t\tx"));
        assert_eq!(tex[0].len(), 9, "two tabs, then the character");
        assert_eq!(&tex[0][..8], &[BLANK; 8]);
        assert_eq!(tex[0][8], ink(Token::Plain));
    }

    /// Trailing blanks cost no bytes — most of a file is the ragged right edge.
    #[test]
    fn the_right_edge_is_absence() {
        let tex = texture(&lines_of("ab   \n\n"));
        assert_eq!(tex[0].len(), 2, "the trailing run is dropped");
        assert!(tex[1].is_empty(), "a blank line is no bytes at all");
    }

    /// Long lines are clipped, not squeezed, so the left edge keeps its scale.
    #[test]
    fn long_lines_are_clipped() {
        let tex = texture(&lines_of(&"x".repeat(SPAN * 3)));
        assert_eq!(tex[0].len(), SPAN);
    }

    /// Denser rectangles get heavier glyphs, and an empty one gets nothing.
    #[test]
    fn shades_climb_with_ink() {
        assert_eq!(shade(0, 8), ' ');
        assert_eq!(shade(1, 8), '░');
        assert_eq!(shade(3, 8), '▒');
        assert_eq!(shade(5, 8), '▓');
        assert_eq!(shade(8, 8), '█');
    }

    /// Every row samples at least one line, and never more than [`SAMPLES`] —
    /// the bound that keeps a paint off the whole file.
    #[test]
    fn every_row_samples_between_one_and_four_lines() {
        for lines in [1usize, 40, 137, 20_000] {
            for rows in [1u16, 12, 45] {
                for r in 0..rows {
                    let n = samples(r, rows, lines).count();
                    assert!(
                        (1..=SAMPLES).contains(&n) || line_at(r, rows, lines) >= lines,
                        "row {r} of {rows} over {lines} lines sampled {n}"
                    );
                }
            }
        }
    }

    /// A click puts what was clicked in the middle of the window, not on its
    /// top row.
    #[test]
    fn a_click_centres_what_it_hit() {
        // Row 20 of 40 over 400 lines is line 200; a 30-row window centres it.
        assert_eq!(scroll_to(20, 40, 400, 30), 185);
        // Near the top there is nothing to centre into, and it clamps.
        assert_eq!(scroll_to(0, 40, 400, 30), 0);
    }
}
