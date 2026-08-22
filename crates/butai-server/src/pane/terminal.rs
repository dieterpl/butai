//! Terminal panes: a PTY child process plus a server-side VT emulator grid.
//!
//! portable-pty's I/O is blocking, so each pane owns a reader thread that
//! forwards output chunks into the ServerCore event channel, and a waiter
//! thread that reports child exit.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use butai_protocol::{InputEvent, PaneId, SessionId};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tokio::sync::mpsc::UnboundedSender;

use crate::core::{Event, OutputTx};
use crate::input::encode::{encode_key, encode_paste};
use crate::pane::term_emu::{RowFormat, TermEmulator, Vt100Emulator};

/// Read chunks up to this size per PTY wake; bigger reads coalesce bursts.
const READ_CHUNK: usize = 65536;

/// Output younger than this means the program is actively working.
const WORKING_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a foreground-command probe stays fresh before it is taken again.
const FG_TTL: std::time::Duration = std::time::Duration::from_millis(500);

/// Cap on a rendered command line, so a pathological argv never becomes a
/// megabyte-long marquee.
const FG_MAX: usize = 512;

/// ...but only once the burst has been *streaming* for at least this long.
/// Opening a pane from another client resizes it, and the child answers the
/// SIGWINCH with a one-shot full repaint; the same happens on any redraw. Those
/// bursts are over in milliseconds, yet on raw recency they read as a whole
/// turn — the pane flips to "working" and, once it settles, fires a spurious
/// "agent finished" notification. A real turn streams for far longer, and an
/// agent with a known busy marker is recognized instantly anyway (see
/// [`BUSY_MARKERS`]), so this only delays the fallback path.
const WORKING_MIN_SPAN: std::time::Duration = std::time::Duration::from_secs(1);

/// Substrings that, when visible in a pane's footer, mean an interactive agent
/// is *actively working* (its status line offers a way to interrupt the turn).
/// Matched case-insensitively. This is a far steadier signal than raw output
/// recency: the line stays on screen for the whole turn, so a "thinking" pause
/// no longer looks like the turn ended.
///
/// Every entry is anchored to the key you would press. The bare verbs are not:
/// an agent writes "to interrupt", "to stop" or "running in the background" in
/// ordinary prose, that prose scrolls through the footer band, and a match
/// there would pin the pane to "busy" for as long as the sentence stayed on
/// screen — no spinner ever stopping, no "finished" notification.
const BUSY_MARKERS: &[&str] = &[
    "esc to interrupt",
    "escape to interrupt",
    "ctrl+c to interrupt",
    "ctrl-c to interrupt",
    "^c to interrupt",
    "esc to cancel",
    "ctrl+c to cancel",
    "ctrl-c to cancel",
    "ctrl+c to stop",
    "ctrl-c to stop",
];

/// Working markers that are a whole status *line* rather than a keyed hint, so
/// they are matched at the start of a footer line (after its box gutter). Same
/// phrase mid-sentence — "the dev server is running in the background" — is
/// prose and must not count.
const BUSY_LINE_STARTS: &[&str] = &["running in the background"];

/// How deep the band at the bottom of the visible grid is — where
/// [`BUSY_MARKERS`] count, and, measured up from the last *written* row
/// instead, where a question does (see [`TerminalPane::dialog_lines`]).
/// Agent spinner/status lines live in the footer (the
/// interrupt hint sits just above a two-or-three-line input box), so this
/// covers them while excluding the response body — otherwise the same generic
/// phrases ("to stop", "running in the background", or even an echoed
/// "esc to interrupt") appearing in an agent's own output would pin it to
/// "busy" forever and suppress the finished/needs-you notification.
const FOOTER_SCAN_ROWS: u16 = 8;

/// Footer substrings that are prompt *chrome* — they only occur in interactive
/// confirmation UI, never in prose — so on their own they mean the agent is
/// blocked on your input. This is a *positive* "needs you now" signal, so a
/// question fires the waiting notification immediately instead of waiting on an
/// output lull. A finished agent's ordinary input box (a bare `>`/`❯`) is
/// deliberately not here: idle is not a question.
const PROMPT_MARKERS: &[&str] = &[
    "(y/n)",
    "[y/n]",
    "(y/n/a)",
    "(yes/no)",
    // aider spells its confirmations with the accelerator in parentheses —
    // `Add src/main.rs to the chat? (Y)es/(N)o/(A)ll/(S)kip all [Yes]:` — which
    // none of the forms above match, so an aider agent blocked on a file-add
    // prompt used to report idle.
    "(y)es/(n)o",
    "press enter to continue",
    "press any key to continue",
    // The keyboard hint a modal chooser prints under its options. Claude Code's
    // multiple-choice question ends in `Enter to select · ↑/↓ to navigate · Esc
    // to cancel`, its folder-trust dialog in `Enter to confirm · Esc to cancel`,
    // its permission dialog in `Esc to cancel · Tab to amend`.
    //
    // Naming the act of *choosing* is what makes these safe to match. The cancel
    // half of the same line cannot be the marker: a bare "esc to cancel" is how
    // Gemini spells its *interrupt* hint (see [`BUSY_MARKERS`]).
    //
    // For Claude Code's question dialog this is the only signal that survives.
    // Every option carries a description line, so the highlighted `❯ 1. …` row
    // sits ten to twenty rows above the bottom — far outside
    // [`FOOTER_SCAN_ROWS`] — while the hint line is the last thing on screen at
    // any width or option count.
    "enter to select",
    "enter to confirm",
    "enter to choose",
    "tab to amend",
];

/// Questions a decision dialog asks in words. Weak on their own — an agent can
/// write the same sentence in an answer — so they only count when the footer
/// also shows a numbered option list, and never while a working marker is up
/// (see [`FooterSignals`]).
const QUESTION_MARKERS: &[&str] = &["do you want to", "do you want me to", "proceed?"];

/// Characters that open a boxed dialog line, stripped before a line is matched
/// by shape (option cursor, line-start marker). Claude Code's permission dialog
/// draws its options inside a box, so `❯ 1. Yes` reaches us as `│ ❯ 1. Yes  │`.
const GUTTER_CHARS: &[char] = &['│', '┃', '║', '|', '╎', '┆', '▌', ' ', '\t'];

/// Glyphs a TUI uses to point at the highlighted menu entry.
const CURSOR_GLYPHS: &[char] = &['❯', '▶', '➤', '›', '>', '*'];

/// How far one notch of the wheel moves a pane that is *not* handling the mouse
/// itself.
///
/// Three lines, which is what a terminal does and what this pane has always
/// done — [`Terminal::handle_input`] has spelled it that way since the TUI was
/// the only client. Named because there is a second caller now: a pane-scoped
/// connection routes its wheel through `pane_wheel`, and that one reached for
/// `scroll_page` instead, so the same gesture moved a whole screen for every
/// client attached the modern way. A notch that jumps 40 lines does not read as
/// scrolling; it reads as losing your place.
pub const WHEEL_LINES: i32 = 3;

/// Mouse events forwardable to an application that enabled reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Click,
    WheelUp,
    WheelDown,
}

/// Attention state for the agent/process rails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attention {
    /// Wants your input now: rang the bell since we last looked, or a
    /// question/decision prompt (see [`PROMPT_MARKERS`]) is on screen.
    Waiting,
    /// Output within the last couple of seconds.
    Working,
    Idle,
}

/// Per-agent detection overrides, compiled from an `[[agents]]` block's
/// `waiting_pattern` / `busy_pattern`.
///
/// Each pattern *replaces* the built-in table for that one signal instead of
/// adding to it. The escape hatch exists for markers that misfire on a
/// particular CLI, and an additive pattern can only ever add matches — it
/// could never take back a false positive, which is the harder half of the
/// problem (a pane pinned to "busy" never fires its finished notification).
///
/// Absent overrides leave the generic tables in charge, so an agent nobody has
/// configured behaves exactly as before.
#[derive(Debug, Default)]
pub struct Detect {
    /// Replaces [`PROMPT_MARKERS`] and the question/numbered-option shape rules.
    waiting: Option<regex::Regex>,
    /// Replaces [`BUSY_MARKERS`] and [`BUSY_LINE_STARTS`].
    busy: Option<regex::Regex>,
}

impl Detect {
    /// Compile one agent's pair of overrides. `agent` only names the source in
    /// the warning.
    pub fn compile(agent: &str, waiting: Option<&str>, busy: Option<&str>) -> Self {
        Self {
            waiting: compile_pattern(agent, "waiting_pattern", waiting),
            busy: compile_pattern(agent, "busy_pattern", busy),
        }
    }
}

/// A pattern that does not compile is dropped with a warning rather than
/// failing the spawn: falling back to the built-in markers costs some accuracy,
/// while refusing to start costs the user their agent.
fn compile_pattern(agent: &str, field: &str, pattern: Option<&str>) -> Option<regex::Regex> {
    let pattern = pattern?;
    // Footer lines reach the matcher lowercased, so this only spares the user
    // from having to know that when they write a pattern with capitals in it.
    match regex::RegexBuilder::new(pattern).case_insensitive(true).build() {
        Ok(re) => Some(re),
        Err(e) => {
            tracing::warn!(
                "agent {agent}: {field} {pattern:?} is not a valid regex ({e}); \
                 falling back to the built-in markers"
            );
            None
        }
    }
}

/// What the footer band says about the program's state, read in one pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FooterSignals {
    /// A working marker is up: the turn is live (see [`BUSY_MARKERS`]).
    pub busy: bool,
    /// A question/decision prompt is up: it is blocked on you.
    pub prompt: bool,
}

pub struct TerminalPane {
    emulator: Box<dyn TermEmulator>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// Present until the child exits.
    exit_status: Option<u32>,
    command_label: String,
    /// True for labels set explicitly (agent names): they beat OSC titles.
    label_fixed: bool,
    last_output: std::time::Instant,
    /// When the child was spawned. Unlike [`Self::work_since`] this never
    /// resets, so it answers "how long has this agent been alive" — what the
    /// ALL AGENTS panel picks its sprite from.
    started_at: std::time::Instant,
    /// Start of the current output burst (a new turn after a lull), for the
    /// "working" elapsed timer in the rail. Reset when output resumes after a
    /// gap of at least [`WORKING_WINDOW`]; `None` until the first output.
    work_since: Option<std::time::Instant>,
    bells_acked: usize,
    rows: u16,
    cols: u16,
    /// Trailing partial escape carried between reads so a query split across
    /// two PTY chunks is still recognized (see [`terminal_queries`]).
    query_carry: Vec<u8>,
    /// CSI state carried between reads so an HVP split across two PTY chunks is
    /// still normalized to CUP (see [`HvpRewriter`]).
    hvp: HvpRewriter,
    /// Last foreground-command probe and when it was taken (see [`FG_TTL`]).
    fg_cache: Option<(std::time::Instant, Option<String>)>,
    /// Per-agent status overrides; empty for shells and processes.
    detect: Detect,
    /// Recent raw output, kept so a daemon restart can rebuild this pane's
    /// screen (see [`OutputHistory`]). Empty when restore is disabled.
    history: OutputHistory,
    /// Relay announcements seen but not yet acted on; see
    /// [`take_announcements`](Self::take_announcements).
    announcements: Vec<RemoteAnnounce>,
    /// The live footer band, held only while the view is parked in the
    /// scrollback and the visible grid is therefore not it. `None` — the
    /// common case — means the two are the same thing. See
    /// [`sync_parked_footer`](Self::sync_parked_footer).
    parked_footer: Option<Vec<String>>,
}

/// A butai on the far end of an ssh session running in a pane, saying where it
/// is so the near daemon can dial it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAnnounce {
    /// `user@host` derived from the far side's `$SSH_CONNECTION` — the fallback
    /// for dialling back when the pane's own `ssh` arguments cannot be read.
    pub hint: String,
    /// The far daemon's socket path. Usually left unused: passing it back would
    /// pin `BUTAI_SOCKET` on the far side, and the far `butai` resolving its own
    /// default is what makes it attach to the daemon already running there.
    pub socket: String,
}

/// Bounded ring of the raw bytes the child has written, oldest first.
///
/// Replaying the untouched output stream into a fresh emulator is what makes a
/// restored pane look like the one that was lost: wrapping, scroll regions and
/// colors all land the way they originally did, because the parser is being
/// fed exactly what it was fed the first time. Snapshotting the *rendered*
/// screen instead would cost history — vt100's `contents_formatted` covers the
/// visible rows only — and would have to re-derive what the byte stream
/// already states.
///
/// The tail almost always begins mid-sequence. That is safe: the parser
/// discards the truncated head and resynchronizes at the next escape, so the
/// worst case is one garbled line at the very top of the restored scrollback.
struct OutputHistory {
    buf: std::collections::VecDeque<u8>,
    cap: usize,
}

impl OutputHistory {
    fn new(cap: usize) -> Self {
        Self { buf: std::collections::VecDeque::new(), cap }
    }

    fn push(&mut self, bytes: &[u8]) {
        if self.cap == 0 {
            return;
        }
        // One write bigger than the whole budget (a `cat` of a large file, a
        // full-screen redraw at a big size): only its tail can survive, and
        // slicing here keeps the drain below from walking the excess twice.
        let bytes = match bytes.len() > self.cap {
            true => &bytes[bytes.len() - self.cap..],
            false => bytes,
        };
        let overflow = (self.buf.len() + bytes.len()).saturating_sub(self.cap);
        self.buf.drain(..overflow);
        self.buf.extend(bytes);
    }

    fn snapshot(&self) -> Vec<u8> {
        self.buf.iter().copied().collect()
    }
}

pub struct SpawnSpec<'a> {
    /// Program to exec; when `None`, the user's shell.
    pub program: Option<&'a str>,
    pub args: &'a [String],
    pub env: &'a [(String, String)],
    pub cwd: &'a Path,
    pub shell: &'a str,
    /// When set, run `program` through `shell -c` (used for command strings).
    pub via_shell: bool,
    /// Title label override (e.g. the agent name).
    pub label: Option<&'a str>,
    /// Compiled per-agent status overrides. `None` for shells and processes,
    /// which have no `[[agents]]` block to read them from.
    pub detect: Option<Detect>,
    /// Output captured by a previous daemon, replayed into the emulator before
    /// the new child writes anything (see [`OutputHistory`]).
    pub replay: Option<PaneDump<'a>>,
    /// Which pane this is and which workspace it belongs to, injected into the
    /// child's environment so a program running inside it can name itself
    /// without being told where it is.
    pub pane: PaneId,
    pub ws: SessionId,
    /// The socket the daemon actually bound — *not* whatever `$BUTAI_SOCKET`
    /// said in the daemon's own environment, which is unset under
    /// `daemon::serve` and so would hand every pane the default path.
    pub socket: &'a Path,
}

/// A pane recording: output, and the terminal size it was produced at.
///
/// The size is not decoration. A recording is full of absolute cursor moves —
/// every prompt redraw and every full-screen repaint emits them — and those
/// are meaningful only against the geometry the program was writing for.
/// Replayed a few columns narrower, the moves land in the wrong cells and the
/// text arrives shredded and scattered instead of merely re-wrapped.
#[derive(Clone, Copy)]
pub struct PaneDump<'a> {
    pub cols: u16,
    pub rows: u16,
    pub bytes: &'a [u8],
}

/// Header prefixed to a dump on disk, so the file carries the geometry needed
/// to replay it rather than depending on a second file to stay in step.
const DUMP_MAGIC: &[u8] = b"butai-dump 1 ";

/// Serialize a dump for storage: `butai-dump 1 <cols> <rows>\n`, then the bytes.
pub fn encode_dump(cols: u16, rows: u16, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 24);
    out.extend_from_slice(DUMP_MAGIC);
    out.extend_from_slice(format!("{cols} {rows}\n").as_bytes());
    out.extend_from_slice(bytes);
    out
}

