#!/usr/bin/env python3
"""Capture real butai screens for the landing page, as styled cell grids.

The page does not use screenshots. It stores the cells — character, colour,
weight — and repaints them in the DOM, so what a visitor selects with the mouse
is the text butai actually drew. This script is what makes that claim
reproducible.

    scripts/capture-frames.py --out ../butai-webpage/data/frames.json

**How it works, and why this way.** Until 0.7 this script attached as a framed
client and read *composed* frames off the wire. That path is gone: since
`5a71ce6` the daemon renders a pane and nothing else, and `docs/protocol.md`
says it in one line — "Only a `pane` target receives frames." A session attach
now yields no frames at all, so the old script emitted blank grids.

So it takes the screenshot the way a user's terminal takes one: run the real
TUI under a pty, keep a styled cell grid as its bytes arrive, and write that
grid out. Nothing is composed, nothing is reimplemented, and the only thing it
depends on is the shipped binary and its keymap — which is why it survives a
refactor that moves drawing code around. (`scripts/shoot.py` does the same for
the SVGs in `docs/images/`; this one keeps the extra attributes the page needs
and emits its JSON.)

**Nothing on screen is faked; two things are staged.** The git repo is a real
repo with a real branch, real staged and unstaged edits and an untracked file —
the diffs come from git2 inside the daemon. The agents are shell scripts that
draw what an agent CLI draws: butai reads an agent's state off what its pane
renders, there is no protocol between the two, so a double that draws the same
thing *is* that state as far as the workbench is concerned. See
`testsuite/fakeagents/_lib.sh`, which exists to prove exactly that.

**Verifying a capture.** The client moves the cursor for every styled run, so
**no word on screen is a contiguous run of bytes in the pty stream** — grepping
the raw capture always says no, and that has been mistaken for a broken feature
more than once. Assert on the reconstruction instead: every shot below carries
an `expect` string checked against `Screen.text()`, and `--dump` prints it.

    scripts/capture-frames.py --dump            # print every screen as text
    scripts/capture-frames.py --only main       # one shot, faster
    scripts/capture-frames.py --keep            # leave the daemon up to poke at

Output, consumed by `js/terminal.js`:

    {"pal":    [[fg, bg, modbits], ...],              // interned styles
     "dims":   {name: [cols, rows], ...},
     "frames": {name: [[[text, palIndex], ...], ...]},// rows of runs
     "meta":   [[name, tabLabel, blurbHTML], ...]}    // the hero tab strip
"""

import argparse
import codecs
import fcntl
import json
import os
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import termios
import time
import unicodedata

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)

# The page is painted in `blueprint-dark`, so a cell that arrives with no
# explicit colour has to resolve to that theme's ground/ink. These are the
# literal values in `crates/butai-client/src/theme.rs`, and `js/terminal.js`
# hardcodes the ground as the background it may skip drawing — if they drift,
# the page and the captures drift apart, which is the one thing this is about.
GROUND = "#151a23"
INK = "#dde4ef"

# Mod bits, matching `build()` in `js/terminal.js`.
BOLD, ITALIC, UNDERLINE, DIM = 1, 2, 4, 8


# ---------------------------------------------------------------- the emulator


def display_width(ch):
    if not ch or unicodedata.combining(ch):
        return 0
    return 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1


class Cell:
    __slots__ = ("ch", "fg", "bg", "mods", "cont")

    def __init__(self, ch=" ", fg=None, bg=None, mods=0, cont=False):
        self.ch = ch
        self.fg = fg
        self.bg = bg
        self.mods = mods
        # The trailing column of a wide glyph. It holds no character of its own
        # but it is still a column, and a row that forgot it would shift every
        # cell after it on the line.
        self.cont = cont

    def resolved(self):
        """(fg, bg, modbits) with defaults and `reverse` already applied.

        `reverse` is a terminal attribute rather than a colour, so it is spent
        here and the page never has to know the concept exists.
        """
        fg = self.fg or INK
        bg = self.bg or GROUND
        mods = self.mods
        if mods & _REVERSE:
            fg, bg = bg, fg
            mods &= ~_REVERSE
        return (fg, bg, mods)


# Kept out of the page's mod bits: it is resolved away in `Cell.resolved`.
_REVERSE = 16


