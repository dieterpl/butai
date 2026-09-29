//! Single-cell alternatives for terminal fonts with limited symbol coverage.
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Glyphs {
    Unicode,
    Ascii,
}

impl Default for Glyphs {
    fn default() -> Self {
        if cfg!(windows) {
            Self::Ascii
        } else {
            Self::Unicode
        }
    }
}

impl Glyphs {
    /// Substitute only single-cell graphical symbols. Text, combining marks,
    /// CJK and emoji retain their original encoding and occupied columns.
    pub(crate) fn display(self, symbol: &str) -> &str {
        if self == Self::Unicode {
            return symbol;
        }
        let mut chars = symbol.chars();
        let Some(ch) = chars.next() else { return symbol };
        if chars.next().is_some() {
            return symbol;
        }
        match ch {
            '─' | '━' | '╌' | '╍' | '┄' | '┅' | '┈' | '┉' | '═' | '—' | '–' => {
                "-"
            }
            '│' | '┃' | '╎' | '╏' | '┆' | '┇' | '┊' | '┋' | '║' => "|",
            '\u{250c}'..='\u{254b}' | '\u{2552}'..='\u{256c}' => "+",
            '╭' | '╮' | '╯' | '╰' => "+",
            '╱' => "/",
            '╲' => "\\",
            '╳' => "x",
            '←' | '◀' | '◂' | '‹' | '«' => "<",
            '→' | '▶' | '▸' | '›' | '»' => ">",
            '↑' | '▲' | '▴' => "^",
            '↓' | '▼' | '▾' => "v",
            '↔' => "-",
            '↕' => "|",
            '↵' | '↳' | '↪' => ">",
            '●' | '•' | '◆' | '▪' | '■' => "*",
            '○' | '◌' | '◦' | '◇' | '□' => "o",
            '·' | '⋅' | '…' => ".",
            '✓' | '✔' => "+",
            '✗' | '✘' | '×' => "x",
            '░' => ".",
            '▒' => ":",
            '▓' | '█' => "#",
            '\u{2580}'..='\u{258f}' | '▔' | '▕' => "#",
            '⠋' | '⠼' | '⠇' => "|",
            '⠙' | '⠴' | '⠏' => "/",
            '⠹' | '⠦' => "-",
            '⠸' | '⠧' => "\\",
            '\u{2800}'..='\u{28ff}' => match (ch as u32 - 0x2800).count_ones() {
                0 => " ",
                1..=2 => ".",
                3..=5 => ":",
                _ => "#",
            },
            _ => symbol,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn compatible_graphics_keep_the_same_cell_width() {
        for ch in "╭─┬╮│└═╯▸←↓●○✓✗░▒▓█▁▕⠋⣿⣀…".chars()
        {
            let text = ch.to_string();
            let shown = Glyphs::Ascii.display(&text);
            assert!(shown.is_ascii(), "{ch} has no fallback");
            assert_eq!(shown.width(), text.width(), "{ch} changed width");
        }
    }

    #[test]
    fn text_and_unicode_mode_are_lossless() {
        for text in ["é", "日本語", "e\u{301}", "🦀", "abc", ""] {
            assert_eq!(Glyphs::Ascii.display(text), text);
        }
        for text in ["─", "│", "⣀", "▸", "█"] {
            assert_eq!(Glyphs::Unicode.display(text), text);
        }
    }
}
