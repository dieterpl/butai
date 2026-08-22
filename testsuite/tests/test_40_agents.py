"""Agent status detection.

butai tells you whether an agent is working, waiting or done by re-rendering its
pane and scanning the bottom 8 lines for marker strings — there is no protocol
between butai and the agent. That makes compatibility a property of what each
CLI *draws*, and fragile in ways worth measuring: a reworded status line, a
narrow pane, or a tall footer each break it silently.

The doubles in `testsuite/fakeagents/` reproduce each real CLI's drawing, so
this runs deterministically with no API keys. `test_41_agents_real.py` runs the
same assertions against the real binaries when credentials are available.
"""

import os
import time

from suite.butai import Target, msg_kind
from suite.daemon import Config, fakeagent_dir
from suite.runner import test

AGENTS = ["fake-claude", "fake-codex", "fake-gemini", "fake-aider"]


def agent_config(*specs):
    """Build a config from `(name, fake_script[, binary])` triples.

    `binary` names a script in `testsuite/fakeagents/` and defaults to
    `fake-claude`, whose drawing is the one butai is best at reading.
    """
    config = Config()
    for spec in specs:
        name, script = spec[0], spec[1]
        binary = spec[2] if len(spec) > 2 else "fake-claude"
        config.agent(name, os.path.join(fakeagent_dir(), binary), env={"FAKE_SCRIPT": script})
    return config


def spawn(d, ws, name):
    d.http.spawn_agent(ws, name)
    agents = d.http.poll_until(
        f"/v1/workspaces/{ws}/agents", lambda a: len(a) >= 1, "the agent appeared", timeout=30
    )
    return agents[-1]["pane"]


def await_state(d, ws, pane, wanted, timeout=40):
    agents = d.http.poll_until(
        f"/v1/workspaces/{ws}/agents",
        lambda a: any(x["pane"] == pane and x["state"] in wanted for x in a),
        f"the agent reported {'/'.join(sorted(wanted))}",
        timeout=timeout,
    )
    return next(x for x in agents if x["pane"] == pane)


def state_within(d, ws, pane, wanted, timeout=25):
    """`await_state` that returns None instead of failing — for the matrices."""
    try:
        return await_state(d, ws, pane, wanted, timeout=timeout)
    except AssertionError:
        return None


# Comfortably longer than the sum of everything that can keep an agent in
# `working` without a marker: a streaming double's turn (~8s), the output
# recency window (2s), the sampler tick (2s), and the settle window before
# `finished` (3s).
QUIET = 20


def state_after_quiet(d, ws, pane, seconds=QUIET):
    """The agent's state once its pane has been silent for a while.

    butai has two independent working signals: a footer marker, and raw output
    recency. Only the marker survives a quiet pane, so a test that reads the
    state immediately cannot tell which one fired — and a double that repainted
    once would look like a working agent either way. Waiting out the recency
    window is what makes a marker assertion actually about the marker.
    """
    time.sleep(seconds)
    agents = d.http.agents(ws)
    row = next((a for a in agents if a["pane"] == pane), None)
    assert row is not None, f"pane {pane} left the rail: {agents}"
    return row


@test(profile="smoke", tags=("agents",), timeout=120)
def a_recognised_interrupt_hint_reports_working(ctx):
    """The steady signal, and the reason it exists: the hint stays on screen for
    the whole turn, so a thinking pause no longer looks like the turn ended.

    The pane goes silent after drawing once, so by the time the second assertion
    runs the output-recency fallback has long expired — only the marker can
    still be holding the agent in `working`.
    """
    d = ctx.daemon(config=agent_config(("claude", "busy:120")))
    ctx.cover("agent:working")
    ws = d.http.new_workspace(path=d.work)
    pane = spawn(d, ws, "claude")
    agent = await_state(d, ws, pane, {"working"})
    assert agent["question"] is False, agent

    settled = state_after_quiet(d, ws, pane)
    assert settled["state"] == "working", (
        f"the agent dropped to {settled['state']!r} while its interrupt hint was still "
        "on screen — a thinking pause is being mistaken for the end of a turn"
    )


@test(profile="smoke", tags=("agents",))
def a_permission_dialog_reports_waiting_and_flags_a_question(ctx):
    d = ctx.daemon(config=agent_config(("claude", "question:120")))
    ctx.cover("agent:waiting")
    ws = d.http.new_workspace(path=d.work)
    pane = spawn(d, ws, "claude")
    agent = await_state(d, ws, pane, {"waiting"})
    assert agent["question"] is True, f"a decision dialog should set question: {agent}"

    d.http.poll_until(
        "/v1/workspaces",
        lambda w: any(x["id"] == ws and x.get("waiting", 0) >= 1 for x in w),
        "the workspace summary counts the waiting agent",
        timeout=25,
    )


