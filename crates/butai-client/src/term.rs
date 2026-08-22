//! Terminal state that has to survive every exit path.
//!
//! [`crate::tui::TerminalGuard`] puts the terminal back on graceful exits and
//! on panic, but neither runs when a signal kills the process: the terminal is
//! then left in mouse-tracking mode, bracketed paste, the alternate screen and
//! raw mode, so every mouse move spews escape sequences into the shell. That
//! happens on `kill`, on a closed window or dropped ssh connection (SIGHUP),
//! and on a hard crash.
//!
//! So this module keeps its own copy of what a restore needs — a tty fd and
//! the cooked termios — in statics a signal handler is allowed to touch, and
//! installs handlers that put the terminal back with two async-signal-safe
//! syscalls. It can't reuse crossterm for that: `disable_raw_mode` takes a
//! mutex and opens `/dev/tty`, and neither is safe from a handler.
//!
//! Nothing catches `SIGKILL`. [`reset_terminal`] (`butai reset`) is the way out
//! of that, and out of a terminal an older butai already wedged.
//!
//! Known remaining hole: `SIGTSTP`. Backgrounding butai leaves the terminal raw
//! until it is resumed. Job control can't reach butai through raw mode's
//! disabled `ISIG`, so this only happens via an explicit `kill -TSTP`.

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Once;

use anyhow::{bail, Result};
use libc::{c_int, c_void};

/// The mouse reporting butai actually consumes: button-event tracking (press,
/// release, and motion *while a button is held*, which is what `MouseDrag`
/// needs) plus SGR coordinates, so columns past 223 survive.
///
/// Deliberately *not* `?1003h` (any-event tracking), which crossterm's
/// `EnableMouseCapture` sets: the input loop drops `Moved` events and the
/// protocol has no motion variant, so it buys nothing — and it is the mode
/// that turns a missed restore into a flood of garbage on idle mouse movement
/// instead of the odd stray click.
pub(crate) const ENABLE: &[u8] = b"\x1b[?1002h\x1b[?1006h";

/// Deliberately a superset of [`ENABLE`]: it also clears modes an older butai
/// (or an unrelated program) may have left set, so the same sequence serves
/// both the normal teardown and `butai reset`. Encodings off first, then every
/// tracking mode, then the screen state.
///
/// The screen state ends with `CSI 0 SP q` — the cursor shape back to whatever
/// the user configured. The workbench sets it: an unfocused stage gets a steady
/// underline, and without this the shell you dropped back into would keep it.
pub(crate) const RESTORE: &[u8] = b"\x1b[?1006l\x1b[?1015l\x1b[?1005l\
    \x1b[?1003l\x1b[?1002l\x1b[?1001l\x1b[?1000l\
    \x1b[?2004l\x1b[?1004l\
    \x1b[?1049l\x1b[?25h\x1b[0 q\x1b[0m";

/// Signals that would otherwise leave the terminal wedged. The first four are
/// ordinary termination; the rest are crashes, where chaining to whatever was
/// installed before us keeps Rust's "has overflowed its stack" message. There
/// is no entry for SIGKILL or SIGSTOP — they cannot be caught, which is what
/// [`reset_terminal`] exists for.
const SIGNALS: [c_int; 9] = [
    libc::SIGHUP,
    libc::SIGINT,
    libc::SIGQUIT,
    libc::SIGTERM,
    libc::SIGSEGV,
    libc::SIGBUS,
    libc::SIGILL,
    libc::SIGFPE,
    libc::SIGABRT,
];

/// The controlling terminal, kept open for the process lifetime so the signal
/// handler never has to open a file. -1 until [`install`] runs.
static TTY_FD: AtomicI32 = AtomicI32::new(-1);
/// Whether a restore is still owed. Cleared by the first restore to win, so
/// the handler is a no-op once the guard has already cleaned up.
static ARMED: AtomicBool = AtomicBool::new(false);
/// Whether [`ORIG_TERMIOS`] holds a real capture (false when stdout is a pipe).
static HAVE_TERMIOS: AtomicBool = AtomicBool::new(false);
static INSTALLED: Once = Once::new();

