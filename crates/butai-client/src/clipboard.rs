//! Reading the *local* clipboard — the one thing a butai client can do that the
//! daemon cannot.
//!
//! The daemon owns every pane and composes every frame, so almost nothing has
//! to live out here. A clipboard is the exception: over `ssh host butai proxy`
//! the daemon is on another machine and the image the user just copied is on
//! this one. So the daemon asks (`ServerMsg::ReadClipboardImage`) and this
//! answers (`Command::PutFile`), which is the same shape as OSC 52 in the other
//! direction — the client is the part with a desktop attached.
//!
//! arboard hands back raw RGBA rather than whatever encoded form was on the
//! clipboard, so this re-encodes to PNG. That is not a round trip we can skip:
//! agent CLIs take an image as a path to a file they can decode, and raw
//! samples in a `.png` are not that.

use butai_protocol::{b64, Command, MAX_PUT_FILE_BYTES};

/// The image on the clipboard as a `put_file` command, or a human-readable
/// reason there isn't one.
///
/// `Err` is a sentence for a footer flash, not a type to match on: everything
/// that can fail here fails for a reason the user has to fix themselves
/// (nothing copied, copied text instead of an image, no display server).
pub fn image_as_put_file() -> Result<Command, String> {
    no_display_server()?;
    let mut clipboard = arboard::Clipboard::new().map_err(describe)?;
    let image = clipboard.get_image().map_err(describe)?;
    let (w, h) = (image.width, image.height);
    if w == 0 || h == 0 {
        return Err("the clipboard image is empty".into());
    }

    let png = encode_png(w, h, &image.bytes).map_err(|e| format!("encoding the image: {e}"))?;
    // Checked here as well as at the daemon so a screenshot too big to send
    // fails against the local machine's clipboard, naming the size — rather
    // than after crossing an ssh link to be refused.
    if png.len() > MAX_PUT_FILE_BYTES {
        return Err(format!(
            "that image is {} MB encoded, over the {} MB limit",
            png.len() / 1_000_000,
            MAX_PUT_FILE_BYTES / 1_000_000
        ));
    }
    Ok(Command::PutFile { name: "clipboard.png".into(), data: b64::encode(&png) })
}

/// Refuse before arboard when there is plainly no display server to ask.
///
/// This is the *common* case for butai, not an edge one: the TUI's home is an
/// ssh session, where `$DISPLAY` is unset unless X was forwarded. Left to
/// arboard, that becomes a multi-second X11 connection attempt — synchronous,
/// in the middle of the event loop — ending in "Unknown error while interacting
/// with the clipboard", which tells the user nothing they can act on.
///
/// Linux only. macOS has a clipboard and no `$DISPLAY`, so the same check there
/// would refuse the machines where this works best.
fn no_display_server() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let var = |k| std::env::var(k).ok();
        if !has_display(var("DISPLAY").as_deref(), var("WAYLAND_DISPLAY").as_deref()) {
            return Err("no display server here, so there is no clipboard to read".into());
        }
    }
    Ok(())
}

/// Whether either variable names a display. Takes its inputs rather than
/// reading the environment so a test can drive it: `set_var` is process-global
/// and this crate's tests run in one process, in parallel.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn has_display(x11: Option<&str>, wayland: Option<&str>) -> bool {
    [x11, wayland].into_iter().flatten().any(|v| !v.is_empty())
}

/// arboard's errors are typed but its `Display` is not always a sentence a user
/// can act on — `ContentNotAvailable` in particular is the common case here and
/// reads as an internal fault.
fn describe(e: arboard::Error) -> String {
    match e {
        arboard::Error::ContentNotAvailable => "no image on the clipboard".into(),
        arboard::Error::ClipboardNotSupported => {
            "no clipboard on this machine (no display server?)".into()
        }
        other => format!("clipboard: {other}"),
    }
}

/// RGBA8 → PNG.
fn encode_png(w: usize, h: usize, rgba: &[u8]) -> Result<Vec<u8>, png::EncodingError> {
    let mut out = Vec::new();
    // `u32` is the width/height PNG itself carries; a clipboard image bigger
    // than that is not a thing, but the cast should still be the checked one.
    let mut enc = png::Encoder::new(&mut out, w as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header()?;
    writer.write_image_data(rgba)?;
    writer.finish()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ssh session is the TUI's home and has no `$DISPLAY`, so this is the
    /// ordinary case rather than an edge one. Getting it wrong costs a
    /// multi-second X11 timeout in the middle of the event loop.
    #[test]
    fn a_session_with_no_display_has_no_clipboard() {
        assert!(!has_display(None, None), "ssh without X forwarding");
        // Unset and empty are the same thing; `DISPLAY=` is what a stripped
        // environment leaves behind.
        assert!(!has_display(Some(""), Some("")));
        assert!(has_display(Some(":0"), None), "X11");
        assert!(has_display(None, Some("wayland-0")), "wayland");
        assert!(has_display(Some(""), Some("wayland-0")), "wayland alone is enough");
    }

    #[test]
    fn encodes_a_real_png() {
        let (w, h) = (3usize, 2usize);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i * 17 % 256) as u8).collect();
        let png = encode_png(w, h, &rgba).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\x0a", "not a PNG signature");

        // Decode it back rather than trusting the header: the failure this
        // guards against is handing an agent a file it cannot open.
        let decoder = png::Decoder::new(std::io::Cursor::new(&png));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (w as u32, h as u32));
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(&buf[..info.buffer_size()], &rgba[..]);
    }

    #[test]
    fn wrong_sized_buffer_is_an_error_not_a_panic() {
        // arboard promises width*height*4; if it ever doesn't, this must come
        // back as a flash rather than take the client down.
        assert!(encode_png(4, 4, &[0u8; 8]).is_err());
    }

    #[test]
    fn content_not_available_reads_as_an_empty_clipboard() {
        assert_eq!(describe(arboard::Error::ContentNotAvailable), "no image on the clipboard");
    }
}
