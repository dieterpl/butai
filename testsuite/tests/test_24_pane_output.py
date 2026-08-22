"""Reading a pane's output as text.

The daemon owns the VT emulator, so it is the only party that can turn a cell
grid back into lines. This is where that conversion is measured against real
PTYs writing real escape sequences — wide characters, colour, alternate-screen
apps — rather than against a hand-built buffer.

The two negative assertions matter as much as the positive ones: a read is a
*query*, so it must not resize the pane or clear its bell. A scripted reader
polling a sibling would otherwise perturb the very agent it is watching.
"""

from suite import fixtures
from suite.runner import test


def _shell(d, ws, name, command):
    """Start a process pane running `command` and return its pane id."""
    d.http.new_process(ws, name, command)
    procs = d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: any(x["name"] == name for x in p),
        f"the {name} process appeared",
    )
    return next(x["pane"] for x in procs if x["name"] == name)


@test(profile="smoke", tags=("http", "panes"))
def a_pane_reads_back_as_plain_text(ctx):
    d = ctx.daemon()
    ctx.cover("GET /v1/workspaces/{id}/panes/{pane}/output")
    ws = d.http.new_workspace(path=fixtures.workspace(d.work, "readable"))

    pane = _shell(d, ws, "talker", "printf 'ALPHA\\nBETA\\n'; sleep 300")
    d.http.poll_until(
        f"/v1/workspaces/{ws}/panes/{pane}/output",
        lambda o: "BETA" in o["lines"],
        "the pane's output showed up",
    )
    out = d.http.pane_output(ws, pane)

    assert "ALPHA" in out["lines"] and "BETA" in out["lines"], out["lines"]
    assert out["pane"] == pane and out["cols"] > 0 and out["rows"] > 0, out
    assert out["alt_screen"] is False, out
    # No escape sequences survive a text read; that is the whole point of doing
    # the conversion server-side.
    assert not any("\x1b" in line for line in out["lines"]), out["lines"]
    # Trailing blank grid rows are padding, not output.
    assert out["lines"][-1].strip(), f"trailing padding was not trimmed: {out['lines']}"


@test(profile="smoke", tags=("http", "panes"))
def a_read_counts_lines_from_the_end_of_the_output(ctx):
    d = ctx.daemon()
    ws = d.http.new_workspace(path=fixtures.workspace(d.work, "counting"))

    pane = _shell(d, ws, "counter", "for i in $(seq 1 40); do echo line$i; done; sleep 300")
    d.http.poll_until(
        f"/v1/workspaces/{ws}/panes/{pane}/output",
        lambda o: "line40" in o["lines"],
        "all 40 lines were written",
    )

    tail = d.http.pane_output(ws, pane, lines=3)
    assert len(tail["lines"]) <= 3, tail["lines"]
    # The newest lines, not the oldest — and not three rows of blank padding
    # from the bottom of a grid taller than the output.
    assert "line40" in tail["lines"], tail["lines"]
    assert "line1" not in tail["lines"], tail["lines"]
    assert tail["more"] is True, "older lines were left behind and must be reported"


@test(profile="standard", tags=("http", "panes"))
def a_read_survives_wide_characters_and_colour(ctx):
    d = ctx.daemon()
    ws = d.http.new_workspace(path=fixtures.workspace(d.work, "unicode"))

    pane = _shell(d, ws, "cjk", "printf '\\033[31m日本語\\033[0m ok\\n'; sleep 300")
    d.http.poll_until(
        f"/v1/workspaces/{ws}/panes/{pane}/output",
        lambda o: any("日本語" in line for line in o["lines"]),
        "the wide text was rendered",
    )

    text = d.http.pane_output(ws, pane)
    line = next(x for x in text["lines"] if "日本語" in x)
    # One entry per grapheme: no filler cell for a wide character's second
    # column, which is the rule the framed protocol makes clients implement.
    assert "日本語 ok" in line, repr(line)
    assert "\x1b" not in line, repr(line)

    ansi = d.http.pane_output(ws, pane, fmt="ansi")
    assert any("\x1b" in x for x in ansi["lines"]), "ansi format must keep the colour"


@test(profile="standard", tags=("http", "panes"))
def the_footer_source_is_what_the_detector_scans(ctx):
    d = ctx.daemon()
    ws = d.http.new_workspace(path=fixtures.workspace(d.work, "footer"))

    pane = _shell(d, ws, "chatty", "echo hello; sleep 300")
    d.http.poll_until(
        f"/v1/workspaces/{ws}/panes/{pane}/output",
        lambda o: "hello" in o["lines"],
        "output arrived",
    )

    footer = d.http.pane_output(ws, pane, source="footer")
    assert footer["source"] == "footer"
    # `FOOTER_SCAN_ROWS` in `pane/terminal.rs`. Reading the same band the state
    # machine reads is what makes "why is this agent 'working'?" answerable.
    assert len(footer["lines"]) <= 8, footer["lines"]

    screen = d.http.pane_output(ws, pane, source="screen")
    assert screen["source"] == "screen"
    # The viewport keeps its padding: its contract is what the pane looks like.
    assert len(screen["lines"]) == screen["rows"], (len(screen["lines"]), screen["rows"])


@test(profile="smoke", tags=("http", "panes", "agents"))
def reading_a_pane_does_not_disturb_it(ctx):
    d = ctx.daemon()
    ws = d.http.new_workspace(path=fixtures.workspace(d.work, "undisturbed"))

    pane = _shell(d, ws, "beller", "printf '\\a'; sleep 300")
    before = d.http.pane_output(ws, pane)

    # Read it repeatedly, at every source and an absurd length.
    for kwargs in ({}, {"lines": 1}, {"lines": 5000}, {"source": "screen"}, {"source": "footer"}):
        d.http.pane_output(ws, pane, **kwargs)

    after = d.http.pane_output(ws, pane)
    # A framed `pane` attach resizes the pane to the reader's dimensions. This
    # must not, or a script polling a sibling would reflow the terminal of the
    # agent it is watching.
    assert (after["cols"], after["rows"]) == (before["cols"], before["rows"]), (before, after)


@test(profile="standard", tags=("http", "panes"))
def a_read_rejects_nonsense_and_missing_panes(ctx):
    d = ctx.daemon()
    ws = d.http.new_workspace(path=fixtures.workspace(d.work, "bad-reads"))
    pane = _shell(d, ws, "quiet", "sleep 300")

    for query, named in (
        ("source=sideways", "source"),
        ("format=morse", "format"),
        ("lines=lots", "lines"),
    ):
        res = d.http.get(f"/v1/workspaces/{ws}/panes/{pane}/output?{query}")
        assert res.status == 400, f"{query} -> {res.status}: {res.text[:200]}"
        assert named in res.text, f"the error should name {named}: {res.text[:200]}"

    res = d.http.get(f"/v1/workspaces/{ws}/panes/9999/output")
    assert res.status == 404, f"{res.status}: {res.text[:200]}"
