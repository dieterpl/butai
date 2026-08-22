#!/usr/bin/env python3
"""Photograph the butai workbench: real pty capture in, SVG out.

The client draws the screen now — the daemon only renders panes — so a
screenshot is taken the way a user's terminal takes one: run the TUI under a
pty, keep a styled cell grid as its bytes arrive, and write that grid out. There
is no frame protocol to read and nothing here is composed. (Its predecessor,
`capture-frames.py`, attached as a framed client and read the daemon's composed
frames; those no longer exist.)

    scripts/shoot.py                       # stage, shoot everything, tear down
    scripts/shoot.py --only workbench      # one shot
    scripts/shoot.py --keep                # leave the daemon and repo up
    scripts/shoot.py --out /tmp/pics       # somewhere other than docs/images

It stands up its own daemon under a throwaway `HOME` and talks to it on an
explicit `--socket`, because a daemon on the default paths **restores the real
session** — it would open your workspaces and spawn your agents. Everything it
starts is stopped by socket (`butai --socket … kill-server`), never by process
name: `pkill -f butai` matches the daemon you are actually using.

Keep `--home` short. The socket lives under it and `sockaddr_un.sun_path` is
108 bytes.

Nothing on screen is faked, but two things are *staged*: the git repo (a small
Rust crate with a real branch, staged and unstaged edits and an untracked file)
and the agents, which are shell scripts that draw what a real agent CLI draws.
butai reads agent state off the pane's own output — there is no protocol between
them — so a double that draws the same thing is the same thing as far as the
workbench is concerned. See `testsuite/fakeagents/_lib.sh`.

Verifying a shot: the client moves the cursor for every styled run, so **no word
on screen is a contiguous run of bytes in the pty stream**. Grepping the capture
always says no. Read `Screen.text()` instead — that is the reconstruction, and
`--dump` prints it.
"""

import argparse
import codecs
import os
import re
import shutil
import signal
import subprocess
import sys
import time
import unicodedata

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)

# ---------------------------------------------------------------- the emulator


def display_width(ch):
    if not ch or unicodedata.combining(ch):
        return 0
    return 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1


class Cell:
    __slots__ = ("ch", "fg", "bg", "bold", "italic", "cont")

    def __init__(self, ch=" ", fg=None, bg=None, bold=False, italic=False, cont=False):
        self.ch = ch
        self.fg = fg
        self.bg = bg
        self.bold = bold
        self.italic = italic
        # The trailing column of a wide glyph: it holds no character of its own
        # but it is still a column, and a run that forgot it would shift every
        # cell after it on the line.
        self.cont = cont

    def style(self):
        return (self.fg, self.bg, self.bold, self.italic)


