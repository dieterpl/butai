"""The framed protocol: framing, dispatch, handshake, encodings, sessions.

Ground truth is `crates/butai-protocol/src/lib.rs`. Where `docs/protocol.md`
disagrees, these tests follow the code and say so.
"""

import json
import socket
import struct
import time

from suite.butai import (
    MAX_FRAME_LEN,
    PROTOCOL_VERSION,
    SNIFF_CEILING,
    Framed,
    Http,
    Target,
    key,
    msg_body,
    msg_kind,
)
from suite.runner import test


@test(profile="smoke", tags=("protocol",))
def hello_handshake_returns_a_session(ctx):
    d = ctx.daemon()
    ctx.cover("client:hello", "server:hello", "target:new", "encoding:json")
    with Framed(d.socket) as client:
        reply = client.hello(Target.new(name="hs"), cwd=d.work)
        assert msg_kind(reply) == "hello", reply
        body = msg_body(reply)
        assert body["proto_version"] == PROTOCOL_VERSION
        session = body["session"]
        assert session["name"] == "hs", session
        assert session["cwd"] == d.work
        assert session["windows"] >= 1, session
        # The snapshot is taken before this client is registered, so it does not
        # count itself. Worth pinning: a client author would reasonably expect
        # otherwise, and the workspace summary is where the live count lives.
        assert session["attached_clients"] == 0, session
        d.http.poll_until(
            "/v1/workspaces",
            lambda w: any(x["name"] == "hs" and x["attached_clients"] == 1 for x in w),
            "the workspace summary counts the attached client",
            timeout=15,
        )


@test(profile="smoke", tags=("protocol",))
def one_socket_serves_framed_and_http(ctx):
    """A single peeked byte routes the connection: `0x00` is a length prefix,
    anything else is an HTTP method. Both must work on the same path."""
    d = ctx.daemon()
    ctx.cover("GET /v1/workspaces")
    with Framed(d.socket) as client:
        assert msg_kind(client.hello(Target.new(name="shared"), cwd=d.work)) == "hello"
    names = [w["name"] for w in Http(d.socket).workspaces()]
    assert "shared" in names, names


@test(profile="standard", tags=("protocol", "encoding"))
def msgpack_is_negotiable_and_only_the_hello_is_json(ctx):
    """The shipped TUI negotiates MessagePack, so the JSON-only path the docs
    describe is not the one most bytes actually take."""
    d = ctx.daemon()
    ctx.cover("encoding:msgpack", "cmd:list_sessions", "server:session_list")
    ws = d.http.new_workspace(path=d.work, name="mp")
    with Framed(d.socket, encoding="msgpack") as client:
        reply = client.hello(Target.pane(d.staged_pane(ws)), cwd=d.work)
        assert msg_kind(reply) == "hello", reply
        client.command("list_sessions")
        listed = client.wait_msg("session_list")
        names = [s["name"] for s in msg_body(listed)]
        assert "mp" in names, names
        # Frames ride the render tick, so they can land after a reply the core
        # answers inline — pump for them rather than assuming they beat it.
        deadline = time.monotonic() + 15
        while client.screen.frames == 0 and time.monotonic() < deadline:
            client.pump(0.3)
        assert client.screen.frames > 0, "a msgpack client should still get frames"


@test(profile="standard", tags=("protocol", "encoding"))
def a_msgpack_client_and_a_json_client_see_the_same_session(ctx):
    """Encoding is per connection, not per session."""
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work, name="both")
    pane = d.staged_pane(ws)
    with Framed(d.socket, encoding="msgpack") as a:
        a.hello(Target.pane(pane), cwd=d.work, cols=100, rows=30)
        a.echo_marker("ENCODING-MARKER", timeout=20)
        with Framed(d.socket, encoding="json") as b:
            b.hello(Target.pane(pane), cols=100, rows=30, cwd=d.work)
            b.wait_text("ENCODING-MARKER", timeout=20)