/// Parse a stored dump. `None` for anything that is not one — a truncated
/// file, or a dump written by a future version whose format we do not know.
/// The caller treats that as "no saved output", which is always safe.
pub fn decode_dump(raw: &[u8]) -> Option<PaneDump<'_>> {
    let rest = raw.strip_prefix(DUMP_MAGIC)?;
    let nl = rest.iter().position(|b| *b == b'\n')?;
    let mut dims = std::str::from_utf8(&rest[..nl]).ok()?.split_whitespace();
    let cols: u16 = dims.next()?.parse().ok()?;
    let rows: u16 = dims.next()?.parse().ok()?;
    if cols == 0 || rows == 0 {
        return None;
    }
    Some(PaneDump { cols, rows, bytes: &rest[nl + 1..] })
}

impl TerminalPane {
    // One over clippy's default. The wide inputs are already gathered in
    // `SpawnSpec`; what is left is the pane's own identity and geometry.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        id: PaneId,
        spec: SpawnSpec<'_>,
        rows: u16,
        cols: u16,
        scrollback: usize,
        restore_bytes: usize,
        events: UnboundedSender<Event>,
        output: OutputTx,
    ) -> Result<Self> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("openpty")?;

        let (mut cmd, label) = match spec.program {
            None => (CommandBuilder::new(spec.shell), shell_label(spec.shell)),
            Some(prog) if spec.via_shell => {
                let mut c = CommandBuilder::new(spec.shell);
                c.arg("-c");
                c.arg(prog);
                // The whole command, not just its first word: a row reading
                // "sudo" says far less than "sudo apt-get update -y".
                (c, prog.to_string())
            }
            Some(prog) => {
                let mut c = CommandBuilder::new(resolve_program(prog));
                for a in spec.args {
                    c.arg(a);
                }
                // Label with what was asked for, not where it was found: the
                // rail should say `claude`, not a 60-character nvm path.
                (c, prog.to_string())
            }
        };
        cmd.cwd(spec.cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        // The one thing the daemon's inherited environment routinely gets wrong.
        // Set here rather than only around `resolve_program` because it is the
        // *child's* lookups that fail: the interpreter behind an agent's
        // launcher, and every command in a `$SHELL -c` process, which reads no
        // rc file. `None` when there is nothing to add — see [`child_path`].
        if let Some(path) = child_path() {
            cmd.env("PATH", path);
        }
        // Marker so a butai client launched inside a pane can detect the nesting
        // and refuse (mirrors tmux's `$TMUX`). Value is the daemon's socket path,
        // and it must be the socket this daemon actually bound rather than the
        // default one: the nesting guard compares it against the socket being
        // attached to, so that attaching a *different* daemon from inside a pane
        // is allowed. A daemon on a non-default socket that advertised the
        // default path here would both refuse legitimate attaches and permit
        // attaching itself.
        cmd.env("BUTAI", spec.socket);
        // Identity. `$BUTAI_PANE` is what `butai whoami` reads, and its *absence*
        // is the test for "not running inside butai" — no separate marker
        // variable, because a pane id is strictly more informative than a
        // boolean. Set before `spec.env` so an `[[agents]] env` entry can still
        // override any of them.
        cmd.env("BUTAI_SOCKET", spec.socket);
        cmd.env("BUTAI_PANE", spec.pane.to_string());
        cmd.env("BUTAI_WORKSPACE", spec.ws.to_string());
        for (k, v) in spec.env {
            cmd.env(k, v);
        }

        let label_fixed = spec.label.is_some();
        let label = spec.label.map(str::to_string).unwrap_or(label);
        let child = pair.slave.spawn_command(cmd).with_context(|| {
            // Name the PATH that was actually searched. The daemon inherits it
            // from whatever started it, which is rarely the login shell the
            // user installed the agent from — so "not found in PATH" without
            // saying *which* PATH sends people hunting in the wrong shell.
            format!(
                "spawn in pty (PATH={})",
                std::env::var("PATH").unwrap_or_else(|_| "<unset>".into())
            )
        })?;
        drop(pair.slave);

        let mut killer = child.clone_killer();
        let reader = pair.master.try_clone_reader().context("clone pty reader")?;
        let writer = pair.master.take_writer().context("take pty writer")?;

        // The child is already running, so a failure past this point would
        // leave an orphan nobody can read from or kill. Reap it on the way out.
        if let Err(e) = spawn_reader_thread(id, reader, output)
            .and_then(|()| spawn_wait_thread(id, child, events))
        {
            let _ = killer.kill();
            return Err(e);
        }

        let mut pane = Self {
            emulator: Box::new(Vt100Emulator::new(rows, cols, scrollback)),
            master: pair.master,
            writer,
            killer,
            exit_status: None,
            command_label: label,
            label_fixed,
            last_output: std::time::Instant::now(),
            started_at: std::time::Instant::now(),
            work_since: None,
            bells_acked: 0,
            rows,
            cols,
            query_carry: Vec::new(),
            hvp: HvpRewriter::default(),
            fg_cache: None,
            detect: spec.detect.unwrap_or_default(),
            history: OutputHistory::new(restore_bytes),
            announcements: Vec::new(),
            parked_footer: None,
        };
        if let Some(saved) = spec.replay {
            pane.replay(saved);
        }
        Ok(pane)
    }

    /// Rebuild the screen a previous daemon left behind, before the new child
    /// writes anything.
    ///
    /// Straight into the emulator rather than through
    /// [`feed_output`](Self::feed_output): these bytes are a recording, not
    /// live output. Answering the cursor-position queries inside them would
    /// write a reply the new child never asked for onto its stdin, and letting
    /// them move the activity clocks would date the pane to the restore
    /// instead of to the work.
    fn replay(&mut self, saved: PaneDump<'_>) {
        // Replay at the geometry the output was written for, then resize to
        // this pane's actual size the way a live pane is resized when the
        // window changes. Feeding it straight in at the new size instead
        // scatters the text (see [`PaneDump`]).
        self.emulator.resize(saved.rows, saved.cols);
        self.emulator.feed(saved.bytes);
        // The recording carries whatever input modes the old child had turned
        // on, and the emulator is the only thing that remembers them — the new
        // child starts with its own idea and will set what it needs. Left as
        // they were, a pane restored mid-`vim` reports mouse events and pastes
        // bracketed to a plain shell that asked for neither.
        self.emulator
            .feed(b"\x1b[?1l\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l");
        self.emulator.resize(self.rows, self.cols);
        // Seed the ring so the next restart still has this pane's history even
        // if the new child never prints enough to refill it on its own.
        self.history.push(saved.bytes);
    }

    /// Recent raw output, for persisting across a daemon restart. Empty when
    /// `[general] restore_bytes` is 0.
    pub fn history(&self) -> Vec<u8> {
        self.history.snapshot()
    }

    /// Current pane geometry, recorded alongside a dump so it can be replayed
    /// against the size it was written for.
    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    pub fn feed_output(&mut self, bytes: &mut [u8]) {
        // Output does *not* move the view. It used to snap it back to the live
        // screen, which made a shell scrollable — nothing arrives while you
        // read — and an agent not: Claude repaints its status band several
        // times a second, so every wheel notch was undone before the next
        // frame reached the client. The pane stays where it was put and the
        // emulator carries the offset forward as lines scroll off, which is
        // what every terminal does; [`Self::handle_input`] is what returns it
        // to live, because typing is aimed at the live screen.
        //
        // vt100 implements CUP but not HVP, so normalize before anything else
        // reads the stream. In place and length-preserving, which keeps the
        // query scanner's offsets below valid.
        self.hvp.rewrite(bytes);
        // Record after the rewrite so a replay feeds the same normalized stream
        // the emulator saw, and never re-runs the rewriter over its own output.
        self.history.push(bytes);
        // vt100 parses terminal queries (cursor-position report, device
        // status/attributes) but never answers them. Programs that query then
        // block waiting for a reply — most visibly readline's reverse-i-search
        // redisplay and fzf's Ctrl-R widget, which emit `ESC[6n` and stall
        // until answered. Reply on the child's behalf, splitting the feed at
        // each query so the position reported is the cursor *at the query*,
        // not wherever the rest of the chunk left it.
        let reply = self.feed_answering_queries(bytes);
        if !reply.is_empty() {
            let _ = self.writer.write_all(&reply);
            let _ = self.writer.flush();
        }
        let now = std::time::Instant::now();
        // A fresh burst after a lull (or the very first output) starts a new
        // "turn" clock for the rail's working timer.
        if self.work_since.is_none() || now.duration_since(self.last_output) >= WORKING_WINDOW {
            self.work_since = Some(now);
        }
        self.last_output = now;
        self.sync_parked_footer();
    }

    /// Feed `bytes` to the emulator, pausing at every terminal query to sample
    /// the cursor, and return the concatenated replies. Splitting the feed
    /// matters because a chunk can carry bytes *after* a query (the core
    /// coalesces PTY reads per pane before handing them over): answering with
    /// the end-of-chunk cursor would report a position the child never had.
    fn feed_answering_queries(&mut self, bytes: &[u8]) -> Vec<u8> {
        let queries = terminal_queries(&mut self.query_carry, bytes);
        if queries.is_empty() {
            self.emulator.feed(bytes);
            return Vec::new();
        }
        let mut reply = Vec::new();
        let mut fed = 0;
        for (end, query) in queries {
            self.emulator.feed(&bytes[fed..end]);
            fed = end;
            reply.extend_from_slice(&query.reply(self.emulator.cursor_pos()));
            if let Query::RemoteAnnounce { hint, socket } = query {
                self.announcements.push(RemoteAnnounce { hint, socket });
            }
        }
        self.emulator.feed(&bytes[fed..]);
        reply
    }

    /// Take any relay announcements this pane has seen since the last call.
    ///
    /// Drained by the core rather than pushed, because acting on one means
    /// dialling ssh and that is the core's business, not a pane's.
    pub fn take_announcements(&mut self) -> Vec<RemoteAnnounce> {
        std::mem::take(&mut self.announcements)
    }

    /// Attention state for the rails: bell or a question wins, then a working
    /// marker / sustained output, then idle. Dead panes are idle (their exit
    /// status is surfaced separately).
    ///
    /// Reads the same signals as [`is_busy`](Self::is_busy) so the live rail and
    /// the debounced state clients see never disagree — in particular a pane
    /// showing an interrupt hint stays "working" through a thinking pause
    /// instead of dropping to idle every time output stalls for two seconds.
    pub fn attention(&self) -> Attention {
        if self.exit_status.is_some() {
            return Attention::Idle;
        }
        let signals = self.footer_signals();
        // A rung bell or a visible question prompt means it wants input — and
        // this beats "recent output", since drawing the prompt *is* output.
        if self.bell_pending() || signals.prompt {
            return Attention::Waiting;
        }
        if signals.busy || self.sustained_output() {
            return Attention::Working;
        }
        Attention::Idle
    }

    /// Whether the pane rang the bell since the user last looked at it.
    pub fn bell_pending(&self) -> bool {
        self.emulator.bell_count() > self.bells_acked
    }

    /// Called when the user looks at the pane (it takes the stage).
    pub fn acknowledge(&mut self) {
        self.bells_acked = self.emulator.bell_count();
    }

    /// How long since this pane last produced output.
    pub fn last_output_age(&self) -> std::time::Duration {
        self.last_output.elapsed()
    }

    /// How long the current output burst has been running, for the rail's
    /// "working" timer. `None` before any output.
    pub fn work_elapsed(&self) -> Option<std::time::Duration> {
        self.work_since.map(|t| t.elapsed())
    }

    /// How long since the child was spawned. Whole-life, unlike
    /// [`Self::work_elapsed`], which restarts on every lull.
    pub fn age(&self) -> std::time::Duration {
        self.started_at.elapsed()
    }

    /// When the child was spawned, as unix epoch millis.
    ///
    /// The wall-clock face of [`Self::age`], for clients that draw the timer
    /// themselves. Both times are kept as monotonic `Instant`s internally —
    /// which is right, since a clock adjustment must not make an agent appear to
    /// have started in the future — and converted here at the boundary.
    pub fn started_ms(&self) -> u64 {
        epoch_ms_from(self.started_at)
    }

    /// When the current turn began, as unix epoch millis; `None` when idle.
    pub fn working_since_ms(&self) -> Option<u64> {
        self.work_since.map(epoch_ms_from)
    }

    /// A steady "actively working" signal for the notification tracker: a
    /// visible working marker (survives thinking pauses) or sustained output
    /// (fallback for agents without a known marker).
    pub fn is_busy(&self) -> bool {
        self.exit_status.is_none() && (self.shows_busy_marker() || self.sustained_output())
    }

    /// Whether output is both recent ([`WORKING_WINDOW`]) and has been
    /// streaming for at least [`WORKING_MIN_SPAN`] — i.e. a real burst of work
    /// rather than a one-shot repaint answering a resize or a redraw.
    pub fn sustained_output(&self) -> bool {
        let Some(start) = self.work_since else { return false };
        self.last_output.elapsed() < WORKING_WINDOW
            && self.last_output.duration_since(start) >= WORKING_MIN_SPAN
    }

    /// Test hook: pretend the current burst started `d` earlier, so one
    /// synthetic `feed_output` can stand in for output that kept streaming.
    #[cfg(test)]
    fn backdate_burst(&mut self, d: std::time::Duration) {
        if let Some(start) = self.work_since.as_mut() {
            *start = start.checked_sub(d).unwrap_or(*start);
        }
    }

    /// Whether the visible screen shows a "still working" marker (see
    /// [`BUSY_MARKERS`]). Steady across output pauses within a turn, so it's the
    /// primary signal for the accurate agent status used by notifications.
    pub fn shows_busy_marker(&self) -> bool {
        self.footer_signals().busy
    }

    /// Whether the footer shows a question/decision prompt — a positive
    /// "blocked on your input" signal.
    pub fn shows_input_prompt(&self) -> bool {
        self.footer_signals().prompt
    }

    /// Read both footer signals in one pass. Both need the rendered grid, and
    /// every caller wants both, so rendering once per look keeps the per-tick
    /// scan cheap.
    pub fn footer_signals(&self) -> FooterSignals {
        if self.exit_status.is_some() {
            return FooterSignals::default();
        }
        let lines = self.footer_lines();
        // A dialog's own hint line is chrome, not work, even when it offers
        // "esc to cancel" — the phrase Gemini uses for *interrupt*. Reading that
        // half of `Enter to select · ↑/↓ to navigate · Esc to cancel` as a
        // working marker is what pinned Claude Code's question dialog to
        // "working" for as long as the question went unanswered.
        // The dialog-chrome filter is about *which lines* can carry a working
        // marker, not which table supplies one, so an override inherits it.
        let busy = match self.detect.busy.as_ref() {
            Some(re) => lines.iter().filter(|l| !prompt_chrome(l)).any(|l| re.is_match(l)),
            None => lines.iter().filter(|l| !prompt_chrome(l)).any(|l| {
                BUSY_MARKERS.iter().any(|m| l.contains(m))
                    || BUSY_LINE_STARTS.iter().any(|m| strip_gutter(l).starts_with(m))
            }),
        };
        // A configured waiting pattern speaks for the whole signal: the shape
        // rules below exist to make generic question wording safe, and an agent
        // specific enough to warrant an override does not need them.
        // Questions are read from the last *written* rows rather than the bottom
        // of the grid: a dialog on a screen that has not filled up yet is
        // painted at the top, and there is nothing under it. See
        // [`Self::dialog_lines`] for why the busy scan above keeps the grid.
        let dialog = self.dialog_lines();
        let prompt = match self.detect.waiting.as_ref() {
            Some(re) => dialog.iter().any(|l| re.is_match(l)),
            None => {
                // Prompt chrome and a highlighted menu entry are unambiguous. A
                // plain question sentence is not — an agent writes those in its
                // answers too — so it needs a numbered option list under it,
                // and it loses to a working marker (mid-turn prose, not a
                // dialog).
                let chrome = dialog.iter().any(|l| prompt_chrome(l))
                    || dialog.iter().any(|l| selected_option(l));
                let asked = dialog.iter().any(|l| QUESTION_MARKERS.iter().any(|m| l.contains(m)))
                    && dialog.iter().any(|l| numbered_option(l));
                chrome || (asked && !busy)
            }
        };
        FooterSignals { busy, prompt }
    }

    /// Keep [`Self::footer_lines`] on the live screen while the view is parked
    /// in the scrollback.
    ///
    /// The footer scan reads the *visible* grid, and once someone has scrolled
    /// back that grid is what they are reading rather than what the agent is
    /// doing now: a "may I proceed?" from an hour ago would read as a live
    /// question and ring for it, while the turn actually running underneath
    /// went unseen. So take a copy of the live band whenever the pair (output,
    /// offset) changes and the view is not live. Only a pane someone is
    /// scrolling through pays for it — parked panes are rare and there is at
    /// most one at a time, which is why this is a copy rather than a second
    /// emulator or a `&mut` threaded through every DTO the daemon builds.
    fn sync_parked_footer(&mut self) {
        if self.emulator.scroll_offset() == 0 {
            self.parked_footer = None;
            return;
        }
        // `text_rows` reads at offset 0 and restores the view, which is exactly
        // the live band the scan wants; `false` means "no scrollback", so what
        // comes back is the bottom of the live screen.
        let want = usize::from(FOOTER_SCAN_ROWS);
        let (rows, _) = self.emulator.text_rows(want, false, RowFormat::Text);
        self.parked_footer = Some(rows.iter().map(|l| l.trim_end().to_lowercase()).collect());
    }

    /// The footer band — the last [`FOOTER_SCAN_ROWS`] rows of the visible grid,
    /// lowercased, where agent status/prompt lines live. Scanning only the
    /// footer keeps the same phrases in the agent's response body from matching.
    fn footer_lines(&self) -> Vec<String> {
        // The live band, when the visible grid is not it (see
        // [`Self::sync_parked_footer`]).
        if let Some(parked) = &self.parked_footer {
            return parked.clone();
        }
        let area = Rect::new(0, 0, self.cols, self.rows);
        let mut buf = Buffer::empty(area);
        self.emulator.render_into(&mut buf, area);
        let top = area.bottom().saturating_sub(FOOTER_SCAN_ROWS).max(area.y);
        (top..area.bottom())
            .map(|y| {
                let mut line = String::new();
                for x in area.x..area.right() {
                    line.push_str(buf[(x, y)].symbol());
                }
                line.trim_end().to_lowercase()
            })
            .collect()
    }

    /// The rows the *question* scan reads: the last [`FOOTER_SCAN_ROWS`] rows
    /// with anything on them, wherever they sit on the grid. Same band as
    /// [`Self::footer_lines`] on a screen that has filled up, which is every
    /// agent screen after its first few seconds.
    ///
    /// A dialog is chrome, and chrome is painted wherever the screen currently
    /// ends — on a fresh one, at the top. `agy` opens every session that way:
    /// its folder-trust question sits in the top half of an 80x24 pane with
    /// eleven blank rows under it, so the grid-anchored band read nothing at all
    /// and the rail said "idle" while the agent waited for an answer.
    ///
    /// The busy scan deliberately does not get this. A spinner belongs to the
    /// live bottom of the screen, while the same phrase in an agent's own output
    /// is prose — and reading prose as a working marker pins the pane to busy
    /// for good, swallowing the finished notification with it. The two failures
    /// are not the same size: a question noticed one screen early rings once and
    /// you look at the pane; a turn that never ends is silent forever.
    fn dialog_lines(&self) -> Vec<String> {
        // A parked view is not the live screen, so the same substitution
        // [`Self::footer_lines`] makes applies here — trimmed to its written
        // rows, which is all this band ever asks of the grid.
        if let Some(parked) = &self.parked_footer {
            let (start, end) = footer_window(parked);
            return parked[start..end].to_vec();
        }
        let area = Rect::new(0, 0, self.cols, self.rows);
        let mut buf = Buffer::empty(area);
        self.emulator.render_into(&mut buf, area);
        let rows: Vec<String> = (area.y..area.bottom())
            .map(|y| {
                let mut line = String::new();
                for x in area.x..area.right() {
                    line.push_str(buf[(x, y)].symbol());
                }
                line.trim_end().to_lowercase()
            })
            .collect();
        let (start, end) = footer_window(&rows);
        rows[start..end].to_vec()
    }

    /// Rendered output as text, for `GET .../panes/{pane}/output`.
    ///
    /// A *query*. Unlike [`Self::acknowledge`] or a framed attach, it neither
    /// clears the pending bell nor resizes the pane, so a script can poll a
    /// sibling without perturbing it — or the state machine watching it.
    ///
    /// `want` is a maximum; the `bool` is `true` when older lines exist that
    /// were not returned.
    pub fn text_output(
        &mut self,
        want: usize,
        scrollback: bool,
        footer_only: bool,
        format: RowFormat,
    ) -> (Vec<String>, bool) {
        // The footer band is exactly what `footer_signals` scans, so a caller
        // can see what the detector saw — minus the lowercasing it applies
        // internally. Sharing [`footer_window`] rather than re-deriving the
        // bounds is what keeps the two in step: the band ends at the last
        // written row, so taking the bottom eight rows here would answer "what
        // did the detector see?" with rows it never looked at.
        // `screen` means the whole viewport, and `footer` the detector's band;
        // only a scrollback read is bounded by what the caller asked for.
        if footer_only {
            let all = usize::from(self.rows);
            let (text, _) = self.emulator.text_rows(all, false, RowFormat::Text);
            let (start, end) = footer_window(&text);
            // The window is indices into one screen's rows, so any format can
            // be sliced with it — `--format ansi` must not shift the band.
            let rows = match format {
                RowFormat::Text => text,
                _ => self.emulator.text_rows(all, false, format).0,
            };
            return (rows[start..end].to_vec(), start > 0);
        }
        let want = match scrollback {
            false => usize::from(self.rows),
            true => want,
        };
        self.emulator.text_rows(want, scrollback, format)
    }

    /// Whether a full-screen application (vim, htop) currently owns the pane.
    pub fn alternate_screen(&self) -> bool {
        self.emulator.alternate_screen()
    }

    pub fn exit_code(&self) -> Option<u32> {
        self.exit_status
    }

    /// The raw OSC title the application set (independent of the fixed
    /// label), for the agents rail's live activity display.
    pub fn osc_title(&self) -> String {
        self.emulator.title()
    }

    /// Full command line of the tty's current foreground process group leader,
    /// e.g. "sudo apt-get update -y". `None` when the child has exited, when
    /// the foreground process is just the login shell sitting at its prompt,
    /// or when the platform lookup fails. Lets a shell row say what is running
    /// in it rather than a bare "shell".
    ///
    /// Probes are cached for [`FG_TTL`]: the rail repaints every animation tick
    /// while anything marquees, and each probe is a syscall.
    pub fn foreground_cmdline(&mut self) -> Option<String> {
        if self.exit_status.is_some() {
            return None;
        }
        if let Some((at, cached)) = &self.fg_cache {
            if at.elapsed() < FG_TTL {
                return cached.clone();
            }
        }
        // `process_group_leader` yields a `libc::pid_t`, which is `i32` on every
        // target with a `read_argv` body — naming the libc type here would break
        // Linux, where butai-server has no libc dependency.
        let fresh = self
            .master
            .process_group_leader()
            .filter(|pgid| !is_own_group(*pgid))
            .and_then(read_argv)
            .filter(|argv| !is_self(argv))
            .and_then(display_argv);
        self.fg_cache = Some((std::time::Instant::now(), fresh.clone()));
        fresh
    }

    /// The `ssh` invocation running in this pane, as arguments to reuse.
    ///
    /// This is what makes the handoff need no configuration: the pane already
    /// holds a working way to reach that machine — the command the user typed —
    /// so we dial back with *their* flags (`-p`, `-i`, `-J`, a `~/.ssh/config`
    /// alias) rather than guessing from a hostname. Returns everything after
    /// `ssh` up to the destination, and the destination.
    ///
    /// `None` when the foreground process is not ssh, which is the case worth
    /// falling back for: the announcement carries a `user@host` hint too.
    pub fn ssh_dial_back(&self) -> Option<(Vec<String>, String)> {
        let argv = self.master.process_group_leader().and_then(read_argv)?;
        let program = std::path::Path::new(argv.first()?).file_name()?.to_str()?;
        if program != "ssh" {
            return None;
        }
        split_ssh_argv(&argv[1..])
    }

    /// Whether the inner application asked for mouse reporting.
    pub fn wants_mouse(&self) -> bool {
        self.emulator.mouse_active()
    }

    /// Forward a mouse event to the application as SGR sequences
    /// (pane-relative 0-based coordinates).
    pub fn send_mouse(&mut self, kind: MouseKind, x: u16, y: u16) {
        let (col, row) = (x + 1, y + 1);
        let bytes = match kind {
            MouseKind::Click => format!("\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m").into_bytes(),
            MouseKind::WheelUp => format!("\x1b[<64;{col};{row}M").into_bytes(),
            MouseKind::WheelDown => format!("\x1b[<65;{col};{row}M").into_bytes(),
        };
        let _ = self.writer.write_all(&bytes);
        let _ = self.writer.flush();
    }

    pub fn handle_input(&mut self, ev: &InputEvent) {
        let modes = self.emulator.modes();
        let bytes = match ev {
            InputEvent::Key(key) => encode_key(key, modes),
            InputEvent::Paste(text) => encode_paste(text, modes),
            InputEvent::ScrollUp { .. } => {
                self.scroll_lines(-WHEEL_LINES);
                return;
            }
            InputEvent::ScrollDown { .. } => {
                self.scroll_lines(WHEEL_LINES);
                return;
            }
            InputEvent::MouseDown { .. }
            | InputEvent::MouseDrag { .. }
            | InputEvent::MouseUp { .. } => return,
        };
        if !bytes.is_empty() {
            // Typing is aimed at the live screen, so it is also the gesture
            // that leaves the scrollback — the counterpart to output no longer
            // doing it (see [`Self::feed_output`]). Keyed off the encoded
            // bytes rather than the event, so a key the program never receives
            // does not move the view either.
            self.scroll_to_live();
            let _ = self.writer.write_all(&bytes);
            let _ = self.writer.flush();
        }
    }

    /// Scroll the view; negative = toward older output.
    pub fn scroll_lines(&mut self, delta: i32) {
        // Emulator offset counts lines back from live, so invert.
        self.emulator.scroll_view(-delta);
        self.sync_parked_footer();
    }

    /// Put the view back on the live screen.
    pub fn scroll_to_live(&mut self) {
        self.emulator.scroll_reset();
        self.parked_footer = None;
    }

    pub fn scroll_page(&mut self, pages: i16) {
        let page = self.rows.saturating_sub(1).max(1) as i32;
        self.scroll_lines(pages as i32 * page);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 || (rows == self.rows && cols == self.cols) {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        self.emulator.resize(rows, cols);
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        // A resize reshapes the band the scan reads, so the copy taken at the
        // old size is no longer the live footer.
        self.sync_parked_footer();
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        self.emulator.render_into(buf, area);
    }

    pub fn cursor(&self) -> Option<(u16, u16)> {
        if self.exit_status.is_some() {
            return None;
        }
        self.emulator.cursor()
    }

    pub fn title(&self) -> String {
        let osc = self.emulator.title();
        let base =
            if self.label_fixed || osc.is_empty() { self.command_label.clone() } else { osc };
        match self.exit_status {
            Some(0) => format!("{base} [exited]"),
            Some(code) => format!("{base} [exited {code}]"),
            None => base,
        }
    }

    /// Title for an agent row. Agents (Claude &c.) continuously rewrite their
    /// OSC title to reflect the current task/state, so prefer that live title
    /// over the static command label; fall back to the label when the program
    /// hasn't set one yet. This mirrors what the terminal agents rail shows.
    pub fn agent_title(&self) -> String {
        let osc = self.emulator.title();
        let base = if osc.trim().is_empty() { self.command_label.clone() } else { osc };
        match self.exit_status {
            Some(0) => format!("{base} [exited]"),
            Some(code) => format!("{base} [exited {code}]"),
            None => base,
        }
    }

    pub fn mark_exited(&mut self, status: u32) {
        self.exit_status = Some(status);
    }

    pub fn is_dead(&self) -> bool {
        self.exit_status.is_some()
    }

    pub fn scroll_offset(&self) -> usize {
        self.emulator.scroll_offset()
    }
}