class Screen:
    """Enough of a terminal to answer "what does this look like?".

    `testsuite/suite/tty.py` keeps the same grid and throws the styling away,
    which is right for an assertion about text and useless for a picture. This
    keeps colour and weight, and needs nothing else: butai emits absolute cursor
    moves (`CSI H`), SGR, and the alt-screen toggle. No relative motion, no
    scrolling, and — measured on a real capture — no erase at all.
    """

    _CSI = re.compile(r"\x1b\[([0-9;?]*)([ -/]*)([@-~])")
    _OSC = re.compile(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)")

    def __init__(self, cols, rows):
        self.cols = cols
        self.rows = rows
        self.x = self.y = 0
        self.fg = self.bg = None
        self.bold = self.italic = False
        self.pending = ""
        # Incremental, because a pty read boundary falls wherever the kernel put
        # it: a box-drawing `─` is three bytes and lands split across two chunks
        # often enough to show up as one U+FFFD in the middle of a border. A
        # per-chunk `bytes.decode` cannot see the other half; this buffers it.
        self._decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.clear()

    def clear(self):
        self.grid = [[Cell() for _ in range(self.cols)] for _ in range(self.rows)]

    def feed(self, data):
        raw = self.pending + self._decoder.decode(data)
        self.pending = ""
        i = 0
        while i < len(raw):
            ch = raw[i]
            if ch == "\x1b":
                rest = raw[i:]
                m = self._CSI.match(rest)
                if m:
                    if not m.group(2):  # an intermediate byte means cursor shape
                        self._csi(m.group(1), m.group(3))
                    i += m.end()
                    continue
                m = self._OSC.match(rest)
                if m:
                    i += m.end()
                    continue
                if len(rest) < 32:
                    self.pending = rest
                    return
                i += 1
                continue
            if ch == "\r":
                self.x = 0
            elif ch == "\n":
                self.y = min(self.rows - 1, self.y + 1)
                self.x = 0
            elif ch == "\x08":
                self.x = max(0, self.x - 1)
            elif ch >= " ":
                w = display_width(ch)
                if 0 <= self.y < self.rows and 0 <= self.x < self.cols:
                    self.grid[self.y][self.x] = Cell(
                        ch, self.fg, self.bg, self.bold, self.italic
                    )
                    if w == 2 and self.x + 1 < self.cols:
                        self.grid[self.y][self.x + 1] = Cell(
                            "", self.fg, self.bg, self.bold, self.italic, cont=True
                        )
                self.x += max(1, w)
            i += 1

    def _csi(self, args, cmd):
        if cmd == "m":
            self._sgr(args)
            return
        nums = [int(v) for v in args.split(";") if v.isdigit()]

        def n(idx, default=1):
            return nums[idx] if idx < len(nums) else default

        if cmd in ("H", "f"):
            self.y = max(0, min(self.rows - 1, n(0) - 1))
            self.x = max(0, min(self.cols - 1, n(1) - 1))
        elif cmd == "A":
            self.y = max(0, self.y - n(0))
        elif cmd == "B":
            self.y = min(self.rows - 1, self.y + n(0))
        elif cmd == "C":
            self.x = min(self.cols - 1, self.x + n(0))
        elif cmd == "D":
            self.x = max(0, self.x - n(0))
        elif cmd == "G":
            self.x = max(0, n(0) - 1)
        elif cmd == "J":
            if n(0, 0) in (2, 3):
                self.clear()
            else:
                for x in range(self.x, self.cols):
                    self.grid[self.y][x] = Cell()
                for y in range(self.y + 1, self.rows):
                    self.grid[y] = [Cell() for _ in range(self.cols)]
        elif cmd == "K":
            span = range(self.x, self.cols) if n(0, 0) == 0 else range(self.cols)
            for x in span:
                self.grid[self.y][x] = Cell()

    def _sgr(self, args):
        parts = [int(v) for v in (args or "0").split(";") if v != ""] or [0]
        i = 0
        while i < len(parts):
            p = parts[i]
            if p == 0:
                self.fg = self.bg = None
                self.bold = self.italic = False
            elif p == 1:
                self.bold = True
            elif p == 3:
                self.italic = True
            elif p in (22,):
                self.bold = False
            elif p == 23:
                self.italic = False
            elif p == 39:
                self.fg = None
            elif p == 49:
                self.bg = None
            elif p in (38, 48):
                target = "fg" if p == 38 else "bg"
                if i + 1 < len(parts) and parts[i + 1] == 2:
                    rgb = tuple(parts[i + 2 : i + 5])
                    setattr(self, target, "#%02x%02x%02x" % rgb if len(rgb) == 3 else None)
                    i += 4
                elif i + 1 < len(parts) and parts[i + 1] == 5:
                    setattr(self, target, ansi256(parts[i + 2]))
                    i += 2
            elif 30 <= p <= 37:
                self.fg = ANSI16[p - 30]
            elif 90 <= p <= 97:
                self.fg = ANSI16[p - 90 + 8]
            elif 40 <= p <= 47:
                self.bg = ANSI16[p - 40]
            elif 100 <= p <= 107:
                self.bg = ANSI16[p - 100 + 8]
            i += 1

    def line(self, y):
        return "".join(c.ch if c.ch and not c.cont else " " for c in self.grid[y])

    def text(self):
        return "\n".join(self.line(y).rstrip() for y in range(self.rows))

    def find(self, needle):
        for y in range(self.rows):
            x = self.line(y).find(needle)
            if x >= 0:
                return (x, y)
        return None


ANSI16 = [
    "#151a23", "#f7768e", "#9ece6a", "#e0af68",
    "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6",
    "#414868", "#f7768e", "#9ece6a", "#e0af68",
    "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5",
]