@test(profile="standard", tags=("agents",))
def a_multiple_choice_question_reports_waiting(ctx):
    """Claude Code's AskUserQuestion dialog, which nothing about the permission
    box prepares you for: options with descriptions push the highlighted `❯ 1.`
    thirteen rows up, out of the 8-row band, and the question is a plain "which
    …?" rather than a "do you want to…". Its hint line is all that is left to
    read — and half of that line ("esc to cancel") is another CLI's *working*
    marker, so before this was matched the agent sat in `working` for as long as
    the question went unanswered."""
    d = ctx.daemon(config=agent_config(("claude", "choice:120")))
    ctx.cover("agent:waiting")
    ws = d.http.new_workspace(path=d.work)
    pane = spawn(d, ws, "claude")
    agent = await_state(d, ws, pane, {"waiting"})
    assert agent["question"] is True, f"a multiple-choice dialog should set question: {agent}"

    d.http.poll_until(
        "/v1/workspaces",
        lambda w: any(x["id"] == ws and x.get("questions", 0) >= 1 for x in w),
        "the workspace summary counts the question",
        timeout=25,
    )


@test(profile="standard", tags=("agents",))
def prose_that_merely_mentions_interrupting_changes_nothing(ctx):
    """The negative control, and the reason every marker is anchored to a key
    press: an agent writes "to interrupt" and "do you want to" in ordinary
    sentences, and that prose scrolls through the footer band."""
    d = ctx.daemon(config=agent_config(("claude", "prose:60")))
    ctx.cover("agent:idle")
    ws = d.http.new_workspace(path=d.work)
    pane = spawn(d, ws, "claude")
    agent = await_state(d, ws, pane, {"idle", "finished"}, timeout=30)
    assert agent["state"] != "working", f"prose was mistaken for a working marker: {agent}"
    assert agent["question"] is False, f"prose was mistaken for a question: {agent}"


@test(profile="standard", tags=("agents",))
def sustained_output_alone_reports_working(ctx):
    """The fallback for agents whose status line butai does not recognise —
    aider, and anything new. Recency is a weaker signal, which is why it needs a
    full second of streaming before it counts."""
    d = ctx.daemon(config=agent_config(("aider", "noisy:20", "fake-aider")))
    ws = d.http.new_workspace(path=d.work)
    pane = spawn(d, ws, "aider")
    await_state(d, ws, pane, {"working"}, timeout=30)


@test(profile="standard", tags=("agents",))
def a_turn_that_settles_reports_finished(ctx):
    """`finished` is 'your move', distinct from `waiting`'s 'act now' — it needs
    a quiet window so a thinking pause is not mistaken for the end of a turn."""
    d = ctx.daemon(config=agent_config(("claude", "busy:6,idle:600")))
    ctx.cover("agent:finished")
    ws = d.http.new_workspace(path=d.work)
    pane = spawn(d, ws, "claude")
    await_state(d, ws, pane, {"working"}, timeout=30)
    await_state(d, ws, pane, {"finished"}, timeout=45)


@test(profile="standard", tags=("agents", "protocol"))
def an_agent_that_needs_you_rings_the_client_watching_the_workspace(ctx):
    """The bell is what reaches you when you are looking at something else.

    So it goes to every interactive client on the workspace, and "on the
    workspace" now means holding *any* of its panes: a workbench is one pane
    connection plus `/v1/*`, and this client is deliberately watching the shell
    rather than the agent that rings.
    """
    d = ctx.daemon(config=agent_config(("claude", "question:120")))
    ctx.cover("server:bell")
    ws, client = d.stage(cols=120, rows=36)
    with client:
        d.http.spawn_agent(ws, "claude")
        assert msg_kind(client.wait_msg("bell", timeout=45)) == "bell"


@test(profile="standard", tags=("agents",))
def acking_a_bell_clears_the_waiting_state(ctx):
    """Without this a non-TUI client can never dismiss an alert, and the agent
    reports `waiting` forever."""
    d = ctx.daemon(config=agent_config(("claude", "bell:120")))
    ws = d.http.new_workspace(path=d.work)
    pane = spawn(d, ws, "claude")
    await_state(d, ws, pane, {"waiting"}, timeout=30)

    d.http.ok("POST", f"/v1/workspaces/{ws}/panes/{pane}/ack")
    await_state(d, ws, pane, {"idle", "finished"}, timeout=30)


@test(profile="standard", tags=("agents",))
def killing_an_agent_removes_it_from_the_rail(ctx):
    d = ctx.daemon(config=agent_config(("claude", "busy:600")))
    ws = d.http.new_workspace(path=d.work)
    pane = spawn(d, ws, "claude")
    await_state(d, ws, pane, {"working"}, timeout=30)
    d.http.ok("DELETE", f"/v1/workspaces/{ws}/panes/{pane}")
    d.http.poll_until(
        f"/v1/workspaces/{ws}/agents",
        lambda a: all(x["pane"] != pane for x in a),
        "the agent left the rail",
        timeout=30,
    )


