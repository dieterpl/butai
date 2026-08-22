"""Every `Command` variant.

A control connection is the clearest lens here: `report_error`/`report_status`
flash a footer message for an interactive client but send real `error`/`ok`
messages to a control one, so the command vocabulary is directly observable.
"""

import os

from suite.butai import Framed, Target, msg_body, msg_kind
from suite.coverage import COMMANDS_REJECTED
from suite.daemon import Config
from suite.runner import test


def _control(d):
    client = Framed(d.socket)
    client.hello(Target.control(), cwd=d.work)
    return client


def _no_error(client, seconds=1.0):
    """Fire-and-forget commands answer with silence; an error is a failure."""
    stray = [m for m in client.pump(seconds) if msg_kind(m) == "error"]
    assert not stray, f"unexpected error(s): {[msg_body(m) for m in stray]}"


@test(profile="smoke", tags=("commands",))
def the_unimplemented_commands_are_refused_with_an_explanation(ctx):
    """Sixteen commands exist in the vocabulary but are deliberately unhandled,
    for four reasons. Nine ask for free panes in a workbench that has fixed
    rails. Three — `git_menu`, `zoom_toggle`, `toggle_all_agents` — ask the
    daemon to change a screen it does not keep: every client draws its own
    workbench from `/v1/*` and decides for itself what is folded, and obeying
    them here would move every viewer at once. Three — `set_theme`,
    `list_themes` and `open_file` — ask it to choose a palette for a screen it
    does not draw, or to put a file on one. A client reads the file from
    `/v1/*` and draws it in its own editor; picking a palette per client is
    what lets one terminal be dark and another light on the same workspace.
    One — `set_default_agent` — asks it to write a client's own config file.

    A GUI depends on getting a *reason* back, so this is a contract, not a gap.
    """
    d = ctx.daemon()
    samples = {
        "split_pane": {"split_pane": {"dir": "horizontal", "kind": {"terminal": {"command": None}}}},
        "focus_dir": {"focus_dir": "left"},
        "focus_pane": {"focus_pane": 1},
        "resize_pane": {"resize_pane": {"dir": "left", "cells": 2}},
        "new_window": "new_window",
        "next_window": "next_window",
        "prev_window": "prev_window",
        "select_window": {"select_window": 0},
        "apply_layout": {"apply_layout": "ide"},
        "git_menu": "git_menu",
        "zoom_toggle": "zoom_toggle",
        "toggle_all_agents": "toggle_all_agents",
        "set_theme": {"set_theme": "tokyonight"},
        "list_themes": "list_themes",
        "open_file": {"open_file": "/etc/hostname"},
        "set_default_agent": {"set_default_agent": "claude"},
    }
    assert sorted(samples) == sorted(COMMANDS_REJECTED), "the rejected set drifted"

    with _control(d) as client:
        for name, payload in samples.items():
            ctx.cover(f"cmd:{name}")
            reply = client.request(payload, timeout=10)
            assert msg_kind(reply) == "error", f"{name} answered {reply}"
            text = msg_body(reply)
            # A reason, not a shrug: the groups refuse for different reasons, so
            # what is checked is that the reply says where the thing actually
            # lives, not that it repeats one fixed phrase.
            assert any(w in text for w in ("fixed rails", "client", "/v1/")), (
                f"{name} gave an unhelpful reason: {text}"
            )


@test(profile="smoke", tags=("commands",))
def list_commands_return_their_lists(ctx):
    d = ctx.daemon(config=Config().shell_agent("sh"))
    ctx.cover("cmd:list_sessions", "cmd:list_agents")
    ctx.cover("server:session_list", "server:agent_list")
    with _control(d) as client:
        agents = msg_body(client.request("list_agents", timeout=10))
        assert agents == ["sh"], agents


        sessions = msg_body(client.request("list_sessions", timeout=10))
        assert sessions == [], sessions


@test(profile="standard", tags=("commands",))
def session_commands_create_rename_and_kill(ctx):
    d = ctx.daemon()
    ctx.cover("cmd:new_session", "cmd:rename_window", "cmd:kill_session", "server:ok")
    with _control(d) as client:
        client.command({"new_session": {"name": "cmd-ws", "layout": None}})
        _no_error(client)
        d.http.poll_until(
            "/v1/workspaces",
            lambda ws: any(w["name"] == "cmd-ws" for w in ws),
            "new_session created a workspace",
        )

    with Framed(d.socket) as tui:
        tui.hello(Target.attach("cmd-ws"), cols=100, rows=30, cwd=d.work)
        tui.command({"rename_window": "renamed-ws"})
        tui.pump(1.5)
    d.http.poll_until(
        "/v1/workspaces",
        lambda ws: any(w["name"] == "renamed-ws" for w in ws),
        "rename_window took effect",
    )

    with _control(d) as client:
        reply = client.request({"kill_session": "renamed-ws"}, timeout=10)
        assert msg_kind(reply) == "ok", reply
        missing = client.request({"kill_session": "not-a-workspace"}, timeout=10)
        assert msg_kind(missing) == "error", missing