/// A monotonic `Instant`, as unix epoch millis.
///
/// An `Instant` has no epoch of its own, so the conversion needs a reference
/// pair — one `Instant` and one `SystemTime` sampled together. That pair is
/// taken **once** per process and reused, so a given `Instant` always converts
/// to the same number.
///
/// The obvious alternative, `now_epoch - t.elapsed()`, is *nearly* stable — both
/// clocks advance together, so the subtraction mostly cancels — but the two
/// reads happen at slightly different moments and both truncate to
/// milliseconds, so it lands a millisecond out perhaps one call in five. That
/// matters because these timestamps ride in `AgentDto`, and the event stream
/// pushes a workspace only when its detail differs from the last one sent: a
/// field that changes on its own makes a workspace look modified when nothing
/// happened. Measured, it is extra pushes rather than a flood — the daemon only
/// diffs when something has marked it dirty — but a value that is not equal to
/// itself is the wrong primitive to build a change-detecting stream on.
///
/// Pinning the base also means a wall-clock adjustment cannot make an agent
/// appear to have started in the future.
fn epoch_ms_from(t: std::time::Instant) -> u64 {
    use std::sync::OnceLock;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};
    static BASE: OnceLock<(Instant, u64)> = OnceLock::new();
    let (base_instant, base_ms) = *BASE.get_or_init(|| {
        let ms =
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        (Instant::now(), ms)
    });
    if t >= base_instant {
        base_ms.saturating_add(t.duration_since(base_instant).as_millis() as u64)
    } else {
        base_ms.saturating_sub(base_instant.duration_since(t).as_millis() as u64)
    }
}

impl Drop for TerminalPane {
    fn drop(&mut self) {
        if self.exit_status.is_none() {
            let _ = self.killer.kill();
        }
    }
}

/// The directories a login shell would have put on `PATH`, which a daemon's
/// inherited environment usually has not.
///
/// The daemon inherits its environment from whatever started it — a desktop
/// session, a systemd unit, an ssh command, the first client to auto-spawn it.
/// That is rarely the login shell the user installed their tools from, and the
/// most common install locations are invisible without one: `cargo
/// install`/pipx land in `~/.local/bin`, and an npm-installed `claude` lands
/// under whichever `~/.nvm/versions/node/*/bin` nvm's shell hook selects.
///
/// Returned in two groups because nvm is not like the others: it keeps one
/// directory per installed node version and a `PATH` may name only one of them,
/// so [`child_path`] has to treat the set as a unit rather than as three more
/// directories. Newest version first, so a machine with several gets the one
/// most likely to be current.
fn login_bin_dirs() -> (Vec<PathBuf>, Vec<PathBuf>) {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return (Vec::new(), Vec::new());
    };
    let user = vec![home.join(".local/bin"), home.join(".bun/bin"), home.join("bin")];
    let mut nvm = Vec::new();
    if let Ok(entries) = std::fs::read_dir(home.join(".nvm/versions/node")) {
        let mut versions: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        versions.sort();
        nvm.extend(versions.into_iter().rev().map(|v| v.join("bin")));
    }
    (user, nvm)
}

