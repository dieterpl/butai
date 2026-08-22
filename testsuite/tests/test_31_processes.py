"""The PROCESSES rail: `.butai.toml`, ready markers, exit codes, restarts.

Opening a workspace brings its processes up like a Procfile, so this is the
first thing a user sees go wrong — and the `ready` matcher in particular has a
sharp edge worth measuring rather than guessing at.
"""

import os
import time

from suite import fixtures
from suite.butai import Target
from suite.daemon import Config
from suite.runner import test

# `[ui] left_rail` default, from `Chrome::compute`. The rail is where a
# process row is drawn; the stage to its right echoes what you type.


@test(profile="smoke", tags=("processes",))
def a_workspace_file_brings_processes_up_like_a_procfile(ctx):
    d = ctx.daemon()
    project = fixtures.workspace(
        d.work,
        "procfile-ws",
        butai_file=fixtures.butai_toml(
            processes=[
                ("dev", "echo 'Local:   http://localhost:5173'; sleep 300", "Local:"),
                ("test", "sleep 300"),
            ]
        ),
    )
    ws = d.http.new_workspace(path=project)
    procs = d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: {"dev", "test"} <= {x["name"] for x in p},
        "both processes appeared",
        timeout=30,
    )
    by_name = {p["name"]: p for p in procs}
    assert by_name["dev"]["command"].startswith("echo"), by_name["dev"]

    ready = d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: any(x["name"] == "dev" and x["status"] == "ok" for x in p),
        "the ready substring flipped dev to ok",
        timeout=30,
    )
    assert next(p for p in ready if p["name"] == "test")["status"] != "ok", (
        "a process with no ready marker must not claim ok"
    )


@test(profile="standard", tags=("processes",))
def exit_codes_become_row_status(ctx):
    """`done` for a clean exit, `FAIL(n)` for anything else — and a failed row
    stays in the rail so its output is still readable."""
    d = ctx.daemon()
    project = fixtures.workspace(
        d.work,
        "exit-ws",
        butai_file=fixtures.butai_toml(
            processes=[("good", "echo fine"), ("bad", "echo broken; exit 7")]
        ),
    )
    ws = d.http.new_workspace(path=project)
    procs = d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: any(x["name"] == "bad" and x["status"].startswith("FAIL") for x in p),
        "the failing process reported FAIL",
        timeout=30,
    )
    bad = next(p for p in procs if p["name"] == "bad")
    assert bad["status"] == "FAIL(7)", bad
    assert bad["exited"] == 7, bad
    ctx.note("a process that exits non-zero stays in the rail as FAIL(n)")


@test(profile="standard", tags=("processes",))
def a_ready_marker_wrapped_in_colour_still_matches(ctx):
    """The matcher runs over raw PTY bytes, so SGR codes *around* the marker are
    harmless. Worth pinning: almost every dev server colours its ready line."""
    d = ctx.daemon()
    project = fixtures.workspace(
        d.work,
        "colour-ready-ws",
        butai_file=fixtures.butai_toml(
            processes=[("dev", "printf '\\033[32mLocal:\\033[0m ready\\n'; sleep 300", "Local:")]
        ),
    )
    ws = d.http.new_workspace(path=project)
    d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: any(x["name"] == "dev" and x["status"] == "ok" for x in p),
        "a colourised ready marker was matched",
        timeout=30,
    )


@test(profile="standard", tags=("processes",))
def a_ready_marker_split_across_two_writes_is_still_matched(ctx):
    """A marker straddling two output bursts still counts.

    Output arrives coalesced per drain, and a server's startup banner is
    routinely written in more than one syscall or lands on a 64 KiB read
    boundary — so the scan carries the previous burst's tail."""
    d = ctx.daemon()
    script = fixtures.write(
        os.path.join(d.work, "split.sh"),
        "#!/bin/sh\nprintf 'REA'\nsleep 0.8\nprintf 'DY-MARKER\\n'\nsleep 300\n",
        mode=0o755,
    )
    project = fixtures.workspace(
        d.work,
        "split-ready-ws",
        butai_file=fixtures.butai_toml(processes=[("dev", script, "READY-MARKER")]),
    )
    ws = d.http.new_workspace(path=project)
    d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: any(x["name"] == "dev" and x["status"] == "ok" for x in p),
        "the split ready marker was matched",
        timeout=25,
    )


