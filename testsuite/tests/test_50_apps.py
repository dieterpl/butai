"""Terminal-app fidelity.

Agents are ordinary CLIs in PTY panes with "full TUI fidelity", and the daemon
— not the client — owns the VT emulator. So the question this file answers is:
does a real terminal application, rendered server-side and shipped as styled
cell runs, still look like itself?

Everything here reads the app back through the framed protocol, i.e. through
exactly the path a GUI client sees.
"""

import os
import re
import shutil
import time

from suite import fixtures
from suite.butai import PROTOCOL_VERSION, Target
from suite.metrics import human_bytes, rss_kb
from suite.runner import test

# (name, command, [signatures]) — a signature is text the app is expected to
# draw. Several are accepted per app so a version bump does not fail the suite
# for a cosmetic reason. `{work}` expands to the workspace directory.
APPS = [
    ("htop", "htop", ["Tasks", "Load average", "Mem", "F1"]),
    ("top", "top", ["load average", "Tasks", "%Cpu"]),
    ("btop", "btop", ["btop", "cpu", "Cpu", "MEM"]),
    ("ncdu", "ncdu /usr/share", ["ncdu", "Total disk usage", "Scanning"]),
    ("vim", "vim", ["VIM - Vi IMproved", "VIM"]),
    ("nano", "nano {work}/nano-probe.txt", ["GNU nano", "Get Help", "Exit"]),
    ("less", "less /etc/os-release", ["Debian", "PRETTY_NAME", "NAME="]),
    ("mc", "mc", ["Command", "Left", "Right", "Name"]),
    ("tmux", "tmux new-session -s inner", ["inner", "bash", "sh"]),
]

# Apps whose absence or failure should fail the suite rather than be reported:
# these are the shapes every other TUI is a variation of (a full-screen redraw
# loop, an alt-screen editor, and a pager).
#
# btop earns its place for a reason the others do not cover: it positions the
# cursor with HVP (`CSI row;col f`) rather than CUP (`CSI row;col H`), and the
# emulator normalizes the two (`HvpRewriter` in `pane/terminal.rs`). Every other
# app here uses CUP, so btop is the only guard against that regressing — and
# when it did, it rendered as overlapping soup rather than not drawing at all.
REQUIRED = {"htop", "vim", "less", "btop"}


def _stage(d, cols=140, rows=42):
    """A workspace plus a client attached to its shell pane, full-bleed."""
    ws = d.http.new_workspace(path=d.work)
    pane = d.http.detail(ws)["processes"][0]["pane"]
    client = d.framed()
    client.hello(Target.pane(pane), cols=cols, rows=rows, cwd=d.work)
    return ws, pane, client


def _run_probe(ctx, d, name, body=None, cols=120, rows=36):
    script = fixtures.probe(d.work, name, extra=body)
    ws = d.http.new_workspace(path=d.work)
    d.http.new_process(ws, name, script)
    procs = d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: any(x["name"] == name for x in p),
        f"the {name} probe started",
        timeout=30,
    )
    pane = next(p for p in procs if p["name"] == name)["pane"]
    client = d.framed()
    client.hello(Target.pane(pane), cols=cols, rows=rows, cwd=d.work)
    return ws, pane, client


@test(profile="standard", tags=("apps", "matrix"), timeout=600)
def full_screen_terminal_apps_render_through_the_daemon(ctx):
    """The compatibility matrix. Each app is spawned as a process row and read
    back through the wire protocol, so a pass means a GUI client would see it
    too."""
    d = ctx.daemon()
    fixtures.write(os.path.join(d.work, "nano-probe.txt"), "nano probe\n")
    failures = []

    for name, template, signatures in APPS:
        command = template.format(work=d.work)
        binary = command.split()[0]
        if shutil.which(binary) is None:
            ctx.row("terminal apps", app=name, result="not installed", matched="-")
            if name in REQUIRED:
                failures.append(f"{name} is required but not installed")
            continue

        ws = d.http.new_workspace(path=d.work)
        d.http.new_process(ws, name, command)
        procs = d.http.poll_until(
            f"/v1/workspaces/{ws}/processes",
            lambda p: any(x["name"] == name for x in p),
            f"the {name} row appeared",
            timeout=30,
        )
        pane = next(p for p in procs if p["name"] == name)["pane"]

        matched = None
        with d.framed() as client:
            client.hello(Target.pane(pane), cols=140, rows=42, cwd=d.work)
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline and matched is None:
                client.pump(0.5)
                text = client.screen.text()
                matched = next((s for s in signatures if s in text), None)
            dump = client.screen.dump(limit=6)

        ctx.row(
            "terminal apps",
            app=name,
            result="ok" if matched else "no",
            matched=matched or "(nothing drew)",
        )
        if matched is None and name in REQUIRED:
            failures.append(f"{name} never rendered; first rows were:\n{dump}")
        d.http.delete(f"/v1/workspaces/{ws}")

    assert not failures, "\n".join(failures)
    d.assert_healthy()