@test(profile="standard", tags=("agents",))
def aiders_confirmation_prompt_is_recognised(ctx):
    """aider spells its confirmation `(Y)es/(N)o/(A)ll/(S)kip all [Yes]:`, which
    is chrome no prose contains — so it is a prompt marker in its own right."""
    d = ctx.daemon(config=agent_config(("aider", "question:120", "fake-aider")))
    ws = d.http.new_workspace(path=d.work)
    pane = spawn(d, ws, "aider")
    await_state(d, ws, pane, {"waiting"}, timeout=30)


@test(
    profile="standard",
    tags=("agents",),
    xfail="Status is read only from the bottom 8 rendered rows (FOOTER_SCAN_ROWS). An agent "
    "that draws its interrupt hint above a taller footer — a context/model/shortcuts block, "
    "which several CLIs do — has a perfectly good marker that butai never sees. It reads as "
    "working only while its output is still recent, then falls to idle mid-turn.",
    timeout=120,
)
def a_marker_above_a_tall_footer_is_still_seen(ctx):
    d = ctx.daemon(config=agent_config(("verbose", "busy:120", "fake-tallfooter")))
    ws = d.http.new_workspace(path=d.work)
    pane = spawn(d, ws, "verbose")
    settled = state_after_quiet(d, ws, pane)
    assert settled["state"] == "working", (
        f"the agent reads {settled['state']!r} while its interrupt hint is on screen, "
        "just above the 8-row band"
    )


@test(profile="standard", tags=("agents", "matrix"), timeout=600)
def agent_compatibility_matrix(ctx):
    """One row per shipped agent: does butai see it working, does that survive a
    thinking pause, and does it see the agent ask a question?

    The middle column is the one that matters in daily use. A footer marker
    holds `working` for the whole turn; output recency only holds it while the
    agent is actively printing, so an agent detected that way flickers back to
    idle every time it stops to think. Reported rather than asserted per-agent,
    because it is a fact about each CLI's UI — the one hard assertion is that no
    agent goes completely unnoticed, which would make the rail useless.
    """
    unnoticed = []
    for name in AGENTS:
        config = agent_config(
            ("busy", "busy:120", name),
            ("ask", "question:120", name),
        )
        d = ctx.daemon(config=config, name=f"matrix-{name}")

        ws = d.http.new_workspace(path=d.work)
        busy_pane = spawn(d, ws, "busy")
        busy = state_within(d, ws, busy_pane, {"working"}, timeout=30)
        held = state_after_quiet(d, ws, busy_pane) if busy else None

        ws2 = d.http.new_workspace(path=d.work)
        asked = state_within(d, ws2, spawn(d, ws2, "ask"), {"waiting"}, timeout=30)

        holds = bool(held and held["state"] == "working")
        ctx.row(
            "agent compatibility",
            agent=name.replace("fake-", ""),
            working="detected" if busy else "missed",
            holds_through_a_pause="yes" if holds else "no",
            via="footer marker" if holds else "output recency",
            waiting="detected" if asked else "missed",
            question="yes" if asked and asked.get("question") else "no",
        )
        if not busy:
            unnoticed.append(name)
        if busy and not holds:
            ctx.note(
                f"{name}: butai only sees it working while it is printing — it drops to "
                f"{held['state']!r} during a pause, so the rail flickers mid-turn"
            )
        if not asked:
            ctx.note(
                f"{name}: its confirmation prompt is not recognised, so the agent reports "
                "idle while it is blocked on you"
            )
        d.stop()

    assert not unnoticed, (
        "butai never noticed these agents were working: " + ", ".join(unnoticed)
    )


@test(profile="standard", tags=("agents", "matrix"), timeout=600)
def pane_size_envelope_for_status_detection(ctx):
    """Every pane in a workspace is sized to the stage, so the attached client's
    terminal decides whether an agent's status line lands inside the 8-row band.

    Reported as a table: this is the envelope a user is really operating in when
    they run butai in a narrow split or from a phone-sized client.
    """
    for cols, rows in ((80, 24), (110, 30), (200, 50)):
        d = ctx.daemon(config=agent_config(("claude", "busy:120")), name=f"size-{cols}x{rows}")
        name = f"w{cols}x{rows}"
        with d.attach(Target.new(name=name), cols=cols, rows=rows) as client:
            ws = next(w["id"] for w in d.http.workspaces() if w["name"] == name)
            pane = spawn(d, ws, "claude")
            client.pump(1.0)
            # Read the state once the pane is quiet, so this measures the marker
            # rather than the output-recency fallback — otherwise every size
            # would look fine for the first two seconds.
            settled = state_after_quiet(d, ws, pane)
        detected = settled["state"] == "working"
        ctx.row(
            "status detection by client size",
            client=f"{cols}x{rows}",
            rails="collapsed" if cols < 86 else "shown",
            working="detected" if detected else "missed",
        )
        if not detected:
            ctx.note(
                f"at a {cols}x{rows} client the agent pane is narrow enough that the "
                "status block wraps past 8 rows, so the interrupt hint falls out of the "
                f"scanned band and the agent reads {settled['state']!r} mid-turn"
            )
        d.stop()