/// A static a signal handler may read. Every one of these is written exactly
/// once, before the handlers are armed, and only read afterwards.
struct HandlerCell<T>(UnsafeCell<MaybeUninit<T>>);
// SAFETY: write-once-before-arm, read-only after; see above.
unsafe impl<T> Sync for HandlerCell<T> {}
impl<T> HandlerCell<T> {
    const fn new() -> Self {
        Self(UnsafeCell::new(MaybeUninit::uninit()))
    }
    fn as_ptr(&self) -> *mut T {
        self.0.get().cast()
    }
}

/// The terminal settings from before raw mode, to hand back on the way out.
static ORIG_TERMIOS: HandlerCell<libc::termios> = HandlerCell::new();
/// The disposition each signal in [`SIGNALS`] had before we took it over.
static PREV: [HandlerCell<libc::sigaction>; SIGNALS.len()] =
    [const { HandlerCell::new() }; SIGNALS.len()];

/// Capture the terminal state and arm the signal handlers.
///
/// Call this *before* entering raw mode — the whole point is to save the
/// cooked settings. Degrades to a no-op when there is no terminal to restore
/// (output redirected, CI), which is why it can't fail.
pub fn install() {
    let Some(fd) = open_tty() else { return };
    // SAFETY: fd is a live tty; the cell is written before ARMED is set, and
    // never written again.
    let ok = unsafe { libc::tcgetattr(fd, ORIG_TERMIOS.as_ptr()) == 0 };
    HAVE_TERMIOS.store(ok, Ordering::SeqCst);
    TTY_FD.store(fd, Ordering::SeqCst);
    INSTALLED.call_once(install_handlers);
    ARMED.store(true, Ordering::SeqCst);
}

/// Stand the handlers down after a normal restore, so a signal arriving during
/// shutdown doesn't write escape sequences over the shell that follows us.
pub fn disarm() {
    ARMED.store(false, Ordering::SeqCst);
}

/// Whether a restore is still owed, i.e. whether the TUI currently owns the
/// terminal.
pub fn is_armed() -> bool {
    ARMED.load(Ordering::SeqCst)
}

/// `butai reset`: unwedge a terminal that a killed or crashed butai left in
/// mouse-tracking and raw mode. Runs in a fresh process with nothing saved, so
/// it builds sane settings from scratch rather than restoring them.
pub fn reset_terminal() -> Result<()> {
    let Some(fd) = open_tty() else {
        bail!("not a terminal (run `butai reset` from the terminal you want to fix)")
    };
    // Sequences first: even if the termios work fails, the mouse goes quiet.
    // SAFETY: fd is a live tty for the duration of the call.
    unsafe {
        write_all(fd, RESTORE);
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut t) == 0 {
            make_sane(&mut t);
            // TCSANOW, not TCSADRAIN/TCSAFLUSH: those wait for the output queue
            // to empty, and we just put a restore sequence in it. On a live
            // terminal that drains instantly, but `butai reset` must not be able
            // to hang on a wedged one.
            libc::tcsetattr(fd, libc::TCSANOW, &t);
        }
        // Throw away the mouse reports already queued on the input side — most
        // of what the user is staring at. Unlike the flush built into
        // TCSAFLUSH, this never waits on output.
        libc::tcflush(fd, libc::TCIFLUSH);
    }
    Ok(())
}

