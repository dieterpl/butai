#!/usr/bin/env python3
"""Minimal third-party butai client — no dependencies beyond the stdlib.

Connects to the daemon socket (spawning nothing; run `butai` or
`butai daemon` first, or point BUTAI_SOCKET at a running daemon), creates a
session, runs a command in its shell pane, and prints the rendered screen.

Usage:  python3 examples/api-client.py [command...]
"""
import json
import os
import socket
import struct
import sys
import time
import unicodedata

PROTO_VERSION = 1
COLS, ROWS = 100, 28


def socket_path() -> str:
    if os.environ.get("BUTAI_SOCKET"):
        return os.environ["BUTAI_SOCKET"]
    home = os.path.expanduser("~")
    if home and home != "~":
        return os.path.join(home, ".butai", "butai.sock")
    return f"/tmp/butai-{os.getuid()}/butai.sock"


class Butai:
    """Length-prefixed JSON frames over the daemon's Unix socket."""

    def __init__(self, path: str):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.buf = b""

    def send(self, msg: dict) -> None:
        payload = json.dumps(msg).encode()
        self.sock.sendall(struct.pack(">I", len(payload)) + payload)

    def recv(self, timeout: float = 10.0) -> dict:
        self.sock.settimeout(timeout)
        while True:
            if len(self.buf) >= 4:
                (n,) = struct.unpack(">I", self.buf[:4])
                if len(self.buf) >= 4 + n:
                    payload, self.buf = self.buf[4 : 4 + n], self.buf[4 + n :]
                    return json.loads(payload)
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("daemon closed the connection")
            self.buf += chunk


class Screen:
    """Apply frame updates to a character grid."""

    def __init__(self, cols: int, rows: int):
        self.grid = [[" "] * cols for _ in range(rows)]

    def apply(self, frame: dict) -> None:
        for run in frame["cells"]:
            x, y = run["x"], run["y"]
            for cell in run["cells"]:
                ch = cell["ch"] or " "
                if y < len(self.grid) and x < len(self.grid[0]):
                    self.grid[y][x] = ch[0]
                # Advance by the grapheme's width. A run is a sequence of
                # consecutive graphemes with no filler cell for the second
                # column of a wide character, so stepping one column per cell
                # would shift everything after the first CJK or emoji glyph.
                x += 2 if unicodedata.east_asian_width(ch[0]) in ("W", "F") else 1

    def text(self) -> str:
        return "\n".join("".join(row).rstrip() for row in self.grid)


def main() -> None:
    command = " ".join(sys.argv[1:]) or "echo hello from the butai API"
    butai = Butai(socket_path())

    butai.send({
        "hello": {
            "proto_version": PROTO_VERSION,
            "encoding": "json",
            "cols": COLS,
            "rows": ROWS,
            "target": {"new": {"name": f"api-{os.getpid()}", "layout": None}},
            "cwd": os.getcwd(),
        }
    })
    hello = butai.recv()
    session = hello["hello"]["session"]
    print(f"attached to session {session['name']!r}", file=sys.stderr)

    screen = Screen(COLS, ROWS)
    # Type the command into the shell pane, then Enter.
    for ch in command:
        butai.send({"input": {"key": {"code": {"char": ch}, "mods": {}}}})
    butai.send({"input": {"key": {"code": "enter", "mods": {}}}})

    # Collect frames briefly, then print what the pane shows.
    deadline = time.time() + 3.0
    while time.time() < deadline:
        try:
            msg = butai.recv(timeout=max(0.1, deadline - time.time()))
        except (TimeoutError, socket.timeout):
            break
        if "frame" in msg:
            screen.apply(msg["frame"])
        elif "error" in msg:
            print("error:", msg["error"], file=sys.stderr)

    print(screen.text())

    # Clean up: kill our scratch session (the shell dies with it).
    butai.send({"command": {"kill_session": f"api-{os.getpid()}"}})
    try:
        butai.recv(timeout=2.0)
    except Exception:
        pass


if __name__ == "__main__":
    main()