@test(profile="standard", tags=("apps", "render"), timeout=120)
def btop_positions_with_hvp_and_still_lines_up(ctx):
    """btop seeks with HVP (`CSI row;col f`), which vt100 does not implement —
    the emulator rewrites it to CUP. When that broke, btop still drew all of its
    *text*, just piled onto whatever line the cursor was on, so the signature
    match above passed while the pane was unreadable. This asserts the frame
    instead: closed box rows can only appear if the seeks actually landed."""
    if shutil.which("btop") is None:
        ctx.row("btop layout", result="not installed", framed="-")
        raise AssertionError("btop is required for the HVP regression guard")

    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    d.http.new_process(ws, "btop", "btop")
    procs = d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: any(x["name"] == "btop" for x in p),
        "the btop row appeared",
        timeout=30,
    )
    pane = next(p for p in procs if p["name"] == "btop")["pane"]

    rows, framed, dump = 42, [], ""
    with d.framed() as client:
        client.hello(Target.pane(pane), cols=140, rows=rows, cwd=d.work)
        deadline = time.monotonic() + 25
        while time.monotonic() < deadline and not framed:
            client.pump(0.5)
            # A box row btop drew in one piece: opens and closes on the same
            # line. Without the HVP rewrite no line ever closes its frame.
            framed = [
                i
                for i in range(rows)
                if (line := client.screen.display_line(i).rstrip())
                and line[0] in "╭├╰"
                and line[-1] in "╮┤╯│"
            ]
        dump = client.screen.dump(limit=6)
    d.http.delete(f"/v1/workspaces/{ws}")

    ctx.row("btop layout", result="ok" if framed else "no", framed=f"{len(framed)} rows")
    assert framed, (
        "btop drew no closed box row, so its HVP seeks were dropped "
        f"(the emulator must rewrite `CSI ...f` to `CSI ...H`); screen:\n{dump}"
    )


@test(profile="standard", tags=("apps", "render"))
def text_attributes_survive_the_round_trip(ctx):
    """The wire carries six modifiers, but the vt100 -> ratatui bridge forwards
    four. This asserts which ones actually reach a client, so nobody has to find
    out by shipping a GUI that renders dim text as normal."""
    d = ctx.daemon()
    _, _, client = _run_probe(ctx, d, "sgr")
    with client:
        client.wait_text("SGR-PROBE-DONE", timeout=25)
        client.pump(0.5)
        seen = client.screen.styles_in_use()
        colors = client.screen.colors_in_use()

    for attr in ("bold", "italic", "underline", "reverse"):
        assert attr in seen, f"{attr} did not survive the round trip; saw {sorted(seen)}"

    dropped = [a for a in ("dim", "crossed_out") if a not in seen]
    if dropped:
        ctx.note(
            f"{', '.join(dropped)} exist in the wire format but are dropped by the "
            "vt100 -> ratatui bridge, so a client can never render them"
        )
    ctx.row(
        "sgr attributes",
        forwarded=", ".join(sorted(seen)) or "none",
        dropped=", ".join(dropped) or "none",
    )

    rgb = [c for c in colors if isinstance(c, tuple) and c[0] == "rgb"]
    indexed = [c for c in colors if isinstance(c, tuple) and c[0] == "indexed"]
    assert rgb, f"truecolor did not reach the client; colors seen: {sorted(map(str, colors))}"
    assert indexed, "256-colour indices did not reach the client"


