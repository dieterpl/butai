"""Clients for the two protocols butai serves on one Unix socket.

`Framed` speaks the length-prefixed JSON/MessagePack stream; `Http` speaks the
Docker-style REST API; `Events` reads the SSE stream. All three connect to the
same socket path — proving that dispatch works is itself one of the tests.

Everything here is standard library only, on purpose: the daemon's API is
supposed to be reachable from anything that can open a Unix socket, and a test
suite that needed a client library would be assuming away the thing it tests.
"""

import json
import os
import socket
import struct
import threading
import time

from . import msgpack
from .screen import Screen

__all__ = [
    "PROTOCOL_VERSION",
    "MAX_FRAME_LEN",
    "SNIFF_CEILING",
    "Framed",
    "Http",
    "Events",
    "Response",
    "Target",
    "key",
    "socket_path",
    "msg_kind",
    "msg_body",
    "ProtocolError",
    "HttpError",
]

PROTOCOL_VERSION = 1

# `butai_protocol::framing::MAX_FRAME_LEN`.
MAX_FRAME_LEN = 32 * 1024 * 1024

# A connection's first byte decides framed-vs-HTTP (`client_conn.rs` peeks it).
# A framed length prefix only has a zero top byte below 16 MiB, so that — not
# MAX_FRAME_LEN — is the real ceiling for a connection's *first* frame.
SNIFF_CEILING = 16 * 1024 * 1024

DEFAULT_TIMEOUT = 15.0


class ProtocolError(Exception):
    pass


class HttpError(Exception):
    pass


def socket_path():
    """Where the daemon listens, matching `butai_core::paths::socket_path`."""
    env = os.environ.get("BUTAI_SOCKET")
    if env:
        return env
    home = os.path.expanduser("~")
    return os.path.join(home, ".butai", "butai.sock")


# ---------------------------------------------------------------------------
# message helpers
# ---------------------------------------------------------------------------


class Target:
    """`AttachTarget` constructors, in wire form."""

    @staticmethod
    def attach(name):
        return {"attach": {"name": name}}

    @staticmethod
    def new(name=None, layout=None):
        return {"new": {"name": name, "layout": layout}}

    @staticmethod
    def default():
        return "default"

    @staticmethod
    def control():
        return "control"

    @staticmethod
    def pane(pane_id):
        return {"pane": {"pane": pane_id}}


def key(code, ctrl=False, alt=False, shift=False):
    """An `InputEvent::Key`. `code` is a wire KeyCode: "enter", {"char": "a"}."""
    mods = {}
    if ctrl:
        mods["ctrl"] = True
    if alt:
        mods["alt"] = True
    if shift:
        mods["shift"] = True
    event = {"code": code}
    if mods:
        event["mods"] = mods
    return {"key": event}


def msg_kind(msg):
    """The variant name of an externally-tagged message.

    Serde writes unit variants as bare strings (`"ok"`, `"bell"`) and everything
    else as a one-key map, so both forms have to be understood.
    """
    if isinstance(msg, str):
        return msg
    if isinstance(msg, dict) and len(msg) == 1:
        return next(iter(msg))
    raise ProtocolError(f"not an externally-tagged message: {msg!r}")


def msg_body(msg):
    """The payload of a tagged message; None for a unit variant."""
    if isinstance(msg, str):
        return None
    return next(iter(msg.values()))


# ---------------------------------------------------------------------------
# framed protocol
# ---------------------------------------------------------------------------