/// Find `prog` when the daemon's own `PATH` cannot.
///
/// The failure this exists for is confusing rather than obviously environmental
/// — the agent works when the user runs it by hand, and the daemon insists it
/// does not exist — and because a missing agent fails again on every session
/// restore, it reads as permanent breakage rather than a stale daemon.
///
/// So: try `PATH` first and change nothing when it works, then fall back to
/// [`login_bin_dirs`]. This is the same reasoning as `butai-connect`'s
/// `REMOTE_CMD`, which resolves the far-side `butai` by checking `~/.local/bin`
/// before `command -v` for exactly this reason.
///
/// Finding the launcher is only half of it: whatever it needs to *run* has to be
/// on the child's `PATH` too, which is [`child_path`]'s job.
///
/// Shared with the usage sampler rather than copied, because the two answering
/// differently is the bug: a USAGE page that reports `claude` absent while the
/// AGENTS rail launches it happily describes a machine nobody is running.
///
/// Returns an absolute path when the fallback finds one, otherwise `prog`
/// unchanged so the caller's error names what was asked for.
pub(crate) fn resolve_program(prog: &str) -> String {
    // A path, relative or absolute, is the user being explicit. Don't second-guess.
    if prog.contains('/') {
        return prog.to_string();
    }
    if which_on_path(prog).is_some() {
        return prog.to_string();
    }
    let (user, nvm) = login_bin_dirs();
    for dir in user.into_iter().chain(nvm) {
        let candidate = dir.join(prog);
        if is_executable_file(&candidate) {
            return candidate.to_string_lossy().into_owned();
        }
    }
    prog.to_string()
}

/// The `PATH` a pane's child is given, or `None` to pass the daemon's own along
/// untouched.
///
/// [`resolve_program`] finds an agent the daemon's `PATH` cannot — and then the
/// child still cannot run it, because an npm-installed CLI is a
/// `#!/usr/bin/env node` script and the first thing it does is look `node` up on
/// the `PATH` it was handed. With nvm's directory missing that is the
/// distribution's `node`, which is routinely years older than the one the CLI
/// was built for: on the machine this was found on, `/usr/bin/node` is v10 and
/// the agent dies on a syntax error before printing anything of its own. It
/// reads as "the agent is broken", not as "the daemon's `PATH` is short".
///
/// The same gap is what breaks a managed process. `[[processes]]` runs through
/// `$SHELL -c`, and a non-interactive shell sources none of the files that put
/// `~/.local/bin` or nvm on `PATH` — so `npm run dev` in a `.butai.toml` cannot
/// find `npm`, even though the identical line works when typed into a pane,
/// where the shell is interactive and has read its rc file.
///
/// So the daemon puts the directories a login shell would have added in front of
/// what it inherited — but only the ones that exist and are not already there. A
/// daemon started from a login shell gets its `PATH` back byte for byte, which
/// is the case worth protecting: this must add what is missing and never reorder
/// what is present.
///
/// In front rather than behind because that is where a login shell puts them
/// (nvm's hook prepends; `~/.profile` prepends `~/.local/bin`) and because
/// behind would fix nothing — the failure above is a stale `/usr/bin/node`
/// winning a lookup it would not have won in the user's own shell.
///
/// The usage sampler's `--version` probe is a child of exactly this kind and
/// takes the same `PATH`: `gemini` is a `#!/usr/bin/env node` script, so
/// probing it on the daemon's inherited `PATH` asks a decade-old node to parse
/// a file with top-level `await` in it, and the version comes back blank for a
/// CLI that is installed and working.
pub(crate) fn child_path() -> Option<std::ffi::OsString> {
    let inherited = std::env::var_os("PATH")?;
    let present: Vec<PathBuf> = std::env::split_paths(&inherited).collect();
    let (user, nvm) = login_bin_dirs();
    let mut extra: Vec<PathBuf> =
        user.into_iter().filter(|d| !present.contains(d) && d.is_dir()).collect();
    // nvm is all-or-nothing. A `PATH` already naming a version was written by
    // the user's own hook and picks the node they meant; adding the versions it
    // left out would put an older one in front of it, which is this function's
    // own failure mode turned back on itself.
    if !nvm.iter().any(|d| present.contains(d)) {
        extra.extend(nvm.into_iter().find(|d| d.is_dir()));
    }
    if extra.is_empty() {
        return None;
    }
    std::env::join_paths(extra.into_iter().chain(present)).ok()
}

/// Whether `prog` resolves against the current `PATH`.
fn which_on_path(prog: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(prog)).find(|p| is_executable_file(p))
}

fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    // Follows symlinks, so an nvm shim pointing at a real binary still counts,
    // and a dangling one correctly does not.
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Whether `pgid` is our own process group rather than a pane's child.
///
/// There is a window between forking a pane's child and that child claiming the
/// terminal, and inside it `tcgetpgrp` still answers with the group that opened
/// the pty — ours. Reading `/proc` for that pid then describes *us*, and the
/// rail would briefly label a pane with whatever this process happens to be
/// called. [`is_self`] was meant to catch that by name, but it only sees a name:
/// under a test harness the kernel's accounting name is the thread's, so the
/// check missed and the rail showed a thread name. Comparing the group id is
/// exact and needs no guessing.
fn is_own_group(pgid: i32) -> bool {
    // `getpgrp` cannot fail and takes no argument on any Unix.
    pgid == rustix::process::getpgrp().as_raw_nonzero().get()
}

/// Whether `argv` is really the butai process itself rather than a command
/// running in the pane.
///
/// A pty child that has forked but not yet `exec`'d still carries its parent's
/// identity, and that window is wide enough to lose a race with the first
/// render. Without this guard a freshly opened pane briefly labels its row with
/// butai's own name, and the [`FG_TTL`] cache then holds that wrong label on
/// screen for half a second. Matching on the executable's file name catches
/// both the full-argv read and the short accounting-name fallback.
fn is_self(argv: &[String]) -> bool {
    static EXE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let exe = EXE.get_or_init(|| {
        let path = std::env::current_exe().ok()?;
        Some(path.file_name()?.to_string_lossy().into_owned())
    });
    let (Some(exe), Some(arg0)) = (exe.as_deref(), argv.first()) else {
        return false;
    };
    let base = Path::new(arg0).file_name().map(|n| n.to_string_lossy().into_owned());
    // The kernel truncates its accounting name (16 chars on macOS), so a lone
    // long prefix counts too — but not a short one, or a program actually named
    // `b` would be mistaken for a truncated `butai`.
    base.is_some_and(|b| b == exe || (argv.len() == 1 && b.len() >= 8 && exe.starts_with(&b)))
}

/// Upper bound on the argv blob read back from the kernel. Linux allows a
/// cmdline up to ARG_MAX (megabytes); the rail is 28 columns wide, so reading
/// the whole thing twice a second would be pure waste.
const ARGV_READ_MAX: usize = 16 * 1024;

/// argv of `pid`, or `None` when the platform will not say. The syscall is kept
/// apart from the parsing so the parsing stays pure and testable.
///
/// A process that gained privilege is no longer dumpable, and the kernel denies
/// its `cmdline` to an unprivileged reader — an empty read, not an error. `comm`
/// is not gated that way, so [`proc_comm`] still names the row.
#[cfg(target_os = "linux")]
fn read_argv(pid: i32) -> Option<Vec<String>> {
    let mut buf = Vec::new();
    let read = std::fs::File::open(format!("/proc/{pid}/cmdline"))
        .and_then(|f| f.take(ARGV_READ_MAX as u64).read_to_end(&mut buf));
    let argv = if read.is_ok() { split_nul(&buf) } else { Vec::new() };
    if !argv.is_empty() {
        return Some(argv);
    }
    // An empty `cmdline` has a second cause that looks identical: a child
    // between `fork` and `exec` has no argv yet, and — this is the trap — its
    // `comm` is still the *forking thread's*, inherited across the fork. Every
    // freshly spawned pane passes through that window, so falling back there
    // named panes after whatever thread happened to spawn them.
    //
    // The two are told apart by ownership. The privilege case is a process that
    // is no longer ours to read; a child mid-exec is still running as us.
    if proc_uid(pid) == Some(rustix::process::getuid().as_raw()) {
        return None;
    }
    proc_comm(pid).map(|n| vec![n])
}

/// The real uid `pid` runs as, from the owner of its `/proc` directory.
#[cfg(target_os = "linux")]
fn proc_uid(pid: i32) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(format!("/proc/{pid}")).ok().map(|m| m.uid())
}

/// The kernel's short name for `pid`, readable even when its argv is not.
#[cfg(target_os = "linux")]
fn proc_comm(pid: i32) -> Option<String> {
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// macOS has no `/proc`. `KERN_PROCARGS2` returns the top of the process's
/// stack (argc, exec path, argv, envp) — but only for a process whose effective
/// uid matches ours, so anything running with privilege refuses. That case falls
/// back to naming the executable, which is exactly what Linux's
/// `/proc/<pid>/comm` gives this rail.
#[cfg(target_os = "macos")]
fn read_argv(pid: i32) -> Option<Vec<String>> {
    let mut buf = vec![0u8; procargs_buf_len()];
    let mut len = buf.len();
    let mut mib: [libc::c_int; 3] = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 {
        buf.truncate(len);
        if let Some(argv) = parse_procargs2(&buf) {
            return Some(argv);
        }
    }
    exec_name(pid).or_else(|| proc_name(pid)).map(|n| vec![n])
}

/// Name of the executable `pid` is running, from the one interface the kernel
/// will answer about a process that is not ours: `proc_pidpath`.
///
/// The obvious fallback, libproc's `proc_name`, is refused (EPERM) for exactly
/// the processes whose argv is also refused — a setuid program runs with an
/// effective uid of root, and `/usr/bin/top` is setuid on macOS. With only
/// `proc_name` to fall back on, a pane running `top` reported nothing at all and
/// its row kept the generic `shell` label. `proc_pidpath` answers for any pid,
/// and its name is not clipped to the kernel's 16-char accounting limit.
#[cfg(target_os = "macos")]
fn exec_name(pid: i32) -> Option<String> {
    let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    let path = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
    Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned())
}

/// `kern.argmax`, read once and clamped — it is fixed for the life of the
/// kernel. A buffer shorter than argmax is fine: `KERN_PROCARGS2` copies out
/// what fits, which is how `ps` works.
#[cfg(target_os = "macos")]
fn procargs_buf_len() -> usize {
    static ARGMAX: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *ARGMAX.get_or_init(|| {
        crate::sys::sysctl_u64("kern.argmax")
            .map(|v| (v as usize).min(ARGV_READ_MAX))
            .unwrap_or(ARGV_READ_MAX)
    })
}

/// The kernel's short accounting name for `pid` (libproc lives in libSystem, so
/// this needs no extra link flags). A backstop for [`exec_name`] on a process
/// whose executable path the kernel will not resolve.
#[cfg(target_os = "macos")]
fn proc_name(pid: i32) -> Option<String> {
    let mut buf = [0u8; 64];
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    let name = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
    (!name.is_empty()).then_some(name)
}

/// Platforms with neither `/proc` nor `KERN_PROCARGS2`: a shell row simply keeps
/// its configured name.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_argv(_pid: i32) -> Option<Vec<String>> {
    None
}

/// Pull argv out of a `KERN_PROCARGS2` buffer, whose layout is: a native-endian
/// `i32` argc; the exec path, NUL-terminated; NUL padding; then exactly argc
/// NUL-terminated argv strings (the environment follows, and is ignored).
/// Tolerates a tail cut short by [`ARGV_READ_MAX`] — the complete arguments are
/// kept. Compiled in on every platform so its tests run everywhere.
#[cfg(any(target_os = "macos", test))]
fn parse_procargs2(buf: &[u8]) -> Option<Vec<String>> {
    let argc = usize::try_from(i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?)).ok()?;
    let rest = buf.get(4..)?;
    let after_path = rest.iter().position(|b| *b == 0)? + 1;
    let start = after_path + rest[after_path..].iter().take_while(|b| **b == 0).count();
    let mut tail = rest.get(start..)?;
    let mut argv = Vec::with_capacity(argc);
    for _ in 0..argc {
        // Each argument must be NUL-terminated; an unterminated tail is a
        // buffer cut short by ARGV_READ_MAX, so stop rather than keep a
        // half-argument.
        let Some(end) = tail.iter().position(|b| *b == 0) else { break };
        argv.push(String::from_utf8_lossy(&tail[..end]).into_owned());
        tail = &tail[end + 1..];
    }
    (!argv.is_empty()).then_some(argv)
}

/// Split a NUL-separated blob into strings, dropping empties (notably the one
/// the trailing NUL would otherwise produce). Compiled in on every platform so
/// its tests run everywhere; only the Linux reader uses it.
#[cfg(any(target_os = "linux", test))]
fn split_nul(raw: &[u8]) -> Vec<String> {
    raw.split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// Basenames that mean "a shell sitting at its prompt" rather than a command
/// worth naming a row after.
const LOGIN_SHELLS: &[&str] = &["bash", "zsh", "sh", "fish", "dash", "tcsh", "ksh"];

/// Render argv as a rail label, or `None` when it is just the login shell
/// waiting at its prompt — nothing worth renaming the row for.
///
/// A login shell spells argv[0] "-zsh" while ours is the configured path
/// ("/bin/sh"), so both the dash and the directory are stripped before
/// matching. Only a shell invoked with nothing but flags counts as idle:
/// `zsh build.sh` and `sh -c 'make'` are real commands.
fn display_argv(argv: Vec<String>) -> Option<String> {
    let arg0 = argv.first()?;
    let base = Path::new(arg0)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| arg0.clone());
    let flags_only = argv[1..].iter().all(|a| a.starts_with('-'));
    if LOGIN_SHELLS.contains(&base.trim_start_matches('-')) && flags_only {
        return None;
    }
    let out: String = argv.join(" ").chars().take(FG_MAX).collect();
    (!out.trim().is_empty()).then_some(out)
}

fn shell_label(shell: &str) -> String {
    Path::new(shell)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| shell.to_string())
}