def ansi256(idx):
    if idx < 16:
        return ANSI16[idx]
    if idx < 232:
        idx -= 16
        levels = [0, 95, 135, 175, 215, 255]
        return "#%02x%02x%02x" % (
            levels[idx // 36], levels[(idx // 6) % 6], levels[idx % 6]
        )
    v = 8 + (idx - 232) * 10
    return "#%02x%02x%02x" % (v, v, v)


# --------------------------------------------------------------- the SVG sheet
#
# Metrics chosen so a 120x34 grid lands on 1040x644, which is what the images
# already in `docs/images/` are — a replacement drops in without touching the
# pages that reference it.

CELL_W = 8.4
CELL_H = 18.0
PAD = 16.0
FONT = 14
BASELINE = 13.3  # from the top of the row


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def to_svg(screen, ground, ink, label):
    w = screen.cols * CELL_W + 2 * PAD
    h = screen.rows * CELL_H + 2 * PAD
    out = [
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 %g %g" width="%g" '
        'height="%g" font-family="ui-monospace,SFMono-Regular,Menlo,Consolas,monospace" '
        'font-size="%d" role="img" aria-label="%s">' % (w, h, w, h, FONT, esc(label)),
        # The background travels with the file: these are dark-terminal grids and
        # GitHub will show them on a white page as readily as a dark one.
        '<rect width="%g" height="%g" rx="8" fill="%s"/>' % (w, h, ground),
    ]
    rects, texts = [], []
    for y in range(screen.rows):
        top = PAD + y * CELL_H
        for start, end, cells in runs(screen.grid[y]):
            first = cells[0]
            x = PAD + start * CELL_W
            width = (end - start) * CELL_W
            if first.bg:
                rects.append(
                    '<rect x="%g" y="%g" width="%g" height="%g" fill="%s"/>'
                    % (x, top, width, CELL_H, first.bg)
                )
            body = "".join(c.ch for c in cells if not c.cont)
            if not body.strip():
                continue
            attrs = ' fill="%s"' % (first.fg or ink)
            if first.bold:
                attrs += ' font-weight="700"'
            if first.italic:
                attrs += ' font-style="italic"'
            texts.append(
                '<text x="%g" y="%g"%s textLength="%g" lengthAdjust="spacingAndGlyphs" '
                'xml:space="preserve">%s</text>'
                % (x, top + BASELINE, attrs, width, esc(body))
            )
    # Backgrounds first, then every glyph on top of them — a run's background is
    # one cell taller than its text and would clip the descenders of the line
    # above if the two were interleaved.
    out += rects + texts + ["</svg>"]
    return "\n".join(out) + "\n"


def runs(row):
    """Maximal spans of one style. `textLength` pins each to its own columns.

    Without that pin the viewer's monospace advance decides where a run ends, it
    is never exactly 8.4px, and the error accumulates until a rail's text is
    sitting in its neighbour.
    """
    out = []
    i = 0
    while i < len(row):
        j = i + 1
        style = row[i].style()
        while j < len(row) and row[j].style() == style:
            j += 1
        out.append((i, j, row[i:j]))
        i = j
    return out


# -------------------------------------------------------------------- fixtures

CARGO = '[package]\nname = "shipyard"\nversion = "0.4.1"\nedition = "2021"\n'

BERTH_BEFORE = """use std::fmt;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Id(pub u16);

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "berth-{:03}", self.0)
    }
}

/// Berths that can take a vessel of `draft` metres at `tide` metres.
pub fn available(draft: f32, tide: f32) -> Vec<Id> {
    DEPTHS
        .iter()
        .enumerate()
        .filter(|(_, depth)| **depth + tide >= draft)
        .map(|(i, _)| Id(i as u16))
        .collect()
}

const DEPTHS: [f32; 6] = [8.2, 9.1, 11.4, 7.8, 12.0, 10.3];
"""

BERTH_AFTER = BERTH_BEFORE.replace(
    """/// Berths that can take a vessel of `draft` metres at `tide` metres.
pub fn available(draft: f32, tide: f32) -> Vec<Id> {
    DEPTHS
        .iter()
        .enumerate()
        .filter(|(_, depth)| **depth + tide >= draft)
        .map(|(i, _)| Id(i as u16))
        .collect()
}
""",
    """/// Berths that can take a vessel of `draft` metres at `tide` metres,
/// deepest first so the scheduler can take the safest option greedily.
pub fn available(draft: f32, tide: f32) -> Vec<Id> {
    let mut open: Vec<_> = DEPTHS
        .iter()
        .enumerate()
        .filter(|(_, depth)| **depth + tide >= draft + CLEARANCE)
        .collect();
    open.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap());
    open.into_iter().map(|(i, _)| Id(i as u16)).collect()
}

/// Under-keel clearance the harbour master insists on, in metres.
const CLEARANCE: f32 = 0.6;
""",
)

TIDE_BEFORE = """/// A slack-water window a vessel can be moved in.
#[derive(Copy, Clone, Debug)]
pub struct Window {
    pub opens: i64,
    pub closes: i64,
}

impl Window {
    pub fn contains(&self, t: i64) -> bool {
        t >= self.opens && t < self.closes
    }
}
"""

TIDE_AFTER = TIDE_BEFORE + """
impl Window {
    /// The overlap with `other`, or `None` when they do not touch.
    pub fn overlap(&self, other: &Window) -> Option<Window> {
        let opens = self.opens.max(other.opens);
        let closes = self.closes.min(other.closes);
        (opens < closes).then_some(Window { opens, closes })
    }
}
"""

BASE_FILES = {
    "Cargo.toml": CARGO,
    "README.md": "# shipyard\n\nBerth scheduling against the tide table.\n",
    "src/lib.rs": "//! Port scheduling for the shipyard client.\n\npub mod berth;\n"
                  "pub mod manifest;\npub mod tide;\n",
    "src/berth.rs": BERTH_BEFORE,
    "src/tide.rs": TIDE_BEFORE,
    "src/manifest.rs": """use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct Manifest {
    pub vessel: String,
    pub draft: f32,
    pub containers: u32,
}
""",
    ".butai.toml": """[[processes]]
name = "dev"
cmd = "./bin/dev"
ready = "Local:"

[[processes]]
name = "test"
cmd = "./bin/test"
""",
}

# Applied after the first commit, so CHANGES has something to show: two staged,
# two unstaged, one untracked. A rail with one row in it photographs as an empty
# rail with a line in it.
EDITS = {
    "src/tide.rs": ("stage", TIDE_AFTER),
    "README.md": ("stage", """# shipyard

Berth scheduling against the tide table.

## Clearance

Every assignment keeps 0.6 m of under-keel clearance at the *lowest* point of
the window, not at slack water. `berth::available` applies it; the scheduler
never sees a berth it could not safely use.
"""),
    "src/berth.rs": ("dirty", BERTH_AFTER),
    "src/lib.rs": ("dirty", "//! Port scheduling for the shipyard client.\n\n"
                            "pub mod berth;\npub mod manifest;\npub mod schedule;\n"
                            "pub mod tide;\n"),
    "src/schedule.rs": ("new", """use crate::{berth, manifest::Manifest, tide};

/// Greedily assign berths for `arrivals` within the tide windows given.
pub fn plan(arrivals: &[Manifest], windows: &[tide::Window]) -> Vec<berth::Id> {
    let mut out = Vec::new();
    for (m, w) in arrivals.iter().zip(windows) {
        let _ = w;
        if let Some(id) = berth::available(m.draft, 1.2).first() {
            out.push(*id);
        }
    }
    out
}
"""),
}

# One process that comes up and stays up, one that fails — a PROCESSES rail
# where every row is green says nothing about what the rail is for.
BIN = {
    "dev": r"""#!/bin/bash
printf '\033[38;5;39m  VITE\033[0m v5.4.2  \033[2mready in 412 ms\033[0m\n\n'
printf '  \033[32m\xe2\x9e\x9c\033[0m  \033[1mLocal:\033[0m   http://localhost:5173/\n'
printf '  \033[32m\xe2\x9e\x9c\033[0m  \033[1mNetwork:\033[0m use --host to expose\n\n'
n=0
while true; do
  sleep 6; n=$((n+1))
  printf '\033[2m%s\033[0m \033[36mhmr update\033[0m /src/berth.rs \033[2m(%dms)\033[0m\n' \
    "$(date +%H:%M:%S)" $((18 + n * 3))
done
""",
    "test": r"""#!/bin/bash
printf '   \033[1;32mCompiling\033[0m shipyard v0.4.1 (%s)\n' "$PWD"
printf '    \033[1;32mFinished\033[0m `test` profile in 1.84s\n\n'
printf 'running 9 tests\n'
printf 'test berth::tests::deepest_first ... \033[32mok\033[0m\n'
printf 'test berth::tests::respects_clearance ... \033[32mok\033[0m\n'
printf 'test tide::tests::contains_is_half_open ... \033[32mok\033[0m\n'
printf 'test tide::tests::overlap_touching ... \033[31mFAILED\033[0m\n'
printf 'test schedule::tests::plan_is_stable ... \033[31mFAILED\033[0m\n'
printf 'test manifest::tests::parses ... \033[32mok\033[0m\n\n'
printf 'test result: \033[1;31mFAILED\033[0m. 7 passed; 2 failed; 0 ignored\n'
sleep 3
exit 2
""",
}

# The agents. There is no protocol between butai and an agent CLI: the daemon
# re-renders the pane and reads the last FOOTER_SCAN_ROWS (8) rows for the
# marker strings in `crates/butai-server/src/pane/terminal.rs` — BUSY_MARKERS
# for working, PROMPT_MARKERS for blocked-on-you, and a working->quiet
# transition for finished. A script that draws those lines *is* an agent in that
# state as far as the workbench is concerned; `testsuite/fakeagents/_lib.sh`
# exists to prove exactly that and works the same way.
#
# Two rules the transcripts have to obey, both learned the hard way:
#
#   * Draw at the BOTTOM of the pane. A marker printed at row 1 is outside the
#     footer band and the agent reports idle — which is what every row in the
#     first version of this said.
#   * Fit the stage. At 120 columns the rails take 28 and 38, so the stage is 52
#     wide and a line written for 80 wraps into an unreadable ladder.

E = "\033"


def sgr(code, text):
    return "%s[%sm%s%s[0m" % (E, code, text, E)


def dim(s):
    return sgr("2", s)


def bold(s):
    return sgr("1", s)


def green(s):
    return sgr("32", s)


def red(s):
    return sgr("31", s)


def cyan(s):
    return sgr("36", s)


def magenta(s):
    return sgr("35", s)


def gutter(no, sign, text):
    """One line of Claude Code's inline edit view: a number, then the code."""
    head = dim("%5d " % no)
    if sign == "+":
        return head + green("+  " + text)
    if sign == "-":
        return head + red("-  " + text)
    return head + dim("   ") + text


BOX_W = 50


def box(lines, top="╭", bot="╰"):
    """A rounded input/dialog box, the chrome every agent CLI draws."""
    out = [dim(top + "─" * (BOX_W - 2) + ("╮" if top == "╭" else "╯"))]
    for line in lines:
        out.append(dim("│") + line)
        out[-1] += " " * max(0, BOX_W - 2 - visible(line)) + dim("│")
    out.append(dim(bot + "─" * (BOX_W - 2) + ("╯" if bot == "╰" else "╮")))
    return out


def visible(line):
    """Printable width of a line that carries SGR escapes."""
    out, i = 0, 0
    while i < len(line):
        if line[i] == E:
            j = line.find("m", i)
            i = len(line) if j < 0 else j + 1
            continue
        out += 1
        i += 1
    return out


# Finished: the turn ran, then went quiet. This is the reply the hero shot is
# looking at, so it is the longest of the three.
CLAUDE = "\n".join(
    [
        green("●") + " I'll add the under-keel clearance to the",
        "  berth filter and sort what survives deepest",
        "  first, so the scheduler takes the safest",
        "  berth greedily.",
        "",
        green("●") + " " + bold("Read") + dim("(src/berth.rs)"),
        "  " + dim("⎿  Read 24 lines"),
        "",
        green("●") + " " + bold("Update") + dim("(src/berth.rs)"),
        "  " + dim("⎿  Updated with 8 additions, 3 removals"),
        gutter(12, "+", "/// deepest first, so the"),
        gutter(13, "+", "/// scheduler can take the safest"),
        gutter(14, " ", "pub fn available(draft: f32) -> {"),
        gutter(15, "-", "    DEPTHS"),
        gutter(16, "+", "    let mut open: Vec<_> = DEPTHS"),
        gutter(17, " ", "        .iter()"),
        gutter(18, "-", "        .filter(|d| *d >= draft)"),
        gutter(19, "+", "        .filter(|d| *d >= draft"),
        gutter(20, "+", "            + CLEARANCE)"),
        gutter(21, " ", "}"),
        "",
        green("●") + " " + bold("Bash") + dim("(cargo test -p shipyard berth)"),
        "  " + dim("⎿  running 2 tests"),
        "     test berth::deepest_first ... " + green("ok"),
        "     test berth::respects_clearance ... " + green("ok"),
        "",
        green("●") + " Both berth tests pass. " + bold("overlap_touching"),
        "  was already failing on main — a half-open",
        "  interval bug, not this change.",
        "",
    ]
    + box([" > "])
)

# The turn CLAUDE is the end of. Drawn first and held for a few seconds, so the
# daemon sees a real working -> quiet transition: a pane that was only ever
# silent reports `idle`, which is a different row and a different colour.
CLAUDE_BUSY = "\n".join(
    [
        green("●") + " I'll add the under-keel clearance to the",
        "  berth filter and sort what survives deepest",
        "  first, so the scheduler takes the safest",
        "  berth greedily.",
        "",
        green("●") + " " + bold("Read") + dim("(src/berth.rs)"),
        "  " + dim("⎿  Read 24 lines"),
        "",
        magenta("✻") + " Distilling… " + dim("(9s · esc to interrupt)"),
    ]
)

# Working: the status line's interrupt hint is a BUSY_MARKER, and it holds the
# state for as long as it is on screen — steadier than output recency, which is
# why this script can draw once and then sleep.
CODEX = "\n".join(
    [
        bold("codex") + "  fix the half-open interval in overlap",
        "",
        dim("thinking") + "  the window is [opens, closes), so two",
        dim("thinking") + "  windows that merely touch must not",
        dim("thinking") + "  overlap. `(opens < closes)` is right —",
        dim("thinking") + "  the test asserts the old behaviour.",
        "",
        "  " + cyan("apply_patch") + " src/tide.rs",
        "  " + cyan("shell") + " cargo test -p shipyard tide",
        "",
        magenta("✻")
        + " Working "
        + dim("(24s · ↑ 2.1k tokens · esc to interrupt)"),
    ]
)

# Blocked on you: a permission dialog. The hint line under the options is
# prompt chrome and never prose, so the workbench calls it out immediately —
# the rail row, the tab bar and the footer all say so at once.
GEMINI = "\n".join(
    [
        bold("gemini") + "  regenerate the tide fixture",
        "",
        "  tests/fixtures/tides.bin is 4 MB and checked",
        "  in. I can generate it from data/tides.csv at",
        "  test time instead.",
        "",
    ]
    + box(
        [
            " Delete tests/fixtures/tides.bin?",
            "",
            " " + sgr("7", "❯ 1. Yes"),
            "   2. Yes, and don't ask again",
            "   3. No, keep it checked in",
        ]
    )
    + [dim("  Enter to select · ↑/↓ to navigate · Esc")]
)

# name -> (transcript, the turn to draw first, how long to hold it)
AGENTS = {
    # Spawn order is rail order, and the last one spawned takes the stage — so
    # the one whose reply the hero is looking at is deliberately last.
    "gemini": (GEMINI, None, 0),
    "codex": (CODEX, None, 0),
    "claude": (CLAUDE, CLAUDE_BUSY, 7),
}

# One runner for all three: butai starts an agent as an ordinary command in a
# PTY pane, so this is all an "agent CLI" has to be.
AGENT_SH = r"""#!/bin/bash
# Draw a transcript the way a real agent CLI does — at the bottom of the pane,
# repainted on resize. `stty size` rather than `tput`, which wants a terminfo
# entry this pane may not have.
D="$(dirname "$0")"
size() { (stty size 2>/dev/null || echo "30 80") | cut -d' ' -f1; }

draw() {
  rows=$(size)
  lines=$(wc -l < "$1")
  pad=$((rows - lines - 1))
  printf '\033[2J\033[H'
  i=0
  while [ "$i" -lt "$pad" ]; do echo ""; i=$((i + 1)); done
  cat "$1"
}

if [ -n "$AGENT_BUSY" ]; then
  draw "$D/$AGENT_BUSY"
  sleep "${AGENT_BUSY_FOR:-6}"
fi
draw "$D/$AGENT_TRANSCRIPT"

# Polled rather than trapped on SIGWINCH: the trap is not reliably delivered to
# a script parked in `wait`, and a pane that did not repaint would leave its
# marker stranded above the footer band — an agent that quietly went idle.
last=$(size)
while true; do
  sleep 1
  cur=$(size)
  if [ "$cur" != "$last" ]; then
    last="$cur"
    draw "$D/$AGENT_TRANSCRIPT"
  fi
done
"""


CONFIG = """[general]
default_shell = "/bin/bash"
exit_when_empty = false
scrollback = 5000

%(agents)s"""


def sh(cmd, cwd=None, env=None):
    subprocess.run(cmd, cwd=cwd, env=env, check=True, capture_output=True)


def stage(home, work):
    """Build the throwaway HOME, the demo repo and the agent/process doubles."""
    for path in (home, work):
        shutil.rmtree(path, ignore_errors=True)
    butai_dir = os.path.join(home, ".butai")
    os.makedirs(butai_dir, mode=0o700)
    os.makedirs(work)

    for path, body in BASE_FILES.items():
        full = os.path.join(work, path)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "w") as fh:
            fh.write(body)

    binp = os.path.join(work, "bin")
    os.makedirs(binp)
    for name, body in BIN.items():
        p = os.path.join(binp, name)
        with open(p, "w") as fh:
            fh.write(body)
        os.chmod(p, 0o755)

    agent_dir = os.path.join(home, "agents")
    os.makedirs(agent_dir)
    runner = os.path.join(agent_dir, "agent.sh")
    with open(runner, "w") as fh:
        fh.write(AGENT_SH)
    os.chmod(runner, 0o755)

    blocks = []
    for name, (transcript, busy, hold) in AGENTS.items():
        with open(os.path.join(agent_dir, name + ".ans"), "w") as fh:
            fh.write(transcript + "\n")
        env = ['AGENT_TRANSCRIPT = "%s.ans"' % name]
        if busy:
            with open(os.path.join(agent_dir, name + ".busy"), "w") as fh:
                fh.write(busy + "\n")
            env += ['AGENT_BUSY = "%s.busy"' % name, 'AGENT_BUSY_FOR = "%d"' % hold]
        # One command, three personas, chosen through the environment — which is
        # also how a real `[[agents]]` entry teaches butai a CLI it does not
        # ship configured.
        blocks.append(
            '[[agents]]\nname = "%s"\ncommand = "%s"\nargs = []\nenv = { %s }\n'
            % (name, runner, ", ".join(env))
        )
    with open(os.path.join(butai_dir, "config.toml"), "w") as fh:
        fh.write(CONFIG % {"agents": "\n".join(blocks)})

    env = dict(
        os.environ,
        GIT_AUTHOR_NAME="Harbour", GIT_AUTHOR_EMAIL="h@shipyard",
        GIT_COMMITTER_NAME="Harbour", GIT_COMMITTER_EMAIL="h@shipyard",
        GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1",
    )
    sh(["git", "init", "-q"], work, env)
    # `git init -b` is too new for some of the gits this has to run on.
    sh(["git", "symbolic-ref", "HEAD", "refs/heads/main"], work, env)
    sh(["git", "add", "-A"], work, env)
    sh(["git", "commit", "-qm", "shipyard: berths, tides and manifests"], work, env)
    sh(["git", "checkout", "-qb", "berth-clearance"], work, env)
    for path, (kind, body) in EDITS.items():
        with open(os.path.join(work, path), "w") as fh:
            fh.write(body)
        if kind == "stage":
            sh(["git", "add", path], work, env)


