//! A window that says something, may ask for a line of text, and can say why
//! the answer did not work.
//!
//! The other modals each answer one fixed question: [`super::ConfirmOverlay`] is
//! a yes/no about a file, [`super::PromptOverlay`] is a commit message. A dialog
//! is the shape they are special cases of, for the questions that arrive from
//! *outside* the workbench and cannot be known in advance — ssh asking whether
//! to trust a host key, asking for a passphrase, or reporting that the key it
//! was shown is not the one it remembered.
//!
//! Everything it can show is data on [`DialogOverlay`], so a caller builds one
//! and a test can read it back. The rows and the box come from one layout
//! function that drawing and hit-testing both read, for the reason
//! `overlay_layout` gives: a click must land on what was painted.

use super::model::{ellipsize, Role};
use super::{draw_box, put_str, LRect, Pen, Theme};
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::buffer::Buffer;

/// Widest a dialog grows. A fingerprint is 50 columns and a sentence about it
/// reads badly stretched across a wide terminal.
const MAX_WIDTH: u16 = 66;
/// Columns between two buttons.
const BUTTON_GAP: u16 = 2;

/// How alarming the dialog is. Picks the border and the title mark, nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// A question with nothing wrong yet: a passphrase, a message.
    Info,
    /// A question where the wrong answer has a cost: trusting an unknown key.
    Warn,
    /// Something failed, or looks like an attack.
    Danger,
}

/// What a button does. The dialog reports which was chosen and the caller
/// decides what that means for the thing that asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Submit: the answer is the input's text, or just "yes" when there is none.
    Accept,
    /// A deliberate "no" — distinct from walking away, for the questions where
    /// the two differ.
    Decline,
    /// Dismiss without answering. What `esc` always means.
    Cancel,
    /// Anything else a particular dialog offers, named by the caller.
    Other(&'static str),
}

/// One button. `key` is what is drawn in front of the label (`enter`, `esc`,
/// `y`); `mnemonic` is the character that presses it when nothing is being
/// typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub key: &'static str,
    pub label: String,
    pub mnemonic: Option<char>,
    pub action: Action,
}

impl Button {
    pub fn new(
        key: &'static str,
        label: impl Into<String>,
        mnemonic: Option<char>,
        action: Action,
    ) -> Self {
        Self { key, label: label.into(), mnemonic, action }
    }

    fn text(&self) -> String {
        format!(" {} {} ", self.key, self.label)
    }
}

/// A labelled value in a block of its own: a fingerprint, a file, a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    pub label: String,
    pub value: String,
    pub role: Role,
}

/// A single line being typed into.
///
/// One line, with the same char-indexed caret as [`super::PromptOverlay`]; kept
/// separate because that type is welded to the workbench's own prompts
/// (`PromptKind`), and what is wanted here is the editing alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub label: String,
    pub text: String,
    /// Caret as a char index in `0..=text.chars().count()`.
    pub cursor: usize,
    /// Drawn as dots. A passphrase on a screen someone may be watching, or
    /// recording, is a passphrase that has been published.
    pub secret: bool,
}

impl Input {
    pub fn new(label: impl Into<String>, secret: bool) -> Self {
        Self { label: label.into(), text: String::new(), cursor: 0, secret }
    }

    fn byte_at(&self, i: usize) -> usize {
        self.text.char_indices().nth(i).map(|(b, _)| b).unwrap_or(self.text.len())
    }

    fn insert(&mut self, c: char) {
        let at = self.byte_at(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let (from, to) = (self.byte_at(self.cursor - 1), self.byte_at(self.cursor));
        self.text.replace_range(from..to, "");
        self.cursor -= 1;
    }

    fn delete(&mut self) {
        if self.cursor >= self.text.chars().count() {
            return;
        }
        let (from, to) = (self.byte_at(self.cursor), self.byte_at(self.cursor + 1));
        self.text.replace_range(from..to, "");
    }

    fn shift(&mut self, delta: isize) {
        let len = self.text.chars().count() as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, len) as usize;
    }
}