/// `stty sane`, applied in place so the baud rate, character size and the
/// `c_cc` entries we don't name survive. Only libc constants — never bare
/// integer literals, because `tcflag_t` is `u32` on Linux and `u64` on macOS.
fn make_sane(t: &mut libc::termios) {
    t.c_iflag |= libc::BRKINT | libc::ICRNL | libc::IXON | libc::IMAXBEL;
    t.c_iflag &= !(libc::IGNBRK | libc::INLCR | libc::IGNCR | libc::IXOFF);
    t.c_oflag |= libc::OPOST | libc::ONLCR;
    t.c_oflag &= !(libc::OCRNL | libc::ONOCR | libc::ONLRET);
    t.c_cflag |= libc::CREAD | libc::CS8;
    t.c_lflag |= libc::ISIG
        | libc::ICANON
        | libc::IEXTEN
        | libc::ECHO
        | libc::ECHOE
        | libc::ECHOK
        | libc::ECHOCTL
        | libc::ECHOKE;
    t.c_lflag &= !(libc::ECHONL | libc::NOFLSH | libc::TOSTOP);
    // Raw mode rewrites these, and a shell reading with VMIN=0 spins on
    // garbage. The control characters are respelled too: `ISIG` alone is no
    // use if whatever wedged the terminal also cleared VINTR.
    t.c_cc[libc::VMIN] = 1;
    t.c_cc[libc::VTIME] = 0;
    t.c_cc[libc::VINTR] = 0x03; // ^C
    t.c_cc[libc::VQUIT] = 0x1c; // ^\
    t.c_cc[libc::VERASE] = 0x7f; // DEL
    t.c_cc[libc::VKILL] = 0x15; // ^U
    t.c_cc[libc::VEOF] = 0x04; // ^D
    t.c_cc[libc::VSTART] = 0x11; // ^Q
    t.c_cc[libc::VSTOP] = 0x13; // ^S
    t.c_cc[libc::VSUSP] = 0x1a; // ^Z
}

/// The controlling terminal, or the first standard stream that still is one.
/// Opened read-write because `tcsetattr` wants more than a write end, and via
/// `/dev/tty` rather than stdout so the restore still lands when output is
/// redirected to a file or a pipe.
///
/// The fd is never closed: the signal handler reads it, and a closed number
/// could be reused by a later `open` — which we would then scribble escape
/// sequences into. `O_CLOEXEC` keeps it out of the spawned daemon.
fn open_tty() -> Option<RawFd> {
    // SAFETY: plain syscalls on a constant path / the standard descriptors.
    unsafe {
        let fd = libc::open(c"/dev/tty".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOCTTY);
        if fd >= 0 {
            return Some(fd);
        }
        [libc::STDOUT_FILENO, libc::STDERR_FILENO, libc::STDIN_FILENO]
            .into_iter()
            .find(|&fd| libc::isatty(fd) == 1)
    }
}

fn install_handlers() {
    for (slot, &sig) in SIGNALS.iter().enumerate() {
        // SAFETY: `sig` is a catchable signal and both sigaction arguments are
        // valid, initialised pointers.
        unsafe {
            // Zeroed, never a struct literal: Linux adds `sa_restorer` and
            // private padding that differ per arch.
            let mut sa: libc::sigaction = std::mem::zeroed();
            let handler: extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void) = on_signal;
            sa.sa_sigaction = handler as *const () as libc::sighandler_t;
            // SA_ONSTACK because a genuine stack overflow arrives with no room
            // left on the normal stack, and Rust already installed an alt stack.
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_RESTART | libc::SA_ONSTACK;
            // Block everything else for the duration: the restore must not be
            // re-entered halfway through an escape sequence.
            libc::sigfillset(&mut sa.sa_mask);
            let prev = PREV[slot].as_ptr();
            if libc::sigaction(sig, &sa, prev) != 0 {
                continue;
            }
            // A signal the parent asked us to ignore stays ignored: put the
            // disposition back and leave that slot alone.
            if (*prev).sa_sigaction == libc::SIG_IGN {
                libc::sigaction(sig, prev, std::ptr::null_mut());
            }
        }
    }
}

