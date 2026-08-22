"""A client-side screen grid, built from the daemon's `FrameUpdate` damage.

This is the whole client-side rendering contract: the daemon owns the VT
emulator and ships styled cell runs, so a client only has to paint them. If
this class can reconstruct a readable screen, so can a GUI.

Mirrors `Screen` in `crates/butai-server/tests/e2e_socket.rs`, with three
additions the Rust helper does not need: it honours `full` (clear before
apply), it keeps per-cell style so the SGR/colour probes can assert on
attributes rather than just text, and it advances by each grapheme's **display
width**.

That last one is the part every client author gets wrong once. A run is a
sequence of consecutive graphemes with no filler cell for the second column of
a wide character, so advancing one column per cell shifts everything after the
first CJK or emoji glyph on the line.
"""

import unicodedata

__all__ = ["Screen", "Cell", "display_width"]


def display_width(grapheme):
    """Columns a grapheme occupies — 2 for East Asian wide/fullwidth, else 1.

    A client needs this because the daemon does **not** emit a placeholder cell
    for a wide character's second column: a run carries consecutive graphemes,
    and the reader has to advance by each one's width. Advancing by one per cell
    silently shifts everything after the first CJK or emoji character on a line.
    """
    if not grapheme:
        return 0
    width = 1
    for ch in grapheme:
        if unicodedata.combining(ch):
            continue
        if unicodedata.east_asian_width(ch) in ("W", "F"):
            width = 2
    return width


class Cell:
    __slots__ = ("ch", "fg", "bg", "mods", "cont")

    def __init__(self, ch=" ", fg="default", bg="default", mods=(), cont=False):
        self.ch = ch
        self.fg = fg
        self.bg = bg
        self.mods = frozenset(mods)
        # True for the second column of a wide character. The daemon never sends
        # one; this grid synthesizes it so a column stays a column.
        self.cont = cont

    def __repr__(self):
        return f"Cell({self.ch!r}, fg={self.fg!r}, bg={self.bg!r}, mods={sorted(self.mods)})"


def _blank_row(cols):
    return [Cell() for _ in range(cols)]


class Screen:
    """A cols x rows grid that `apply()` mutates from wire frames."""

    def __init__(self, cols=80, rows=24):
        self.cols = cols
        self.rows = rows
        self.grid = [_blank_row(cols) for _ in range(rows)]
        self.cursor = None
        self.cursor_shape = "block"
        self.frames = 0
        self.full_frames = 0
        self.cells_received = 0

    def resize(self, cols, rows):
        self.cols = cols
        self.rows = rows
        self.clear()

    def clear(self):
        self.grid = [_blank_row(self.cols) for _ in range(self.rows)]

    def apply(self, frame):
        """Apply one `FrameUpdate` dict."""
        self.frames += 1
        if frame.get("full"):
            self.full_frames += 1
            self.clear()
        for run in frame.get("cells") or []:
            x = run["x"]
            y = run["y"]
            for raw in run["cells"]:
                self.cells_received += 1
                cell = _cell_from_wire(raw)
                width = display_width(cell.ch) or 1
                if 0 <= y < self.rows and 0 <= x < self.cols:
                    self.grid[y][x] = cell
                    if width == 2 and x + 1 < self.cols:
                        self.grid[y][x + 1] = Cell(ch="", cont=True)
                # Advance by the grapheme's width, not by one — see
                # `display_width`.
                x += width
        cursor = frame.get("cursor")
        self.cursor = tuple(cursor) if cursor else None
        self.cursor_shape = frame.get("cursor_shape", "block")

    # -- reading -----------------------------------------------------------

    def line(self, y):
        """One character per column, so a string index is a column index.

        A wide character's second column reads as a space here; use
        `display_line` when asserting on text that contains one.
        """
        if not 0 <= y < self.rows:
            return ""
        return "".join(c.ch if c.ch and not c.cont else " " for c in self.grid[y])

    def display_line(self, y):
        """What the row looks like: wide characters are not padded."""
        if not 0 <= y < self.rows:
            return ""
        return "".join("" if c.cont else (c.ch or " ") for c in self.grid[y])

    def text(self):
        return "\n".join(self.line(y) for y in range(self.rows))

    def display_text(self):
        return "\n".join(self.display_line(y) for y in range(self.rows))

    def contains(self, needle):
        return needle in self.text()

    def contains_collapsed(self, needle):
        """Match ignoring runs of whitespace, for text the chrome may pad."""
        return _collapse(needle) in _collapse(self.text())

    def footer(self, rows=8):
        """The bottom `rows` lines — the band butai scans for agent status."""
        start = max(0, self.rows - rows)
        return "\n".join(self.line(y) for y in range(start, self.rows))

    def cell(self, x, y):
        return self.grid[y][x]

    def find(self, needle):
        """(x, y) of the first occurrence, or None."""
        for y in range(self.rows):
            x = self.line(y).find(needle)
            if x >= 0:
                return (x, y)
        return None

    def styles_in_use(self):
        """Every distinct modifier seen anywhere on screen."""
        seen = set()
        for row in self.grid:
            for cell in row:
                seen |= cell.mods
        return seen

    def colors_in_use(self):
        seen = set()
        for row in self.grid:
            for cell in row:
                seen.add(_color_key(cell.fg))
                seen.add(_color_key(cell.bg))
        return seen

    def dump(self, limit=None):
        """Printable snapshot for failure messages."""
        lines = [self.line(y).rstrip() for y in range(self.rows)]
        if limit:
            lines = lines[:limit]
        width = max((len(line) for line in lines), default=0)
        bar = "-" * min(width, self.cols)
        return "\n".join([bar] + lines + [bar])


def _cell_from_wire(raw):
    mods = raw.get("mods") or {}
    return Cell(
        # `ch` is kept verbatim: an empty string is the trailing half of a wide
        # character, which is exactly what the wide-cell probe needs to see. It
        # is rendered as a space only when reading a line as text.
        ch=raw.get("ch", ""),
        fg=raw.get("fg", "default"),
        bg=raw.get("bg", "default"),
        mods=frozenset(k for k, v in mods.items() if v),
    )


def _color_key(color):
    if isinstance(color, str):
        return color
    if isinstance(color, dict):
        if "indexed" in color:
            return ("indexed", color["indexed"])
        if "rgb" in color:
            return ("rgb", tuple(color["rgb"]))
    return ("unknown", repr(color))


def _collapse(text):
    return " ".join(text.split())