/// What a dialog is on behalf of, so the loop that gets the answer back knows
/// where to send it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    /// ssh asked a question through its askpass helper; the answer goes back to
    /// the request with this id.
    Ask(u64),
    /// ssh refused to connect because the host's key is not the saved one.
    /// Carries what removing the saved entry and trying again needs.
    HostKeyChanged { target: String, host: String, file: String },
    /// Nothing is waiting on it.
    Notice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogOverlay {
    pub purpose: Purpose,
    pub title: String,
    pub tone: Tone,
    /// The question or the news, one paragraph per entry; wrapped to fit.
    pub lead: Vec<String>,
    pub facts: Vec<Fact>,
    /// Smaller print under the facts: what answering will do.
    pub note: Vec<String>,
    pub input: Option<Input>,
    /// Why the last answer did not work. Drawn under the input in the danger
    /// colour; cleared by the next keystroke into the input.
    pub error: Option<String>,
    pub buttons: Vec<Button>,
    /// The highlighted button. What `enter` does.
    pub sel: usize,
}

impl DialogOverlay {
    /// A message with nothing to decide: one `ok`.
    pub fn notice(title: impl Into<String>, tone: Tone, lead: Vec<String>) -> Self {
        Self {
            purpose: Purpose::Notice,
            title: title.into(),
            tone,
            lead,
            facts: Vec::new(),
            note: Vec::new(),
            input: None,
            error: None,
            buttons: vec![Button::new("enter", "ok", None, Action::Cancel)],
            sel: 0,
        }
    }

    /// Handle one key. `Some` means a button was pressed and the dialog is done;
    /// `None` means it stays open (and may have changed).
    ///
    /// Pure, so the whole keyboard contract is testable without a terminal.
    pub fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Option<Action> {
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        let last = self.buttons.len().saturating_sub(1);
        match code {
            KeyCode::Esc => return Some(Action::Cancel),
            KeyCode::Enter => {
                return Some(
                    self.buttons.get(self.sel).map_or(Action::Cancel, |b| b.action.clone()),
                );
            }
            KeyCode::Tab => self.sel = if self.sel >= last { 0 } else { self.sel + 1 },
            KeyCode::BackTab => self.sel = if self.sel == 0 { last } else { self.sel - 1 },
            _ => {}
        }
        if let Some(input) = self.input.as_mut() {
            let edited = match code {
                KeyCode::Backspace => {
                    input.backspace();
                    true
                }
                KeyCode::Delete => {
                    input.delete();
                    true
                }
                KeyCode::Left => {
                    input.shift(-1);
                    false
                }
                KeyCode::Right => {
                    input.shift(1);
                    false
                }
                KeyCode::Home => {
                    input.cursor = 0;
                    false
                }
                KeyCode::End => {
                    input.cursor = input.text.chars().count();
                    false
                }
                KeyCode::Char(c) if !ctrl => {
                    input.insert(c);
                    true
                }
                _ => false,
            };
            if edited {
                self.error = None;
            }
            return None;
        }
        // Nothing is being typed, so letters are free to be shortcuts.
        match code {
            KeyCode::Right | KeyCode::Char('l') => {
                self.sel = if self.sel >= last { 0 } else { self.sel + 1 }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.sel = if self.sel == 0 { last } else { self.sel - 1 }
            }
            KeyCode::Char(c) if !ctrl => {
                return self
                    .buttons
                    .iter()
                    .find(|b| b.mnemonic == Some(c))
                    .map(|b| b.action.clone());
            }
            _ => {}
        }
        None
    }

    /// Text pasted into the dialog. Lands in the input if there is one, as a
    /// single line — a passphrase pasted out of a password manager brings its
    /// newline with it, and that must not become an `enter`.
    pub fn paste(&mut self, text: &str) {
        let Some(input) = self.input.as_mut() else { return };
        for c in text.trim_end_matches(['\r', '\n']).chars().filter(|c| !c.is_control()) {
            input.insert(c);
        }
        self.error = None;
    }