class Screen:
    """Enough of a terminal to answer "what does this look like?".

    `testsuite/suite/tty.py`'s `_Screen` keeps the same grid and throws the
    styling away, which is right for an assertion about text and useless for a
    picture. This keeps colour and the four attributes the page can draw.

    butai emits absolute cursor moves, SGR and the alt-screen toggle; the
    relative moves and erases below are handled anyway, because a pane's own
    program (a shell, `git log`) emits whatever it likes and its bytes reach the
    same screen.
    """

    _CSI = re.compile(r"\x1b\[([0-9;?]*)([ -/]*)([@-~])")
    _OSC = re.compile(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)")

    def __init__(self, cols, rows):
        self.cols = cols
        self.rows = rows
        self.x = self.y = 0
        self.fg = self.bg = None
        self.mods = 0
        self.pending = ""
        # A read can end in the middle of a multi-byte character as readily as
        # in the middle of an escape sequence, and a plain `bytes.decode` would
        # answer that with U+FFFD — a replacement glyph baked into the frame the
        # page then paints. An incremental decoder holds the tail instead.
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.clear()

    def clear(self):
        self.grid = [[Cell() for _ in range(self.cols)] for _ in range(self.rows)]

    def snapshot(self):
        out = Screen(self.cols, self.rows)
        out.grid = [[Cell(c.ch, c.fg, c.bg, c.mods, c.cont) for c in row] for row in self.grid]
        return out

    def feed(self, data):
        raw = self.pending + self.decoder.decode(data)
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
                # A sequence split across two reads: hold it and try again with
                # the next chunk. 32 bytes is longer than anything butai emits.
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
            elif ch == "\x07":
                pass
            elif ch >= " ":
                w = display_width(ch)
                if 0 <= self.y < self.rows and 0 <= self.x < self.cols:
                    self.grid[self.y][self.x] = Cell(ch, self.fg, self.bg, self.mods)
                    if w == 2 and self.x + 1 < self.cols:
                        self.grid[self.y][self.x + 1] = Cell(
                            "", self.fg, self.bg, self.mods, cont=True
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
            self.x = max(0, min(self.cols - 1, n(0) - 1))
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
        parts = [int(v) for v in (args or "0").split(";") if v.isdigit()] or [0]
        i = 0
        while i < len(parts):
            p = parts[i]
            if p == 0:
                self.fg = self.bg = None
                self.mods = 0
            elif p == 1:
                self.mods |= BOLD
            elif p == 2:
                self.mods |= DIM
            elif p == 3:
                self.mods |= ITALIC
            elif p == 4:
                self.mods |= UNDERLINE
            elif p == 7:
                self.mods |= _REVERSE
            elif p == 22:
                self.mods &= ~(BOLD | DIM)
            elif p == 23:
                self.mods &= ~ITALIC
            elif p == 24:
                self.mods &= ~UNDERLINE
            elif p == 27:
                self.mods &= ~_REVERSE
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

    # -- reading it back ----------------------------------------------------

    def line(self, y):
        return "".join(c.ch if c.ch and not c.cont else " " for c in self.grid[y])

    def text(self):
        return "\n".join(self.line(y).rstrip() for y in range(self.rows))

    def row_of(self, needle, start=0):
        """Row index of the first line containing `needle`, or None.

        Crops are anchored by what a row *says* rather than by a row number, so
        a rail that grows a line does not silently shift every tile.
        """
        for y in range(start, self.rows):
            if needle in self.line(y):
                return y
        return None

    def crop(self, x0, y0, w, h):
        out = Screen(w, h)
        for y in range(h):
            for x in range(w):
                sy, sx = y0 + y, x0 + x
                if 0 <= sy < self.rows and 0 <= sx < self.cols:
                    c = self.grid[sy][sx]
                    out.grid[y][x] = Cell(c.ch, c.fg, c.bg, c.mods, c.cont)
        return out


# The curated ANSI-16 the reference web client uses (`web/palette.js`), so an
# indexed colour out of a pane looks the way that client would draw it rather
# than however a terminal feels. butai's own chrome is truecolor and never
# reaches this.
ANSI16 = [
    "#0e1116", "#f85149", "#3fb950", "#d29922",
    "#58a6ff", "#bc8cff", "#39c5cf", "#b1bac4",
    "#6e7681", "#ff7b72", "#56d364", "#e3b341",
    "#79c0ff", "#d2a8ff", "#56d4dd", "#f0f6fc",
]


def ansi256(idx):
    if idx < 16:
        return ANSI16[idx]
    if idx < 232:
        idx -= 16
        cube = [0, 95, 135, 175, 215, 255]
        return "#%02x%02x%02x" % (cube[idx // 36], cube[(idx // 6) % 6], cube[idx % 6])
    v = 8 + (idx - 232) * 10
    return "#%02x%02x%02x" % (v, v, v)


# ----------------------------------------------------------------- the encoder


class Palette:
    """Interns (fg, bg, mods) triples so the emitted blob stays small."""

    def __init__(self):
        self.index = {}
        self.list = []

    def id(self, key):
        if key not in self.index:
            self.index[key] = len(self.list)
            self.list.append(list(key))
        return self.index[key]


def encode(screen, pal):
    """Screen -> rows of `[text, palIndex]` runs, merging neighbours that match."""
    rows = []
    for row in screen.grid:
        runs = []
        for cell in row:
            # The filler column of a wide glyph carries no character: the
            # browser gives the glyph its two columns by itself.
            if cell.cont:
                continue
            sid = pal.id(cell.resolved())
            ch = cell.ch or " "
            if runs and runs[-1][1] == sid:
                runs[-1][0] += ch
            else:
                runs.append([ch, sid])
        # A tail of default-styled blanks carries no information; the page
        # renders an empty row as `&nbsp;`.
        while runs and not runs[-1][0].strip() and pal.list[runs[-1][1]][1] == GROUND:
            runs.pop()
        rows.append(runs)
    return rows


def emit(frames, path, meta):
    pal = Palette()
    encoded = {name: encode(s, pal) for name, s in frames.items()}
    blob = {
        "pal": pal.list,
        "dims": {name: [s.cols, s.rows] for name, s in frames.items()},
        "frames": encoded,
        "meta": [m for m in meta if m[0] in encoded],
    }
    os.makedirs(os.path.dirname(os.path.abspath(path)) or ".", exist_ok=True)
    with open(path, "w") as fh:
        json.dump(blob, fh, separators=(",", ":"))
    return os.path.getsize(path)


# -------------------------------------------------------------------- fixtures
#
# A project that looks like something anyone might actually be working on. The
# diff matters more than the code does: the CHANGES rail is only interesting
# when it has volume and a mix of staged, unstaged and untracked.

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

BERTH_AFTER = """use std::fmt;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Id(pub u16);

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "berth-{:03}", self.0)
    }
}

/// Berths that can take a vessel of `draft` metres at `tide` metres,
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

const DEPTHS: [f32; 6] = [8.2, 9.1, 11.4, 7.8, 12.0, 10.3];
"""

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

    pub fn minutes(&self) -> i64 {
        (self.closes - self.opens) / 60
    }
}
"""

TIDE_AFTER = """/// A slack-water window a vessel can be moved in.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Window {
    pub opens: i64,
    pub closes: i64,
}

impl Window {
    pub fn contains(&self, t: i64) -> bool {
        t >= self.opens && t < self.closes
    }

    pub fn minutes(&self) -> i64 {
        (self.closes - self.opens) / 60
    }

    /// The overlap with `other`, or `None` when they do not touch.
    pub fn overlap(&self, other: &Window) -> Option<Window> {
        let opens = self.opens.max(other.opens);
        let closes = self.closes.min(other.closes);
        (opens < closes).then_some(Window { opens, closes })
    }
}
"""

BASE_FILES = {
    "Cargo.toml": '[package]\nname = "shipyard"\nversion = "0.4.1"\nedition = "2021"\n',
    "README.md": "# shipyard\n\nBerth scheduling against the tide table.\n",
    "src/lib.rs": "//! Port scheduling for the shipyard client.\n\n"
                  "pub mod berth;\npub mod manifest;\npub mod tide;\n\n"
                  "/// A berth assignment, resolved against the tide table.\n"
                  "pub struct Assignment {\n    pub berth: berth::Id,\n"
                  "    pub window: tide::Window,\n}\n",
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
    "docs/berthing.md": "# Berthing\n\nHow a vessel is assigned a berth, and what the\n"
                        "harbour master's clearance rule means for the scheduler.\n",
    "tests/tide.rs": """use shipyard::tide::Window;

#[test]
fn contains_is_half_open() {
    let w = Window { opens: 0, closes: 60 };
    assert!(w.contains(0));
    assert!(!w.contains(60));
}
""",
    # Committed with the rest, so the processes come up with the workspace and
    # the CHANGES rail is not showing this rig's own scaffolding.
    ".butai.toml": """[[processes]]
name = "dev"
cmd = "./bin/dev"
ready = "Local:"

[[processes]]
name = "api"
cmd = "./bin/api"
ready = "listening"

[[processes]]
name = "test"
cmd = "./bin/test"
""",
}

# Applied after the first commit, so CHANGES has something to show: two staged,
# three unstaged, one of them untracked.
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
                            "pub mod tide;\n\n"
                            "/// A berth assignment, resolved against the tide table.\n"
                            "pub struct Assignment {\n    pub berth: berth::Id,\n"
                            "    pub window: tide::Window,\n    pub confidence: f32,\n}\n"),
    "src/schedule.rs": ("new", """use crate::{berth, manifest::Manifest, tide, Assignment};

/// Greedily assign berths for `arrivals` within the tide windows given.
pub fn plan(arrivals: &[Manifest], windows: &[tide::Window]) -> Vec<Assignment> {
    let mut out = Vec::new();
    for (vessel, window) in arrivals.iter().zip(windows) {
        let Some(berth) = berth::available(vessel.draft, 1.4).first().copied() else {
            continue;
        };
        out.push(Assignment { berth, window: *window, confidence: 0.82 });
    }
    out
}
"""),
}

# Two processes that come up and stay up, one that fails — a PROCESSES rail
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
    "api": r"""#!/bin/bash
printf '\033[38;5;208m shipyard-api \033[0m 0.4.1\n'
printf '\033[2mmigrations: 14 applied, 0 pending\033[0m\n'
printf '\033[32mlistening\033[0m on 127.0.0.1:8080\n\n'
codes=(200 200 201 200 304 200 404 200)
paths=(/v1/berths /v1/tide /v1/manifests /v1/berths/4 /v1/health /v1/schedule)
i=0
while true; do
  sleep 4
  c=${codes[$((i % 8))]}
  p=${paths[$((i % 6))]}
  col=32; [ "$c" -ge 400 ] && col=31; [ "$c" -ge 300 ] && [ "$c" -lt 400 ] && col=33
  printf '\033[2m%s\033[0m \033[%dm%s\033[0m GET %s \033[2m%dms\033[0m\n' \
    "$(date +%H:%M:%S)" $col $c "$p" $((3 + i % 40))
  i=$((i+1))
done
""",
    "test": r"""#!/bin/bash
printf '   \033[1;32mCompiling\033[0m shipyard v0.4.1 (%s)\n' "$PWD"
sleep 1
printf '    \033[1;32mFinished\033[0m `test` profile [unoptimized + debuginfo] in 2.41s\n'
printf '     \033[1;32mRunning\033[0m unittests src/lib.rs (target/debug/deps/shipyard)\n\n'
printf 'running 9 tests\n'
printf 'test berth::tests::deepest_first ... \033[32mok\033[0m\n'
printf 'test berth::tests::respects_clearance ... \033[32mok\033[0m\n'
printf 'test berth::tests::rejects_shallow ... \033[32mok\033[0m\n'
printf 'test tide::tests::contains_is_half_open ... \033[32mok\033[0m\n'
printf 'test tide::tests::overlap_disjoint ... \033[32mok\033[0m\n'
printf 'test tide::tests::overlap_touching ... \033[31mFAILED\033[0m\n'
printf 'test schedule::tests::plan_is_stable ... \033[31mFAILED\033[0m\n'
printf 'test manifest::tests::parses ... \033[32mok\033[0m\n'
printf 'test manifest::tests::restricted_above_cap ... \033[32mok\033[0m\n\n'
printf '\033[1;31mfailures:\033[0m\n\n'
printf '%s\n' '---- tide::tests::overlap_touching stdout ----'
printf "thread 'main' panicked at src/tide.rs:24:\n"
printf '  \033[31massertion `left == right` failed\033[0m\n'
printf '    left: None\n'
printf '   right: Some(Window { opens: 900, closes: 900 })\n\n'
printf '%s\n' '---- schedule::tests::plan_is_stable stdout ----'
printf "thread 'main' panicked at tests/schedule.rs:15:\n"
printf '  \033[31massertion `left == right` failed\033[0m\n'
printf '    left: 0\n'
printf '   right: 1\n\n'
printf 'test result: \033[1;31mFAILED\033[0m. 7 passed; 2 failed; 0 ignored\n\n'
printf '\033[1;31merror\033[0m: test failed, to rerun pass `-p shipyard --lib`\n'
sleep 3
exit 2
""",
}


# -------------------------------------------------------------------- the cast
#
# butai decides what an agent is doing by re-rendering its pane and reading the
# bottom FOOTER_SCAN_ROWS (8) lines for the marker strings in
# `crates/butai-server/src/pane/terminal.rs` — BUSY_MARKERS for working,
# PROMPT_MARKERS for blocked-on-you, silence for finished. There is no protocol
# between butai and an agent, so a script that draws those lines is, to the
# workbench, an agent in that state. `testsuite/fakeagents/` is the same idea
# with a shorter transcript.

E = "\033"


def sgr(code, text):
    return f"{E}[{code}m{text}{E}[0m"


def DIM_(s):
    return sgr("2", s)


def BOLD_(s):
    return sgr("1", s)


def GREEN(s):
    return sgr("32", s)


def RED(s):
    return sgr("31", s)


def YELLOW(s):
    return sgr("33", s)


def BLUE(s):
    return sgr("34", s)


def CYAN(s):
    return sgr("36", s)


def MAGENTA(s):
    return sgr("35", s)


def ORANGE(s):
    return sgr("38;5;208", s)


def diff_line(no, sign, text):
    """One line of Claude Code's inline edit view: number gutter, then the code."""
    gutter = DIM_(f"{no:>6} ")
    if sign == "+":
        return gutter + GREEN(f"+  {text}")
    if sign == "-":
        return gutter + RED(f"-  {text}")
    return gutter + DIM_("   ") + text


CLAUDE_WORKING = "\n".join([
    GREEN("⏺") + " I'll add the under-keel clearance to the berth filter and sort the",
    "  survivors deepest-first, so the scheduler takes the safest berth greedily.",
    "",
    GREEN("⏺") + " " + BOLD_("Read") + DIM_("(src/berth.rs)"),
    "  " + DIM_("⎿  Read 24 lines"),
    "",
    GREEN("⏺") + " " + BOLD_("Update") + DIM_("(src/berth.rs)"),
    "  " + DIM_("⎿  Updated src/berth.rs with 10 additions and 5 removals"),
    diff_line(11, " ", "/// Berths that can take a vessel of `draft` metres at `tide` metres,"),
    diff_line(12, "+", "/// deepest first so the scheduler can take the safest option greedily."),
    diff_line(13, " ", "pub fn available(draft: f32, tide: f32) -> Vec<Id> {"),
    diff_line(14, "-", "    DEPTHS"),
    diff_line(15, "-", "        .iter()"),
    diff_line(16, "+", "    let mut open: Vec<_> = DEPTHS"),
    diff_line(17, "+", "        .iter()"),
    diff_line(18, " ", "        .enumerate()"),
    diff_line(19, "-", "        .filter(|(_, depth)| **depth + tide >= draft)"),
    diff_line(20, "+", "        .filter(|(_, depth)| **depth + tide >= draft + CLEARANCE)"),
    diff_line(21, "+", "        .collect();"),
    diff_line(22, "+", "    open.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap());"),
    diff_line(23, "+", "    open.into_iter().map(|(i, _)| Id(i as u16)).collect()"),
    diff_line(24, " ", "}"),
    "",
    GREEN("⏺") + " " + BOLD_("Bash") + DIM_("(cargo test -p shipyard berth)"),
    "  " + DIM_("⎿  running 2 tests"),
    "     test berth::tests::deepest_first ... " + GREEN("ok"),
    "     test berth::tests::respects_clearance ... " + GREEN("ok"),
    "",
    GREEN("⏺") + " Now the tide overlap. " + BOLD_("overlap_touching") + " expects an empty window",
    "  where the implementation returns None — checking which one `schedule::plan`",
    "  actually relies on before I pick a side.",
    "",
    GREEN("⏺") + " " + BOLD_("Search") + DIM_('(pattern: "overlap\\(", path: "src")'),
    "  " + DIM_("⎿  Found 3 files"),
    "",
    MAGENTA("✻") + " Cogitating… " + DIM_("(18s · ↑ 4.1k tokens · esc to interrupt)"),
    "",
    DIM_("╭──────────────────────────────────────────────────────────────────────╮"),
    DIM_("│") + " > " + " " * 67 + DIM_("│"),
    DIM_("╰──────────────────────────────────────────────────────────────────────╯"),
    # Claude Code's own footer hint. It sits below the composer, so it pushes
    # the `esc to interrupt` marker one row further from the bottom — still
    # well inside FOOTER_SCAN_ROWS (8), which is what makes the row read
    # `working`. Anything added here has to keep that true.
    DIM_("  ? for shortcuts") + " " * 37 + DIM_("opus · /tmp/shipyard"),
])

CODEX_WAITING = "\n".join([
    ORANGE("▌") + " Read " + BOLD_("src/tide.rs") + DIM_(" · 41 lines"),
    ORANGE("▌") + " Read " + BOLD_("tests/schedule.rs") + DIM_(" · 16 lines"),
    "",
    # Every line here is kept under 76 columns on purpose: a pty pane cannot
    # reflow, so at phone width a longer one wraps and the frame reads as a
    # layout defect in butai rather than as an agent that drew a wide line.
    "  " + BOLD_("Window::overlap") + " returns None for touching windows, and the test",
    "  wants an empty window back. The scheduler reads None as \"no berth\"",
    "  and quietly drops a vessel that could have moved at slack water.",
    "",
    "  I want to run the suite to confirm the drop before changing the contract.",
    "",
    DIM_("╭──────────────────────────────────────────────────────────────────────╮"),
    DIM_("│") + " Allow codex to run " + BOLD_("`cargo test -p shipyard`") + "?" + " " * 25 + DIM_("│"),
    DIM_("│") + " " * 70 + DIM_("│"),
    DIM_("│") + " " + CYAN("❯ 1. Yes, run it") + " " * 53 + DIM_("│"),
    DIM_("│") + "   2. Yes, and don't ask again this session" + " " * 27 + DIM_("│"),
    DIM_("│") + "   3. No, keep going without it" + " " * 39 + DIM_("│"),
    DIM_("╰──────────────────────────────────────────────────────────────────────╯"),
    DIM_("  Enter to select · ↑/↓ to navigate · Esc to cancel"),
])

GEMINI_WORKING = "\n".join([
    BLUE("✦") + " Reading the tide table fixtures to work out which window shape the",
    "  scheduler is actually given at runtime.",
    "",
    "  " + DIM_("ReadManyFiles  tests/fixtures/*.json  ") + GREEN("14 files"),
    "  " + DIM_("SearchText     'slack'                ") + GREEN("6 matches"),
    "",
    BLUE("✦") + " The fixtures never contain a zero-length window, which is why the bug",
    "  survived. Generating one to reproduce it.",
    "",
    CYAN("⠹") + " Thinking… " + DIM_("(esc to cancel, 6s)"),
    "",
    DIM_("  > type your message"),
])

AMP_DONE = "\n".join([
    CYAN("▪") + " " + BOLD_("edit") + DIM_("  README.md") + "  " + GREEN("+7") + DIM_(" -0"),
    "",
    "  Documented the clearance rule under its own heading, and wrote down the",
    "  part that was only ever in the commit message: the margin applies at the",
    "  lowest point of the window, not at slack water.",
    "",
    CYAN("▪") + " " + BOLD_("shell") + DIM_("  git add README.md"),
    "",
    "  Staged, since it is only prose.",
    "",
    "  Two things I did " + BOLD_("not") + " touch:",
    "",
    "  " + YELLOW("·") + " schedule::plan still assumes a berth is free for the whole window",
    "  " + YELLOW("·") + " Manifest::restricted is unused until the outer-berth rule lands",
    "",
    DIM_("─" * 70),
    GREEN("✓") + " done " + DIM_("· 4 tools · 11.2s · 2.4k tokens"),
    "",
    DIM_("› "),
])

# The turn `AMP_DONE` is the end of. Drawn first, so the daemon sees a real
# working -> quiet transition and the row reports `finished` rather than `idle`:
# `core.rs` only calls it a turn if a marker was up or it ran for MIN_TURN.
AMP_BUSY = "\n".join([
    CYAN("▪") + " " + BOLD_("edit") + DIM_("  README.md") + "  " + GREEN("+7") + DIM_(" -0"),
    "",
    CYAN("⣟") + " working " + DIM_("(6s · ctrl-c to interrupt)"),
])

# Never worked, so it is `idle` rather than `finished` — a different row and a
# different colour, and a distinction the page is specifically about.
OPENCODE_IDLE = "\n".join([
    DIM_("  opencode") + "  " + DIM_("v0.4.9  ·  /tmp/shipyard  ·  berth-clearance"),
    "",
    DIM_("  /help") + " for commands   " + DIM_("/model") + " to switch   "
    + DIM_("/undo") + " to revert",
    "",
    DIM_("─" * 70),
    "",
    DIM_("› ask me something about this repo"),
])

AIDER_EXIT = "\n".join([
    DIM_("aider v0.74.2"),
    DIM_("Model: claude-opus-5 with diff edit format"),
    DIM_("Git repo: .git with 8 files"),
    "",
    "Applying edit to src/schedule.rs",
    "",
    RED("Traceback (most recent call last):"),
    '  File "aider/coders/base_coder.py", line 1104, in run',
    "    self.apply_updates()",
    '  File "aider/coders/editblock_coder.py", line 62, in apply_updates',
    "    raise ValueError(err)",
    RED("ValueError") + ": SEARCH block did not match src/schedule.rs",
    "",
    DIM_("The file may have changed since it was added to the chat."),
])

# (name, transcript, wanted state, a busy transcript drawn first). The order is
# the order they are spawned in, which is the order the rail lists them.
CAST = [
    ("claude", CLAUDE_WORKING, "working", None),
    ("codex", CODEX_WAITING, "waiting", None),
    ("gemini", GEMINI_WORKING, "working", None),
    ("amp", AMP_DONE, "finished", AMP_BUSY),
    ("opencode", OPENCODE_IDLE, "idle", None),
    ("aider", AIDER_EXIT, "exited", None),
]

# Every persona is drawn by the same script, which picks its lines from the
# environment rather than a counter: restore respawns every agent at once, so
# anything read-modify-write raced and panes came back wearing each other's
# transcripts.
AGENT_SH = r"""#!/bin/bash
D="$(dirname "$0")"
n="${DEMO_PERSONA:-0}"
FILE="$D/p$n.ans"
[ -f "$FILE" ] || FILE="$D/p0.ans"

# `stty size` rather than `tput`, which needs a TERM this pane may not have.
size() { (stty size 2>/dev/null || echo "40 100") | cut -d' ' -f1; }

draw() {
  rows=$(size)
  lines=$(wc -l < "$1")
  # Push the transcript to the bottom of the pane: butai reads an agent's state
  # from the last eight rendered rows, and a real CLI's status line is down
  # there because its own output filled the screen above it. A transcript taller
  # than the pane is left to scroll, exactly as the real thing would.
  pad=$((rows - lines - 1))
  printf '\033[2J\033[H'
  i=0
  while [ "$i" -lt "$pad" ]; do echo ""; i=$((i + 1)); done
  cat "$1"
}

# A turn that ran before this one settled, so the daemon has a working->quiet
# transition to report as `finished`. AGENT_SETTLE and MIN_TURN are 3s each.
if [ -f "$D/p$n.busy" ]; then
  draw "$D/p$n.busy"
  sleep 5
fi
draw "$FILE"

if [ -n "$DEMO_EXIT" ]; then
  sleep "${DEMO_EXIT_AFTER:-4}"
  exit "$DEMO_EXIT"
fi

# Redraw when the pane changes size, polled rather than trapped on SIGWINCH:
# the trap is not reliably delivered to a script parked in `wait`, and a stale
# layout costs the marker its place in the footer band — which reads as an agent
# that quietly went idle. Polling `stty` prints nothing, so the pane stays
# quiet and the finished/idle personas keep their state.
last=$(size)
while true; do
  sleep 1
  cur=$(size)
  if [ "$cur" != "$last" ]; then
    last="$cur"
    draw "$FILE"
  fi
done
"""

CONFIG = """[general]
default_shell = "/bin/bash"
exit_when_empty = false
scrollback = 5000

[theme]
name = "blueprint-dark"

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
        os.makedirs(os.path.dirname(full) or work, exist_ok=True)
        with open(full, "w") as fh:
            fh.write(body)

    binp = os.path.join(work, "bin")
    os.makedirs(binp)
    for name, body in BIN.items():
        p = os.path.join(binp, name)
        with open(p, "w") as fh:
            fh.write(body)
        os.chmod(p, 0o755)

    # The personas live outside the repo: anything this rig writes into the
    # workspace shows up in CHANGES as an untracked file, and a page selling the
    # git rail should not be showing its own scaffolding.
    agent_dir = os.path.join(home, "agents")
    os.makedirs(agent_dir)
    for i, (_, transcript, _, busy) in enumerate(CAST):
        with open(os.path.join(agent_dir, "p%d.ans" % i), "w") as fh:
            fh.write(transcript + "\n")
        if busy:
            with open(os.path.join(agent_dir, "p%d.busy" % i), "w") as fh:
                fh.write(busy + "\n")
    runner = os.path.join(agent_dir, "agent.sh")
    with open(runner, "w") as fh:
        fh.write(AGENT_SH)
    os.chmod(runner, 0o755)

    # One `[[agents]]` entry per cast member, pinned to its persona through the
    # environment. That block is the documented way to teach butai a CLI it does
    # not ship configured, which is exactly what `amp` and `opencode` are here
    # for: an agent is an ordinary command in a pty pane.
    blocks = []
    for i, (name, _, state, _) in enumerate(CAST):
        env = ['DEMO_PERSONA = "%d"' % i]
        if state == "exited":
            env += ['DEMO_EXIT = "1"', 'DEMO_EXIT_AFTER = "6"']
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
        full = os.path.join(work, path)
        os.makedirs(os.path.dirname(full) or work, exist_ok=True)
        with open(full, "w") as fh:
            fh.write(body)
        if kind == "stage":
            sh(["git", "add", path], work, env)


# --------------------------------------------------------------------- the rig


class Rig:
    """An isolated daemon, plus the real TUI attached to it through a pty."""

    def __init__(self, binary, home, work):
        self.binary = binary
        self.home = home
        self.work = work
        self.socket = os.path.join(home, "b.sock")
        self.daemon = None
        self.tui = None
        self.master = None
        self.screen = None

    def env(self):
        env = dict(os.environ)
        # A pane inherits this, and inheriting the *outer* butai's values would
        # point every `butai` command run inside the capture at the wrong
        # daemon — including this script's own teardown.
        for var in ("BUTAI", "BUTAI_WORKSPACE", "BUTAI_PANE", "BUTAI_SOCKET"):
            env.pop(var, None)
        env.update({
            "HOME": self.home,
            "BUTAI_SOCKET": self.socket,
            "TERM": "xterm-256color",
            "COLORTERM": "truecolor",
            # This HOME has no dotfiles, and zsh answers that with its first-run
            # setup wizard — which would be on camera in the shell pane.
            "SHELL": "/bin/bash",
        })
        return env

    # -- the daemon ---------------------------------------------------------

    def start_daemon(self):
        if len(self.socket) > 100:
            raise SystemExit(
                "socket path is %d bytes, over sun_path's budget: %s\n"
                "pass a shorter --home." % (len(self.socket), self.socket)
            )
        log = open(os.path.join(self.home, "daemon.log"), "ab")
        self.daemon = subprocess.Popen(
            [self.binary, "--socket", self.socket, "daemon"],
            env=self.env(), cwd=self.work, stdin=subprocess.DEVNULL,
            stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
        )
        deadline = time.time() + 25
        while time.time() < deadline:
            if os.path.exists(self.socket) and self.cli("--json", "ls").returncode == 0:
                return
            if self.daemon.poll() is not None:
                raise SystemExit("daemon exited %s; see %s/daemon.log"
                                 % (self.daemon.returncode, self.home))
            time.sleep(0.1)
        raise SystemExit("daemon never became ready")

    def cli(self, *args, **kw):
        return subprocess.run(
            [self.binary, "--socket", self.socket, *args],
            env=self.env(), cwd=kw.get("cwd", self.work),
            capture_output=True, text=True, timeout=kw.get("timeout", 30),
        )

    def json(self, *args):
        out = self.cli("--json", *args)
        if out.returncode != 0:
            return None
        try:
            return json.loads(out.stdout)
        except json.JSONDecodeError:
            return None

    def kill_server(self):
        # By socket. `pkill -f "butai daemon"` matches whatever daemon the
        # person running this is actually working in, and has killed one before.
        subprocess.run([self.binary, "--socket", self.socket, "kill-server"],
                       env=self.env(), capture_output=True, timeout=25)

    # -- the client ---------------------------------------------------------

    def attach(self, cols, rows):
        """Start a fresh TUI at this size and reconstruct its screen.

        A fresh client per size rather than a `TIOCSWINSZ` on the master: the
        child is `setsid`-ed and never opens the slave itself, so it has no
        controlling terminal and SIGWINCH is not reliably delivered to it.
        Re-attaching is two seconds and always works.
        """
        import pty

        self.detach()
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        self.master = master
        self.screen = Screen(cols, rows)
        self.tui = subprocess.Popen(
            [self.binary, "--socket", self.socket, "new"],
            env=self.env(), cwd=self.work,
            stdin=slave, stdout=slave, stderr=slave, start_new_session=True,
        )
        os.close(slave)
        self.pump(4.0)
        return self.screen

    def resize(self, cols, rows):
        """Change the pty's window size and tell the client about it.

        SIGWINCH is sent explicitly rather than left to the kernel: the client
        is `setsid`-ed and never opens the slave itself, so it has no
        controlling terminal and the automatic delivery cannot be relied on.
        crossterm hooks the signal, so an explicit one is enough — and resizing
        beats re-attaching because the stage selection is *client* state, and a
        fresh client comes up looking at the shell. That is how the phone-width
        frames ended up photographing an empty prompt.
        """
        if (self.screen.cols, self.screen.rows) == (cols, rows):
            # Nothing to do — and doing it anyway is a trap: the client repaints
            # only what changed, so a fresh blank grid here would collect a
            # damage diff against a screen it never had and come out in pieces.
            self.pump(0.6)
            return self.screen
        fcntl.ioctl(self.master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        self.screen = Screen(cols, rows)
        if self.tui and self.tui.poll() is None:
            self.tui.send_signal(signal.SIGWINCH)
        self.pump(2.5)
        return self.screen

    def detach(self):
        if self.tui and self.tui.poll() is None:
            self.tui.send_signal(signal.SIGTERM)
            try:
                self.tui.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.tui.kill()
        self.tui = None
        if self.master is not None:
            try:
                os.close(self.master)
            except OSError:
                pass
            self.master = None

    def pump(self, seconds):
        """Read the pty until it goes quiet, or `seconds` elapse."""
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

    def send(self, data, settle=0.7):
        """Type at the client.

        One `write` per key, deliberately: `alt-o` is `ESC o`, and crossterm
        only reads it as Alt when both bytes arrive in the same read. Split
        across two writes it is Escape followed by a letter.
        """
        os.write(self.master, data.encode() if isinstance(data, str) else data)
        self.pump(settle)

    def keys(self, seq, settle=0.7):
        for k in seq:
            self.send(k, settle)

    def click(self, x, y, times=1, settle=0.6):
        """An SGR-1006 mouse press and release at a cell.

        The client turns mouse reporting on (`\\x1b[?1002h\\x1b[?1006h` in
        `term.rs`), so this is the same event a terminal would send. Clicking is
        how this script selects a row: a rail's cursor is a *highlight*, not a
        glyph, so it cannot be read back out of the reconstruction, and counting
        `j` presses is how a shot silently ends up photographing its neighbour.
        A second click on an already-selected row is what opens it.
        """
        for _ in range(times):
            os.write(self.master, ("\x1b[<0;%d;%dM\x1b[<0;%d;%dm"
                                   % (x + 1, y + 1, x + 1, y + 1)).encode())
            self.pump(settle)

    def close(self):
        self.detach()
        self.kill_server()
        if self.daemon and self.daemon.poll() is None:
            try:
                self.daemon.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.daemon.terminate()


ESC = "\x1b"


def ALT(ch):
    return ESC + ch


ENTER = "\r"

# The three viewports the page shows the same workspace at. 80 columns for the
# phone: below ~88 the client drops the rails and keeps the stage, and that
# frame is the proof it is one layout rather than three.
BIG = (168, 44)
MED = (104, 32)
SMALL = (80, 26)
# BOOTH's four columns need the room; see the shot below.
WIDE = (180, 44)


# -------------------------------------------------------------------- the shots


def probe(rig, spec, log):
    """Type a `|`-separated key script and dump the screen wherever it says to.

    Working out the keys for a new frame is the fiddly part of adding one, and
    guessing costs a four-minute run each time. This does it interactively
    against one staged daemon:

        --probe 'alt-o|!|j*3|!|enter|!'

    Tokens: `alt-x`, `ctrl-x`, `enter`, `esc`, `tab`, `space`, `bs`, a literal
    string, `TOKEN*N` to repeat, `~2` to wait, `!` to print the screen.
    """
    named = {"enter": "\r", "esc": ESC, "tab": "\t", "space": " ", "bs": "\x7f"}
    for token in spec.split("|"):
        token = token.strip()
        if not token:
            continue
        times = 1
        if "*" in token:
            token, _, n = token.rpartition("*")
            times = int(n)
        for _ in range(times):
            if token == "!":
                log("--- after %r\n%s" % (spec, rig.screen.text()))
            elif token.startswith("~"):
                rig.pump(float(token[1:] or 1))
            elif token.startswith("alt-"):
                rig.send(ALT(token[4:]), 0.7)
            elif token.startswith("ctrl-"):
                rig.send(chr(ord(token[5:].lower()) - 96), 0.7)
            elif token in named:
                rig.send(named[token], 0.7)
            else:
                for ch in token:
                    rig.send(ch, 0.4)


def wait_for_states(rig, ws, log, timeout=120):
    """Block until every state the page claims is on the rail at once.

    `finished` is the slow one: it needs a working turn to have ended and then
    AGENT_SETTLE to pass, so it arrives long after the others.
    """
    # `agent ls` delegates to `pane ls`, whose DTO has no attention state — it
    # lists processes too. `workspace show` is the route that carries it.
    want = {s for _, _, s, _ in CAST}
    deadline = time.time() + timeout
    seen = []
    while time.time() < deadline:
        detail = rig.json("workspace", "show", str(ws)) or {}
        rows = detail.get("agents") or []
        seen = ["exited" if r.get("exited") is not None else r.get("state") for r in rows]
        if len(rows) >= len(CAST) and want.issubset(set(seen)):
            log("  states: %s" % ", ".join(str(s) for s in seen))
            return True
        time.sleep(1.5)
    log("  warning: states %s after %ds — wanted all of %s"
        % (seen or "none", timeout, sorted(want)))
    return False


def find_cell(screen, needle, after=0, x0=0):
    """(x, y) of `needle` on the screen, or None. Anchors every click.

    `x0` restricts the search to one column band — everything at or right of
    it. A filename is not unique on this screen: `src/berth.rs` is a row in
    the CHANGES rail *and* a line in claude's transcript on the stage, and an
    unconstrained top-down scan clicks whichever the layout happens to put
    higher. That is how `diff` and `diff_lines` came out of a run missing
    while `diff_hunk`, whose file only ever appears in the rail, came out
    fine. Pass the rail's own x from `geometry()` when clicking a rail row.
    """
    for y in range(after, screen.rows):
        x = screen.line(y).find(needle, x0)
        if x >= 0:
            return (x, y)
    return None


def click_text(rig, needle, times=1, log=print, after=0, x0=0):
    """Click the row that says `needle`. Returns False when it is not there."""
    at = find_cell(rig.screen, needle, after, x0)
    if at is None:
        log("  warning: nothing on screen says %r; skipping the click" % needle)
        return False
    rig.click(at[0] + 1, at[1], times=times)
    return True


def titles(screen):
    """The row of box openings, which is where every pane says what it holds.

    The stage's title is the *content's* name, not the word STAGE — a diff is
    `┌ diff src/berth.rs`, a file is the file, BOOTH's middle column is the
    agent. Reading the wrong row here is what made the last revision of this
    script quietly photograph an agent where it claimed a diff.
    """
    y = screen.row_of("┌ CHANGES")
    if y is None:
        y = screen.row_of("┌ ")
    return screen.line(y) if y is not None else ""


def stage_diff(rig, want, log, x0=0):
    """Put one named file's diff on the stage, checked by the pane's own title.

    Enter, not `d`: `handle_changes_key` in `crates/butai-client/src/workbench.rs`
    has arms for `s`, `u`, `x`, `o`, `t`, `a`, `c` and `p` and none for `d`,
    though `changes_verbs` advertises `d diff` in the footer under the rail.
    Enter is `Flow::OpenSelectedDiff` and always works. (Worth fixing in the
    client; noted in the landing page's CAPTURING.md.)

    `x0` is the CHANGES rail's left edge. Without it the click can land on the
    same filename printed by whatever is on the stage — see `find_cell`.
    """
    rig.send(ALT("g"), 0.7)
    if not click_text(rig, want, times=2, log=log, x0=x0):
        return False
    rig.pump(1.5)
    if want in titles(rig.screen):
        return True
    # A first click that only moved the cursor: ask again with Enter.
    rig.send(ENTER, 1.5)
    if want in titles(rig.screen):
        return True
    log("  warning: never landed on a diff of %r" % want)
    return False


def geometry(screen, log):
    """Where each rail starts and how wide it is, read off the frame itself.

    The top row of boxes carries every opening — `┌ VIEWS`, `┌ AGENTS`,
    `┌ STAGE` and `┌ CHANGES` — so one row gives every column boundary in the
    layout. Measured rather than assumed twice over: the rail widths are
    configurable, and the VIEWS rail did not exist when this script was first
    written, so a tile anchored at column 0 photographed the wrong box.
    """
    y = screen.row_of("AGENTS")
    row = screen.line(y) if y is not None else ""
    agents_x = row.find("┌ AGENTS")
    stage_x = row.find("┌ STAGE")
    chg_x = row.find("┌ CHANGES")
    if min(agents_x, stage_x, chg_x) < 0:
        log("  warning: could not read the rail boundaries; falling back")
        agents_x, stage_x, chg_x = 0, 28, screen.cols - 38
    rail = stage_x - agents_x
    return {
        "c_agents": (agents_x, rail, 9),
        "c_procs": (agents_x, rail, 8),
        "c_stage": (stage_x, min(72, chg_x - stage_x), 11),
        "c_changes": (chg_x, screen.cols - chg_x, 14),
    }


# The hero's tab strip. Each entry is [frame name, tab label, blurb HTML].
META = [
    ["main", "Mission control",
     "Five states at once. One agent is blocked — the rail says so, and the "
     "footer names it from anywhere in the workbench."],
    ["diff", "Review",
     "The diff on the stage, driven from the rail. <b>s</b> stages, <b>c</b> commits, "
     "<b>p</b> pushes."],
    ["proc", "Processes",
     "Dev server, API and tests, declared in one file and supervised. The failing run "
     "is one Enter away."],
    ["files", "Files",
     "A second space: the tree marks what changed, and the viewer is the daemon's own "
     "buffer."],
    ["booth_wide", "Booth",
     "One page over every machine you are connected to — the fleet, what needs you, "
     "the live screen, and what it is all running on."],
    ["help", "Every key",
     "One reference holds the entire keymap. There is one layout, so this is all of it."],
    ["zen", "Zen",
     "<b>Alt-z</b> collapses both rails. A blocked agent is still one glance away."],
]


def capture(rig, ws, wanted, log, dump=False):
    """Drive the real client through every view the page shows."""
    frames = {}

    def keep(name, screen, expect=None):
        # The client draws cell by cell, so the raw pty bytes contain no word on
        # screen. The reconstruction is the only thing worth asserting on.
        if expect and expect not in screen.text():
            log("  ! %s: expected %r on screen — capture is suspect" % (name, expect))
        frames[name] = screen
        log("  %-14s %d×%d" % (name, screen.cols, screen.rows))
        if dump:
            log("--- %s\n%s" % (name, screen.text()))

    def want(name):
        return not wanted or name in wanted

    rig.attach(*BIG)

    # Put the *working* agent on the stage. The rail's cursor lands on whichever
    # pane was created last, which is the one that exited — a dead agent is the
    # least interesting thing this can be showing.
    rig.send(ALT("a"), 0.7)
    click_text(rig, "claude", times=2, log=log)
    rig.pump(2.0)
    main = rig.screen.snapshot()

    if want("main"):
        keep("main", main, "AGENTS")

    # The crops are sub-rectangles of that very frame, both edges measured off
    # it rather than assumed.
    cols = geometry(main, log)
    for name, needle in (("c_agents", "AGENTS"), ("c_procs", "PROCESSES"),
                         ("c_changes", "CHANGES"), ("c_stage", "STAGE")):
        if not want(name):
            continue
        y = main.row_of(needle)
        if y is None:
            log("  warning: no row containing %r; skipping %s" % (needle, name))
            continue
        x, w, h = cols[name]
        keep(name, main.crop(x, y, w, h), needle)

    # The CHANGES rail's footer is contextual — it lists the verbs of whatever
    # row the cursor is on. Two crops of the same list with different rows
    # selected is the cheapest way to show that, and it is a claim about the
    # interface rather than about this repo.
    if want("c_changes_unstaged") or want("c_changes_staged"):
        cx, cw, _ = cols["c_changes"]
        rig.send(ALT("g"), 0.7)
        # One click selects; the tile has to reach the bottom of the column
        # because that is where the contextual verbs are written.
        for name, row in (("c_changes_unstaged", "src/berth.rs"),
                          ("c_changes_staged", "README.md")):
            if not want(name):
                continue
            if not click_text(rig, row, times=1, log=log, x0=cx):
                continue
            rig.pump(0.8)
            y = rig.screen.row_of("┌ CHANGES")
            if y is None:
                continue
            # To the box's own bottom border, not past it into the workbench
            # footer — the tile is one rail, not a slice of the whole screen.
            keep(name, rig.screen.snapshot().crop(cx, y, cw, rig.screen.rows - y - 1))

    if want("diff") or want("diff_lines"):
        if stage_diff(rig, "src/berth.rs", log, x0=cols["c_changes"][0]):
            rig.pump(1.2)
            if want("diff"):
                keep("diff", rig.screen.snapshot(), "CHANGES")
            # The diff's own keys belong to the diff pane, so the cursor has to
            # be on the stage: `C-b s`. `v` enters line-select, where `space`
            # picks and `enter` applies — partial staging without leaving here.
            if want("diff_lines"):
                rig.send("\x02", 0.4)
                rig.send("s", 1.0)
                rig.send("v", 1.0)
                for _ in range(4):
                    rig.send("j", 0.25)
                rig.send(" ", 0.8)
                keep("diff_lines", rig.screen.snapshot(), "berth.rs")
                rig.send(ESC, 0.6)
                rig.send(ESC, 0.8)

    # A *staged* file's diff, with the cursor walked to its second hunk: the
    # rail's verbs read `u unstage` here rather than `s stage`, and `]` has
    # somewhere to go — berth.rs is one hunk, so `]` on it is a no-op and the
    # frame was an exact duplicate of `diff`.
    if want("diff_hunk"):
        if stage_diff(rig, "src/tide.rs", log, x0=cols["c_changes"][0]):
            rig.send("\x02", 0.4)
            rig.send("s", 1.0)
            rig.send("]", 1.2)
            keep("diff_hunk", rig.screen.snapshot(), "tide.rs")
            rig.send(ESC, 0.8)

    if want("proc"):
        rig.send(ALT("p"), 0.7)
        # The PROCESSES list, on the run that failed. `FAIL` rather than the
        # name: the point of the frame is the row that is not green.
        click_text(rig, "FAIL", times=2, log=log)
        rig.pump(1.8)
        keep("proc", rig.screen.snapshot(), "PROCESSES")

    if want("files"):
        rig.send(ALT("o"), 1.5)
        rig.pump(1.2)
        # Down into `src/`, then open the file with the biggest diff. The tree
        # marks a changed file, so this is also the shot that shows the marks.
        if click_text(rig, "src/", times=2, log=log):
            rig.pump(1.2)
        click_text(rig, "berth.rs", times=2, log=log)
        rig.pump(1.5)
        keep("files", rig.screen.snapshot(), "berth.rs")
        rig.send(ALT("o"), 1.2)  # each space key toggles back to work

    if want("help"):
        rig.send(ALT("a"), 0.7)
        rig.send("?", 1.8)
        rig.pump(1.2)
        keep("help", rig.screen.snapshot())
        rig.send(ESC, 1.0)

    if want("zen"):
        rig.send(ALT("a"), 0.7)
        click_text(rig, "claude", times=2, log=log)
        rig.send(ALT("z"), 1.5)
        rig.pump(1.2)
        keep("zen", rig.screen.snapshot())
        rig.send(ALT("z"), 1.2)

    # The same second on three screens: one workspace, three viewports.
    #
    # A diff is on the stage rather than an agent, deliberately: a pty pane
    # cannot reflow — the child drew at the width it had — so a narrow capture of
    # one shows the *agent* failing to fit, which is not the claim. The diff pane
    # is the daemon's own, so what the three sizes show is only ever layout.
    if any(want(n) for n in ("size_lg", "size_md", "size_sm")):
        rig.resize(*BIG)
        stage_diff(rig, "src/berth.rs", log, x0=cols["c_changes"][0])
        rig.pump(1.2)
        if want("size_lg"):
            keep("size_lg", rig.screen.snapshot(), "berth.rs")
        for name, size in (("size_md", MED), ("size_sm", SMALL)):
            rig.resize(*size)
            rig.pump(1.0)
            if want(name):
                keep(name, rig.screen.snapshot(), "berth.rs")

    # The same claim with an agent on the stage. Chosen at full width and then
    # shrunk, because below ~88 columns the rails are gone and there is nothing
    # left to pick a pane with — which is the point of the frame.
    if want("size_sm_agent"):
        rig.resize(*BIG)
        rig.send(ALT("a"), 0.8)
        click_text(rig, "codex", times=2, log=log)
        rig.pump(1.5)
        rig.resize(*SMALL)
        # Longer than the rest: a pty pane cannot reflow, so this frame is only
        # honest once the *program* has answered the new size and redrawn. The
        # doubles poll `stty size` once a second, exactly as a real CLI answers
        # SIGWINCH — and a frame taken before that shows the old width's lines
        # wrapped, which reads as a layout bug that is not there.
        rig.pump(5.0)
        keep("size_sm_agent", rig.screen.snapshot(), "STAGE")

    # BOOTH last and widest. It is one page over every machine you are connected
    # to — the fleet, a NEEDS YOU tray, the selected agent's own screen and a
    # COMPUTE column — and it is the one view whose columns are all real at once
    # only when there is room for them.
    if want("booth") or want("booth_wide"):
        rig.resize(*WIDE)
        rig.send(ALT("0"), 1.8)
        rig.pump(2.0)
        # BOOTH's boxes are FLEET / NEEDS YOU / COMPUTE; the word BOOTH itself
        # is only the tab chip, so the assertion names a box.
        if want("booth_wide"):
            keep("booth_wide", rig.screen.snapshot(), "NEEDS YOU")
        if want("booth"):
            rig.resize(*BIG)
            rig.pump(1.5)
            keep("booth", rig.screen.snapshot(), "NEEDS YOU")

    # The kill-server pair, which `js/scenes.js` puts behind a button: the same
    # workspace either side of a real daemon restart. The pair is only worth
    # showing if it is the same claim both times, so the working agent is on the
    # stage in both and the second is not taken until the rail has settled back
    # into the states the first one had.
    if want("restart_before") or want("restart_after"):
        rig.attach(*BIG)
        rig.send(ALT("a"), 0.7)
        click_text(rig, "claude", times=2, log=log)
        rig.pump(2.0)
        keep("restart_before", rig.screen.snapshot(), "AGENTS")

        log("  killing the daemon for real…")
        rig.detach()
        rig.kill_server()          # by socket, never by pattern
        try:
            rig.daemon.wait(timeout=25)
        except subprocess.TimeoutExpired:
            rig.daemon.kill()
        rig.start_daemon()
        log("  daemon back; waiting for restore to settle…")
        wait_for_states(rig, ws, log, timeout=150)

        rig.attach(*BIG)
        rig.send(ALT("a"), 0.7)
        click_text(rig, "claude", times=2, log=log)
        rig.pump(2.5)
        keep("restart_after", rig.screen.snapshot(), "AGENTS")

    return frames


def main():
    ap = argparse.ArgumentParser(
        description=__doc__.splitlines()[0],
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    ap.add_argument("--out", default=os.path.join(HERE, "frames.json"),
                    help="where to write the blob")
    ap.add_argument("--bin", dest="binary",
                    default=os.environ.get("BUTAI_BIN", os.path.expanduser("~/.local/bin/butai")),
                    help="the butai binary to photograph")
    ap.add_argument("--home", default="/tmp/bt",
                    help="throwaway HOME — keep it short, the socket lives under it")
    ap.add_argument("--work", default="/tmp/shipyard",
                    help="the demo workspace, whose path is in every footer")
    ap.add_argument("--only", action="append", help="capture just these frames")
    ap.add_argument("--probe", help="type a key script against the staged daemon "
                                    "and print the screen; see probe() for the grammar")
    ap.add_argument("--dump", action="store_true", help="print each screen as text")
    ap.add_argument("--keep", action="store_true", help="leave the daemon up afterwards")
    args = ap.parse_args()

    if not os.path.exists(args.binary):
        raise SystemExit("no butai binary at %s — pass --bin or set BUTAI_BIN"
                         % args.binary)
    log = print

    log("staging %s" % args.work)
    stage(args.home, args.work)

    rig = Rig(args.binary, args.home, args.work)
    frames = {}
    try:
        rig.start_daemon()
        log("daemon on %s (%s)" % (rig.socket, subprocess.run(
            [args.binary, "--version"], capture_output=True, text=True).stdout.strip()))
        # Open the workspace before any client sees it, so the processes in
        # `.butai.toml` are already up and the rail is not photographed filling.
        ws = rig.json("workspace", "create", "--cwd", args.work, "--name", "shipyard")
        ws_id = (ws or {}).get("id", 1)
        for name, _, _, _ in CAST:
            out = rig.cli("agent", "spawn", name, "-w", str(ws_id), "--background")
            if out.returncode != 0:
                log("  ! agent %s: %s" % (name, out.stderr.strip()[:200]))
            time.sleep(0.8)
        log("  waiting for the cast to take its positions…")
        wait_for_states(rig, ws_id, log)

        if args.probe:
            rig.attach(*BIG)
            probe(rig, args.probe, log)
            return
        frames = capture(rig, ws_id, args.only, log, dump=args.dump)
    finally:
        if args.keep:
            rig.detach()
            log("kept %s — kill it with: %s --socket %s kill-server"
                % (args.home, args.binary, rig.socket))
        else:
            rig.close()
            shutil.rmtree(args.home, ignore_errors=True)
            shutil.rmtree(args.work, ignore_errors=True)

    if not frames:
        raise SystemExit("nothing captured")
    size = emit(frames, args.out, META)
    log("\nwrote %s — %d frames, %.0f KB" % (args.out, len(frames), size / 1024))


if __name__ == "__main__":
    sys.exit(main())
