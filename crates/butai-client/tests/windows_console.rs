//! Exercise the native Console mode behind the TUI, including its last cell.
#![cfg(windows)]

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};
use windows_sys::Win32::System::Console::*;

struct Console {
    allocated: bool,
    screen: HANDLE,
    original: HANDLE,
    input: HANDLE,
    original_input: HANDLE,
}

impl Drop for Console {
    fn drop(&mut self) {
        butai_client::term::disarm();
        unsafe {
            SetStdHandle(STD_OUTPUT_HANDLE, self.original);
            SetStdHandle(STD_INPUT_HANDLE, self.original_input);
            CloseHandle(self.input);
            CloseHandle(self.screen);
            if self.allocated {
                FreeConsole();
            }
        }
    }
}

#[test]
fn unicode_and_bottom_right_cell_survive_and_console_state_is_restored() {
    // CI may redirect standard handles. Allocate a console only when this
    // process does not already have one, and paint an inactive private buffer.
    unsafe {
        let allocated = AllocConsole() != 0;
        let screen = CreateConsoleScreenBuffer(
            0x8000_0000 | 0x4000_0000, // GENERIC_READ | GENERIC_WRITE
            1 | 2,                     // FILE_SHARE_READ | FILE_SHARE_WRITE
            std::ptr::null(),
            CONSOLE_TEXTMODE_BUFFER,
            std::ptr::null(),
        );
        assert_ne!(screen, INVALID_HANDLE_VALUE, "{}", std::io::Error::last_os_error());
        let name: Vec<u16> = "CONIN$\0".encode_utf16().collect();
        let input = CreateFileW(
            name.as_ptr(),
            0x8000_0000 | 0x4000_0000,
            1 | 2,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        assert_ne!(input, INVALID_HANDLE_VALUE, "{}", std::io::Error::last_os_error());
        let console = Console {
            allocated,
            screen,
            original: GetStdHandle(STD_OUTPUT_HANDLE),
            input,
            original_input: GetStdHandle(STD_INPUT_HANDLE),
        };
        assert_ne!(SetStdHandle(STD_OUTPUT_HANDLE, screen), 0);
        assert_ne!(SetStdHandle(STD_INPUT_HANDLE, input), 0);
        let mut original_mode = 0;
        assert_ne!(GetConsoleMode(screen, &mut original_mode), 0);
        let original_cp = (GetConsoleCP(), GetConsoleOutputCP());
        butai_client::term::install();
        assert!(butai_client::term::is_armed());
        assert_eq!((GetConsoleCP(), GetConsoleOutputCP()), (65001, 65001));

        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        assert_ne!(GetConsoleScreenBufferInfo(screen, &mut info), 0);
        let write = |text: &str| {
            let text: Vec<u16> = text.encode_utf16().collect();
            let mut written = 0;
            assert_ne!(
                WriteConsoleW(
                    screen,
                    text.as_ptr().cast(),
                    text.len() as u32,
                    &mut written,
                    std::ptr::null()
                ),
                0,
            );
            assert_eq!(written as usize, text.len());
        };
        assert_ne!(SetConsoleCursorPosition(screen, COORD { X: 0, Y: 0 }), 0);
        write("┌─é");
        assert_ne!(
            SetConsoleCursorPosition(screen, COORD { X: info.dwSize.X - 1, Y: info.dwSize.Y - 1 }),
            0
        );
        write("X");
        let mut actual = [0u16; 3];
        let mut read = 0;
        assert_ne!(
            ReadConsoleOutputCharacterW(
                screen,
                actual.as_mut_ptr(),
                3,
                COORD { X: 0, Y: 0 },
                &mut read
            ),
            0
        );
        assert_eq!(
            String::from_utf16(&actual).unwrap(),
            "┌─é",
            "last-cell write scrolled the screen"
        );

        butai_client::term::disarm();
        let mut restored_mode = 0;
        assert_ne!(GetConsoleMode(screen, &mut restored_mode), 0);
        assert_eq!(restored_mode, original_mode);
        assert_eq!((GetConsoleCP(), GetConsoleOutputCP()), original_cp);
        drop(console);
    }
}