@test(profile="standard", tags=("processes",))
def a_malformed_workspace_file_degrades_instead_of_failing(ctx):
    """A `.butai.toml` typo must not stop you opening the project — but it is
    also entirely silent to an API client, which is worth knowing."""
    d = ctx.daemon()
    project = fixtures.workspace(d.work, "broken-toml-ws", butai_file="[[processes]\nname = \n")
    ws = d.http.new_workspace(path=project)
    procs = d.http.processes(ws)
    assert procs, "the workspace should still get its shell row"
    assert all(p["name"] == "shell" or p["command"] for p in procs), procs
    ctx.note(
        "a malformed .butai.toml yields zero configured processes and warns only in "
        "the daemon log — nothing surfaces over the API"
    )
    d.assert_healthy()


@test(profile="standard", tags=("processes",))
def restarting_clears_the_ready_flag(ctx):
    """`ready_seen` is sticky for the life of a pane, so a restart has to reset
    it — otherwise a crashed dev server keeps reporting ok."""
    d = ctx.daemon()
    marker = os.path.join(d.work, "started-once")
    script = fixtures.write(
        os.path.join(d.work, "slow-ready.sh"),
        "#!/bin/sh\n"
        f"if [ -f {marker} ]; then sleep 300; else touch {marker}; "
        "echo SERVER-READY; sleep 300; fi\n",
        mode=0o755,
    )
    project = fixtures.workspace(
        d.work,
        "restart-ready-ws",
        butai_file=fixtures.butai_toml(processes=[("dev", script, "SERVER-READY")]),
    )
    ws = d.http.new_workspace(path=project)
    procs = d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: any(x["name"] == "dev" and x["status"] == "ok" for x in p),
        "dev reported ok on first start",
        timeout=30,
    )
    pane = next(p for p in procs if p["name"] == "dev")["pane"]

    d.http.ok("POST", f"/v1/workspaces/{ws}/processes/{pane}/restart")
    after = d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: any(x["name"] == "dev" and x["pane"] != pane for x in p),
        "dev restarted on a new pane",
        timeout=30,
    )
    status = next(p for p in after if p["name"] == "dev")["status"]
    assert status != "ok", f"a restarted process kept its stale ready flag ({status})"


@test(profile="standard", tags=("processes", "http"))
def the_api_reports_what_a_shell_row_is_running(ctx):
    """The rail's relabelling reaches REST clients too, so a GUI that draws
    natively — the web client, the native apps — shows what the TUI shows
    instead of six identical `shell` rows."""
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    pane = d.http.detail(ws)["processes"][0]["pane"]
    with d.attach(Target.pane(pane), cols=110, rows=30) as client:
        client.type_line("sleep 23456")
        time.sleep(2.0)
        d.http.poll_until(
            f"/v1/workspaces/{ws}/processes",
            lambda procs: any("23456" in p["name"] or "23456" in p["command"] for p in procs),
            "the API reported the running command",
            timeout=20,
        )


@test(profile="standard", tags=("processes", "agents"))
def autostart_agents_come_up_with_the_workspace(ctx):
    d = ctx.daemon(config=Config().fake_agents("fake-claude"))
    project = fixtures.workspace(
        d.work,
        "autostart-ws",
        butai_file=fixtures.butai_toml(autostart=["fake-claude"]),
    )
    ws = d.http.new_workspace(path=project)
    agents = d.http.poll_until(
        f"/v1/workspaces/{ws}/agents",
        lambda a: len(a) >= 1,
        "the autostart agent appeared",
        timeout=30,
    )
    assert agents[0]["title"], agents


@test(profile="standard", tags=("processes",))
def exiting_the_lone_shell_closes_the_workspace(ctx):
    """A workspace whose only pane is its shell goes away when you type `exit`.
    With the shipped `exit_when_empty` default, the last one takes the daemon
    with it — which is exactly the behaviour that makes a naive container
    entrypoint look flaky."""
    d = ctx.daemon(config=Config().set(exit_when_empty=False))
    ws = d.http.new_workspace(path=d.work)
    pane = d.http.detail(ws)["processes"][0]["pane"]
    with d.attach(Target.pane(pane), cols=100, rows=30) as client:
        client.type_line("exit")
        client.pump(2.0)
    d.http.poll_until(
        "/v1/workspaces",
        lambda w: all(x["id"] != ws for x in w),
        "the workspace closed with its shell",
        timeout=30,
    )
    assert d.alive(), "exit_when_empty = false should keep the daemon up"