/// Spawning these threads is the one step that fails for a reason outside our
/// control: every pane costs two OS threads, so a container's `--pids-limit`
/// (or `RLIMIT_NPROC`) eventually refuses. That has to surface as an error on
/// the one pane being created — panicking here runs inside the core actor's
/// task and takes the whole daemon's core with it, so every *other* workspace,
/// agent and client dies too, and the API starts answering "core dropped the
/// reply".
fn spawn_reader_thread(
    id: PaneId,
    mut reader: Box<dyn Read + Send>,
    output: OutputTx,
) -> Result<()> {
    std::thread::Builder::new()
        .name(format!("pty-read-{id}"))
        .spawn(move || {
            let mut buf = vec![0u8; READ_CHUNK];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        // The output channel is bounded: when the core falls
                        // behind, this `blocking_send` parks the reader thread,
                        // which stops draining the PTY, fills the kernel pipe
                        // buffer, and finally throttles the child. That
                        // backpressure caps CPU and memory under a flood
                        // instead of growing an unbounded queue that would
                        // starve control events (kill-server, input).
                        if output.blocking_send((id, buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        })
        .context("spawn pty reader thread")?;
    Ok(())
}

fn spawn_wait_thread(
    id: PaneId,
    mut child: Box<dyn Child + Send + Sync>,
    tx: UnboundedSender<Event>,
) -> Result<()> {
    std::thread::Builder::new()
        .name(format!("pty-wait-{id}"))
        .spawn(move || {
            let code = match child.wait() {
                Ok(status) => status.exit_code(),
                Err(_) => 1,
            };
            let _ = tx.send(Event::PaneExited(id, code));
        })
        .context("spawn pty wait thread")?;
    Ok(())
}

/// Where [`HvpRewriter`] is in a CSI sequence. Carried between reads because a
/// sequence can straddle two PTY chunks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum CsiState {
    /// Ordinary output.
    #[default]
    Ground,
    /// Saw `ESC`; the next byte decides whether a CSI opens.
    Esc,
    /// Inside a CSI, collecting parameter/intermediate bytes.
    Csi,
}

/// Rewrites HVP (`CSI row;col f`) to CUP (`CSI row;col H`) in the PTY stream.
///
/// ECMA-48 gives the two the same meaning, but vt100's `csi_dispatch` has an
/// `'H'` arm and no `'f'` arm, so it drops every HVP on the floor with nothing
/// but a debug log. An app that positions with HVP therefore has all of its
/// seeks discarded and paints wherever the cursor happened to land. btop is the
/// one in our matrix that does this — it uses HVP *exclusively*, ~700 seeks per
/// frame — and renders as overlapping soup without this pass.
///
/// The swap is a single byte and length-preserving, which is what lets it run
/// before [`terminal_queries`] without invalidating any of the offsets that
/// scanner reports.
#[derive(Debug, Clone, Copy, Default)]
struct HvpRewriter {
    state: CsiState,
    /// Whether the CSI in progress carries only `0-9`/`;` parameters. HVP takes
    /// no private markers or intermediates, so a `?`, `>`, `<`, `=` or an
    /// intermediate byte clears this and the sequence is left alone.
    plain: bool,
}

impl HvpRewriter {
    /// Rewrite in place, returning how many sequences were converted.
    fn rewrite(&mut self, buf: &mut [u8]) -> usize {
        let mut converted = 0;
        let mut i = 0;
        while i < buf.len() {
            match self.state {
                // Output is overwhelmingly ESC-free, so jump to the next one
                // rather than walking byte by byte. This is the fast path that
                // keeps the pass off the profile for ordinary panes.
                CsiState::Ground => match memchr::memchr(0x1b, &buf[i..]) {
                    Some(off) => {
                        self.state = CsiState::Esc;
                        i += off + 1;
                    }
                    None => break,
                },
                CsiState::Esc => {
                    match buf[i] {
                        b'[' => {
                            self.state = CsiState::Csi;
                            self.plain = true;
                        }
                        // `ESC ESC` restarts the escape; the second one is
                        // still an introducer, so stay put and re-examine.
                        0x1b => {}
                        _ => self.state = CsiState::Ground,
                    }
                    i += 1;
                }
                CsiState::Csi => {
                    let b = buf[i];
                    if (0x40..=0x7e).contains(&b) {
                        if b == b'f' && self.plain {
                            buf[i] = b'H';
                            converted += 1;
                        }
                        self.state = CsiState::Ground;
                    } else if !(b.is_ascii_digit() || b == b';') {
                        self.plain = false;
                    }
                    i += 1;
                }
            }
        }
        converted
    }
}

/// A terminal query the child blocks on until answered.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Query {
    /// DSR cursor position (`ESC[6n`).
    CursorPos,
    /// DSR terminal status (`ESC[5n`).
    Status,
    /// Primary device attributes (`ESC[c` / `ESC[0c`).
    PrimaryDa,
    /// Secondary device attributes (`ESC[>c` / `ESC[>0c`).
    SecondaryDa,
    /// `ESC _ butai;here;<hint>;<socket> ESC \` — a butai at the far end of an
    /// ssh session running in this pane, telling us it is there.
    ///
    /// Not a query: there is nothing to write back. It rides the query scanner
    /// because that is already the one place every byte a pane emits is walked
    /// for escape sequences, with the carry that makes a sequence split across
    /// two PTY reads still parse.
    RemoteAnnounce { hint: String, socket: String },
}

impl Query {
    /// The bytes to write back. `cursor` is the emulator's `(row, col)` at the
    /// point in the output stream where the query appeared.
    fn reply(&self, cursor: (u16, u16)) -> Vec<u8> {
        match self {
            // Reported 1-based.
            Query::CursorPos => format!("\x1b[{};{}R", cursor.0 + 1, cursor.1 + 1).into_bytes(),
            Query::Status => b"\x1b[0n".to_vec(),
            Query::PrimaryDa => b"\x1b[?1;2c".to_vec(),
            // `98` is `b`, identifying butai the way tmux identifies itself with
            // `84` (`T`). This is load-bearing beyond politeness: it is how a
            // `butai` started on the far end of an ssh session inside one of our
            // panes discovers that it is inside one — DA2 is answered by every
            // terminal, so the far side gets a prompt yes *or* no rather than
            // waiting out a timeout. See `butai/src/handoff.rs`.
            Query::SecondaryDa => {
                format!("\x1b[>{DA2_BUTAI_ID};{};0c", butai_protocol::PROTOCOL_VERSION).into_bytes()
            }
            Query::RemoteAnnounce { .. } => Vec::new(),
        }
    }
}

/// The `Pp` field butai reports in its Secondary DA response: `b`.
pub const DA2_BUTAI_ID: u32 = 98;

/// Scan PTY output (prefixed by any `carry` from a previous read) for terminal
/// queries the child expects an answer to. Returns each query paired with the
/// index **into `bytes`** just past it, so the caller can feed the emulator up
/// to that point before sampling the cursor. `carry` is replaced with any
/// trailing incomplete escape so a query split across two reads is still
/// recognized next time.
///
/// Answered: DSR cursor-position (`ESC[6n`) and status (`ESC[5n`), primary
/// (`ESC[c` / `ESC[0c`) and secondary (`ESC[>c` / `ESC[>0c`) device attributes.
/// Everything else is left to the emulator.
fn terminal_queries(carry: &mut Vec<u8>, bytes: &[u8]) -> Vec<(usize, Query)> {
    // Common case: nothing carried, so scan `bytes` in place with no copy (this
    // runs on every read, including floods). Only when a query straddled the
    // previous read boundary do we join the small carried fragment with `bytes`.
    if carry.is_empty() {
        let (found, tail) = scan_queries(bytes);
        set_carry(carry, &bytes[tail..]);
        found
    } else {
        let skip = carry.len();
        let mut joined = std::mem::take(carry);
        joined.extend_from_slice(bytes);
        let (found, tail) = scan_queries(&joined);
        set_carry(carry, &joined[tail..]);
        // Only `bytes` is fed to the emulator — the carried fragment was fed on
        // the read it arrived on — so shift indices back into `bytes` space. A
        // query completed by this read ends inside `bytes`; one that lay wholly
        // in the carry cannot exist (it would have been reported back then).
        found.into_iter().map(|(end, q)| (end.saturating_sub(skip), q)).collect()
    }
}

/// Scan `buf` for complete terminal queries, returning each query with the
/// index just past it, plus the index where a trailing incomplete escape begins
/// (`buf.len()` if the buffer ends cleanly). vt100 parses these sequences but
/// never answers them, so a program that queries would otherwise block waiting.
fn scan_queries(buf: &[u8]) -> (Vec<(usize, Query)>, usize) {
    let mut found = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        if buf[i] != 0x1b {
            i += 1;
            continue;
        }
        // A lone trailing ESC might begin a CSI split across reads: carry it.
        if i + 1 >= buf.len() {
            return (found, i);
        }
        // APC (`ESC _`), which is where the relay announcement lives.
        if buf[i + 1] == b'_' {
            match scan_apc(&buf[i + 2..]) {
                ApcScan::Complete { end, query } => {
                    i = i + 2 + end;
                    if let Some(query) = query {
                        found.push((i, query));
                    }
                }
                ApcScan::Incomplete => return (found, i),
                // Too long to be ours and still unterminated. Stop holding it:
                // a kitty graphics payload is megabytes of base64, and carrying
                // it would grow the carry buffer without bound waiting for a
                // terminator we do not care about.
                ApcScan::TooLong => i += 2,
            }
            continue;
        }
        if buf[i + 1] != b'[' {
            i += 1;
            continue;
        }
        // CSI: parameter/intermediate bytes up to a final byte (0x40..=0x7e).
        let mut j = i + 2;
        while j < buf.len() && !(0x40..=0x7e).contains(&buf[j]) {
            j += 1;
        }
        if j >= buf.len() {
            // Incomplete CSI at the end of the buffer: carry it for next read.
            return (found, i);
        }
        let query = match (buf[j], &buf[i + 2..j]) {
            (b'n', b"6") => Some(Query::CursorPos),
            (b'n', b"5") => Some(Query::Status),
            (b'c', b"" | b"0") => Some(Query::PrimaryDa),
            (b'c', b">" | b">0") => Some(Query::SecondaryDa),
            _ => None,
        };
        i = j + 1;
        if let Some(query) = query {
            found.push((i, query));
        }
    }
    (found, buf.len())
}

/// ssh options that consume the next argument. Anything else beginning with
/// `-` is a flag, and the first argument that is neither is the destination.
const SSH_VALUE_OPTS: &[char] = &[
    'b', 'c', 'D', 'E', 'e', 'F', 'I', 'i', 'J', 'L', 'l', 'm', 'O', 'o', 'P', 'p', 'Q', 'R', 'S',
    'W', 'w',
];

/// ssh flags that must not be reused when we re-dial.
///
/// `-t`/`-T` decide whether there is a pty, and we need there not to be — the
/// relay speaks a binary protocol over that stdio and a pty would echo and
/// newline-translate it. `-N` and `-f` say "run no command" and "go to the
/// background", and we are dialling precisely in order to run one in the
/// foreground.
const SSH_DROP_FLAGS: &[char] = &['t', 'T', 'N', 'f'];

/// Split an ssh argument list into (flags, destination), dropping the remote
/// command and anything that would fight with how we re-dial.
///
/// Separated from [`TerminalPane::ssh_dial_back`] so it can be tested without a
/// pty: the argument grammar is the fiddly part, not the `/proc` read.
fn split_ssh_argv(args: &[String]) -> Option<(Vec<String>, String)> {
    let mut flags = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let Some(body) = arg.strip_prefix('-') else {
            // First non-option argument: the destination. Everything after it
            // is the remote command, which we replace with our own.
            return Some((flags, arg.clone()));
        };
        // A bare `-` is not an option, and `--` is not ssh syntax; treat both
        // as "stop parsing" rather than guess.
        if body.is_empty() {
            return None;
        }
        // `-p2222` and `-p 2222` are both legal, and only the second consumes
        // the following argument.
        let takes_value = body.chars().next().is_some_and(|c| SSH_VALUE_OPTS.contains(&c));
        let attached = takes_value && body.chars().count() > 1;
        let keep = !body.chars().all(|c| SSH_DROP_FLAGS.contains(&c));
        if keep {
            flags.push(arg.clone());
        }
        i += 1;
        if takes_value && !attached {
            // An option that takes a value with nothing after it is a malformed
            // command line, not an ssh invocation we can read.
            let value = args.get(i)?;
            if keep {
                flags.push(value.clone());
            }
            i += 1;
        }
    }
    None
}

/// Longest unterminated APC we will hold on to across PTY reads. Our
/// announcement is a hostname and a socket path; anything past this is another
/// application's payload (kitty's graphics protocol, most likely) and we stop
/// tracking it rather than buffer it.
const MAX_APC_LEN: usize = 4096;

/// Whether an APC body is ours, and what is left of it if so.
///
/// Any name this program has shipped under, because the announcement is written
/// by the *far* machine's binary — which is whatever version is installed
/// there, not this one. A rename that only this side knows about would make
/// every un-upgraded machine's handoff land as an unrecognised APC and be
/// silently dropped, which is the same breakage
/// [`butai_client::dial::find_binary`] exists to undo, one hop earlier.
fn strip_apc_prefix(body: &str) -> Option<&str> {
    butai_protocol::names::BINARIES.iter().find_map(|n| body.strip_prefix(*n)?.strip_prefix(';'))
}

enum ApcScan {
    /// `end` is the index just past the terminator, relative to the slice.
    Complete {
        end: usize,
        query: Option<Query>,
    },
    Incomplete,
    TooLong,
}

/// Scan an APC body (everything after `ESC _`) for its terminator, and parse it
/// if it is ours.
///
/// Terminated by ST (`ESC \`) per ECMA-48, or by BEL, which terminals accept
/// interchangeably for string sequences and some programs emit.
fn scan_apc(body: &[u8]) -> ApcScan {
    let mut i = 0;
    while i < body.len() {
        if i > MAX_APC_LEN {
            return ApcScan::TooLong;
        }
        match body[i] {
            0x07 => return ApcScan::Complete { end: i + 1, query: parse_apc(&body[..i]) },
            0x1b if i + 1 < body.len() && body[i + 1] == b'\\' => {
                return ApcScan::Complete { end: i + 2, query: parse_apc(&body[..i]) }
            }
            // A lone trailing ESC: the `\` may be in the next read.
            0x1b if i + 1 >= body.len() => return ApcScan::Incomplete,
            // An ESC that is not ST ends the string sequence in practice —
            // a terminal resynchronizes rather than swallowing the rest of the
            // stream, and so must we.
            0x1b => return ApcScan::Complete { end: i, query: None },
            _ => i += 1,
        }
    }
    if body.len() > MAX_APC_LEN {
        ApcScan::TooLong
    } else {
        ApcScan::Incomplete
    }
}

/// Parse `butai;here;<hint>;<socket>`. Anything else is another application's
/// APC and is left alone.
fn parse_apc(body: &[u8]) -> Option<Query> {
    let body = std::str::from_utf8(body).ok()?;
    let rest = strip_apc_prefix(body)?;
    let rest = rest.strip_prefix("here;")?;
    // Split once, so a socket path containing `;` survives.
    let (hint, socket) = rest.split_once(';')?;
    if hint.is_empty() {
        return None;
    }
    Some(Query::RemoteAnnounce { hint: hint.to_string(), socket: socket.to_string() })
}

/// Drop a boxed line's left border and padding, so a dialog rendered inside a
/// frame (`│ ❯ 1. Yes   │`) can still be matched by its shape.
fn strip_gutter(line: &str) -> &str {
    line.trim_start_matches(|c| GUTTER_CHARS.contains(&c))
}

/// The band [`TerminalPane::footer_signals`] scans, as a `[start, end)` window
/// into one screen's rows: the last [`FOOTER_SCAN_ROWS`] rows that have
/// anything on them.
///
/// Anchoring on the last *written* row rather than the bottom of the grid is
/// what lets a dialog painted from the top of an otherwise empty screen be
/// seen. `agy` opens every session that way — its folder-trust question sits in
/// the top half of a fresh 80x24 pane and the whole band underneath is blank,
/// so a band pinned to the grid read eight empty rows and called the agent idle
/// while it waited on an answer. On an ordinary agent screen the last row *is*
/// the last written one and nothing moves; where it is not, this reads eight
/// rows of content instead of blanks the agent never drew.
fn footer_window(rows: &[String]) -> (usize, usize) {
    let end = rows.iter().rposition(|l| !l.trim_end().is_empty()).map_or(0, |i| i + 1);
    (end.saturating_sub(usize::from(FOOTER_SCAN_ROWS)), end)
}

/// Whether a line carries prompt chrome (see [`PROMPT_MARKERS`]). Also read as
/// a *veto* on the working markers: a line that is dialog chrome describes the
/// dialog, not a running turn, however it spells its escape key.
fn prompt_chrome(line: &str) -> bool {
    PROMPT_MARKERS.iter().any(|m| line.contains(m))
}

/// Whether a line is the *highlighted* entry of a choice menu — `❯ 1. Yes`,
/// `> 2. No`, `❯ Yes`. Matching the cursor's shape rather than literal option
/// text catches the dialog whichever entry is selected, in any wording, while
/// an agent's prose (which never carries a cursor glyph) stays out.
fn selected_option(line: &str) -> bool {
    let line = strip_gutter(line);
    let Some(rest) = CURSOR_GLYPHS.iter().find_map(|g| line.strip_prefix(*g)) else {
        return false;
    };
    // The cursor must actually point at something, and at a space: `>>= x` and
    // `> some quoted text` are not menus.
    let Some(rest) = rest.strip_prefix(' ') else { return false };
    let rest = rest.trim_start();
    numbered_option(rest) || rest.starts_with("yes") || rest.starts_with("no")
}

/// Whether a line opens a numbered choice: `1. Yes`, `2) No`.
fn numbered_option(line: &str) -> bool {
    let mut chars = strip_gutter(line).chars();
    matches!(chars.next(), Some(c) if c.is_ascii_digit())
        && matches!(chars.next(), Some('.') | Some(')'))
}

