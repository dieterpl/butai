"""The opt-in lane: the real agent CLIs.

The fakes in `test_40_agents.py` assert that butai's detection works against the
strings those CLIs draw *today*. This file is what catches the day one of them
rewords its status line — the only way detection can silently break, since
there is no protocol between butai and an agent to version.

Nothing here runs unless the binary is installed (`run.sh --real-agents` builds
the layer that installs them) and, for the turn-driving test, credentials are in
the environment. Otherwise each test reports SKIP with the reason.
"""

import os
import shutil
import time

from suite.butai import Target
from suite.daemon import Config
from suite.runner import test
from tests.test_40_agents import spawn, state_within

# (agent, binary, the auto-approve flag butai ships for it, credential env var)
REAL_AGENTS = [
    ("claude", "claude", ["--dangerously-skip-permissions"], "ANTHROPIC_API_KEY"),
    ("codex", "codex", ["--dangerously-bypass-approvals-and-sandbox"], "OPENAI_API_KEY"),
    ("gemini", "gemini", ["--yolo"], "GEMINI_API_KEY"),
    ("aider", "aider", ["--yes-always"], "OPENAI_API_KEY"),
]

# Long enough to observe a turn, cheap enough to run on every CI night.
WARMUP_PROMPT = "Reply with exactly the word: ready"


def _installed():
    return [spec for spec in REAL_AGENTS if shutil.which(spec[1])]


def _credentialled():
    return [spec for spec in _installed() if os.environ.get(spec[3])]


@test(profile="standard", tags=("agents", "real"), timeout=600)
def the_real_agent_clis_launch_and_draw(ctx):
    """No credentials needed: this asks only whether each CLI starts and paints.

    It records the bottom eight lines of each pane — the exact band butai scans —
    so a diff in this table is the early warning that detection is about to
    break.
    """
    installed = _installed()
    ctx.require(installed, "no real agent CLIs installed (build with --real-agents)")

    for name, binary, args, _ in installed:
        d = ctx.daemon(config=Config().agent(name, binary, args), name=f"real-{name}")
        ws = d.http.new_workspace(path=d.work)
        pane = spawn(d, ws, name)

        with d.framed() as client:
            client.hello(Target.pane(pane), cols=140, rows=42, cwd=d.work)
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline and not client.screen.text().strip():
                client.pump(0.5)
            footer = client.screen.footer(8).strip()

        ctx.row(
            "real agent launch",
            agent=name,
            launched="ok" if footer else "no output",
            last_footer_line=(footer.splitlines()[-1][:56] if footer else "-"),
        )
        assert footer, f"{name} produced no output at all in a pane"
        d.stop()


@test(profile="standard", tags=("agents", "real"))
def the_shipped_auto_approve_flags_are_still_accepted(ctx):
    """butai launches each built-in agent with that CLI's auto-approve flag. A
    renamed flag means the agent dies the instant it spawns, which reads to a
    user as butai being broken."""
    installed = _installed()
    ctx.require(installed, "no real agent CLIs installed (build with --real-agents)")

    rejected = []
    for name, binary, args, _ in installed:
        d = ctx.daemon(config=Config().agent(name, binary, args), name=f"flag-{name}")
        ws = d.http.new_workspace(path=d.work)
        pane = spawn(d, ws, name)
        time.sleep(8)
        row = next((a for a in d.http.agents(ws) if a["pane"] == pane), None)
        died = row is None or (row["state"] == "exited" and row.get("exited"))
        ctx.row(
            "auto-approve flags",
            agent=name,
            flag=" ".join(args) or "(none)",
            accepted="no" if died else "ok",
        )
        if died:
            rejected.append(f"{name} ({' '.join(args)})")
        d.stop()

    assert not rejected, (
        "these agents exited immediately with the flags butai launches them with: "
        + ", ".join(rejected)
    )


@test(profile="standard", tags=("agents", "real"), timeout=1200)
def the_real_agents_status_lines_still_match(ctx):
    """Drives one real turn per credentialled agent and checks butai notices.

    This is the assertion the fakes stand in for. If it fails while
    `test_40_agents.py` passes, the CLI changed its wording: add the new marker
    to `BUSY_MARKERS`/`PROMPT_MARKERS` in `pane/terminal.rs`, then update the
    double to match — not the other way round.
    """
    available = _credentialled()
    ctx.require(
        available,
        "no agent credentials in the environment (set one of "
        + ", ".join(sorted({s[3] for s in REAL_AGENTS}))
        + ")",
    )

    misses = []
    for name, binary, args, cred in available:
        secret = {cred: os.environ[cred]}
        d = ctx.daemon(
            config=Config().agent(name, binary, args, env=secret),
            name=f"turn-{name}",
            env=secret,
        )
        ws = d.http.new_workspace(path=d.work)
        pane = spawn(d, ws, name)
        time.sleep(6)  # let the CLI finish its own startup before typing at it

        d.http.post(f"/v1/workspaces/{ws}/panes/{pane}/input", json_body={"paste": WARMUP_PROMPT})
        d.http.post(
            f"/v1/workspaces/{ws}/panes/{pane}/input", json_body={"key": {"code": "enter"}}
        )

        working = state_within(d, ws, pane, {"working"}, timeout=60)
        settled = state_within(d, ws, pane, {"finished", "waiting", "idle"}, timeout=90)

        ctx.row(
            "real agent detection",
            agent=name,
            working="detected" if working else "missed",
            settled=settled["state"] if settled else "never settled",
        )
        if not working:
            misses.append(name)
        d.stop()

    assert not misses, (
        "butai did not notice these real agents working — their status wording has "
        "probably changed upstream: " + ", ".join(misses)
    )
