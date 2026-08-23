"""Daemon lifecycle and the `butai` CLI, against a real binary in a real
Linux userspace — the layer the in-process crate tests cannot reach.
"""

import base64
import json
import os
import re
import time
import subprocess

from suite.butai import Framed, Target, msg_body, msg_kind
from suite.daemon import Config, binary_path
from suite.runner import test
from suite.tty import PtyProcess


@test(profile="smoke", tags=("daemon",))
def daemon_binds_and_serves(ctx):
    d = ctx.daemon()
    ctx.cover("GET /v1/workspaces", "cli:daemon")
    assert d.http.workspaces() == [], "a fresh daemon should own no workspaces"
    d.assert_healthy()


@test(profile="smoke", tags=("daemon", "security"))
def socket_directory_is_private(ctx):
    """The daemon chmods the socket's *parent* 0700 — that is the whole
    authorization model, so it is worth asserting rather than assuming."""
    d = ctx.daemon()
    mode = os.stat(d.butai_dir).st_mode & 0o777
    assert mode == 0o700, f"expected 0700 on {d.butai_dir}, got {oct(mode)}"
    assert os.path.exists(d.socket)


@test(profile="standard", tags=("daemon",))
def second_daemon_refuses_the_same_home(ctx):
    """`<socket>.lock` is the single-instance guard."""
    d = ctx.daemon()
    result = subprocess.run(
        [binary_path(), "daemon"],
        env=d.env(),
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert result.returncode != 0, "a second daemon on the same socket should refuse to start"
    combined = result.stdout + result.stderr
    assert "already running" in combined, f"unhelpful refusal: {combined[:400]}"
    d.assert_healthy()


@test(profile="standard", tags=("daemon",))
def a_daemon_refuses_to_update_itself_unless_configured(ctx):
    """`POST /v1/update` is off unless `[update] allow_remote` says otherwise.

    The socket's only access control is the `0700` on its directory, and over a
    forward or `butai proxy` the far end is whoever holds the ssh session. That
    is a much weaker claim than "may replace the program this machine runs", so
    the default has to be no, and the refusal has to say what changes it.
    """
    d = ctx.daemon()
    ctx.cover("POST /v1/update")

    result = d.http.post("/v1/update")
    assert result.status == 400, f"{result.status}: {result.text}"
    assert "allow_remote" in result.text, f"the refusal must name the key: {result.text}"
    d.assert_healthy()


@test(profile="standard", tags=("daemon",))
def a_stale_socket_file_does_not_block_startup(ctx):
    """After a SIGKILL the socket inode survives; the next daemon must remove it."""
    d = ctx.daemon(start=False)
    os.makedirs(d.butai_dir, mode=0o700, exist_ok=True)
    with open(d.socket, "w") as fh:
        fh.write("")
    d.start()
    assert d.http.workspaces() == []


@test(profile="standard", tags=("daemon",))
def daemon_survives_its_last_workspace_closing_when_configured(ctx):
    """`exit_when_empty` is the knob every long-lived deployment needs."""
    config = Config().set(exit_when_empty=False)
    d = ctx.daemon(config=config)
    ws = d.http.new_workspace(path=d.work)
    ctx.cover("POST /v1/workspaces", "DELETE /v1/workspaces/{id}")
    assert d.http.delete(f"/v1/workspaces/{ws}").status == 200
    d.http.poll_until("/v1/workspaces", lambda w: w == [], "workspace list empties")
    assert d.alive(), "daemon exited despite exit_when_empty = false"


@test(profile="standard", tags=("daemon",))
def daemon_exits_with_its_last_workspace_by_default(ctx):
    """The shipped default: closing the last workspace ends the daemon. In a
    container that reads as 'butai keeps dying', so pin the behaviour."""
    config = Config().set(exit_when_empty=True)
    d = ctx.daemon(config=config)
    ws = d.http.new_workspace(path=d.work)
    d.http.delete(f"/v1/workspaces/{ws}")
    code = d.wait_dead(timeout=15)
    assert code is not None, "daemon outlived its last workspace with exit_when_empty = true"
    ctx.note(f"daemon exited with code {code} when its last workspace closed")


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


@test(profile="smoke", tags=("cli",))
def cli_ls_reports_sessions(ctx):
    d = ctx.daemon()
    ctx.cover("cli:ls", "cmd:list_sessions", "server:session_list")
    empty = d.cli("ls")
    assert empty.returncode == 0, empty.stderr
    assert "no sessions" in empty.stdout, empty.stdout

    with d.attach(Target.new(name="cli-ws"), cwd=d.work):
        listed = d.cli("ls")
    assert listed.returncode == 0, listed.stderr
    assert "cli-ws" in listed.stdout, listed.stdout
    assert "window" in listed.stdout and "client" in listed.stdout, listed.stdout


@test(profile="standard", tags=("cli",))
def cli_kills_a_session_and_then_the_server(ctx):
    d = ctx.daemon()
    ctx.cover("cli:kill-session", "cli:kill-server", "cmd:kill_session", "cmd:kill_server")
    with d.attach(Target.new(name="doomed"), cwd=d.work):
        pass
    assert "doomed" in d.cli("ls").stdout

    killed = d.cli("kill-session", "-t", "doomed")
    assert killed.returncode == 0, killed.stderr
    assert "doomed" not in d.cli("ls").stdout

    missing = d.cli("kill-session", "-t", "never-existed")
    assert missing.returncode != 0, "killing an unknown session should fail loudly"

    assert d.cli("kill-server").returncode == 0
    assert d.wait_dead(timeout=15) is not None, "kill-server left the daemon running"


@test(profile="standard", tags=("cli",))
def cli_proxy_bridges_both_protocols(ctx):
    """`ssh host butai proxy` is the entire remote story, and it is byte
    transparent — so an HTTP request survives the round trip."""
    d = ctx.daemon()
    ctx.cover("cli:proxy")
    request = "GET /v1/workspaces HTTP/1.1\r\nHost: butai\r\nConnection: close\r\n\r\n"
    result = subprocess.run(
        [binary_path(), "proxy"],
        env=d.env(),
        input=request.encode(),
        capture_output=True,
        timeout=30,
    )
    body = result.stdout.decode("utf-8", "replace")
    assert body.startswith("HTTP/1.1 200"), f"proxy did not carry HTTP: {body[:200]!r}"
    assert "[" in body, body[-200:]


@test(profile="standard", tags=("cli", "tui"))
def cli_new_attaches_a_tui_and_detaches_cleanly(ctx):
    """The shipped TUI is just another client — and the only one that
    negotiates MessagePack, so this is also the msgpack smoke test."""
    d = ctx.daemon()
    ctx.cover("cli:new", "cli:attach", "encoding:msgpack", "target:new")
    with PtyProcess([binary_path(), "new", "-s", "tui-ws"], env=d.env(), cwd=d.work) as tui:
        assert tui.wait_output("AGENTS", timeout=20), (
            "the workbench never rendered:\n" + tui.text()[-2000:]
        )
        names = [w["name"] for w in d.http.workspaces()]
        assert "tui-ws" in names, names

        tui.write(b"\x02d")  # C-b d
        assert tui.wait(timeout=15) is not None, "detach did not end the client"

    assert d.alive(), "detaching a client must not stop the daemon"
    assert "tui-ws" in d.cli("ls").stdout, "the workspace should outlive its client"

    with PtyProcess([binary_path(), "attach", "-t", "tui-ws"], env=d.env(), cwd=d.work) as again:
        assert again.wait_output("AGENTS", timeout=20), again.text()[-2000:]


@test(profile="standard", tags=("cli",))
def cli_standalone_runs_without_a_daemon(ctx):
    """`standalone` has to work when the daemon is exactly what is broken."""
    d = ctx.daemon(start=False)
    os.makedirs(d.butai_dir, mode=0o700, exist_ok=True)
    os.makedirs(d.work, exist_ok=True)
    with open(os.path.join(d.butai_dir, "config.toml"), "w") as fh:
        fh.write(d.config.render())
    ctx.cover("cli:standalone")
    with PtyProcess(
        [binary_path(), "standalone"], env=d.env(), cwd=d.work, cols=120, rows=40
    ) as app:
        assert app.wait_output("PROCESSES", timeout=25), app.text()[-2000:]
        assert not os.path.exists(d.socket), "standalone should not bind a daemon socket"


@test(profile="standard", tags=("cli", "tui"))
def the_stage_puts_a_cursor_where_the_program_has_one(ctx):
    """The one thing on screen that is not a cell.

    A pane's cursor is the daemon's to know — it holds the PTY, and the escape
    sequences that move a cursor are parsed there and never reach this terminal
    — and the client's to place. Both halves have been missing at once: the
    position rode on every frame while the painter that took over from the
    daemon dropped it, and a terminal pane had no caret in it at all.

    Nothing in the crate tests can see this. They can assert where the caret
    *should* go; only a real client on a real pty says whether the bytes that
    put it there were ever written. So this reads them: `CSI <style> SP q`,
    `CSI row;col H`, `CSI ?25h`, queued together at the end of each paint.
    """
    d = ctx.daemon(start=False)
    os.makedirs(d.butai_dir, mode=0o700, exist_ok=True)
    os.makedirs(d.work, exist_ok=True)
    with open(os.path.join(d.butai_dir, "config.toml"), "w") as fh:
        fh.write(d.config.render())
    ctx.cover("cli:standalone")

    caret = re.compile(rb"\x1b\[(\d) q\x1b\[(\d+);(\d+)H\x1b\[\?25h")

    def caret_at(app, label, prev=None, timeout=8.0):
        """The last caret the client drew, waiting for it to *change*.

        `prev` is what it was before the keys this call is about. Sampling on a
        fixed read instead passes before the frame answering those keys has
        arrived — the caret is then the one from last time, and the assertion
        it feeds either passes for the wrong reason or fails at a position
        nothing put it in.
        """
        deadline = time.monotonic() + timeout
        found = None
        while time.monotonic() < deadline:
            app.read(timeout=0.2)
            hits = caret.findall(app.output)
            if hits:
                found = tuple(int(v) for v in hits[-1])
                if prev is None or found != prev:
                    return found
        assert found is not None, f"{label}: the client never showed a cursor"
        return found

    with PtyProcess(
        [binary_path(), "standalone"], env=d.env(), cwd=d.work, cols=120, rows=40
    ) as app:
        assert app.wait_output("PROCESSES", timeout=25), app.text()[-2000:]
        # The shell's prompt is the first thing that moves it off (0, 0).
        at_rest = caret_at(app, "at rest")
        style, row, col = at_rest
        assert style == 0, f"a focused stage must use the terminal's own shape, got {style}"
        assert row > 1 and col > 1, f"the caret is in the chrome at {row};{col}, not in the pane"

        # It tracks the program cell for cell: ten characters, ten columns.
        app.write(b"echo hello")
        typed = caret_at(app, "after typing", prev=at_rest)
        _, row2, col2 = typed
        assert row2 == row, f"the caret changed row while typing: {row} -> {row2}"
        assert col2 == col + 10, f"the caret moved {col2 - col} columns for 10 characters"

        # And back, so it is following the program rather than counting the
        # keys this test sent. Backspace and not a left arrow: the fixture's
        # shell is `/bin/sh`, which has no line editing, and an arrow key there
        # is echoed as its own bytes rather than moving anything.
        app.write(b"\x7f\x7f")
        moved = caret_at(app, "after backspace", prev=typed)
        _, row3, col3 = moved
        assert (row3, col3) == (row, col + 8), f"backspace left the caret at {row3};{col3}"

        # Off the stage it stays visible and says it is not listening: a steady
        # underline, the terminal's nearest thing to a hollow cursor.
        app.write(b"\x02P")  # C-b P, focus the PROCESSES rail
        unfocused = caret_at(app, "rail focused", prev=moved)
        style4, row4, col4 = unfocused
        assert style4 == 4, f"an unfocused stage must be a steady underline, got {style4}"
        assert (row4, col4) == (row3, col3), "the caret moved when focus left the stage"

        app.write(b"\x02s")  # C-b s, back to the stage
        style5, _, _ = caret_at(app, "stage refocused", prev=unfocused)
        assert style5 == 0, f"the terminal's own shape did not come back, got {style5}"


@test(profile="standard", tags=("cli", "tui"))
def a_url_on_screen_is_a_link(ctx):
    """The half of the feature that only exists in bytes.

    A hyperlink is invisible in a screen dump — it is state the terminal keeps
    beside the cells — so a reconstructed grid cannot see it and the crate tests
    can only assert what *should* be written. This reads what was: `OSC 8 ; id=
    ; target ST` around the cells of a URL a shell just printed, and the empty
    one that closes it.

    The picker is the other half, and it is on the screen, so it is asserted
    there. `y` closes the loop: its copy is an OSC 52 on the same stream, which
    is what carries a URL back to the machine the terminal is on when there is
    no browser on this one.
    """
    d = ctx.daemon(start=False)
    os.makedirs(d.butai_dir, mode=0o700, exist_ok=True)
    os.makedirs(d.work, exist_ok=True)
    with open(os.path.join(d.butai_dir, "config.toml"), "w") as fh:
        fh.write(d.config.render())
    ctx.cover("cli:standalone")

    url = "https://example.com/a?b=1"
    with PtyProcess(
        [binary_path(), "standalone"], env=d.env(), cwd=d.work, cols=100, rows=30
    ) as app:
        assert app.wait_output("PROCESSES", timeout=25), app.text()[-2000:]

        app.write(f"echo {url}\r".encode())
        assert app.wait_output("example.com", timeout=15), app.text()[-2000:]
        # The frame carrying the shell's output is not necessarily the one that
        # matched: `wait_output` returns on the first repaint that has the text.
        time.sleep(0.5)
        app.read(timeout=0.5)

        assert b"\x1b]8;id=" in app.output, "no hyperlink was written for a URL on screen"
        assert b"\x1b]8;;\x1b\\" in app.output, "a hyperlink was opened and never closed"
        # The whole address, not the prefix that happened to fit a cell run.
        targets = [chunk.split(b"\x1b\\")[0] for chunk in app.output.split(b"\x1b]8;id=")[1:]]
        assert any(t.endswith(url.encode()) for t in targets), (
            f"the link target was truncated: {targets[:3]}"
        )

        # The picker. `C-b f` rather than a bare `f`, because the stage has the
        # keyboard here and a bare key there belongs to the shell.
        app.write(b"\x02f")
        assert app.wait_output("LINKS", timeout=10), app.text()[-2000:]
        assert url in app.text(), "the picker did not list the URL on screen"

        before = len(app.output)
        app.write(b"y")
        time.sleep(0.6)
        app.read(timeout=0.5)
        tail = app.output[before:]
        b64 = base64.b64encode(url.encode())
        assert b"\x1b]52;c;" in tail, "`y` did not put anything on the clipboard"
        assert b64 in tail, "the clipboard payload was not the URL"

        app.write(b"\x02d")
        assert app.wait(timeout=15) is not None, "detach did not end the client"


@test(profile="standard", tags=("cli",))
def cli_reset_restores_a_terminal(ctx):
    """`butai reset` talks only to the tty — no daemon, no nesting guard — so it
    works from the wedged shell a SIGKILLed butai left behind."""
    d = ctx.daemon(start=False)
    ctx.cover("cli:reset")
    env = d.env()
    env["BUTAI"] = "/some/stale/socket"  # the nesting guard must not apply here
    with PtyProcess([binary_path(), "reset"], env=env) as reset:
        code = reset.wait(timeout=15)
        reset.read(timeout=0.3)
    assert code == 0, f"reset exited {code}: {reset.raw()[-500:]}"


@test(profile="standard", tags=("cli", "daemon"))
def a_pane_refuses_to_nest_an_attaching_client(ctx):
    """Every pane gets `$BUTAI`, and a client that sees it refuses to attach —
    otherwise a stray `butai` inside a pane draws the workbench into itself and
    captures keys for the wrong layer.

    The guard is on the attaching paths only (`butai`, `new`, `attach`); one-shot
    control commands like `ls` are unaffected, which this also pins.
    """
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    workspace_name = d.http.detail(ws)["name"]
    pane = d.http.detail(ws)["processes"][0]["pane"]

    with d.attach(Target.pane(pane), cols=110, rows=30) as client:
        client.run_line(f"{binary_path()} new -s nested", timeout=25)
        refusal = client.screen.text()

        client.run_line(f"clear; {binary_path()} ls", timeout=25)
        listing = client.screen.text()

    # "this butai", not "butai": the guard compares sockets, so it refuses only
    # the daemon you are already inside. Attaching a *different* one from a pane
    # is the remote-workbench gesture and stays allowed.
    assert "already inside this butai" in refusal, (
        "attaching a client from inside a pane should be refused; screen was:\n" + refusal
    )
    assert not any(w["name"] == "nested" for w in d.http.workspaces()), (
        "the refused client created a workspace anyway"
    )
    assert workspace_name in listing, (
        "`butai ls` is a one-shot control command and must still work from inside a "
        f"pane; screen was:\n{listing}"
    )


@test(profile="standard", tags=("daemon", "protocol"))
def a_control_connection_needs_no_viewport(ctx):
    """`control` is how one-shot CLIs and structured-state GUIs connect: no
    frames at all, so a client that only wants data pays nothing to render."""
    d = ctx.daemon()
    ctx.cover("target:control", "client:command", "cmd:list_agents", "server:agent_list")
    with Framed(d.socket) as client:
        reply = client.hello(Target.control())
        assert msg_kind(reply) == "hello"
        assert msg_body(reply)["session"] is None, "a control connection has no session"
        client.command("list_agents")
        agents = client.wait_msg("agent_list")
        assert isinstance(msg_body(agents), list)
        assert client.screen.frames == 0, "control connections must not receive frames"


@test(profile="smoke", tags=("cli",))
def cli_workspace_speaks_the_rest_face(ctx):
    """`butai workspace` is an HTTP client of the daemon socket, the way `docker`
    is a client of dockerd. The point of the test is that the CLI and the REST
    API cannot disagree: `--json` re-emits the daemon's own response body, so
    what a script parses is byte-for-byte what `GET /v1/workspaces` returned."""
    d = ctx.daemon()
    ctx.cover("cli:workspace", "GET /v1/workspaces", "POST /v1/workspaces")

    empty = d.cli("workspace", "ls")
    assert empty.returncode == 0, empty.stderr
    assert "no workspaces" in empty.stdout, empty.stdout

    created = d.cli("ws", "create", "--name", "cli-orch", "--cwd", str(d.work))
    assert created.returncode == 0, created.stderr
    ws_id = int(created.stdout.strip())

    listed = d.cli("workspace", "ls", "--json")
    assert listed.returncode == 0, listed.stderr
    assert json.loads(listed.stdout) == d.http.workspaces(), (
        "`--json` must pass the daemon's body through unchanged, so the CLI's "
        f"JSON and the REST API's can never drift; got:\n{listed.stdout}"
    )

    # A name resolves to the same workspace an id does.
    by_name = d.cli("ws", "show", "cli-orch", "--json")
    by_id = d.cli("ws", "show", str(ws_id), "--json")
    assert by_name.returncode == 0, by_name.stderr
    assert json.loads(by_name.stdout) == json.loads(by_id.stdout)

    # --quiet means the exit code is the whole answer.
    quiet = d.cli("ws", "show", str(ws_id), "--quiet")
    assert quiet.returncode == 0, quiet.stderr
    assert quiet.stdout == "", f"--quiet must print nothing, got {quiet.stdout!r}"

    unknown = d.cli("ws", "show", "no-such-workspace")
    assert unknown.returncode != 0, "an unknown name must fail"
    assert "no-such-workspace" in unknown.stderr, unknown.stderr

    removed = d.cli("ws", "rm", str(ws_id))
    assert removed.returncode == 0, removed.stderr
    assert d.http.workspaces() == [], "rm must actually close the workspace"
