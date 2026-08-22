"""Every `InputEvent` and `KeyCode`, verified by what the PTY actually receives.

Keys are asserted against the bytes a program sees, not against "no error":
`cat -v` in raw mode renders every control byte visibly, so the pane's own
output is the assertion.
"""

from suite.butai import Target, key, msg_body, msg_kind
from suite.daemon import Config
from suite.runner import test

# What `cat -v` prints for each key, given `input/encode.rs`.
KEY_BYTES = [
    ("char", {"char": "x"}, "x"),
    ("enter", "enter", "^M"),
    ("esc", "esc", "^["),
    ("backspace", "backspace", "^?"),
    ("tab", "tab", "^I"),
    ("back_tab", "back_tab", "^[[Z"),
    ("left", "left", "^[[D"),
    ("right", "right", "^[[C"),
    ("up", "up", "^[[A"),
    ("down", "down", "^[[B"),
    ("home", "home", "^[[H"),
    ("end", "end", "^[[F"),
    ("page_up", "page_up", "^[[5~"),
    ("page_down", "page_down", "^[[6~"),
    ("delete", "delete", "^[[3~"),
    ("insert", "insert", "^[[2~"),
    ("f", {"f": 5}, "^[[15~"),
]


def _raw_pane(ctx, d, cols=140, rows=30):
    """A pane echoing every byte it receives, visibly.

    `stty raw -echo` takes the line discipline out of the way so keys arrive as
    the application would see them, and `cat -t` renders control bytes as `^X`
    (`-t` covers tabs too, which plain `-v` leaves as literal whitespace).
    """
    ws = d.http.new_workspace(path=d.work)
    pane = d.http.detail(ws)["processes"][0]["pane"]
    client = d.framed()
    client.hello(Target.pane(pane), cols=cols, rows=rows, cwd=d.work)
    # The readiness marker is assembled by the shell: `stty raw -echo` only
    # takes effect after the line has been echoed, so a literal marker would be
    # on screen before `cat` was running to receive the keys sent below.
    client.type_line("clear; stty raw -echo; printf 'RAW%s\\n' READY; cat -t")
    client.wait_text("RAWREADY", timeout=25)
    return ws, pane, client


@test(profile="smoke", tags=("input",))
def every_key_code_reaches_the_pane_as_the_right_bytes(ctx):
    d = ctx.daemon()
    ctx.cover("client:input", "input:key")
    _, _, client = _raw_pane(ctx, d)
    with client:
        for name, code, _ in KEY_BYTES:
            ctx.cover(f"key:{name}")
            client.input(key(code))
        client.pump(2.0)
        text = client.screen.text()

    missing = [f"{name} -> {want!r}" for name, _, want in KEY_BYTES if want not in text]
    assert not missing, "keys never arrived: " + ", ".join(missing) + f"\nscreen:\n{text}"


@test(profile="standard", tags=("input",))
def modifiers_change_the_encoding(ctx):
    """Ctrl-a is 0x01, Alt-a is ESC-prefixed, and a modified arrow carries the
    xterm modifier parameter. A GUI client sending mods has to get this back."""
    d = ctx.daemon()
    _, _, client = _raw_pane(ctx, d)
    with client:
        client.input(key({"char": "a"}, ctrl=True))
        client.input(key({"char": "b"}, alt=True))
        client.input(key("up", shift=True))
        client.pump(2.0)
        text = client.screen.text()

    assert "^A" in text, f"ctrl+a did not arrive as 0x01:\n{text}"
    assert "^[b" in text, f"alt+b did not arrive ESC-prefixed:\n{text}"
    assert "^[[1;2A" in text, f"shift+up lost its modifier parameter:\n{text}"


@test(profile="standard", tags=("input",))
def a_paste_arrives_whole(ctx):
    """Paste is its own event so butai can wrap it for apps in bracketed-paste
    mode; a plain reader must still see the literal text."""
    d = ctx.daemon()
    ctx.cover("input:paste")
    _, _, client = _raw_pane(ctx, d)
    with client:
        client.send({"input": {"paste": "PASTED-PAYLOAD-42"}})
        client.wait_text("PASTED-PAYLOAD-42", timeout=15)