@test(profile="standard", tags=("apps", "render"))
def a_wide_character_consumes_two_columns_without_a_placeholder_cell(ctx):
    """The sharpest edge in the whole wire format for a client author.

    A run carries *consecutive graphemes* and nothing else: there is no filler
    cell for the second column of a wide character. So a client must advance its
    cursor by each grapheme's display width, and one that advances by one per
    cell shifts every character after the first CJK or emoji on the line.

    Both `examples/api-client.py` and `testsuite/suite/screen.py` advance by
    width; a reader that does not is the bug this pins.
    """
    d = ctx.daemon()
    _, _, client = _run_probe(ctx, d, "unicode")
    with client:
        client.wait_text("UNICODE-PROBE-DONE", timeout=25)
        client.pump(0.5)
        found = client.screen.find("CJK:")
        assert found, f"probe never drew:\n{client.screen.dump(limit=8)}"
        x, y = found
        cells = [client.screen.cell(x + i, y) for i in range(4, 10)]
        line = client.screen.display_line(y)

    assert cells[0].ch == "日", f"wide glyph landed as {cells[0]!r}"
    assert cells[2].ch == "本", (
        f"the second wide glyph should sit two columns on, found {cells[2]!r} at +6 "
        f"and {cells[1]!r} at +5 — the run packs graphemes with no filler cell, so a "
        "client that advanced by one per cell would have placed it at +5"
    )
    assert cells[4].ch == "語", f"third wide glyph misplaced: {cells[4]!r}"
    ctx.note(
        "wide characters carry no placeholder cell: a run is consecutive graphemes and "
        "the client advances by display width (`CJK:日本語` is 7 cells over 10 columns)"
    )
    assert "日本語" in line, line


@test(profile="standard", tags=("apps", "vt"))
def the_daemon_answers_terminal_queries_on_the_childs_behalf(ctx):
    """vt100 parses cursor-position and device-attribute queries but never
    replies, so butai answers them itself. If that regressed, every app that
    probes its terminal on startup would hang before drawing anything."""
    d = ctx.daemon()
    _, _, client = _run_probe(ctx, d, "queries")
    with client:
        client.wait_text("QUERIES-PROBE-DONE", timeout=25)
        client.pump(0.5)
        text = client.screen.text()

    cpr = re.search(r"CPR=\[(\d+);(\d+)\|", text)
    assert cpr, f"no cursor-position report came back:\n{text[:600]}"
    assert "DA1=[?1;2|" in text, f"no primary device-attributes reply:\n{text[:600]}"
    # Secondary DA identifies butai in its `Pp` field with 98 (`b`), the way tmux
    # answers 84 (`T`), rather than the generic 0. That is not decoration: it is
    # how a `butai` started on the far end of an ssh session inside one of our
    # panes finds out that it is inside one, and `butai/src/handoff.rs` matches on
    # exactly this prefix. `Pv` carries the protocol version, so it is matched as
    # a number rather than pinned to today's value.
    da2 = re.search(r"DA2=\[>98;(\d+);0\|", text)
    assert da2, f"no secondary device-attributes reply naming butai:\n{text[:600]}"
    assert int(da2.group(1)) == PROTOCOL_VERSION, (
        f"DA2 reported protocol version {da2.group(1)}, expected {PROTOCOL_VERSION}"
    )
    ctx.note(f"cursor reported at row {cpr.group(1)}, col {cpr.group(2)}")
    ctx.note(f"secondary DA names butai: 98;{da2.group(1)};0")


@test(
    profile="standard",
    tags=("apps", "vt"),
    xfail="Only DSR (ESC[6n, ESC[5n) and DA (ESC[c, ESC[>c) are answered. XTVERSION "
    "(ESC[>0q), XTGETTCAP and DECRQM get no reply at all, so an app that blocks on one "
    "waits forever instead of falling back. Answering with an empty/negative response "
    "would unblock them.",
)
def unanswered_terminal_queries_do_not_hang_an_app(ctx):
    d = ctx.daemon()
    _, _, client = _run_probe(ctx, d, "xtversion")
    with client:
        client.wait_text("XTVERSION-PROBE-START", timeout=25)
        client.wait_text("XTVERSION-PROBE-ANSWERED", timeout=12)