    /// The pointer pressed button `i`. The same answer as `enter` on it.
    pub fn press(&mut self, i: usize) -> Option<Action> {
        let action = self.buttons.get(i)?.action.clone();
        self.sel = i;
        Some(action)
    }
}

// ---- layout ------------------------------------------------------------------

enum Row {
    Blank,
    Rule,
    Lead(String),
    Note(String),
    Fact { label: String, value: String, role: Role, label_w: usize },
    Input,
    Error(String),
    Buttons,
}

/// A dialog resolved into rows and a box.
struct Laid {
    rect: LRect,
    rows: Vec<Row>,
    /// Row index of the buttons, and each button's `(start, end)` column offset
    /// from `rect.x`.
    button_row: usize,
    spans: Vec<(u16, u16)>,
}

/// Break `text` into lines of at most `width` chars at spaces; a word longer
/// than a line is cut where it overflows rather than lost.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            let mut word = word.to_string();
            while word.chars().count() > width {
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                }
                let head: String = word.chars().take(width).collect();
                word = word.chars().skip(width).collect();
                out.push(head);
            }
            let need = line.chars().count() + usize::from(!line.is_empty()) + word.chars().count();
            if need > width && !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&word);
        }
        out.push(line);
    }
    out
}

fn layout(cols: u16, rows: u16, d: &DialogOverlay) -> Laid {
    let w = MAX_WIDTH.min(cols.saturating_sub(4)).max(24);
    // Text starts one column inside the border plus one of padding, and stops
    // the same distance from the other side.
    let text_w = w.saturating_sub(4) as usize;

    let mut out: Vec<Row> = vec![Row::Blank];
    for para in &d.lead {
        out.extend(wrap(para, text_w).into_iter().map(Row::Lead));
    }
    if !d.facts.is_empty() {
        out.push(Row::Blank);
        let label_w = d.facts.iter().map(|f| f.label.chars().count()).max().unwrap_or(0);
        for f in &d.facts {
            // The value wraps under itself, so a long path stays in its column.
            let value_w = text_w.saturating_sub(label_w + 2).max(8);
            for (i, line) in wrap(&f.value, value_w).into_iter().enumerate() {
                let label = if i == 0 { f.label.clone() } else { String::new() };
                out.push(Row::Fact { label, value: line, role: f.role, label_w });
            }
        }
    }
    if !d.note.is_empty() {
        out.push(Row::Blank);
        for para in &d.note {
            out.extend(wrap(para, text_w).into_iter().map(Row::Note));
        }
    }
    if d.input.is_some() {
        out.push(Row::Blank);
        out.push(Row::Input);
    }
    if let Some(e) = &d.error {
        // Directly under the input it is about, not set off by a blank.
        out.extend(wrap(e, text_w.saturating_sub(2)).into_iter().map(Row::Error));
    }
    out.push(Row::Blank);
    out.push(Row::Rule);
    let button_row = out.len();
    out.push(Row::Buttons);

    // Right-aligned, last button against the edge: the default is drawn
    // highlighted wherever it sits, so position carries no meaning, but the
    // right edge is where the eye goes for "go on".
    let widths: Vec<u16> = d.buttons.iter().map(|b| b.text().chars().count() as u16).collect();
    let total: u16 =
        widths.iter().sum::<u16>() + BUTTON_GAP * widths.len().saturating_sub(1) as u16;
    let mut x = w.saturating_sub(2 + total);
    let mut spans = Vec::new();
    for bw in &widths {
        spans.push((x, x + bw));
        x += bw + BUTTON_GAP;
    }

    let h = (out.len() as u16 + 2).min(rows.saturating_sub(2)).max(5);
    let rect = LRect::new(cols.saturating_sub(w) / 2, rows.saturating_sub(h) / 2, w, h);
    Laid { rect, rows: out, button_row, spans }
}