@test(profile="standard", tags=("protocol",))
def a_version_mismatch_is_refused_with_a_reason(ctx):
    d = ctx.daemon()
    ctx.cover("server:error", "server:detached")
    with Framed(d.socket) as client:
        client.send(
            {
                "hello": {
                    "proto_version": PROTOCOL_VERSION + 99,
                    "encoding": "json",
                    "cols": 80,
                    "rows": 24,
                    "target": Target.default(),
                    "cwd": d.work,
                }
            }
        )
        client._sent_hello = True
        kinds = []
        for _ in range(2):
            try:
                kinds.append(msg_kind(client.recv(timeout=5)))
            except (ConnectionError, TimeoutError):
                break
        assert "error" in kinds, f"expected an error for a bad version, got {kinds}"
        assert "detached" in kinds, f"expected a detach after the error, got {kinds}"
    d.assert_healthy()


@test(profile="standard", tags=("protocol",))
def a_non_hello_first_frame_is_dropped(ctx):
    """The hello is mandatory: anything else on frame one ends the connection."""
    d = ctx.daemon()
    with Framed(d.socket) as client:
        client.send_raw(json.dumps({"input": key("enter")}).encode())
        seen = client.expect_closed(timeout=5)
        assert not seen, f"daemon answered a headless first frame: {seen}"
    d.assert_healthy()


@test(profile="standard", tags=("protocol",))
def a_frame_over_the_size_cap_is_rejected(ctx):
    """`MAX_FRAME_LEN` is 32 MiB; only the length header has to be read to know
    a frame is too big, so this costs four bytes, not 32 MiB."""
    d = ctx.daemon()
    with Framed(d.socket) as client:
        client.hello(Target.new(name="big"), cwd=d.work)
        client.send_prefix(MAX_FRAME_LEN + 1, b"\x00" * 16)
        client.expect_closed(timeout=8)
    d.assert_healthy()


@test(profile="standard", tags=("protocol",))
def the_first_frame_ceiling_is_the_dispatch_byte_not_the_size_cap(ctx):
    """Documented behaviour, not a bug — but worth pinning.

    Dispatch peeks one byte: `0x00` means framed, anything else means HTTP. A
    length prefix only has a zero top byte below 16 MiB, so a *first* frame at
    or above 16 MiB is handed to the HTTP parser instead of the frame decoder.
    Every real first frame is a small hello, so nothing legitimate hits this —
    but a client that assumed the full 32 MiB applies from byte one would see a
    confusing HTTP error rather than a protocol one.
    """
    d = ctx.daemon()
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(8)
    sock.connect(d.socket)
    try:
        sock.sendall(struct.pack(">I", SNIFF_CEILING) + b"{}")
        try:
            reply = sock.recv(4096)
        except socket.timeout:
            reply = b""
    finally:
        sock.close()
    assert not reply.startswith(b"\x00"), (
        "a >=16 MiB first frame was decoded as a frame; the dispatch rule changed "
        "and this test should be revisited"
    )
    ctx.note(
        f"effective first-frame ceiling is {SNIFF_CEILING // (1024 * 1024)} MiB "
        f"(dispatch byte), not the {MAX_FRAME_LEN // (1024 * 1024)} MiB MAX_FRAME_LEN"
    )
    d.assert_healthy()


@test(profile="smoke", tags=("protocol", "session"))
def detach_and_reattach_preserves_the_screen(ctx):
    """The whole reason the daemon exists: close the terminal, come back, and
    the pane is mid-sentence where you left it."""
    d = ctx.daemon()
    ctx.cover("client:detach", "target:pane", "server:frame")
    ws = d.http.new_workspace(path=d.work, name="persist")
    pane = d.staged_pane(ws)
    with Framed(d.socket) as first:
        first.hello(Target.pane(pane), cols=100, rows=30, cwd=d.work)
        first.echo_marker("REATTACH-MARKER", timeout=20)
        assert first.screen.frames > 0, "a client streaming a pane must receive frames"
        first.detach()
        detached = first.wait_msg("detached", timeout=10)
        assert msg_body(detached)["reason"], "a detach should carry a reason"

    with Framed(d.socket) as second:
        second.hello(Target.pane(pane), cols=100, rows=30, cwd=d.work)
        second.wait_text("REATTACH-MARKER", timeout=20)


@test(profile="standard", tags=("protocol", "session"))
def two_clients_mirror_one_pane(ctx):
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work, name="mirror")
    pane = d.staged_pane(ws)
    with Framed(d.socket) as a:
        a.hello(Target.pane(pane), cols=100, rows=30, cwd=d.work)
        with Framed(d.socket) as b:
            b.hello(Target.pane(pane), cols=100, rows=30, cwd=d.work)
            a.echo_marker("MIRRORED-OUTPUT", timeout=20)
            b.wait_text("MIRRORED-OUTPUT", timeout=20)