class Framed:
    """A framed client connection.

    Frames are applied to `self.screen` as they arrive; every other message is
    queued so a test can assert on it without racing the render stream.
    """

    def __init__(self, path=None, encoding="json", timeout=DEFAULT_TIMEOUT):
        self.path = path or socket_path()
        self.encoding = encoding
        self.timeout = timeout
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(timeout)
        self.sock.connect(self.path)
        self.buf = b""
        self.screen = Screen()
        self.queue = []
        self.session = None
        self._sent_hello = False
        # Each side's first frame is always JSON; the negotiated encoding starts
        # with the second. Counting frames rather than watching for the hello
        # matters when the handshake is rejected: the server's first frame is
        # then an `error`, and the `detached` that follows is already encoded.
        self._received = 0
        self._commands = 0
        self.closed = False

    # -- context manager ---------------------------------------------------

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        self.close()
        return False

    def close(self):
        if not self.closed:
            self.closed = True
            try:
                self.sock.close()
            except OSError:
                pass

    # -- raw framing -------------------------------------------------------

    def send_raw(self, payload):
        """Send one frame with an explicit payload (no encoding applied)."""
        self.sock.sendall(struct.pack(">I", len(payload)) + payload)

    def send_prefix(self, length, payload=b""):
        """Send a hand-built length prefix — for framing-violation tests."""
        self.sock.sendall(struct.pack(">I", length) + payload)

    def send(self, msg):
        """Send a `ClientMsg`, using the negotiated encoding after the hello."""
        if self._sent_hello and self.encoding == "msgpack":
            payload = msgpack.encode(msg)
        else:
            payload = json.dumps(msg).encode()
        self.send_raw(payload)

    def _read_frame(self, deadline):
        while True:
            if len(self.buf) >= 4:
                (n,) = struct.unpack(">I", self.buf[:4])
                if len(self.buf) >= 4 + n:
                    payload = self.buf[4 : 4 + n]
                    self.buf = self.buf[4 + n :]
                    return payload
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"no frame within {self.timeout}s")
            self.sock.settimeout(remaining)
            chunk = self.sock.recv(1 << 16)
            if not chunk:
                raise ConnectionError("daemon closed the connection")
            self.buf += chunk

    def recv(self, timeout=None):
        """Next message off the wire, decoding with the negotiated encoding."""
        deadline = time.monotonic() + (timeout if timeout is not None else self.timeout)
        payload = self._read_frame(deadline)
        if self._received and self.encoding == "msgpack":
            msg = msgpack.decode(payload)
        else:
            msg = json.loads(payload)
        self._received += 1
        if msg_kind(msg) == "frame":
            self.screen.apply(msg_body(msg))
        return msg

    def pump(self, seconds=0.0):
        """Drain whatever has arrived, applying frames; returns non-frame msgs."""
        deadline = time.monotonic() + seconds
        out = []
        while True:
            remaining = max(0.0, deadline - time.monotonic())
            try:
                msg = self.recv(timeout=remaining if remaining else 0.05)
            except (TimeoutError, socket.timeout):
                break
            except ConnectionError:
                break
            if msg_kind(msg) != "frame":
                out.append(msg)
                self.queue.append(msg)
            if time.monotonic() >= deadline:
                break
        return out

    # -- handshake ---------------------------------------------------------

    def hello(self, target=None, cols=80, rows=24, cwd="/", proto_version=None):
        """Send the hello and return the server's reply."""
        msg = {
            "hello": {
                "proto_version": PROTOCOL_VERSION if proto_version is None else proto_version,
                "encoding": self.encoding,
                "cols": cols,
                "rows": rows,
                "target": Target.default() if target is None else target,
                "cwd": cwd,
            }
        }
        self.send(msg)
        self._sent_hello = True
        self.screen.resize(cols, rows)
        reply = self.recv()
        if msg_kind(reply) == "hello":
            self.session = msg_body(reply).get("session")
        return reply

    # -- convenience -------------------------------------------------------

    def command(self, cmd):
        self.send({"command": cmd})

    def watch(self, pane):
        """Re-point this pane connection at another pane.

        The stage moves when an agent is spawned, so following the work means
        following the stage — without tearing the connection down. The daemon
        answers with a full frame, which the screen clears against, so what is
        on it afterwards is the new pane and not the old one showing through.
        """
        self.send({"watch": {"pane": pane}})

    def input(self, event):
        self.send({"input": event})

    def type_text(self, text):
        for ch in text:
            self.input(key({"char": ch}))

    def type_line(self, text):
        self.type_text(text)
        self.input(key("enter"))

    # A pane echoes your keystrokes, so any marker written literally into a
    # command line is on screen before the command runs. Waiting for it proves
    # nothing and, worse, returns early — the next thing typed then lands in
    # whatever the previous command left running. Both helpers below assemble
    # the marker inside the shell so it never appears contiguously in the echo.

    def run_line(self, command, timeout=None):
        """Run a command in the pane and wait until it has actually finished."""
        self._commands += 1
        token = f"{self._commands:04d}"
        self.type_line(f"{command}; printf 'CMDDONE%s\\n' {token}")
        self.wait_text(f"CMDDONE{token}", timeout=timeout)
        return self.screen

    def echo_marker(self, marker, timeout=None):
        """Make the pane print `marker`, and wait for the printed copy."""
        cut = max(1, len(marker) // 2)
        head, tail = marker[:cut], marker[cut:]
        self.type_line(f"printf '%s%s\\n' '{head}' '{tail}'")
        self.wait_text(marker, timeout=timeout)
        return self.screen

    def resize(self, cols, rows):
        self.send({"resize": {"cols": cols, "rows": rows}})
        self.screen.resize(cols, rows)

    def detach(self):
        self.send("detach")

    def request(self, cmd, timeout=None):
        """Send a command and wait for the next non-frame reply."""
        self.command(cmd)
        return self.wait_msg(timeout=timeout)

    def wait_msg(self, kind=None, timeout=None):
        """Next queued or incoming non-frame message, optionally of `kind`."""
        limit = timeout if timeout is not None else self.timeout
        deadline = time.monotonic() + limit
        while True:
            for i, msg in enumerate(self.queue):
                if kind is None or msg_kind(msg) == kind:
                    return self.queue.pop(i)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(
                    f"no {kind or 'non-frame'} message within {limit}s"
                    + (f"; saw {[msg_kind(m) for m in self.queue]}" if self.queue else "")
                )
            msg = self.recv(timeout=remaining)
            if msg_kind(msg) != "frame":
                self.queue.append(msg)

    def wait_text(self, needle, timeout=None):
        """Pump frames until `needle` renders, or fail with the screen dump."""
        limit = timeout if timeout is not None else self.timeout
        deadline = time.monotonic() + limit
        while True:
            if self.screen.contains(needle):
                return self.screen
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise AssertionError(
                    f"never saw {needle!r} within {limit}s; screen:\n{self.screen.dump()}"
                )
            try:
                msg = self.recv(timeout=remaining)
            except (TimeoutError, socket.timeout):
                continue
            if msg_kind(msg) != "frame":
                self.queue.append(msg)

    def expect_closed(self, timeout=5.0):
        """Assert the daemon hangs up (and return anything it said first)."""
        deadline = time.monotonic() + timeout
        seen = []
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise AssertionError(f"connection stayed open for {timeout}s; saw {seen}")
            try:
                seen.append(self.recv(timeout=remaining))
            except ConnectionError:
                return seen
            except (TimeoutError, socket.timeout):
                continue


# ---------------------------------------------------------------------------
# HTTP
# ---------------------------------------------------------------------------


class Response:
    def __init__(self, status, reason, headers, body):
        self.status = status
        self.reason = reason
        self.headers = headers
        self.body = body

    @property
    def text(self):
        return self.body.decode("utf-8", "replace")

    def json(self):
        try:
            return json.loads(self.body)
        except ValueError as e:
            raise HttpError(f"body is not JSON ({e}): {self.text[:400]!r}") from e

    def __repr__(self):
        return f"<Response {self.status} {len(self.body)}B>"


class Http:
    """One-shot HTTP/1.1 requests over the daemon socket.

    Matches the minimal client in `crates/butai-server/tests/e2e_http.rs`:
    `Connection: close` and read to EOF, because the daemon closes after each
    response.
    """

    def __init__(self, path=None, timeout=DEFAULT_TIMEOUT):
        self.path = path or socket_path()
        self.timeout = timeout

    def request(self, method, path, json_body=None, raw=None, content_type=None, timeout=None):
        if json_body is not None and raw is not None:
            raise ValueError("pass json_body or raw, not both")
        if json_body is not None:
            payload = json.dumps(json_body).encode()
            content_type = content_type or "application/json"
        elif raw is not None:
            payload = raw if isinstance(raw, bytes) else raw.encode()
            content_type = content_type or "application/octet-stream"
        else:
            payload = b""
            content_type = content_type or "application/json"

        head = (
            f"{method} {path} HTTP/1.1\r\n"
            f"Host: butai\r\n"
            f"Connection: close\r\n"
            f"Content-Type: {content_type}\r\n"
            f"Content-Length: {len(payload)}\r\n\r\n"
        ).encode()

        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.settimeout(timeout or self.timeout)
        try:
            sock.connect(self.path)
            sock.sendall(head + payload)
            chunks = []
            while True:
                chunk = sock.recv(1 << 16)
                if not chunk:
                    break
                chunks.append(chunk)
        finally:
            sock.close()
        return _parse_response(b"".join(chunks))

    def get(self, path, **kw):
        return self.request("GET", path, **kw)

    def post(self, path, json_body=None, **kw):
        return self.request("POST", path, json_body=json_body, **kw)

    def delete(self, path, **kw):
        return self.request("DELETE", path, **kw)

    # -- checked helpers ---------------------------------------------------

    def json_at(self, path, expect=200):
        res = self.get(path)
        if res.status != expect:
            raise HttpError(f"GET {path} -> {res.status} (want {expect}): {res.text[:400]}")
        return res.json()

    def ok(self, method, path, json_body=None, **kw):
        res = self.request(method, path, json_body=json_body, **kw)
        if res.status != 200:
            raise HttpError(f"{method} {path} -> {res.status}: {res.text[:400]}")
        return res

    # -- workspace conveniences -------------------------------------------

    def workspaces(self):
        return self.json_at("/v1/workspaces")

    def new_workspace(self, path=None, name=None, layout=None):
        body = {}
        if path is not None:
            body["path"] = str(path)
        if name is not None:
            body["name"] = name
        if layout is not None:
            body["layout"] = layout
        res = self.post("/v1/workspaces", json_body=body)
        if res.status != 201:
            raise HttpError(f"create workspace -> {res.status}: {res.text[:400]}")
        return res.json()["id"]

    def detail(self, ws):
        return self.json_at(f"/v1/workspaces/{ws}")

    def agents(self, ws):
        return self.json_at(f"/v1/workspaces/{ws}/agents")

    def processes(self, ws):
        return self.json_at(f"/v1/workspaces/{ws}/processes")

    def spawn_agent(self, ws, kind, background=False):
        body = {"type": kind}
        if background:
            body["background"] = True
        return self.ok("POST", f"/v1/workspaces/{ws}/agents", body)

    def pane_output(self, ws, pane, lines=None, source=None, fmt=None):
        """A pane's rendered output as text.

        A query, not an attach: it must not resize the pane or clear its bell,
        so a test can poll a pane without perturbing the state machine that is
        watching it.
        """
        query = []
        if lines is not None:
            query.append(f"lines={lines}")
        if source is not None:
            query.append(f"source={source}")
        if fmt is not None:
            query.append(f"format={fmt}")
        suffix = ("?" + "&".join(query)) if query else ""
        return self.json_at(f"/v1/workspaces/{ws}/panes/{pane}/output{suffix}")

    def new_process(self, ws, name, command):
        return self.ok(
            "POST", f"/v1/workspaces/{ws}/processes", {"name": name, "command": command}
        )

    def poll_until(self, path, predicate, what="condition", timeout=20.0, interval=0.1):
        """Poll a GET until `predicate(parsed_json)` holds.

        Agent and process state is recomputed on the daemon's ~2s sampler tick,
        so almost every state assertion needs this rather than a bare read.
        """
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            try:
                res = self.get(path)
            except OSError as e:
                # A daemon under a heavy flood can refuse a connection briefly.
                # That is what the stress tests measure, not a reason to give up
                # polling — a daemon that never comes back still fails on time.
                last = f"{type(e).__name__}: {e}"
                time.sleep(interval)
                continue
            if res.status == 200:
                last = res.json()
                if predicate(last):
                    return last
            else:
                last = f"HTTP {res.status}: {res.text[:200]}"
            time.sleep(interval)
        raise AssertionError(f"{what} never held for GET {path} within {timeout}s; last: {last}")


def _parse_response(raw):
    if not raw:
        raise HttpError("empty response")
    head, _, body = raw.partition(b"\r\n\r\n")
    lines = head.decode("latin-1").split("\r\n")
    parts = lines[0].split(" ", 2)
    if len(parts) < 2:
        raise HttpError(f"bad status line: {lines[0]!r}")
    status = int(parts[1])
    reason = parts[2] if len(parts) > 2 else ""
    headers = {}
    for line in lines[1:]:
        if ":" in line:
            k, _, v = line.partition(":")
            headers[k.strip().lower()] = v.strip()
    return Response(status, reason, headers, body)


# ---------------------------------------------------------------------------
# SSE
# ---------------------------------------------------------------------------


class Events:
    """Background reader for `GET /v1/events`.

    Optionally throttled, so a test can hold the stream open without draining it
    and watch what the daemon does with a slow subscriber.
    """

    def __init__(self, path=None, read_delay=0.0):
        self.path = path or socket_path()
        self.read_delay = read_delay
        self.events = []
        self.raw_bytes = 0
        self.error = None
        self._sock = None
        self._stop = threading.Event()
        self._thread = None
        self._lock = threading.Lock()

    def __enter__(self):
        self.start()
        return self

    def __exit__(self, *exc):
        self.stop()
        return False

    def start(self):
        self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self._sock.settimeout(2.0)
        self._sock.connect(self.path)
        self._sock.sendall(
            b"GET /v1/events HTTP/1.1\r\nHost: butai\r\nAccept: text/event-stream\r\n\r\n"
        )
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def stop(self):
        self._stop.set()
        if self._sock:
            try:
                self._sock.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            try:
                self._sock.close()
            except OSError:
                pass
        if self._thread:
            self._thread.join(timeout=3.0)

    def _run(self):
        buf = b""
        headers_done = False
        try:
            while not self._stop.is_set():
                if self.read_delay:
                    self._stop.wait(self.read_delay)
                try:
                    chunk = self._sock.recv(1 << 16)
                except socket.timeout:
                    continue
                except OSError:
                    break
                if not chunk:
                    break
                self.raw_bytes += len(chunk)
                buf += chunk
                if not headers_done:
                    head, sep, rest = buf.partition(b"\r\n\r\n")
                    if not sep:
                        continue
                    self.headers = _parse_response(head + b"\r\n\r\n").headers
                    headers_done = True
                    buf = rest
                while b"\n\n" in buf:
                    record, _, buf = buf.partition(b"\n\n")
                    self._on_record(record.decode("utf-8", "replace"))
        except Exception as e:  # surfaced by the test, not swallowed
            self.error = e

    def _on_record(self, record):
        for line in record.splitlines():
            if line.startswith("data:"):
                try:
                    payload = json.loads(line[5:].strip())
                except ValueError:
                    continue
                with self._lock:
                    self.events.append(payload)

    # -- reading -----------------------------------------------------------

    def tags(self):
        with self._lock:
            return [e.get("event") for e in self.events]

    def of(self, tag):
        with self._lock:
            return [e for e in self.events if e.get("event") == tag]

    def wait_for(self, tag, timeout=10.0, predicate=None):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            for event in self.of(tag):
                if predicate is None or predicate(event.get("data")):
                    return event
            time.sleep(0.05)
        raise AssertionError(
            f"no {tag!r} SSE event within {timeout}s; saw tags {sorted(set(self.tags()))}"
        )