@test(profile="standard", tags=("apps", "vt"))
def the_alternate_screen_hides_what_came_before(ctx):
    """Every full-screen app switches to the alt screen; if the primary screen
    bled through, a GUI would show the shell history behind vim."""
    d = ctx.daemon()
    _, _, client = _run_probe(ctx, d, "altscreen")
    with client:
        client.wait_text("PRIMARY-SCREEN-TEXT", timeout=25)
        client.wait_text("ALT-SCREEN-TEXT", timeout=25)
        client.pump(0.5)
        text = client.screen.text()
    assert "PRIMARY-SCREEN-TEXT" not in text, (
        f"primary-screen content bled through the alt screen:\n{text}"
    )


@test(profile="standard", tags=("apps", "resize"))
def an_app_is_told_when_the_pane_changes_size(ctx):
    """The PTY has to be resized and the child SIGWINCHed, or every TUI keeps
    drawing at the old geometry."""
    d = ctx.daemon()
    _, _, client = _run_probe(ctx, d, "winsize", cols=100, rows=30)
    with client:
        client.wait_text("WINSIZE ", timeout=25)
        client.pump(1.0)
        first = [ln for ln in client.screen.text().splitlines() if "WINSIZE" in ln][-1]

        client.resize(132, 44)
        deadline = time.monotonic() + 20
        second = first
        while time.monotonic() < deadline and second == first:
            client.pump(0.5)
            lines = [ln for ln in client.screen.text().splitlines() if "WINSIZE" in ln]
            if lines:
                second = lines[-1]

    assert second != first, f"the app never saw a new size (still {first.strip()!r})"
    ctx.note(f"pane size before/after client resize: {first.strip()} -> {second.strip()}")


@test(profile="standard", tags=("apps", "shells"), timeout=300)
def interactive_shells_run_in_a_pane(ctx):
    """bash, zsh and fish each set up their own line editing, prompt drawing and
    terminal modes; a multiplexer that gets any of it wrong is unusable.

    Each shell is asked for its *own* version variable, so the answer also
    proves the command ran in that shell rather than falling through to the
    parent one — which is what a weaker `echo MARKER` test would miss.
    """
    d = ctx.daemon()
    # zsh runs `zsh-newuser-install` when its HOME has no startup files, which
    # is a modal wizard rather than a shell. Every test daemon gets a fresh
    # HOME, so give it the file a real user would already have.
    fixtures.write(os.path.join(d.home, ".zshrc"), "# testsuite\n")
    versions = {
        "bash": "$BASH_VERSION",
        "zsh": "$ZSH_VERSION",
        "fish": "$FISH_VERSION",
    }
    failures = []
    for shell, variable in versions.items():
        if shutil.which(shell) is None:
            ctx.row("shells", shell=shell, result="not installed", version="-")
            continue
        ws, pane, client = _stage(d)
        version = None
        with client:
            client.type_line(f"{shell} -i")
            time.sleep(2.5)
            client.type_line(f"echo MARK-{variable}-END")
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline and version is None:
                client.pump(0.5)
                # Greedy, so bash's `5.2.15(1)-release` survives its own hyphen;
                # anchored on a leading digit so the echoed command line (which
                # still shows the literal `$BASH_VERSION`) cannot match.
                found = re.search(r"MARK-(\d\S*)-END", client.screen.text())
                if found:
                    version = found.group(1)
            dump = client.screen.dump(limit=12)

        ctx.row(
            "shells",
            shell=shell,
            result="ok" if version else "no version",
            version=version or "-",
        )
        if not version:
            failures.append(f"{shell} never reported {variable}; screen was:\n{dump}")
        d.http.delete(f"/v1/workspaces/{ws}")

    assert not failures, "\n".join(failures)
    d.assert_healthy()


@test(profile="standard", tags=("apps",))
def scrollback_is_capped_rather_than_unbounded(ctx):
    """Panes keep `general.scrollback` lines. A pane that printed a million
    lines must not have kept them all."""
    d = ctx.daemon()
    ws, pane, client = _stage(d, cols=100, rows=30)
    with client:
        client.run_line("seq 1 200000", timeout=120)
    resident = rss_kb(d.pid) or 0
    ctx.metric("rss_after_200k_lines", human_bytes(resident * 1024))
    assert resident < 1024 * 1024, f"daemon held {human_bytes(resident * 1024)} after 200k lines"
    d.assert_healthy()