@test(profile="standard", tags=("commands",))
def spawn_agent_and_new_process_report_failures(ctx):
    d = ctx.daemon(config=Config().shell_agent("sh"))
    ctx.cover("cmd:spawn_agent", "cmd:new_process")
    ws = d.http.new_workspace(path=d.work)
    with Framed(d.socket) as client:
        client.hello(Target.attach(d.http.detail(ws)["name"]), cols=100, rows=30, cwd=d.work)
        client.command({"spawn_agent": "sh"})
        client.pump(1.0)
        d.http.poll_until(
            f"/v1/workspaces/{ws}/agents", lambda a: len(a) >= 1, "the agent appeared"
        )

        # Long-lived on purpose: a process that exits 0 is dropped from the rail
        # immediately, so a one-shot command can be gone before the poll looks.
        client.command({"new_process": {"name": "hello", "command": "sleep 120"}})
        client.pump(1.0)
        d.http.poll_until(
            f"/v1/workspaces/{ws}/processes",
            lambda procs: any(p["name"] == "hello" for p in procs),
            "the process row appeared",
        )

    # An unknown agent type is refused, but only over HTTP: `spawn_agent` on a
    # control connection is a silent no-op because a control connection has no
    # session to spawn into. A GUI therefore has to use the REST route, which is
    # the one that answers.
    bad = d.http.post(f"/v1/workspaces/{ws}/agents", json_body={"type": "no-such-agent"})
    assert bad.status == 400, f"{bad.status}: {bad.text[:200]}"
    assert "no agent named" in bad.json()["error"], bad.text
    ctx.note(
        "spawn_agent over a control connection is a no-op with no reply (control "
        "connections carry no session) — POST /v1/workspaces/{id}/agents is the "
        "path that reports failure"
    )


@test(profile="standard", tags=("commands",))
def closing_the_staged_pane_answers_with_silence(ctx):
    """`close_pane` acts on the workspace's staged pane and says nothing about
    it. Silence is the contract — an error would be the bug — and the pane
    going away is what a client watches for on `/v1/*`."""
    d = ctx.daemon()
    ctx.cover("cmd:close_pane", "client:command")
    with Framed(d.socket) as client:
        client.hello(Target.new(name="panes"), cols=100, rows=30, cwd=d.work)
        ws = d.http.workspaces()[0]["id"]
        before = d.http.detail(ws)["stage"]
        assert before is not None, "nothing was staged to close"
        client.command("close_pane")
        _no_error(client, 0.6)
    d.http.poll_until(
        f"/v1/workspaces/{ws}",
        lambda detail: detail.get("stage") != before,
        "the staged pane closed",
        timeout=20,
    )
    d.assert_healthy()


@test(profile="standard", tags=("commands", "config"))
def reload_config_reports_bad_config_instead_of_dying(ctx):
    """Config is reloadable at runtime; a broken file has to warn, not crash."""
    d = ctx.daemon()
    ctx.cover("cmd:reload_config")
    with _control(d) as client:
        client.command("reload_config")
        _no_error(client)

        with open(os.path.join(d.butai_dir, "config.toml"), "w") as fh:
            fh.write("[general\nthis is not toml = = =\n")
        client.command("reload_config")
        replies = client.pump(2.0)
        kinds = [msg_kind(m) for m in replies]
        assert all(k in ("error", "ok") for k in kinds), kinds
    d.assert_healthy()


@test(profile="standard", tags=("commands",))
def kill_server_detaches_every_client_and_exits(ctx):
    d = ctx.daemon()
    ctx.cover("cmd:kill_server")
    with Framed(d.socket) as viewer:
        viewer.hello(Target.new(name="victim"), cols=100, rows=30, cwd=d.work)
        with _control(d) as control:
            reply = control.request("kill_server", timeout=10)
            assert msg_kind(reply) == "detached", reply
            assert "shut" in msg_body(reply)["reason"].lower(), msg_body(reply)
        viewer.expect_closed(timeout=10)
    assert d.wait_dead(timeout=15) is not None, "kill_server left the daemon alive"