# --------------------------------------------------------------------- the rig


class Rig:
    """An isolated daemon, plus the TUI attached to it through a pty."""

    def __init__(self, binary, home, work, cols, rows):
        self.binary = binary
        self.home = home
        self.work = work
        self.cols = cols
        self.rows = rows
        self.socket = os.path.join(home, "b.sock")
        self.daemon = None
        self.tui = None
        self.master = None
        self.screen = Screen(cols, rows)

    def env(self):
        env = dict(os.environ)
        # A pane inherits this, and inheriting the *outer* butai's values would
        # point every `butai` command run inside the shot at the wrong daemon.
        env.pop("BUTAI", None)
        env.pop("BUTAI_WORKSPACE", None)
        env.pop("BUTAI_PANE", None)
        env.update({
            "HOME": self.home,
            # Explicit, not merely inherited-from-HOME: this script is normally
            # run *inside* butai, where `BUTAI_SOCKET` already points at the
            # daemon holding the operator's real work. A throwaway HOME alone
            # would not stop a single command from reaching it.
            "BUTAI_SOCKET": self.socket,
            # Same reasoning one layer down: nothing this rig starts may read or
            # rewrite the session store that a real daemon restores from.
            "BUTAI_SESSION_FILE": os.path.join(self.home, "session.json"),
            "TERM": "xterm-256color",
            "COLORTERM": "truecolor",
            "RUST_BACKTRACE": "1",
        })
        return env

    def start_daemon(self):
        if len(self.socket) > 100:
            raise SystemExit("socket path %d bytes, over budget: %s" % (len(self.socket), self.socket))
        log = open(os.path.join(self.home, "daemon.log"), "wb")
        self.daemon = subprocess.Popen(
            [self.binary, "--socket", self.socket, "daemon"],
            env=self.env(), cwd=self.work, stdin=subprocess.DEVNULL,
            stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
        )
        deadline = time.time() + 20
        while time.time() < deadline:
            if os.path.exists(self.socket) and self.cli("--json", "ls").returncode == 0:
                return
            if self.daemon.poll() is not None:
                raise SystemExit("daemon exited %s; see %s/daemon.log" % (self.daemon.returncode, self.home))
            time.sleep(0.1)
        raise SystemExit("daemon never became ready")

    def workspace_id(self):
        """The id of the workspace the TUI opened, asked rather than assumed."""
        import json
        out = self.cli("--json", "ws", "ls")
        try:
            rows = json.loads(out.stdout)
        except (ValueError, TypeError):
            return 1
        if isinstance(rows, dict):
            rows = rows.get("workspaces", [])
        return rows[0].get("id", 1) if rows else 1

    def cli(self, *args, **kw):
        return subprocess.run(
            [self.binary, "--socket", self.socket, *args],
            env=self.env(), cwd=kw.get("cwd", self.work),
            capture_output=True, text=True, timeout=kw.get("timeout", 30),
        )

    def start_tui(self):
        import fcntl
        import pty
        import struct
        import termios

        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", self.rows, self.cols, 0, 0))
        self.master = master
        self.tui = subprocess.Popen(
            [self.binary, "--socket", self.socket, "new"],
            env=self.env(), cwd=self.work,
            stdin=slave, stdout=slave, stderr=slave, start_new_session=True,
        )
        os.close(slave)

    def pump(self, seconds):
        import select
        deadline = time.time() + seconds
        while True:
            left = deadline - time.time()
            if left <= 0:
                return
            try:
                ready, _, _ = select.select([self.master], [], [], left)
            except OSError:
                return
            if not ready:
                continue
            try:
                chunk = os.read(self.master, 1 << 16)
            except OSError:
                return
            if not chunk:
                return
            self.screen.feed(chunk)

    def send(self, data, settle=0.6):
        os.write(self.master, data.encode() if isinstance(data, str) else data)
        self.pump(settle)

    def close(self):
        for proc in (self.tui, self.daemon):
            if proc and proc.poll() is None:
                proc.send_signal(signal.SIGTERM)
        if self.master is not None:
            try:
                os.close(self.master)
            except OSError:
                pass


