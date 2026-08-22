//! Encode protocol key events into the byte sequences a program on the PTY
//! slave side expects (xterm-style).

use butai_protocol::{KeyCode, KeyEvent};

/// Modes a terminal application can set that change key encoding.
#[derive(Debug, Clone, Copy, Default)]
pub struct EncodeModes {
    /// DECCKM: arrows/home/end send SS3 (`ESC O A`) instead of CSI.
    pub application_cursor_keys: bool,
    /// Bracketed paste (DEC 2004): wrap pastes in `ESC[200~ .. ESC[201~`.
    pub bracketed_paste: bool,
}

/// Encode one key event. Returns an empty vec for keys that have no byte
/// representation.
pub fn encode_key(key: &KeyEvent, modes: EncodeModes) -> Vec<u8> {
    let mods = key.mods;
    // xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
    let modifier_param =
        1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.ctrl);

    let csi = |suffix: char| -> Vec<u8> {
        if modifier_param > 1 {
            format!("\x1b[1;{modifier_param}{suffix}").into_bytes()
        } else if modes.application_cursor_keys && matches!(suffix, 'A'..='D' | 'H' | 'F') {
            format!("\x1bO{suffix}").into_bytes()
        } else {
            format!("\x1b[{suffix}").into_bytes()
        }
    };
    let csi_tilde = |n: u8| -> Vec<u8> {
        if modifier_param > 1 {
            format!("\x1b[{n};{modifier_param}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };

    match key.code {
        KeyCode::Char(c) => {
            let mut base: Vec<u8> = if mods.ctrl {
                match c {
                    // C-a..C-z -> 0x01..0x1a; a few specials share codes.
                    'a'..='z' => vec![(c as u8) - b'a' + 1],
                    'A'..='Z' => vec![(c.to_ascii_lowercase() as u8) - b'a' + 1],
                    ' ' | '@' => vec![0x00],
                    '[' => vec![0x1b],
                    '\\' => vec![0x1c],
                    ']' => vec![0x1d],
                    '^' => vec![0x1e],
                    '_' | '/' => vec![0x1f],
                    _ => c.to_string().into_bytes(),
                }
            } else {
                c.to_string().into_bytes()
            };
            if mods.alt {
                let mut out = vec![0x1b];
                out.append(&mut base);
                out
            } else {
                base
            }
        }
        KeyCode::Enter => prefix_alt(mods.alt, vec![b'\r']),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Backspace => prefix_alt(mods.alt, vec![0x7f]),
        KeyCode::Tab => prefix_alt(mods.alt, vec![b'\t']),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Up => csi('A'),
        KeyCode::Down => csi('B'),
        KeyCode::Right => csi('C'),
        KeyCode::Left => csi('D'),
        KeyCode::Home => csi('H'),
        KeyCode::End => csi('F'),
        KeyCode::PageUp => csi_tilde(5),
        KeyCode::PageDown => csi_tilde(6),
        KeyCode::Insert => csi_tilde(2),
        KeyCode::Delete => csi_tilde(3),
        KeyCode::F(n) => encode_fkey(n, modifier_param),
    }
}

fn prefix_alt(alt: bool, mut bytes: Vec<u8>) -> Vec<u8> {
    if alt {
        let mut out = vec![0x1b];
        out.append(&mut bytes);
        out
    } else {
        bytes
    }
}

fn encode_fkey(n: u8, modifier_param: u8) -> Vec<u8> {
    // F1-F4 are SS3 P/Q/R/S unmodified, CSI 1;mP style modified;
    // F5+ are CSI <code>~.
    match n {
        1..=4 => {
            let c = (b'P' + n - 1) as char;
            if modifier_param > 1 {
                format!("\x1b[1;{modifier_param}{c}").into_bytes()
            } else {
                format!("\x1bO{c}").into_bytes()
            }
        }
        5..=12 => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                12 => 24,
                _ => unreachable!(),
            };
            if modifier_param > 1 {
                format!("\x1b[{code};{modifier_param}~").into_bytes()
            } else {
                format!("\x1b[{code}~").into_bytes()
            }
        }
        _ => Vec::new(),
    }
}

/// Encode pasted text, honoring bracketed-paste mode.
pub fn encode_paste(text: &str, modes: EncodeModes) -> Vec<u8> {
    if modes.bracketed_paste {
        let mut out = b"\x1b[200~".to_vec();
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.as_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use butai_protocol::KeyMods;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent { code, mods: KeyMods::default() }
    }

    #[test]
    fn plain_and_ctrl_chars() {
        let m = EncodeModes::default();
        assert_eq!(encode_key(&KeyEvent::char('a'), m), b"a");
        assert_eq!(encode_key(&KeyEvent::ctrl('c'), m), vec![0x03]);
        assert_eq!(encode_key(&KeyEvent::ctrl('C'), m), vec![0x03]);
        let alt_x = KeyEvent {
            code: KeyCode::Char('x'),
            mods: KeyMods { alt: true, ..Default::default() },
        };
        assert_eq!(encode_key(&alt_x, m), vec![0x1b, b'x']);
    }

    #[test]
    fn arrows_follow_cursor_mode() {
        let normal = EncodeModes::default();
        let app = EncodeModes { application_cursor_keys: true, ..Default::default() };
        assert_eq!(encode_key(&key(KeyCode::Up), normal), b"\x1b[A");
        assert_eq!(encode_key(&key(KeyCode::Up), app), b"\x1bOA");
        // Modified arrows always use CSI 1;n form.
        let ctrl_up =
            KeyEvent { code: KeyCode::Up, mods: KeyMods { ctrl: true, ..Default::default() } };
        assert_eq!(encode_key(&ctrl_up, app), b"\x1b[1;5A");
    }

    #[test]
    fn function_and_nav_keys() {
        let m = EncodeModes::default();
        assert_eq!(encode_key(&key(KeyCode::F(1)), m), b"\x1bOP");
        assert_eq!(encode_key(&key(KeyCode::F(5)), m), b"\x1b[15~");
        assert_eq!(encode_key(&key(KeyCode::Delete), m), b"\x1b[3~");
        assert_eq!(encode_key(&key(KeyCode::PageUp), m), b"\x1b[5~");
        assert_eq!(encode_key(&key(KeyCode::BackTab), m), b"\x1b[Z");
    }

    #[test]
    fn bracketed_paste() {
        let on = EncodeModes { bracketed_paste: true, ..Default::default() };
        assert_eq!(encode_paste("hi", on), b"\x1b[200~hi\x1b[201~");
        assert_eq!(encode_paste("hi", EncodeModes::default()), b"hi");
    }
}