@test(profile="standard", tags=("protocol",))
def a_session_target_resolves_a_workspace_by_name_or_by_cwd(ctx):
    """The three session targets are how `butai`, `butai new` and `butai attach`
    say which workspace they mean. They resolve one and report it in the hello;
    a client draws the workbench itself from `/v1/*`, so what comes back is the
    answer, not a screen."""
    d = ctx.daemon()
    ctx.cover("target:default", "target:attach", "target:new")
    with Framed(d.socket) as first:
        session = msg_body(first.hello(Target.new(name="recent"), cwd=d.work))["session"]
    with Framed(d.socket) as second:
        again = msg_body(second.hello(Target.default(), cwd=d.work))["session"]
    assert again["id"] == session["id"], f"default attached to {again} not {session}"
    with Framed(d.socket) as third:
        by_name = msg_body(third.hello(Target.attach("recent"), cwd=d.work))["session"]
    assert by_name["id"] == session["id"], f"attach resolved {by_name} not {session}"


@test(profile="standard", tags=("protocol",))
def a_pane_target_streams_one_pane_full_bleed(ctx):
    """How every client's stage works: no chrome, input straight to the pane,
    and no session — so the connection is not 'interactive' server-side.

    Full-bleed means the grid the client is sent *is* the pane: the shell
    prompt starts in column 0 and the frame is the size that was asked for,
    with nothing reserved for a border."""
    d = ctx.daemon()
    ctx.cover("target:pane")
    ws = d.http.new_workspace(path=d.work)
    pane = d.http.detail(ws)["processes"][0]["pane"]
    with Framed(d.socket) as client:
        reply = client.hello(Target.pane(pane), cols=90, rows=24, cwd=d.work)
        assert msg_body(reply)["session"] is None, "a pane target carries no session"
        client.echo_marker("FULL-BLEED-MARKER", timeout=20)
        lines = [client.screen.line(y) for y in range(client.screen.rows)]
    written = [ln for ln in lines if ln.strip()]
    assert written, f"nothing was drawn:\n{lines}"
    assert any(ln[0] != " " for ln in written), (
        "every drawn row started with a blank column, so something is reserving "
        f"one — a pane target has no chrome to reserve it for:\n{written}"
    )


@test(profile="standard", tags=("protocol", "render"))
def resizing_forces_a_full_repaint(ctx):
    """A size change invalidates the damage diff, so the daemon has to resend
    everything — a client that ignored `full` would keep stale cells."""
    d = ctx.daemon()
    ctx.cover("client:resize")
    ws = d.http.new_workspace(path=d.work, name="resize")
    with Framed(d.socket) as client:
        client.hello(Target.pane(d.staged_pane(ws)), cols=100, rows=30, cwd=d.work)
        client.echo_marker("RESIZE-MARKER", timeout=20)
        before = client.screen.full_frames
        client.resize(120, 40)
        client.wait_text("RESIZE-MARKER", timeout=20)
        assert client.screen.full_frames > before, "resize did not produce a full frame"
        assert client.screen.cols == 120


@test(profile="standard", tags=("protocol",))
def a_client_can_scroll_the_stage_scrollback(ctx):
    d = ctx.daemon()
    ctx.cover("cmd:scroll_page")
    ws = d.http.new_workspace(path=d.work, name="scroll")
    with Framed(d.socket) as client:
        client.hello(Target.pane(d.staged_pane(ws)), cols=100, rows=30, cwd=d.work)
        client.run_line("seq 1 200", timeout=20)
        client.command({"scroll_page": -1})
        client.pump(1.5)
        client.command({"scroll_page": 1})
        client.pump(1.5)
    d.assert_healthy()


@test(profile="standard", tags=("protocol", "session"))
def workspaces_survive_the_client_that_made_them(ctx):
    d = ctx.daemon()
    with Framed(d.socket) as client:
        client.hello(Target.new(name="outlives"), cwd=d.work)
    names = [w["name"] for w in d.http.workspaces()]
    assert "outlives" in names, names