/// Which button is under `(x, y)`, if any.
pub fn button_at(cols: u16, rows: u16, d: &DialogOverlay, x: u16, y: u16) -> Option<usize> {
    let l = layout(cols, rows, d);
    let ry = l.rect.y + 1 + l.button_row as u16;
    if y != ry || x < l.rect.x {
        return None;
    }
    let dx = x - l.rect.x;
    l.spans.iter().position(|&(a, b)| dx >= a && dx < b)
}

// ---- drawing -----------------------------------------------------------------

fn tone_color(tone: Tone, theme: &Theme) -> ratatui::style::Color {
    match tone {
        Tone::Info => theme.rule_focus,
        Tone::Warn => theme.attention,
        Tone::Danger => theme.danger,
    }
}

pub(super) fn draw(buf: &mut Buffer, cols: u16, rows: u16, d: &DialogOverlay, theme: &Theme) {
    let l = layout(cols, rows, d);
    let r = l.rect;
    let accent = tone_color(d.tone, theme);

    for y in r.y..r.y + r.height {
        for x in r.x..r.x + r.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_bg(theme.surface);
                cell.set_fg(theme.ink);
            }
        }
    }
    let mark = match d.tone {
        Tone::Info => "",
        Tone::Warn => "▲ ",
        Tone::Danger => "✕ ",
    };
    draw_box(buf, r, &format!(" {mark}{} ", d.title), accent, theme.surface);

    let bound = r.x + r.width - 1;
    let tx = r.x + 2;
    let text_w = r.width.saturating_sub(4) as usize;
    let bg = theme.surface;
    for (i, row) in l.rows.iter().take(r.height.saturating_sub(2) as usize).enumerate() {
        let y = r.y + 1 + i as u16;
        match row {
            Row::Blank => {}
            Row::Rule => {
                for x in r.x..=bound {
                    put_str(buf, x, y, "─", bound + 1, Pen::new(accent, bg));
                }
                put_str(buf, r.x, y, "├", r.x + 1, Pen::new(accent, bg));
                put_str(buf, bound, y, "┤", bound + 1, Pen::new(accent, bg));
            }
            Row::Lead(t) => put_str(buf, tx, y, t, bound, Pen::new(theme.ink, bg)),
            Row::Note(t) => put_str(buf, tx, y, t, bound, Pen::new(theme.faint, bg)),
            Row::Fact { label, value, role, label_w } => {
                put_str(buf, tx, y, label, bound, Pen::new(theme.faint, bg));
                let vx = tx + *label_w as u16 + 2;
                put_str(buf, vx, y, value, bound, Pen::new(theme.role(*role), bg));
            }
            Row::Error(t) => {
                // The mark on the first line only; a wrapped error indents.
                let first = !matches!(l.rows.get(i.wrapping_sub(1)), Some(Row::Error(_)));
                let lead = if first { "✕ " } else { "  " };
                put_str(buf, tx, y, &format!("{lead}{t}"), bound, Pen::new(theme.danger, bg));
            }
            Row::Input => {
                let Some(input) = &d.input else { continue };
                let label = if input.label.is_empty() {
                    String::new()
                } else {
                    format!("{}  ", input.label)
                };
                put_str(buf, tx, y, &label, bound, Pen::new(theme.faint, bg));
                // A marker, so an empty field still reads as one.
                let mx = tx + label.chars().count() as u16;
                put_str(buf, mx, y, "> ", bound, Pen::new(accent, bg));
                let fx = mx + 2;
                let field_w = text_w.saturating_sub(label.chars().count() + 2).max(2);
                // Keep the caret in view by dropping what scrolled off the left.
                let skip = (input.cursor + 1).saturating_sub(field_w);
                let shown: String = input
                    .text
                    .chars()
                    .skip(skip)
                    .take(field_w)
                    .map(|c| if input.secret { '•' } else { c })
                    .collect();
                put_str(buf, fx, y, &ellipsize(&shown, field_w), bound, Pen::new(theme.ink, bg));
                // The terminal's own cursor is parked on the pane underneath, so
                // the caret is a reversed cell, like the other modals.
                let cx = fx + (input.cursor - skip) as u16;
                if cx < bound {
                    if let Some(cell) = buf.cell_mut((cx, y)) {
                        cell.set_bg(theme.ink);
                        cell.set_fg(theme.surface);
                    }
                }
            }
            Row::Buttons => {
                for (n, b) in d.buttons.iter().enumerate() {
                    let Some(&(a, _)) = l.spans.get(n) else { continue };
                    let x = r.x + a;
                    if n == d.sel {
                        let pen = Pen { fg: theme.surface, bg: accent, bold: true };
                        put_str(buf, x, y, &b.text(), bound, pen);
                    } else {
                        // Key in the quiet colour, label in ink: the label is
                        // the part you read, the key the part you press.
                        let key = format!(" {} ", b.key);
                        put_str(buf, x, y, &key, bound, Pen::new(theme.faint, bg));
                        let lx = x + key.chars().count() as u16;
                        put_str(
                            buf,
                            lx,
                            y,
                            &format!("{} ", b.label),
                            bound,
                            Pen::new(theme.ink, bg),
                        );
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn text_of(buf: &Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect()
    }

    fn host_key() -> DialogOverlay {
        DialogOverlay {
            purpose: Purpose::Ask(1),
            title: "Unknown host".into(),
            tone: Tone::Warn,
            lead: vec!["The authenticity of macmini (100.113.243.56) can't be established.".into()],
            facts: vec![
                Fact { label: "type".into(), value: "ED25519".into(), role: Role::Ink },
                Fact {
                    label: "sha256".into(),
                    value: "hnZRMnSrfeIwqOHlgb106J8zCBjGGDvZgQhqkuMz6NI".into(),
                    role: Role::Ok,
                },
            ],
            note: vec!["Trusting saves the key to ~/.ssh/known_hosts.".into()],
            input: None,
            error: None,
            buttons: vec![
                Button::new("esc", "cancel", Some('n'), Action::Cancel),
                Button::new("y", "trust & connect", Some('y'), Action::Accept),
            ],
            sel: 0,
        }
    }

    #[test]
    fn draws_every_part_inside_the_screen() {
        let theme = Theme::default();
        for (cols, rows) in [(100u16, 30u16), (60, 24), (40, 20)] {
            let mut buf = Buffer::empty(Rect::new(0, 0, cols, rows));
            draw(&mut buf, cols, rows, &host_key(), &theme);
            let lines = text_of(&buf).join("\n");
            assert!(lines.contains("Unknown host"), "{cols}x{rows}: no title");
            assert!(lines.contains("trust & connect"), "{cols}x{rows}: no button");
            assert!(lines.contains("sha256"), "{cols}x{rows}: no fact");
        }
    }

    #[test]
    fn a_long_fingerprint_wraps_instead_of_leaving_the_box() {
        let theme = Theme::default();
        let (cols, rows) = (40u16, 24u16);
        let mut buf = Buffer::empty(Rect::new(0, 0, cols, rows));
        draw(&mut buf, cols, rows, &host_key(), &theme);
        let joined: String = text_of(&buf).join("");
        assert!(joined.contains("hnZRMnSrfeIwqOHl"));
        assert!(joined.contains("zCBjGGDvZgQhqkuMz6NI") || joined.contains("6NI"));
    }

    #[test]
    fn the_error_is_drawn_under_the_input_in_the_danger_colour() {
        let theme = Theme::default();
        let mut d = DialogOverlay::notice(
            "SSH password",
            Tone::Info,
            vec!["paul@macmini's password".into()],
        );
        d.input = Some(Input::new("", true));
        d.error = Some("Permission denied, please try again.".into());
        d.buttons = vec![
            Button::new("esc", "cancel", None, Action::Cancel),
            Button::new("enter", "ok", None, Action::Accept),
        ];
        d.sel = 1;
        let (cols, rows) = (80u16, 24u16);
        let mut buf = Buffer::empty(Rect::new(0, 0, cols, rows));
        draw(&mut buf, cols, rows, &d, &theme);
        let lines = text_of(&buf);
        let row = lines.iter().position(|l| l.contains("Permission denied")).expect("error row");
        let x = lines[row].find('P').unwrap() as u16;
        assert_eq!(buf[(x, row as u16)].fg, theme.danger);
        assert!(lines[row].contains('✕'));
        assert!(lines[row - 1].contains("│"), "error sits directly under the input row");
    }

    #[test]
    fn a_secret_is_never_drawn() {
        let theme = Theme::default();
        let mut d = DialogOverlay::notice("SSH password", Tone::Info, vec!["password".into()]);
        let mut input = Input::new("", true);
        "hunter2".chars().for_each(|c| input.insert(c));
        d.input = Some(input);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        draw(&mut buf, 80, 24, &d, &theme);
        let all = text_of(&buf).join("\n");
        assert!(!all.contains("hunter2"));
        assert!(all.contains("•••••••"));
    }

    #[test]
    fn keys_choose_buttons_and_esc_cancels() {
        let mut d = host_key();
        // Opens on cancel, so the key that opened the box cannot be the one
        // that trusts the host.
        assert_eq!(d.on_key(KeyCode::Enter, KeyModifiers::NONE), Some(Action::Cancel));
        assert_eq!(d.on_key(KeyCode::Char('y'), KeyModifiers::NONE), Some(Action::Accept));
        assert_eq!(d.on_key(KeyCode::Tab, KeyModifiers::NONE), None);
        assert_eq!(d.sel, 1);
        assert_eq!(d.on_key(KeyCode::Enter, KeyModifiers::NONE), Some(Action::Accept));
        assert_eq!(d.on_key(KeyCode::Esc, KeyModifiers::NONE), Some(Action::Cancel));
    }

    #[test]
    fn typing_goes_to_the_input_and_clears_the_error() {
        let mut d = DialogOverlay::notice("SSH passphrase", Tone::Info, vec!["key".into()]);
        d.input = Some(Input::new("", true));
        d.error = Some("bad".into());
        d.buttons = vec![Button::new("enter", "ok", None, Action::Accept)];
        // `q` and `y` are text here, not shortcuts.
        assert_eq!(d.on_key(KeyCode::Char('q'), KeyModifiers::NONE), None);
        assert!(d.error.is_none());
        d.on_key(KeyCode::Char('é'), KeyModifiers::NONE);
        d.on_key(KeyCode::Left, KeyModifiers::NONE);
        d.on_key(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(d.input.as_ref().unwrap().text, "é");
        assert_eq!(d.on_key(KeyCode::Enter, KeyModifiers::NONE), Some(Action::Accept));
    }

    #[test]
    fn a_click_finds_the_button_it_was_drawn_on() {
        let d = host_key();
        let (cols, rows) = (100u16, 30u16);
        let l = layout(cols, rows, &d);
        let y = l.rect.y + 1 + l.button_row as u16;
        for (i, &(a, b)) in l.spans.iter().enumerate() {
            assert_eq!(button_at(cols, rows, &d, l.rect.x + a, y), Some(i));
            assert_eq!(button_at(cols, rows, &d, l.rect.x + b - 1, y), Some(i));
        }
        assert_eq!(button_at(cols, rows, &d, l.rect.x + 1, y), None);
        assert_eq!(button_at(cols, rows, &d, l.rect.x + l.spans[0].0, y - 1), None);
    }

    #[test]
    fn wrap_cuts_a_word_longer_than_the_line() {
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("aa bb cc", 5), vec!["aa bb", "cc"]);
    }
}
