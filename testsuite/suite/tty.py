"""Run a command under a real PTY.

The `butai` TUI, `butai standalone` and `butai reset` all talk to a terminal, so
testing them from `subprocess.run` proves nothing — they need a tty on the
other end. This is the smallest thing that gives them one.

It also keeps a **screen**, not just a byte log, because the two are not the
same thing. The client draws cell by cell — each run of matching style gets its
own cursor move — so a word on screen is almost never a contiguous run of bytes
in the stream. `wait_output` searches the reconstructed grid for that reason;
searching the raw bytes silently never matches, which is exactly the failure
that hid here while the daemon was still composing frames (its writer emitted
whole runs, so the old spelling happened to work).
"""

import os
import pty
import re
import select
import signal
import subprocess
import time

__all__ = ["PtyProcess"]


class PtyProcess:
    """A child process attached to a pseudo-terminal."""

    def __init__(self, argv, env=None, cwd=None, cols=100, rows=30):
        self.argv = list(argv)
        self.env = dict(env or os.environ)
        self.env.setdefault("TERM", "xterm-256color")
        self.cwd = cwd
        self.cols = cols
        self.rows = rows
        self.master = None
        self.proc = None
        self.output = b""
        self.screen = _Screen(cols, rows)

    def __enter__(self):
        self.start()
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    def start(self):
        master, slave = pty.openpty()
        _set_size(slave, self.cols, self.rows)
        self.master = master
        self.proc = subprocess.Popen(
            self.argv,
            env=self.env,
            cwd=self.cwd,
            stdin=slave,
            stdout=slave,
            stderr=slave,
            start_new_session=True,
        )
        os.close(slave)
        return self

    # -- io ----------------------------------------------------------------

    def read(self, timeout=0.5):
        """Drain whatever is available; returns the new bytes."""
        chunks = []
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            try:
                ready, _, _ = select.select([self.master], [], [], remaining)
            except OSError:
                break
            if not ready:
                break
            try:
                chunk = os.read(self.master, 1 << 16)
            except OSError:
                break
            if not chunk:
                break
            chunks.append(chunk)
        new = b"".join(chunks)
        self.output += new
        self.screen.feed(new)
        return new

    def write(self, data):
        os.write(self.master, data if isinstance(data, bytes) else data.encode())

    def text(self):
        """What is on screen, as text — one line per terminal row."""
        return self.screen.text()

    def raw(self):
        """Every byte the child wrote, for a failure message that needs it."""
        return self.output.decode("utf-8", "replace")

    def wait_output(self, needle, timeout=10.0):
        """Read until `needle` appears on the reconstructed screen."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if needle in self.text():
                return True
            self.read(timeout=0.25)
        return needle in self.text()

    # -- lifecycle ---------------------------------------------------------

    def alive(self):
        return self.proc is not None and self.proc.poll() is None

    def wait(self, timeout=10.0):
        try:
            return self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            return None

    def close(self):
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=5)
        if self.master is not None:
            try:
                os.close(self.master)
            except OSError:
                pass
            self.master = None


def _set_size(fd, cols, rows):
    import fcntl
    import struct
    import termios

    packed = struct.pack("HHHH", rows, cols, 0, 0)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, packed)


class _Screen:
    """Just enough terminal to answer "what is on screen?".

    Absolute positioning, erase, newline and backspace — which is all a
    ratatui client emits, because it moves the cursor for every run it draws
    rather than relying on relative motion. Styling is dropped: these tests ask
    what a screen *says*.

    A CSI can carry **intermediate bytes** between its parameters and its final
    letter, and one that butai emits does: `CSI 0 SP q`, the cursor shape the
    workbench sets to say whether the stage is listening. Matching only
    `[0-9;?]*` before the letter missed it, and the leftovers — `[0 q` — were
    then typed into the grid as ordinary text, which is a screen the terminal
    never showed and an assertion failing over a cursor sequence.
    """

    _CSI = re.compile(r"\x1b\[([0-9;?]*)([ -/]*)([@-~])")
    _OSC = re.compile(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)")

    def __init__(self, cols, rows):
        self.cols = cols
        self.rows = rows
        self.x = 0
        self.y = 0
        self.pending = ""
        self.clear()

    def clear(self):
        self.grid = [[" "] * self.cols for _ in range(self.rows)]

    def feed(self, data):
        # A control sequence can be split across reads, so carry the tail.
        raw = self.pending + data.decode("utf-8", "replace")
        self.pending = ""
        i = 0
        while i < len(raw):
            ch = raw[i]
            if ch == "\x1b":
                rest = raw[i:]
                m = self._CSI.match(rest)
                if m:
                    # An intermediate byte means it is not motion or erase —
                    # cursor shape is the only one here. Consumed and dropped,
                    # like styling.
                    if not m.group(2):
                        self._csi(m.group(1), m.group(3))
                    i += m.end()
                    continue
                m = self._OSC.match(rest)
                if m:
                    i += m.end()
                    continue
                # An unterminated escape: keep it for the next read, unless it
                # is long enough that it is simply one we do not model.
                if len(rest) < 32:
                    self.pending = rest
                    return
                i += 1
                continue
            if ch == "\r":
                self.x = 0
            elif ch == "\n":
                self.y += 1
                self.x = 0
                if self.y >= self.rows:
                    self.grid.pop(0)
                    self.grid.append([" "] * self.cols)
                    self.y = self.rows - 1
            elif ch == "\x08":
                self.x = max(0, self.x - 1)
            elif ch >= " ":
                if 0 <= self.y < self.rows and 0 <= self.x < self.cols:
                    self.grid[self.y][self.x] = ch
                self.x += 1
            i += 1

    def _csi(self, args, cmd):
        nums = [int(v) for v in args.split(";") if v.isdigit()]

        def n(idx, default=1):
            return nums[idx] if idx < len(nums) else default

        if cmd in ("H", "f"):
            self.y = max(0, n(0) - 1)
            self.x = max(0, n(1) - 1)
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
            elif 0 <= self.y < self.rows:
                for x in range(self.x, self.cols):
                    self.grid[self.y][x] = " "
                for y in range(self.y + 1, self.rows):
                    self.grid[y] = [" "] * self.cols
        elif cmd == "K" and 0 <= self.y < self.rows:
            mode = n(0, 0)
            span = range(self.x, self.cols) if mode == 0 else range(0, self.cols)
            for x in span:
                self.grid[self.y][x] = " "

    def text(self):
        return "\n".join("".join(row).rstrip() for row in self.grid)