/// Store a trailing escape fragment for the next read, bounded so a stream of
/// lone ESCs (or a pathologically long CSI) can't grow it without limit.
fn set_carry(carry: &mut Vec<u8>, tail: &[u8]) {
    carry.clear();
    if tail.len() <= 32 {
        carry.extend_from_slice(tail);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    /// The announcement is written by the *far* machine's binary, so its name is
    /// whatever is installed there — including the name this program used to
    /// have. Recognising only the current one dropped the handoff on every
    /// machine that had not been upgraded, silently: an unrecognised APC is
    /// another application's, and is left alone by design.
    #[test]
    fn an_announcement_under_any_of_our_names_is_ours() {
        for name in butai_protocol::names::BINARIES {
            let body = format!("{name};here;user@far;/run/user/1000/x.sock");
            let query = parse_apc(body.as_bytes())
                .unwrap_or_else(|| panic!("{name} announced itself and was not heard"));
            assert_eq!(
                query,
                Query::RemoteAnnounce {
                    hint: "user@far".into(),
                    socket: "/run/user/1000/x.sock".into(),
                }
            );
        }
    }

    /// And nothing else is. Another application's APC — kitty's graphics
    /// protocol is the one actually seen in the wild — must pass straight
    /// through, including one that merely starts with our name.
    #[test]
    fn someone_elses_apc_is_left_alone() {
        for body in ["Gf=100,a=T;<payload>", "butaish;here;a;b", "butai;elsewhere;a;b", "butai;"] {
            assert_eq!(parse_apc(body.as_bytes()), None, "{body} was claimed as ours");
        }
    }

    /// The same instant must convert to the same number every time.
    ///
    /// An `AgentDto` carries these timestamps and the event stream pushes a
    /// workspace only when its detail differs from the last one sent, so a value
    /// that is not equal to itself reports changes that did not happen.
    #[test]
    fn an_instant_converts_to_a_stable_epoch() {
        let t = std::time::Instant::now();
        let first = epoch_ms_from(t);
        std::thread::sleep(std::time::Duration::from_millis(15));
        assert_eq!(epoch_ms_from(t), first, "epoch conversion drifted between calls");
    }

    /// And it must still be a real wall-clock time, not just a stable one.
    #[test]
    fn the_epoch_conversion_lands_near_the_wall_clock() {
        let now_ms =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
                as u64;
        let converted = epoch_ms_from(std::time::Instant::now());
        assert!(converted.abs_diff(now_ms) < 60_000, "converted {converted} vs now {now_ms}");
    }

    /// Ordering has to survive the conversion: an agent started earlier must
    /// report an earlier timestamp, which is what makes "age" mean anything.
    #[test]
    fn earlier_instants_convert_to_earlier_epochs() {
        let first = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let second = std::time::Instant::now();
        assert!(epoch_ms_from(first) < epoch_ms_from(second));
    }

    #[test]
    fn split_nul_drops_the_trailing_empty() {
        assert_eq!(split_nul(b"sudo\0apt-get\0update\0"), argv(&["sudo", "apt-get", "update"]));
        // No trailing NUL, and repeated separators, are both tolerated.
        assert_eq!(split_nul(b"vim\0\0a.txt"), argv(&["vim", "a.txt"]));
        assert!(split_nul(b"").is_empty());
    }

    #[test]
    fn an_idle_login_shell_is_not_worth_naming_a_row_for() {
        // A login shell spells argv[0] with a leading dash; ours arrives as the
        // configured path. Flags alone still mean "sitting at a prompt".
        assert_eq!(display_argv(argv(&["-zsh"])), None);
        assert_eq!(display_argv(argv(&["/bin/bash"])), None);
        assert_eq!(display_argv(argv(&["sh"])), None);
        assert_eq!(display_argv(argv(&["zsh", "-l"])), None);
    }

    #[test]
    fn a_real_command_keeps_all_of_its_arguments() {
        assert_eq!(
            display_argv(argv(&["sudo", "apt-get", "update", "-y"])).as_deref(),
            Some("sudo apt-get update -y"),
        );
        // A shell with a non-flag argument is running something.
        assert_eq!(display_argv(argv(&["zsh", "build.sh"])).as_deref(), Some("zsh build.sh"));
        assert_eq!(display_argv(argv(&["sh", "-c", "make"])).as_deref(), Some("sh -c make"));
        // Arguments containing spaces are joined plainly, not re-quoted.
        assert_eq!(display_argv(argv(&["vim", "a b.txt"])).as_deref(), Some("vim a b.txt"));
    }

    #[test]
    fn a_pathological_argv_is_capped() {
        let long = display_argv(argv(&["x"; 4000])).unwrap();
        assert_eq!(long.chars().count(), FG_MAX);
    }

    #[test]
    fn procargs2_argv_survives_the_exec_path_and_its_padding() {
        let mut buf = 3i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/usr/bin/sudo\0");
        buf.extend_from_slice(b"\0\0"); // alignment padding
        buf.extend_from_slice(b"sudo\0apt-get\0update\0");
        buf.extend_from_slice(b"PATH=/bin\0"); // environ, must be ignored
        assert_eq!(parse_procargs2(&buf), Some(argv(&["sudo", "apt-get", "update"])));
    }

    #[test]
    fn procargs2_keeps_the_complete_arguments_of_a_truncated_buffer() {
        let mut buf = 3i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/usr/bin/sudo\0\0");
        buf.extend_from_slice(b"sudo\0apt-get\0upda"); // cut mid-argument
        assert_eq!(parse_procargs2(&buf), Some(argv(&["sudo", "apt-get"])));
    }

    #[test]
    fn procargs2_rejects_garbage() {
        assert_eq!(parse_procargs2(b""), None);
        assert_eq!(parse_procargs2(&[1, 0]), None);
        // Negative argc, and argc with no NUL terminating the exec path.
        let mut neg = (-1i32).to_ne_bytes().to_vec();
        neg.extend_from_slice(b"/bin/sh\0\0sh\0");
        assert_eq!(parse_procargs2(&neg), None);
        let mut unterminated = 1i32.to_ne_bytes().to_vec();
        unterminated.extend_from_slice(b"/usr/bin/sudo");
        assert_eq!(parse_procargs2(&unterminated), None);
    }

    #[test]
    fn a_dead_pane_reports_no_foreground_command() {
        let mut pane = agent_pane(10);
        pane.mark_exited(0);
        assert_eq!(pane.foreground_cmdline(), None);
    }

    /// The end-to-end path, exercising the real platform syscall: run a command
    /// in a real pty and read its whole command line back off the tty's
    /// foreground process group. This is what the PROCESSES rail shows.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_command_running_in_the_pane_is_named_in_full() {
        let mut pane = agent_pane(10);
        pane.handle_input(&InputEvent::Paste("sleep 47\n".into()));
        // The probe is TTL-cached, so poll past FG_TTL until the child has
        // exec'd rather than racing it once.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen = None;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(FG_TTL + std::time::Duration::from_millis(50));
            seen = pane.foreground_cmdline();
            if seen.is_some() {
                break;
            }
        }
        // `Drop` kills the child. The point is that the argument survives: the
        // old `/proc/<pid>/comm` read would have yielded a bare "sleep".
        assert_eq!(seen.as_deref(), Some("sleep 47"));
    }

    /// A setuid program keeps its argv to itself: `/usr/bin/top` runs as root,
    /// so `KERN_PROCARGS2` refuses it (EINVAL) and so does libproc's `proc_name`
    /// (EPERM). The row must still be named for what is running in it — a pane
    /// showing `top` used to report nothing and keep reading `shell`.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_privileged_command_is_named_even_when_its_argv_is_refused() {
        let mut pane = agent_pane(10);
        pane.handle_input(&InputEvent::Paste("/usr/bin/top -l 20 -s 1\n".into()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen = None;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(FG_TTL + std::time::Duration::from_millis(50));
            seen = pane.foreground_cmdline();
            if seen.is_some() {
                break;
            }
        }
        // The arguments are the kernel's to withhold; the program name is not.
        assert_eq!(seen.as_deref(), Some("top"));
    }

    #[test]
    fn an_idle_shell_pane_names_no_command_and_caches_the_probe() {
        // The pane's child is a bare login shell, so there is nothing to show —
        // true on Linux, macOS, and the stub platforms alike.
        let mut pane = agent_pane(10);
        assert_eq!(pane.foreground_cmdline(), None);
        assert!(pane.fg_cache.is_some(), "the probe must be cached, not retaken every frame");
    }

    #[test]
    fn bell_drives_attention_until_acknowledged() {
        let mut pane = agent_pane(10);
        // Output that keeps streaming -> working.
        feed(&mut pane, b"thinking...");
        pane.backdate_burst(WORKING_MIN_SPAN);
        assert_eq!(pane.attention(), Attention::Working);
        // A bell outranks activity until the user looks at the pane.
        feed(&mut pane, b"\x07may I proceed?");
        assert_eq!(pane.attention(), Attention::Waiting);
        pane.acknowledge();
        assert_eq!(pane.attention(), Attention::Working);
        // Dead panes are idle regardless.
        pane.mark_exited(0);
        assert_eq!(pane.attention(), Attention::Idle);
    }

    /// [`TerminalPane::feed_output`] normalizes HVP in place and so takes a
    /// mutable buffer; tests hand it literals, which need a copy.
    fn feed(pane: &mut TerminalPane, bytes: &[u8]) {
        pane.feed_output(&mut bytes.to_vec());
    }

    pub(super) fn agent_pane(rows: u16) -> TerminalPane {
        agent_pane_sized(rows, 40)
    }

    fn agent_pane_sized(rows: u16, cols: u16) -> TerminalPane {
        agent_pane_detect(rows, cols, Detect::default())
    }

    /// An agent pane carrying per-agent detection overrides.
    fn agent_pane_detect(rows: u16, cols: u16, detect: Detect) -> TerminalPane {
        let (tx, _rx) = unbounded_channel();
        let (otx, _orx) = tokio::sync::mpsc::channel(16);
        let spec = SpawnSpec {
            pane: PaneId(1),
            ws: SessionId(1),
            socket: Path::new("/tmp/butai-test.sock"),
            program: None,
            args: &[],
            env: &[],
            cwd: Path::new("/"),
            shell: "/bin/sh",
            via_shell: false,
            label: Some("agent"),
            detect: Some(detect),
            replay: None,
        };
        TerminalPane::spawn(PaneId(1), spec, rows, cols, 100, 0, tx, otx).unwrap()
    }

    /// The screen as the client would draw it, top row first.
    fn screen_of(pane: &TerminalPane) -> Vec<String> {
        let area = Rect::new(0, 0, pane.cols, pane.rows);
        let mut buf = Buffer::empty(area);
        pane.render(&mut buf, area);
        (0..area.height)
            .map(|y| {
                (0..area.width).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().into()
            })
            .collect()
    }

    /// Scrollback in an agent pane, which is where it was unusable: an agent
    /// repaints its status band several times a second, and output used to snap
    /// the view back to live, so every wheel notch was undone before the next
    /// frame reached the client. A shell hid the bug — nothing arrives while
    /// you read.
    #[test]
    fn a_repaint_leaves_a_scrolled_back_view_where_it_is() {
        let mut pane = agent_pane(10);
        for i in 0..40 {
            feed(&mut pane, format!("line {i}\r\n").as_bytes());
        }
        pane.scroll_page(-1);
        let parked = pane.scroll_offset();
        assert!(parked > 0, "a page back through 40 lines never left the live screen");
        let looking_at = screen_of(&pane);
        assert!(
            looking_at.iter().any(|l| l == "line 25"),
            "a page back should be showing older output: {looking_at:?}"
        );

        // The status band being rewritten in place: no new line, so nothing
        // moves under the view either.
        feed(&mut pane, b"\r\x1b[K* thinking (12s, 4.1k tokens)");
        assert_eq!(pane.scroll_offset(), parked, "a repaint dragged the view back to live");
        assert_eq!(screen_of(&pane), looking_at, "a repaint moved what was on screen");

        // New lines *do* move under it, and the view stays on the same text
        // rather than being carried along with them.
        for i in 40..45 {
            feed(&mut pane, format!("\r\nline {i}\r\n").as_bytes());
        }
        let after = screen_of(&pane);
        assert!(
            after.iter().any(|l| l == "line 25"),
            "five more lines of output shifted the parked view: {after:?}"
        );
    }

    /// And typing is what leaves the scrollback, so a pane parked and forgotten
    /// comes back the moment you write to it.
    #[test]
    fn typing_puts_the_view_back_on_the_live_screen() {
        let mut pane = agent_pane(10);
        for i in 0..40 {
            feed(&mut pane, format!("line {i}\r\n").as_bytes());
        }
        pane.scroll_page(-1);
        assert!(pane.scroll_offset() > 0);
        pane.handle_input(&InputEvent::Key(butai_protocol::KeyEvent::char('y')));
        assert_eq!(pane.scroll_offset(), 0, "typing left the view in the scrollback");
    }

    /// What the agent is doing is read off its footer band, and a parked view
    /// is not that band. Left reading the visible grid, a question from an hour
    /// ago scrolled back into view would ring as a live one — while the turn
    /// actually running underneath went unseen.
    #[test]
    fn a_parked_view_still_reads_the_live_footer() {
        let mut pane = agent_pane(10);
        feed(&mut pane, b"shall I write the file? (y/n)\r\n");
        for i in 0..40 {
            feed(&mut pane, format!("line {i}\r\n").as_bytes());
        }
        feed(&mut pane, b"esc to interrupt");
        assert!(pane.shows_busy_marker(), "the live footer says the turn is running");
        assert!(!pane.shows_input_prompt(), "the question is long gone from the live screen");

        // Far enough back for the old question to be on screen again.
        pane.scroll_lines(-42);
        let looking_at = screen_of(&pane);
        assert!(
            looking_at.iter().any(|l| l.contains("(y/n)")),
            "the old question should be back in view: {looking_at:?}"
        );
        assert!(!pane.shows_input_prompt(), "an old question read as a live one");
        assert!(pane.shows_busy_marker(), "the running turn went unseen while scrolled back");

        // Back at the bottom, the visible grid is the live one again.
        pane.scroll_to_live();
        assert!(pane.shows_busy_marker());
        assert!(!pane.shows_input_prompt());
    }

    #[test]
    fn a_one_shot_repaint_is_not_working() {
        // Opening a pane from another client resizes it; the agent answers with
        // a single full repaint. That must stay idle — reading it as a turn is
        // what produced the phantom "working" flicker and, once it settled, a
        // spurious "agent finished" notification.
        let mut pane = agent_pane(10);
        feed(&mut pane, b"\x1b[2J\x1b[H> ready\r\n");
        assert!(!pane.sustained_output(), "a repaint burst is not sustained output");
        assert_eq!(pane.attention(), Attention::Idle);
        assert!(!pane.is_busy(), "no marker + no sustained output = not busy");

        // Output that keeps coming does read as working.
        pane.backdate_burst(WORKING_MIN_SPAN);
        assert!(pane.sustained_output());
        assert_eq!(pane.attention(), Attention::Working);
    }

    #[test]
    fn busy_marker_makes_a_pane_busy_immediately() {
        // The marker is a positive signal, so it does not wait on the span: an
        // agent that just started its turn is busy from the first frame.
        let mut pane = agent_pane(10);
        feed(&mut pane, b"\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\nesc to interrupt");
        assert!(!pane.sustained_output());
        assert!(pane.is_busy(), "visible marker means working regardless of span");
    }

    #[test]
    fn busy_marker_only_counts_in_the_footer() {
        // Marker in the response body (top of a 10-row grid, outside the
        // 8-row footer band) must NOT read as busy — otherwise an agent that
        // merely printed "esc to interrupt" would never look finished.
        let mut pane = agent_pane(10);
        feed(&mut pane, b"esc to interrupt\r\n\r\n\r\n");
        assert!(!pane.shows_busy_marker(), "marker in body should not count");

        // Same marker rendered down in the footer band does count.
        let mut pane = agent_pane(10);
        feed(&mut pane, b"\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\nesc to interrupt");
        assert!(pane.shows_busy_marker(), "marker in footer should count");
    }

    /// A pane whose visible grid *ends* with `lines` (blank-padded above) — the
    /// shape a real agent footer has on screen. Keep lines under 40 columns.
    fn pane_showing(lines: &[&str]) -> TerminalPane {
        pane_showing_sized(10, 40, lines)
    }

    /// [`pane_showing`] for a dialog that does not fit the default 40x10 pane —
    /// a real agent's question box is both wider and much taller than that.
    fn pane_showing_sized(rows: u16, cols: u16, lines: &[&str]) -> TerminalPane {
        let mut pane = agent_pane_sized(rows, cols);
        let mut out = "\r\n".repeat((rows as usize).saturating_sub(lines.len()));
        out.push_str(&lines.join("\r\n"));
        feed(&mut pane, out.as_bytes());
        pane
    }

    /// [`pane_showing`] for an agent carrying per-agent detection overrides.
    fn pane_showing_detect(detect: Detect, lines: &[&str]) -> TerminalPane {
        let mut pane = agent_pane_detect(10, 40, detect);
        let mut out = "\r\n".repeat(10usize.saturating_sub(lines.len()));
        out.push_str(&lines.join("\r\n"));
        feed(&mut pane, out.as_bytes());
        pane
    }

    #[test]
    fn a_waiting_pattern_recognizes_what_the_built_ins_miss() {
        // An agent that words its confirmation unlike anything in
        // PROMPT_MARKERS: no "(y/n)", no "enter to select", no numbered list.
        let footer = ["shall i apply this patch to disk"];
        assert!(
            !pane_showing(&footer).shows_input_prompt(),
            "precondition: the built-in markers do not know this wording"
        );

        let pane = pane_showing_detect(Detect::compile("x", Some("shall i apply"), None), &footer);
        assert!(pane.shows_input_prompt(), "waiting_pattern should recognize it");
        assert_eq!(pane.attention(), Attention::Waiting);
    }

    #[test]
    fn a_waiting_pattern_can_take_back_a_false_positive() {
        // The half an additive pattern could never do. This agent prints
        // "(y/n)" in prose the built-in table matches, and the override — which
        // *replaces* that table rather than extending it — declines to.
        let footer = ["answer with (y/n) when you are ready"];
        assert!(
            pane_showing(&footer).shows_input_prompt(),
            "precondition: the built-in marker fires on this line"
        );

        let pane =
            pane_showing_detect(Detect::compile("x", Some("^awaiting input"), None), &footer);
        assert!(!pane.shows_input_prompt(), "an override replaces the built-in markers");
    }

    #[test]
    fn a_busy_pattern_replaces_the_built_in_busy_markers() {
        // Custom spinner wording, with none of the keyed interrupt hints.
        let footer = ["◐ crunching (14s)"];
        assert!(
            !pane_showing(&footer).shows_busy_marker(),
            "precondition: the built-in markers do not know this wording"
        );

        let pane =
            pane_showing_detect(Detect::compile("x", None, Some(r"crunching \(\d+s\)")), &footer);
        assert!(pane.shows_busy_marker(), "busy_pattern should recognize it");
    }

    #[test]
    fn an_uncompilable_pattern_falls_back_to_the_built_ins() {
        // Losing the override costs accuracy; refusing to spawn would cost the
        // user their agent. So a bad regex is dropped and the tables stand.
        let detect = Detect::compile("x", Some("(unclosed"), None);
        let pane = pane_showing_detect(detect, &["do it? (y/n)"]);
        assert!(pane.shows_input_prompt(), "bad pattern should fall back, not disable detection");
    }

    #[test]
    fn prose_does_not_pin_a_pane_to_busy() {
        // The bare verbs used to match: an answer mentioning "to stop" or a
        // backgrounded server left the agent spinning forever and killed its
        // "finished" notification. Only keyed status hints count now.
        let pane = pane_showing(&[
            "I added a guard to stop the retry loop,",
            "and the dev server is running in the",
            "background on :3000.",
            "",
            "> ",
        ]);
        assert!(!pane.shows_busy_marker(), "prose verbs are not a status line");

        // The real status line still reads as busy, spinner glyph and all.
        let pane = pane_showing(&["* Cogitating... (12s - esc to interrupt)", "> "]);
        assert!(pane.shows_busy_marker());
        // ...and it holds through a thinking pause, with no output at all.
        assert_eq!(pane.attention(), Attention::Working);
    }

    #[test]
    fn boxed_permission_dialog_reads_as_waiting() {
        // Claude Code draws its dialog inside a box, and the cursor sits on
        // whichever entry is selected — here the second one, which the old
        // literal `❯ 1.` marker missed entirely.
        let pane = pane_showing(&[
            "+------------------------------+",
            "| Bash command                 |",
            "| rm -rf build                 |",
            "|                              |",
            "| Do you want to proceed?      |",
            "|   1. Yes                     |",
            "| > 2. No                      |",
            "+------------------------------+",
        ]);
        assert!(pane.shows_input_prompt(), "boxed dialog should be detected");
        assert_eq!(pane.attention(), Attention::Waiting);
    }

    /// A pane whose visible grid *starts* with `lines` and is blank underneath —
    /// the shape a CLI paints when it draws a dialog on a fresh screen.
    fn pane_showing_from_top(rows: u16, cols: u16, lines: &[&str]) -> TerminalPane {
        let mut pane = agent_pane_sized(rows, cols);
        feed(&mut pane, lines.join("\r\n").as_bytes());
        pane
    }

    #[test]
    fn the_antigravity_trust_dialog_reads_as_waiting() {
        // Transcribed from a real `agy` pane (1.1.12) spawned by a real daemon,
        // at the 80x24 it was given. The trust dialog is the *first* thing the
        // CLI shows, so a fresh agy row is parked on a question before it has
        // done anything — and it paints from the top, leaving the bottom eleven
        // rows blank. That is the case `footer_window` exists for: pinned to the
        // grid, the band was eleven rows of nothing and the rail said "idle"
        // while the agent waited for an answer.
        //
        // What carries it once the band reaches the content is `selected_option`
        // on the highlighted row. Nothing else does: the question is a sentence
        // no QUESTION_MARKER knows, the options are unnumbered so the
        // numbered-list path does not apply, and the hint line reads `enter
        // Confirm` — near enough to Claude Code's `Enter to confirm` to look
        // covered by PROMPT_MARKERS, not near enough to match one.
        let pane = pane_showing_from_top(
            24,
            80,
            &[
                "Accessing workspace:",
                "",
                "/tmp/ab/proj",
                "",
                "Do you trust the contents of this project?",
                "",
                "Antigravity CLI requires permission to read, edit, and execute files here.",
                "",
                "> Yes, I trust this folder",
                "  No, exit",
                "",
                "  ↑/↓ Navigate · enter Confirm",
                "                                         Gemini 3.6 Flash · high",
            ],
        );
        assert!(pane.shows_input_prompt(), "the trust dialog is a question");
        assert!(!pane.shows_busy_marker(), "a dialog's hint line is not a turn in flight");
        assert_eq!(pane.attention(), Attention::Waiting);
    }

    #[test]
    fn an_idle_antigravity_prompt_is_not_a_question() {
        // The other side of it: the same CLI's ordinary input box, which must
        // stay idle. `? for shortcuts` is a hint about the keyboard, not a
        // choice being offered, and the bare `>` is where you type — reading
        // either as a question would ring on every agy pane doing nothing.
        let pane = pane_showing_sized(
            10,
            100,
            &[
                "──────────────────────────────────────────────────────────────",
                ">",
                "──────────────────────────────────────────────────────────────",
                "? for shortcuts                              Gemini 3.6 Flash · high",
            ],
        );
        assert!(!pane.shows_input_prompt(), "an empty input box is idle, not waiting");
        assert!(!pane.shows_busy_marker());
        assert_eq!(pane.attention(), Attention::Idle);
    }

    #[test]
    fn a_multiple_choice_question_reads_as_waiting() {
        // Transcribed from a real `claude` pane (v2.1.220, 100x30) parked on an
        // AskUserQuestion dialog. Two things about this shape broke the old
        // scan, and both are load-bearing here:
        //
        //   * the highlighted `❯ 1.` sits thirteen rows above the bottom,
        //     because every option carries a two-line description — the 8-row
        //     footer band never sees it;
        //   * the hint line offers "esc to cancel", which is Gemini's *working*
        //     marker, so the pane read as busy for as long as the question went
        //     unanswered. It never asks "do you want to…" either, so the
        //     question-sentence path could not save it.
        //
        // The hint line's "enter to select" is what carries it: it is the last
        // row whatever the width or the option count.
        let pane = pane_showing_sized(20, 100, &[
            "←  ☐ Database  ☐ Deployment  ✔ Submit  →",
            "",
            "Which database would you like to use?",
            "",
            "❯ 1. PostgreSQL",
            "     Robust relational database with advanced features, excellent for production systems",
            "     with complex queries and ACID compliance requirements.",
            "  2. SQLite",
            "     Lightweight embedded database ideal for development, testing, or small-scale",
            "     deployments with minimal infrastructure overhead.",
            "  3. MySQL",
            "     Popular open-source relational database offering good performance and wide hosting",
            "     support, commonly used in web applications.",
            "  4. Type something.",
            "──────────────────────────────────────────────────────────────────────────────────",
            "  5. Chat about this",
            "",
            "Enter to select · Tab/Arrow keys to navigate · Esc to cancel",
        ]);
        assert!(pane.shows_input_prompt(), "a multiple-choice question needs you");
        assert!(!pane.shows_busy_marker(), "a dialog's own hint line is not a turn in flight");
        assert_eq!(pane.attention(), Attention::Waiting);
    }

    #[test]
    fn a_cancel_hint_without_a_choice_is_still_a_live_turn() {
        // The other side of the line above: Gemini spells its interrupt hint
        // "esc to cancel" and nothing else. Nothing on screen names a choice, so
        // this stays a working turn — vetoing the marker on the *chrome* line
        // only, rather than on the phrase, is what keeps both readings true.
        let pane = pane_showing(&["⠹ Thinking… (esc to cancel, 6s)", "", "> "]);
        assert!(pane.shows_busy_marker());
        assert!(!pane.shows_input_prompt(), "an interrupt hint is not a question");
        assert_eq!(pane.attention(), Attention::Working);
    }

    #[test]
    fn a_numbered_list_in_an_answer_is_not_a_question() {
        // An answer that merely lists options — no cursor, no question — must
        // not fire the "needs you" notification.
        let pane = pane_showing(&[
            "Two ways to do this:",
            "  1. Yes-path: keep the guard",
            "  2. No-path: revert it",
            "Both are fine.",
            "> ",
        ]);
        assert!(!pane.shows_input_prompt(), "a list is not a prompt");
        assert_eq!(pane.attention(), Attention::Idle);
    }

    #[test]
    fn a_question_mid_turn_loses_to_the_working_marker() {
        // Same words, but the interrupt hint says the turn is still live: this
        // is the agent writing about a decision, not asking for one.
        let pane = pane_showing(&[
            "Do you want to keep the retry?",
            "  1. Yes",
            "  2. No",
            "* Working... (esc to interrupt)",
        ]);
        assert!(pane.shows_busy_marker());
        assert!(!pane.shows_input_prompt(), "a live turn is not a question");
        assert_eq!(pane.attention(), Attention::Working);
    }

    #[test]
    fn input_prompt_reads_as_waiting_without_a_bell() {
        // A permission-style prompt in the footer is a positive "needs you"
        // signal on its own — no bell required, and it beats recent output.
        let mut pane = agent_pane(10);
        feed(&mut pane, b"\r\n\r\n\r\n\r\nDo you want to make this edit?\r\n  1. Yes\r\n  2. No");
        assert!(pane.shows_input_prompt(), "footer prompt should be detected");
        assert_eq!(pane.attention(), Attention::Waiting);

        // A finished agent's ordinary output must not read as a question.
        let mut done = agent_pane(10);
        feed(&mut done, b"All set. Let me know if you want anything else.\r\n");
        assert!(!done.shows_input_prompt(), "plain output is not a prompt");
    }

    /// Run one read through a fresh rewriter and return the normalized bytes.
    fn hvp(bytes: &[u8]) -> Vec<u8> {
        let mut buf = bytes.to_vec();
        HvpRewriter::default().rewrite(&mut buf);
        buf
    }

    #[test]
    fn hvp_is_normalized_to_cup() {
        // vt100 has no `f` arm, so an unrewritten seek is simply lost.
        assert_eq!(hvp(b"\x1b[3;10fX"), b"\x1b[3;10HX");
        // Defaulted and single-parameter forms.
        assert_eq!(hvp(b"\x1b[fX"), b"\x1b[HX");
        assert_eq!(hvp(b"\x1b[5fX"), b"\x1b[5HX");
        // Several in one read, interleaved with other sequences.
        assert_eq!(hvp(b"\x1b[1;1f\x1b[31ma\x1b[2;2fb"), b"\x1b[1;1H\x1b[31ma\x1b[2;2Hb");
    }

    #[test]
    fn only_a_csi_final_byte_is_rewritten() {
        // A literal `f` in ordinary text, and one inside a parameter run that
        // never opened a CSI, must survive untouched.
        assert_eq!(hvp(b"a f file\x1b[0m"), b"a f file\x1b[0m");
        // Private-marker and intermediate forms are not HVP.
        assert_eq!(hvp(b"\x1b[?1;2f"), b"\x1b[?1;2f");
        assert_eq!(hvp(b"\x1b[>0f"), b"\x1b[>0f");
        assert_eq!(hvp(b"\x1b[ f"), b"\x1b[ f");
        // `ESC f` is its own (non-CSI) escape.
        assert_eq!(hvp(b"\x1bf"), b"\x1bf");
    }

    #[test]
    fn hvp_split_across_reads_is_still_rewritten() {
        // The PTY hands over 64 KB chunks, so a seek can straddle two reads at
        // any byte. Every split of the same sequence must still normalize.
        let whole: &[u8] = b"\x1b[12;34fZ";
        for cut in 0..whole.len() {
            let mut rw = HvpRewriter::default();
            let (mut a, mut b) = (whole[..cut].to_vec(), whole[cut..].to_vec());
            rw.rewrite(&mut a);
            rw.rewrite(&mut b);
            let joined: Vec<u8> = a.into_iter().chain(b).collect();
            assert_eq!(joined, b"\x1b[12;34HZ", "split at {cut}");
        }
    }

    #[test]
    fn hvp_seeks_actually_position_the_emulator() {
        // The end-to-end claim: two HVP seeks land on distinct rows instead of
        // collapsing onto one line the way btop did.
        let mut pane = agent_pane_sized(6, 20);
        feed(&mut pane, b"\x1b[2J\x1b[2;3fAA\x1b[4;5fBB");
        let area = Rect::new(0, 0, 20, 6);
        let mut buf = Buffer::empty(area);
        pane.render(&mut buf, area);
        let row = |y: u16| -> String {
            (0..20).map(|x| buf.cell((x, y)).unwrap().symbol().to_string()).collect()
        };
        assert_eq!(row(1).trim_end(), "  AA");
        assert_eq!(row(3).trim_end(), "    BB");
    }

    /// Replies for one read, as if the cursor sat at `cursor` throughout.
    fn replies(carry: &mut Vec<u8>, bytes: &[u8], cursor: (u16, u16)) -> Vec<u8> {
        terminal_queries(carry, bytes).into_iter().flat_map(|(_, q)| q.reply(cursor)).collect()
    }

    #[test]
    fn answers_cursor_position_and_device_queries() {
        let mut carry = Vec::new();
        // DSR cursor-position report is 1-based row;col.
        assert_eq!(replies(&mut carry, b"\x1b[6n", (4, 9)), b"\x1b[5;10R");
        assert!(carry.is_empty());
        // Status, primary and secondary device attributes.
        assert_eq!(replies(&mut carry, b"\x1b[5n", (0, 0)), b"\x1b[0n");
        assert_eq!(replies(&mut carry, b"\x1b[c", (0, 0)), b"\x1b[?1;2c");
        assert_eq!(replies(&mut carry, b"\x1b[0c", (0, 0)), b"\x1b[?1;2c");
        // Secondary DA names butai in its `Pp` field rather than answering the
        // generic `0`: it is what tells a `butai` on the far end of an ssh
        // session that it is running inside one of our panes. Built from the
        // constants so the reply and `handoff.rs`'s matcher cannot drift.
        let da2 = format!("\x1b[>{DA2_BUTAI_ID};{};0c", butai_protocol::PROTOCOL_VERSION);
        assert_eq!(replies(&mut carry, b"\x1b[>c", (0, 0)), da2.as_bytes());
    }

    #[test]
    fn ignores_non_query_sequences_and_finds_embedded_queries() {
        let mut carry = Vec::new();
        // Ordinary output and cursor-movement CSIs draw nothing back.
        assert!(replies(&mut carry, b"hi\x1b[2Kthere\r\n", (0, 0)).is_empty());
        // A query buried in a redisplay burst is still answered.
        assert_eq!(replies(&mut carry, b"(reverse-i-search)\x1b[6n", (2, 7)), b"\x1b[3;8R");
    }

    #[test]
    fn recognizes_a_query_split_across_two_reads() {
        let mut carry = Vec::new();
        // First read ends mid-CSI: no reply yet, the fragment is carried.
        assert!(replies(&mut carry, b"foo\x1b[6", (1, 1)).is_empty());
        assert_eq!(carry, b"\x1b[6");
        // The final byte arrives next read and completes the query.
        assert_eq!(replies(&mut carry, b"n", (1, 1)), b"\x1b[2;2R");
        assert!(carry.is_empty());
    }

    #[test]
    fn bounds_a_lone_trailing_escape() {
        let mut carry = Vec::new();
        // A dangling ESC is carried (could begin a CSI next read)...
        assert!(replies(&mut carry, b"abc\x1b", (0, 0)).is_empty());
        assert_eq!(carry, b"\x1b");
        // ...but if the next byte isn't a CSI it's simply consumed.
        assert!(replies(&mut carry, b"A", (0, 0)).is_empty());
        assert!(carry.is_empty());
    }

    #[test]
    fn query_is_answered_with_the_cursor_at_the_query_not_end_of_chunk() {
        let (tx, _rx) = unbounded_channel();
        let (otx, mut orx) = tokio::sync::mpsc::channel(16);
        let spec = SpawnSpec {
            pane: PaneId(1),
            ws: SessionId(1),
            socket: Path::new("/tmp/butai-test.sock"),
            program: Some("cat"),
            args: &[],
            env: &[],
            cwd: Path::new("/"),
            shell: "/bin/sh",
            via_shell: false,
            label: None,
            detect: None,
            replay: None,
        };
        // The child is `cat`, so whatever we answer comes straight back out.
        let mut pane = TerminalPane::spawn(PaneId(1), spec, 24, 80, 100, 0, tx, otx).unwrap();
        // One coalesced chunk: query at row 0 col 3, then output that moves the
        // cursor to row 4. The report must be the position at the query (1;4),
        // not where the chunk ended.
        feed(&mut pane, b"abc\x1b[6n\r\n\r\n\r\n\r\nxyz");
        assert_eq!(pane.emulator.cursor_pos(), (4, 3));
        let mut out = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !out.ends_with(b"R") && std::time::Instant::now() < deadline {
            match orx.try_recv() {
                Ok((_, b)) => out.extend_from_slice(&b),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        // `cat` echoes the reply back with the tty's caret notation for ESC.
        assert!(out.ends_with(b"[1;4R"), "echoed reply: {out:?}");
    }

    /// A pane with output capture on, and optionally a dump replayed into it —
    /// the two halves of restart restore.
    fn restorable_pane(rows: u16, cols: u16, replay: Option<PaneDump<'_>>) -> TerminalPane {
        let (tx, _rx) = unbounded_channel();
        let (otx, _orx) = tokio::sync::mpsc::channel(16);
        let spec = SpawnSpec {
            pane: PaneId(1),
            ws: SessionId(1),
            socket: Path::new("/tmp/butai-test.sock"),
            program: None,
            args: &[],
            env: &[],
            cwd: Path::new("/"),
            shell: "/bin/sh",
            via_shell: false,
            label: None,
            detect: None,
            replay,
        };
        TerminalPane::spawn(PaneId(1), spec, rows, cols, 100, 64 * 1024, tx, otx).unwrap()
    }

    fn screen_rows(pane: &TerminalPane, rows: u16, cols: u16) -> Vec<String> {
        let area = Rect::new(0, 0, cols, rows);
        let mut buf = Buffer::empty(area);
        pane.render(&mut buf, area);
        (0..rows)
            .map(|y| {
                (0..cols)
                    .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// The whole point of the capture: what a pane was showing survives into
    /// the pane that replaces it after a restart.
    #[test]
    fn a_dump_repaints_the_pane_it_came_from() {
        let mut pane = restorable_pane(6, 40, None);
        feed(&mut pane, b"work in progress\r\nsecond line\r\n");
        let (cols, rows) = pane.size();
        let stored = encode_dump(cols, rows, &pane.history());

        let dump = decode_dump(&stored).expect("a dump we just wrote parses");
        let restored = restorable_pane(6, 40, Some(dump));
        let text = screen_rows(&restored, 6, 40).join("\n");
        assert!(text.contains("work in progress"), "restored screen:\n{text}");
        assert!(text.contains("second line"), "restored screen:\n{text}");
    }

    /// A recording has to be replayed at the size it was made at, even when
    /// the pane it lands in is a different size. Here the recorded output
    /// wraps at 20 columns and would not wrap at 80, so replaying it at the
    /// new pane's width instead would put the following line on the wrong row.
    #[test]
    fn a_dump_replays_at_its_recorded_size_not_the_new_pane_size() {
        let mut pane = restorable_pane(6, 20, None);
        feed(&mut pane, "A".repeat(30).as_bytes());
        feed(&mut pane, b"\r\nMARKER");
        let (cols, rows) = pane.size();
        assert_eq!((cols, rows), (20, 6), "recorded narrow");
        let stored = encode_dump(cols, rows, &pane.history());

        // Same output, into a pane four times as wide.
        let dump = decode_dump(&stored).unwrap();
        let restored = restorable_pane(6, 80, Some(dump));
        let rows = screen_rows(&restored, 6, 80);
        assert_eq!(
            rows.iter().position(|r| r.contains("MARKER")),
            Some(2),
            "30 columns of output wrapped onto two rows when it was recorded, so \
             the line after it belongs on the third; screen:\n{}",
            rows.join("\n")
        );
    }

    #[test]
    fn a_file_that_is_not_a_dump_is_ignored_rather_than_replayed() {
        assert!(decode_dump(b"").is_none());
        assert!(decode_dump(b"just some raw output").is_none());
        assert!(decode_dump(b"butai-dump 1 80").is_none(), "header must be terminated");
        assert!(decode_dump(b"butai-dump 1 80\n").is_none(), "header must carry both dimensions");
        assert!(decode_dump(b"butai-dump 1 80 24\nx").is_some(), "a well-formed dump parses");
        assert!(
            decode_dump(b"butai-dump 1 0 24\nx").is_none(),
            "a zero dimension cannot be replayed"
        );
        assert!(
            decode_dump(b"butai-dump 2 80 24\nx").is_none(),
            "a future format is not guessed at"
        );
    }

    /// The ring keeps the newest output and drops the oldest, so a pane that
    /// has been running for hours still costs a bounded amount to persist.
    #[test]
    fn the_output_ring_keeps_the_newest_bytes_within_its_budget() {
        let mut ring = OutputHistory::new(8);
        ring.push(b"12345");
        ring.push(b"6789");
        assert_eq!(ring.snapshot(), b"23456789");
        ring.push(b"abcdefghij");
        assert_eq!(ring.snapshot(), b"cdefghij", "one oversized write keeps its tail");

        let mut off = OutputHistory::new(0);
        off.push(b"anything");
        assert!(off.snapshot().is_empty(), "restore_bytes = 0 captures nothing");
    }

    /// Replay must not answer the cursor-position queries buried in the
    /// recording: the reply would be written to the stdin of a new child that
    /// never asked, landing as stray input on its first prompt.
    #[test]
    fn replay_does_not_answer_queries_recorded_in_the_dump() {
        let mut pane = restorable_pane(6, 40, None);
        feed(&mut pane, b"before\x1b[6nafter");
        let stored = encode_dump(40, 6, &pane.history());

        let (tx, _rx) = unbounded_channel();
        let (otx, mut orx) = tokio::sync::mpsc::channel(16);
        let dump = decode_dump(&stored).unwrap();
        let spec = SpawnSpec {
            pane: PaneId(1),
            ws: SessionId(1),
            socket: Path::new("/tmp/butai-test.sock"),
            program: Some("cat"),
            args: &[],
            env: &[],
            cwd: Path::new("/"),
            shell: "/bin/sh",
            via_shell: false,
            label: None,
            detect: None,
            replay: Some(dump),
        };
        // `cat` echoes anything written to it, so a reply would come straight
        // back as pane output.
        let _pane = TerminalPane::spawn(PaneId(1), spec, 6, 40, 100, 0, tx, otx).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(orx.try_recv().is_err(), "replay wrote to the child");
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use crate::testenv::EnvGuard;

    /// A path the user wrote out is honoured as-is, found or not — second-
    /// guessing an explicit path would silently run a different binary.
    #[test]
    fn an_explicit_path_is_left_alone() {
        assert_eq!(resolve_program("/usr/bin/env"), "/usr/bin/env");
        assert_eq!(resolve_program("./agents/claude"), "./agents/claude");
    }

    /// The common case must not change: something already on PATH stays a bare
    /// name, so the rail label and the process argv read the way they always did.
    #[test]
    fn a_program_on_path_stays_a_bare_name() {
        assert_eq!(resolve_program("sh"), "sh");
    }

    /// The bug this exists for: not on PATH, but present in a directory a login
    /// shell would have added. Uses a fake HOME so the test does not depend on
    /// what happens to be installed on the machine running it.
    #[test]
    fn a_program_only_in_a_user_bindir_is_found_by_absolute_path() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let agent = bin.join("fake-agent-xyz");
        std::fs::write(&agent, "#!/bin/sh\n").unwrap();
        let mut perms = std::fs::metadata(&agent).unwrap().permissions();
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o755);
        }
        std::fs::set_permissions(&agent, perms).unwrap();

        let _guard = EnvGuard::set(&[("HOME", tmp.path().to_str().unwrap())]);
        assert_eq!(resolve_program("fake-agent-xyz"), agent.to_string_lossy());
    }

    /// A name that exists nowhere comes back unchanged, so the spawn error names
    /// what the user asked for rather than a guessed path.
    #[test]
    fn an_unresolvable_name_is_returned_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let _guard = EnvGuard::set(&[("HOME", tmp.path().to_str().unwrap())]);
        assert_eq!(resolve_program("definitely-not-installed-xyz"), "definitely-not-installed-xyz");
    }

    /// A non-executable file of the right name is not a program.
    #[test]
    fn a_non_executable_file_does_not_count() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("not-exec-xyz"), "data").unwrap();
        let _guard = EnvGuard::set(&[("HOME", tmp.path().to_str().unwrap())]);
        assert_eq!(resolve_program("not-exec-xyz"), "not-exec-xyz");
    }

    /// A fake home with the given directories under it, and the environment
    /// pointed at it. `PATH` keeps `/usr/bin:/bin` so a pane spawned while the
    /// guard is held can still find a shell.
    fn fake_home(dirs: &[&str]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for d in dirs {
            std::fs::create_dir_all(tmp.path().join(d)).unwrap();
        }
        tmp
    }

    fn entries(path: &std::ffi::OsString) -> Vec<PathBuf> {
        std::env::split_paths(path).collect()
    }

    /// The fix for the agent that "is in the PATH" and still will not run: the
    /// directory it was found in has to be on the child's `PATH` too, in front,
    /// or its `#!/usr/bin/env node` line picks the distribution's node.
    #[test]
    fn a_missing_user_bindir_goes_in_front() {
        let tmp = fake_home(&[".local/bin"]);
        let _g =
            EnvGuard::set(&[("HOME", tmp.path().to_str().unwrap()), ("PATH", "/usr/bin:/bin")]);
        let got = child_path().expect("a missing directory must be added");
        assert_eq!(
            entries(&got),
            vec![tmp.path().join(".local/bin"), PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
        );
    }

    /// Only the newest node, so a machine with several nvm versions does not get
    /// an old one in front of the CLI that needs a current one.
    #[test]
    fn only_the_newest_nvm_version_is_added() {
        let tmp = fake_home(&[".nvm/versions/node/v18.0.0/bin", ".nvm/versions/node/v22.12.0/bin"]);
        let _g =
            EnvGuard::set(&[("HOME", tmp.path().to_str().unwrap()), ("PATH", "/usr/bin:/bin")]);
        let got = child_path().expect("nvm's bin directory must be added");
        assert_eq!(
            entries(&got),
            vec![
                tmp.path().join(".nvm/versions/node/v22.12.0/bin"),
                PathBuf::from("/usr/bin"),
                PathBuf::from("/bin"),
            ]
        );
    }

    /// A daemon started from a login shell already has these, and its `PATH` is
    /// the user's own choice: fill gaps, never reorder.
    #[test]
    fn a_path_that_already_has_them_is_left_alone() {
        let tmp = fake_home(&[".local/bin"]);
        let path = format!("{}:/usr/bin:/bin", tmp.path().join(".local/bin").display());
        let _g = EnvGuard::set(&[("HOME", tmp.path().to_str().unwrap()), ("PATH", &path)]);
        assert_eq!(child_path(), None, "PATH already had everything; it must not be rewritten");
    }

    /// And when the `PATH` names *a* node version, that is the one the user's
    /// hook chose. Adding the others would shadow it with an older node — this
    /// function's own failure mode, self-inflicted.
    #[test]
    fn an_nvm_version_already_on_path_is_not_second_guessed() {
        let tmp = fake_home(&[".nvm/versions/node/v18.0.0/bin", ".nvm/versions/node/v22.12.0/bin"]);
        let chosen = tmp.path().join(".nvm/versions/node/v18.0.0/bin");
        let path = format!("{}:/usr/bin:/bin", chosen.display());
        let _g = EnvGuard::set(&[("HOME", tmp.path().to_str().unwrap()), ("PATH", &path)]);
        assert_eq!(child_path(), None, "v22 must not be put in front of the chosen v18");
    }

    /// The end of the chain the user actually hits. A managed process runs
    /// through `$SHELL -c`, which reads no rc file, so the `PATH` the daemon
    /// hands it is the only one it will ever have.
    #[test]
    fn a_shell_command_pane_gets_the_login_bin_dirs() {
        use tokio::sync::mpsc::unbounded_channel;

        let tmp = fake_home(&[".local/bin"]);
        let bin = tmp.path().join(".local/bin");
        let _g =
            EnvGuard::set(&[("HOME", tmp.path().to_str().unwrap()), ("PATH", "/usr/bin:/bin")]);

        let (tx, _rx) = unbounded_channel();
        let (otx, mut orx) = tokio::sync::mpsc::channel(64);
        let spec = SpawnSpec {
            pane: PaneId(1),
            ws: SessionId(1),
            socket: Path::new("/tmp/butai-test.sock"),
            program: Some("printf %s \"$PATH\""),
            args: &[],
            env: &[],
            cwd: Path::new("/"),
            shell: "/bin/sh",
            via_shell: true,
            label: None,
            detect: None,
            replay: None,
        };
        let _pane = TerminalPane::spawn(PaneId(1), spec, 24, 200, 100, 0, tx, otx).unwrap();

        let mut out = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !out.starts_with(b"/") && std::time::Instant::now() < deadline {
            match orx.try_recv() {
                Ok((_, b)) => out.extend_from_slice(&b),
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        let printed = String::from_utf8_lossy(&out);
        assert!(
            printed.starts_with(&format!("{}:", bin.display())),
            "the process shell's PATH must start with the added directory, got {printed:?}"
        );
    }
}