/// Restore the terminal, then let the signal do what it came to do.
///
/// Async-signal-safe throughout: `write` and `tcsetattr` are on the POSIX
/// list, and nothing here allocates or takes a lock.
extern "C" fn on_signal(sig: c_int, info: *mut libc::siginfo_t, ctx: *mut c_void) {
    if ARMED.swap(false, Ordering::SeqCst) {
        let fd = TTY_FD.load(Ordering::SeqCst);
        if fd >= 0 {
            // SAFETY: fd stays open for the process lifetime, and the termios
            // was captured before the handlers were armed.
            unsafe {
                write_all(fd, RESTORE);
                if HAVE_TERMIOS.load(Ordering::SeqCst) {
                    libc::tcsetattr(fd, libc::TCSANOW, ORIG_TERMIOS.as_ptr());
                }
            }
        }
    }

    // SAFETY: `prev` was filled in by `sigaction` and describes a real handler
    // whose signature we check via SA_SIGINFO before calling it.
    unsafe {
        // Hand the signal on to whoever had it before us — for SIGSEGV that is
        // Rust's alt-stack handler, which is what prints "has overflowed its
        // stack". Passing the original siginfo through matters: that is how it
        // recognises a guard-page fault.
        if let Some(slot) = SIGNALS.iter().position(|&s| s == sig) {
            let prev = *PREV[slot].as_ptr();
            let me = on_signal as *const () as libc::sighandler_t;
            if prev.sa_sigaction != libc::SIG_DFL
                && prev.sa_sigaction != libc::SIG_IGN
                && prev.sa_sigaction != me
            {
                if prev.sa_flags & libc::SA_SIGINFO != 0 {
                    let f = std::mem::transmute::<
                        libc::sighandler_t,
                        extern "C" fn(c_int, *mut libc::siginfo_t, *mut c_void),
                    >(prev.sa_sigaction);
                    f(sig, info, ctx);
                } else {
                    let f = std::mem::transmute::<libc::sighandler_t, extern "C" fn(c_int)>(
                        prev.sa_sigaction,
                    );
                    f(sig);
                }
            }
        }
        // Then die the way we would have with no handler installed at all, so
        // the shell reports "Terminated"/"Segmentation fault" and `$?` is
        // 128+sig.
        libc::signal(sig, libc::SIG_DFL);
        // `sa_mask` blocks this signal for the duration of the handler, and a
        // signal left pending that way is not delivered when the handler
        // returns — the process would just carry on with a terminal it has
        // already handed back. Unblocking first makes the `raise` land here
        // and now, synchronously.
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, sig);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        libc::raise(sig);
    }
}

