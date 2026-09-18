//! Windows Console state restoration, including terminal-window close events.
use anyhow::{bail, Result};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Once;
use windows_sys::Win32::Foundation::{BOOL, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::WriteFile;
use windows_sys::Win32::System::Console::{
    GetConsoleCP, GetConsoleMode, GetConsoleOutputCP, GetStdHandle, SetConsoleCP,
    SetConsoleCtrlHandler, SetConsoleMode, SetConsoleOutputCP, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT,
    CTRL_C_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT, DISABLE_NEWLINE_AUTO_RETURN,
    ENABLE_ECHO_INPUT, ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
    ENABLE_PROCESSED_OUTPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE,
};

pub(crate) const ENABLE: &[u8] = b"\x1b[?1002h\x1b[?1006h";
pub(crate) const RESTORE: &[u8] = b"\x1b[?1006l\x1b[?1015l\x1b[?1005l\x1b[?1003l\x1b[?1002l\x1b[?1001l\x1b[?1000l\x1b[?2004l\x1b[?1004l\x1b[?1049l\x1b[?25h\x1b[0 q\x1b[0m";
static ARMED: AtomicBool = AtomicBool::new(false);
static INPUT: AtomicUsize = AtomicUsize::new(0);
static OUTPUT: AtomicUsize = AtomicUsize::new(0);
static INPUT_MODE: AtomicU32 = AtomicU32::new(0);
static OUTPUT_MODE: AtomicU32 = AtomicU32::new(0);
static INPUT_CP: AtomicU32 = AtomicU32::new(0);
static OUTPUT_CP: AtomicU32 = AtomicU32::new(0);
static INSTALLED: Once = Once::new();

fn configure_output(output: HANDLE, mode: u32) -> std::io::Result<()> {
    // Windows normally wraps immediately at the right edge. Painting the
    // bottom-right cell then scrolls the entire UI before the next cursor move.
    // Deferred wrapping matches the Unix terminals our cell painter expects.
    if unsafe {
        SetConsoleMode(
            output,
            mode | ENABLE_PROCESSED_OUTPUT
                | ENABLE_VIRTUAL_TERMINAL_PROCESSING
                | DISABLE_NEWLINE_AUTO_RETURN,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub fn install() {
    // SAFETY: Console APIs validate process standard handles; only successfully
    // captured handles/modes are published before arming the callback.
    unsafe {
        let input = GetStdHandle(STD_INPUT_HANDLE);
        let output = GetStdHandle(STD_OUTPUT_HANDLE);
        let (mut im, mut om) = (0, 0);
        if GetConsoleMode(input, &mut im) == 0 || GetConsoleMode(output, &mut om) == 0 {
            return;
        }
        INPUT.store(input as usize, Ordering::SeqCst);
        OUTPUT.store(output as usize, Ordering::SeqCst);
        INPUT_MODE.store(im, Ordering::SeqCst);
        OUTPUT_MODE.store(om, Ordering::SeqCst);
        INPUT_CP.store(GetConsoleCP(), Ordering::SeqCst);
        OUTPUT_CP.store(GetConsoleOutputCP(), Ordering::SeqCst);
        INSTALLED.call_once(|| {
            SetConsoleCtrlHandler(Some(on_control), 1);
        });
        let _ = configure_output(output, om);
        SetConsoleCP(65001);
        SetConsoleOutputCP(65001);
        ARMED.store(true, Ordering::SeqCst);
    }
}
pub fn disarm() {
    if ARMED.swap(false, Ordering::SeqCst) {
        unsafe {
            SetConsoleMode(
                INPUT.load(Ordering::SeqCst) as HANDLE,
                INPUT_MODE.load(Ordering::SeqCst),
            );
            SetConsoleMode(
                OUTPUT.load(Ordering::SeqCst) as HANDLE,
                OUTPUT_MODE.load(Ordering::SeqCst),
            );
            restore_code_pages();
        }
    }
}
pub fn is_armed() -> bool {
    ARMED.load(Ordering::SeqCst)
}

unsafe extern "system" fn on_control(event: u32) -> BOOL {
    if matches!(
        event,
        CTRL_C_EVENT
            | CTRL_BREAK_EVENT
            | CTRL_CLOSE_EVENT
            | CTRL_LOGOFF_EVENT
            | CTRL_SHUTDOWN_EVENT
    ) && ARMED.swap(false, Ordering::SeqCst)
    {
        let input = INPUT.load(Ordering::SeqCst) as HANDLE;
        let output = OUTPUT.load(Ordering::SeqCst) as HANDLE;
        // Use Win32 directly: crossterm's mutex may be held on the main thread.
        unsafe {
            SetConsoleMode(
                output,
                OUTPUT_MODE.load(Ordering::SeqCst) | ENABLE_VIRTUAL_TERMINAL_PROCESSING,
            );
            write_restore(output);
            SetConsoleMode(input, INPUT_MODE.load(Ordering::SeqCst));
            SetConsoleMode(output, OUTPUT_MODE.load(Ordering::SeqCst));
            restore_code_pages();
        }
    }
    0 // Continue to the next handler/default termination behavior.
}
unsafe fn restore_code_pages() {
    unsafe {
        SetConsoleCP(INPUT_CP.load(Ordering::SeqCst));
        SetConsoleOutputCP(OUTPUT_CP.load(Ordering::SeqCst));
    }
}
unsafe fn write_restore(output: HANDLE) {
    let mut written = 0;
    unsafe {
        WriteFile(
            output,
            RESTORE.as_ptr(),
            RESTORE.len() as u32,
            &mut written,
            std::ptr::null_mut(),
        );
    }
}

pub fn reset_terminal() -> Result<()> {
    // No saved state in this new process; restore normal console input flags.
    unsafe {
        let input = GetStdHandle(STD_INPUT_HANDLE);
        let output = GetStdHandle(STD_OUTPUT_HANDLE);
        let (mut im, mut om) = (0, 0);
        if input == INVALID_HANDLE_VALUE
            || output == INVALID_HANDLE_VALUE
            || GetConsoleMode(input, &mut im) == 0
            || GetConsoleMode(output, &mut om) == 0
        {
            bail!("not a terminal (run `butai reset` from the terminal you want to fix)");
        }
        SetConsoleMode(output, om | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        write_restore(output);
        // Remove VT/mouse/window input modes and re-enable cooked input.
        im &= !(0x0200 | 0x0010 | 0x0008);
        im |=
            ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT | ENABLE_EXTENDED_FLAGS;
        if SetConsoleMode(input, im) == 0 || SetConsoleMode(output, om) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}