@test(profile="standard", tags=("input",))
def scroll_events_are_accepted_over_a_pane(ctx):
    d = ctx.daemon()
    ctx.cover("input:scroll_up", "input:scroll_down")
    _, client = d.stage(cols=110, rows=32)
    with client:
        client.run_line("seq 1 300", timeout=25)
        for _ in range(3):
            client.send({"input": {"scroll_up": {"x": 60, "y": 12}}})
        client.pump(1.0)
        for _ in range(3):
            client.send({"input": {"scroll_down": {"x": 60, "y": 12}}})
        client.pump(1.0)
        stray = [m for m in client.queue if msg_kind(m) == "error"]
        assert not stray, [msg_body(m) for m in stray]
    d.assert_healthy()


@test(profile="standard", tags=("input", "clipboard"))
def dragging_a_selection_sends_the_text_to_the_clipboard(ctx):
    """A client streaming a pane can drag-select without a VT parser: the daemon
    owns the pane's grid, so it does the extraction and hands back the text to
    put on the system clipboard. That is what `<butai-stage>` and both native
    apps use — the TUI composes its own screen and so does its own selection."""
    d = ctx.daemon()
    ctx.cover("input:mouse_down", "input:mouse_drag", "input:mouse_up", "server:set_clipboard")
    needle = "SELECTABLE-PAYLOAD"
    _, client = d.stage(cols=120, rows=36)
    with client:
        client.echo_marker(needle, timeout=25)

        found = None
        for y in range(client.screen.rows - 1, -1, -1):
            x = client.screen.line(y).find(needle)
            if x >= 0:
                found = (x, y)
                break
        assert found, f"could not locate {needle} on screen:\n{client.screen.dump()}"
        x, y = found

        client.send({"input": {"mouse_down": {"x": x, "y": y, "alt": False}}})
        for step in range(2, len(needle) + 1, 4):
            client.send({"input": {"mouse_drag": {"x": x + step, "y": y, "alt": False}}})
        client.send({"input": {"mouse_up": {"x": x + len(needle) - 1, "y": y}}})

        clip = client.wait_msg("set_clipboard", timeout=15)
        text = msg_body(clip)
    assert "SELECTABLE" in text, f"clipboard carried {text!r}"


@test(profile="smoke", tags=("input", "menu"))
def a_right_click_decodes_as_a_click_and_is_dropped_on_a_pane(ctx):
    """Right-click is a `mouse_down` carrying `button: "right"` — a field, not a
    new variant, so a daemon that predates it reads the event as a left click
    instead of failing to decode and dropping the connection.

    The menu it opens is drawn by the client, from the same `/v1/*` state every
    client has, so what is left here is the wire half: the field decodes, and a
    pane connection — which renders one pane full-bleed and has no chrome for a
    menu to hang off — drops it rather than starting a selection with it.
    """
    d = ctx.daemon(config=Config().agent("claudeish", "/bin/sh"))
    ctx.cover("input:mouse_down")
    ws, client = d.stage(cols=120, rows=36)
    with client:
        client.echo_marker("RIGHT-CLICK-MARKER", timeout=25)
        before = client.screen.text()

        client.send({"input": {"mouse_down": {"x": 3, "y": 3, "button": "right"}}})
        client.send({"input": {"mouse_drag": {"x": 30, "y": 3, "alt": False}}})
        client.send({"input": {"mouse_up": {"x": 30, "y": 3}}})
        client.pump(1.5)

        stray = [m for m in client.queue if msg_kind(m) in ("error", "detached")]
        assert not stray, f"a right click should decode, not fault: {stray}"
        # A left press here would have painted a reversed selection and answered
        # with `set_clipboard`; neither is a right-click's business.
        assert not [m for m in client.queue if msg_kind(m) == "set_clipboard"], (
            "a right-click drag must not copy"
        )
        assert client.screen.text() == before, (
            f"the pane changed under a dropped click:\n{client.screen.dump()}"
        )
    d.assert_healthy()