/// `write(2)` until the buffer is gone. Short writes to a tty are unlikely at
/// this size, but a partial escape sequence is worse than none.
///
/// # Safety
/// `fd` must be open for writing.
unsafe fn write_all(fd: RawFd, mut buf: &[u8]) {
    while !buf.is_empty() {
        let n = libc::write(fd, buf.as_ptr().cast(), buf.len());
        if n <= 0 {
            break;
        }
        buf = &buf[n as usize..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a child left on a pty looks like after `sig` reaches it.
    struct Aftermath {
        /// Everything the child wrote, startup sequences included.
        out: Vec<u8>,
        /// The terminal settings it left behind.
        termios: libc::termios,
        /// Raw `waitpid` status.
        status: c_int,
    }

    /// Put a child on a pty in the state the TUI runs in — handlers armed, raw
    /// mode, mouse reporting on — then kill it with `sig`.
    ///
    /// Returns `None` when the platform won't hand out a pty (some containers),
    /// so CI can't fail spuriously.
    fn kill_under_pty(sig: c_int) -> Option<Aftermath> {
        let (mut master, mut slave) = (0, 0);
        // SAFETY: out-params are valid; null winsize/termios means "defaults".
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if opened != 0 {
            return None;
        }

        // SAFETY: the child touches only async-signal-safe calls before it
        // parks in `pause`, which is what makes forking from the (threaded)
        // test harness sound here.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            unsafe {
                libc::setsid();
                libc::ioctl(slave, libc::TIOCSCTTY as _, 0);
                libc::dup2(slave, 0);
                libc::dup2(slave, 1);
                libc::close(master);
                install();
                // Raw mode, the way crossterm would leave it.
                let mut t: libc::termios = std::mem::zeroed();
                libc::tcgetattr(0, &mut t);
                libc::cfmakeraw(&mut t);
                libc::tcsetattr(0, libc::TCSANOW, &t);
                write_all(1, ENABLE);
                loop {
                    libc::pause();
                }
            }
        }

        // SAFETY: `master` is ours, and `pid` is the child we just forked.
        unsafe {
            libc::close(slave);
            libc::usleep(400_000); // let it arm and go raw
            let mut before: libc::termios = std::mem::zeroed();
            libc::tcgetattr(master, &mut before);
            assert!(before.c_lflag & libc::ICANON == 0, "child never reached raw mode");

            libc::kill(pid, sig);
            libc::usleep(300_000); // let the handler finish writing

            // Collect the aftermath and let go of the pty *before* waiting: the
            // child is a session leader, so its exit blocks in `revoke()` on the
            // controlling terminal until every reference to the master is gone.
            let flags = libc::fcntl(master, libc::F_GETFL);
            libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
            let mut out = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = libc::read(master, buf.as_mut_ptr().cast(), buf.len());
                if n <= 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n as usize]);
            }
            let mut termios: libc::termios = std::mem::zeroed();
            libc::tcgetattr(master, &mut termios);
            libc::close(master);

            let (mut status, mut waited) = (0, 0);
            let got = loop {
                let got = libc::waitpid(pid, &mut status, libc::WNOHANG);
                if got != 0 || waited >= 5_000_000 {
                    break got;
                }
                libc::usleep(20_000);
                waited += 20_000;
            };
            assert_eq!(got, pid, "child not reaped after {waited}us; it wrote {out:?}");
            Some(Aftermath { out, termios, status })
        }
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    /// `butai reset` on a terminal wedged the way a SIGKILLed butai leaves one:
    /// raw, no signal keys, every mouse mode on.
    #[test]
    fn reset_unwedges_a_terminal_no_handler_ever_saw() {
        let (mut master, mut slave) = (0, 0);
        // SAFETY: out-params are valid; null winsize/termios means "defaults".
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if opened != 0 {
            eprintln!("no pty available; skipping");
            return;
        }

        // SAFETY: `slave` is a fresh pty; everything here is a plain syscall.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            libc::tcgetattr(slave, &mut t);
            libc::cfmakeraw(&mut t);
            t.c_cc[libc::VINTR] = 0;
            libc::tcsetattr(slave, libc::TCSANOW, &t);
            write_all(slave, b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?1049h");
        }

        // SAFETY: the child only makes syscalls before `_exit`. No TIOCSCTTY,
        // so `/dev/tty` misses and `reset_terminal` takes its isatty(stdout)
        // path — and the child isn't a session leader, so its exit can't block
        // in `revoke()` while we still hold the master.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0, "fork failed");
        if pid == 0 {
            unsafe {
                libc::setsid();
                libc::dup2(slave, 1);
                libc::close(master);
                let ok = reset_terminal().is_ok();
                libc::_exit(if ok { 0 } else { 1 });
            }
        }

        // SAFETY: reaping a child we forked and reading a pty we own.
        unsafe {
            let (mut status, mut waited) = (0, 0);
            let got = loop {
                let got = libc::waitpid(pid, &mut status, libc::WNOHANG);
                if got != 0 || waited >= 5_000_000 {
                    break got;
                }
                libc::usleep(20_000);
                waited += 20_000;
            };
            // A hang here means `reset` waited on a tty that never drains —
            // exactly what TCSADRAIN/TCSAFLUSH would do.
            assert_eq!(got, pid, "`reset` did not finish within {waited}us");
            assert_eq!(libc::WEXITSTATUS(status), 0, "`reset` reported failure");

            let flags = libc::fcntl(master, libc::F_GETFL);
            libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
            let mut out = Vec::new();
            let mut buf = [0u8; 4096];
            while let n @ 1.. = libc::read(master, buf.as_mut_ptr().cast(), buf.len()) {
                out.extend_from_slice(&buf[..n as usize]);
            }
            for off in ["?1006l", "?1003l", "?1002l", "?1000l", "?1049l", "?25h"] {
                assert!(contains(&out, off), "reset never sent {off}");
            }

            let mut after: libc::termios = std::mem::zeroed();
            libc::tcgetattr(master, &mut after);
            assert!(after.c_lflag & libc::ICANON != 0, "still raw (ICANON)");
            assert!(after.c_lflag & libc::ECHO != 0, "still raw (ECHO)");
            assert!(after.c_lflag & libc::ISIG != 0, "still raw (ISIG)");
            assert_eq!(after.c_cc[libc::VINTR], 0x03, "^C still dead");
            libc::close(master);
            libc::close(slave);
        }
    }

    /// The bug this module exists for: a signalled client used to leave mouse
    /// tracking and raw mode set, so every mouse move spewed into the shell.
    #[test]
    fn a_signalled_client_hands_the_terminal_back() {
        for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
            let Some(a) = kill_under_pty(sig) else {
                eprintln!("no pty available; skipping");
                return;
            };
            for off in ["?1006l", "?1003l", "?1002l", "?1000l", "?2004l", "?1049l", "?25h"] {
                assert!(contains(&a.out, off), "signal {sig}: no {off} in {:?}", a.out);
            }
            let l = a.termios.c_lflag;
            assert!(l & libc::ICANON != 0, "signal {sig}: left ICANON off");
            assert!(l & libc::ECHO != 0, "signal {sig}: left ECHO off");
            assert!(l & libc::ISIG != 0, "signal {sig}: left ISIG off");
        }
    }

    /// Restoring the terminal must not swallow the signal: the shell still has
    /// to print "Terminated" and see a 128+sig status.
    #[test]
    fn a_signalled_client_still_dies_of_that_signal() {
        for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
            let Some(a) = kill_under_pty(sig) else {
                eprintln!("no pty available; skipping");
                return;
            };
            assert!(
                libc::WIFSIGNALED(a.status),
                "signal {sig}: exited normally ({:#x}) instead of dying of the signal",
                a.status
            );
            assert_eq!(libc::WTERMSIG(a.status), sig, "died of the wrong signal");
        }
    }

    fn seqs(bytes: &[u8]) -> Vec<String> {
        String::from_utf8(bytes.to_vec())
            .unwrap()
            .split('\u{1b}')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn enable_asks_for_drag_and_sgr_only() {
        assert_eq!(seqs(ENABLE), ["[?1002h", "[?1006h"]);
    }

    /// Any-event tracking is what makes a leaked mouse mode unusable, and butai
    /// has no use for motion events — so it must never be switched on.
    #[test]
    fn enable_never_asks_for_any_event_tracking() {
        assert!(!seqs(ENABLE).iter().any(|s| s.contains("1003")));
    }

    #[test]
    fn restore_clears_everything_enable_can_set() {
        let restore = seqs(RESTORE);
        for on in seqs(ENABLE) {
            let off = on.replace('h', "l");
            assert!(restore.contains(&off), "RESTORE is missing {off}");
        }
    }

    #[test]
    fn restore_also_clears_modes_an_older_butai_left_behind() {
        let restore = seqs(RESTORE);
        // crossterm's EnableMouseCapture, plus the screen state we set.
        let expected = [
            "[?1000l", "[?1002l", "[?1003l", "[?1015l", "[?1006l", // mouse
            "[?2004l", // bracketed paste
            "[?1049l", // alternate screen
            "[?25h",   // cursor
            // The cursor's *shape*, which the workbench changes to say whether
            // the stage is listening. A client that only puts the cursor back
            // hands the shell an underline caret it never asked for.
            "[0 q",
        ];
        for off in expected {
            let off = off.to_string();
            assert!(restore.contains(&off), "RESTORE is missing {off}");
        }
    }

    #[test]
    fn sane_termios_gives_back_line_editing() {
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        make_sane(&mut t);
        assert!(t.c_lflag & (libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN) != 0);
        assert!(t.c_oflag & (libc::OPOST | libc::ONLCR) != 0);
        assert!(t.c_iflag & (libc::ICRNL | libc::IXON | libc::BRKINT) != 0);
        assert_eq!(t.c_cc[libc::VMIN], 1);
        assert_eq!(t.c_cc[libc::VINTR], 0x03, "^C must still interrupt");
    }
}