ESC = "\x1b"
ALT = lambda ch: ESC + ch  # noqa: E731


# name -> (what to press to get there, what to press to get back, caption)
SHOTS = {
    "workbench": ([], [], "the workbench"),
    "changes-diff": ([ALT("g"), "j", "d"], [ESC, ALT("a")], "a diff on the stage"),
    "booth": ([ALT("0")], [ALT("0")], "BOOTH"),
    "help": (["?"], [ESC], "the help page"),
    "settings": ([ALT("s")], [ESC], "settings"),
}


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--home", default="/tmp/bshot", help="throwaway HOME (keep it short)")
    ap.add_argument("--out", default=os.path.join(REPO, "docs", "images"))
    ap.add_argument("--cols", type=int, default=120)
    ap.add_argument("--rows", type=int, default=34)
    ap.add_argument("--bin", dest="binary",
                    default=os.environ.get("BUTAI_BIN", os.path.expanduser("~/.local/bin/butai")))
    ap.add_argument("--suffix", default="-current", help="appended to each output name")
    ap.add_argument("--only", action="append", choices=sorted(SHOTS), help="just these shots")
    ap.add_argument("--dump", action="store_true", help="also print each reconstructed screen")
    ap.add_argument("--keep", action="store_true", help="leave the daemon and repo running")
    args = ap.parse_args()

    if not os.path.exists(args.binary):
        raise SystemExit("no butai binary at %s (set --bin or BUTAI_BIN)" % args.binary)
    wanted = args.only or list(SHOTS)
    os.makedirs(args.out, exist_ok=True)

    work = os.path.join(args.home, "repo")
    print("staging %s" % work)
    stage(args.home, work)

    rig = Rig(args.binary, args.home, work, args.cols, args.rows)
    written = []
    try:
        rig.start_daemon()
        print("daemon on %s" % rig.socket)
        rig.start_tui()
        rig.pump(4.0)
        # Spawn the agents from outside, so the rail fills without the shot
        # having to show the picker that put them there. `spawn` takes the agent
        # kind as a positional and defaults to `$BUTAI_WORKSPACE`, which the rig
        # deliberately does not carry — hence the explicit `-w`.
        ws = rig.workspace_id()
        for name in AGENTS:
            out = rig.cli("agent", "spawn", name, "-w", str(ws))
            if out.returncode != 0:
                print("  ! agent %s: %s" % (name, (out.stderr or out.stdout).strip()[:200]))
        # Long enough for the last agent's opening turn to end and for the
        # daemon's settle window to pass, so the rail reports `done` rather than
        # catching it mid-turn. Nothing is driven from the keyboard to get the
        # right pane on the stage: a spawn takes the stage, so spawn order alone
        # decides, and `AGENTS` is ordered with the hero's agent last.
        rig.pump(14.0)

        ground, ink = theme_colors(rig.screen)
        for name in wanted:
            enter, leave, caption = SHOTS[name]
            for key in enter:
                rig.send(key, 0.9)
            rig.pump(1.2)
            if args.dump:
                print("--- %s\n%s" % (name, rig.screen.text()))
            path = os.path.join(args.out, "%s%s.svg" % (name, args.suffix))
            with open(path, "w") as fh:
                fh.write(to_svg(rig.screen, ground, ink, caption))
            written.append(path)
            print("  %-10s %s (%d bytes)" % (name, path, os.path.getsize(path)))
            for key in leave:
                rig.send(key, 0.7)
            rig.pump(0.8)
    finally:
        rig.close()
        # By socket. `pkill -f butai` matches whatever daemon the person running
        # this is actually working in, and has killed one before.
        subprocess.run([args.binary, "--socket", rig.socket, "kill-server"],
                       env=rig.env(), capture_output=True, timeout=20)
        if not args.keep:
            shutil.rmtree(args.home, ignore_errors=True)
        else:
            print("kept %s (kill it with: %s --socket %s kill-server)"
                  % (args.home, args.binary, rig.socket))

    print("\n%d file(s)" % len(written))


def theme_colors(screen):
    """The theme's ground and ink, read off the frame rather than hardcoded.

    butai paints every cell, so the most common background on screen *is* the
    theme's ground, and a cell that came through with no explicit foreground is
    drawn in its ink. Reading them means a re-shoot after a theme change still
    produces a file whose margins match its content.
    """
    from collections import Counter
    bgs = Counter()
    fgs = Counter()
    for row in screen.grid:
        for c in row:
            bgs[c.bg] += 1
            if c.ch.strip():
                fgs[c.fg] += 1
    ground = next((b for b, _ in bgs.most_common() if b), "#151a23")
    ink = next((f for f, _ in fgs.most_common() if f), "#dde4ef")
    return ground, ink


if __name__ == "__main__":
    sys.exit(main())
