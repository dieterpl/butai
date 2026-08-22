"""The `GET /v1/events` SSE stream.

Unlike every other response, `ApiEvent` is *internally* tagged
(`{"event": ..., "data": ...}`), and one of its three tags — `notification` —
is undocumented. A client that polls instead of subscribing works, but pays a
round trip per rail per tick; this is the path that makes a GUI feel live.
"""

import os
import time

from suite.butai import Events
from suite.daemon import Config, fakeagent_dir
from suite.metrics import ProcSampler, human_bytes
from suite.runner import test


@test(profile="smoke", tags=("http", "sse"))
def the_event_stream_pushes_system_and_workspace_snapshots(ctx):
    d = ctx.daemon()
    ctx.cover("GET /v1/events", "event:system", "event:workspaces")
    with Events(d.socket) as stream:
        stream.wait_for("system", timeout=20)
        d.http.new_workspace(path=d.work)
        workspaces = stream.wait_for(
            "workspaces", timeout=20, predicate=lambda data: len(data) >= 1
        )
        assert stream.headers.get("content-type", "").startswith("text/event-stream"), (
            stream.headers
        )
        assert stream.error is None, stream.error

    payload = workspaces["data"][0]
    assert "id" in payload and "name" in payload, payload


@test(profile="standard", tags=("sse", "agents", "notifications"))
def an_agent_transition_is_pushed_as_a_notification(ctx):
    """The tag `docs/protocol.md` does not mention, and the one a client needs
    most: it is how "your agent is waiting on you" reaches a GUI that is not
    looking at that workspace."""
    d = ctx.daemon(config=Config().fake_agents("fake-claude"))
    ctx.cover("event:notification")
    ws = d.http.new_workspace(path=d.work)
    with Events(d.socket) as stream:
        stream.wait_for("system", timeout=20)
        d.http.spawn_agent(ws, "fake-claude")
        event = stream.wait_for("notification", timeout=60)

    data = event["data"]
    assert data["ws"] == ws, data
    assert data["kind"] in ("waiting", "finished", "exited"), data
    ctx.note(f"pushed notification: kind={data['kind']} title={data['title']!r}")


@test(profile="standard", tags=("sse",))
def many_subscribers_all_receive_the_stream(ctx):
    d = ctx.daemon()
    streams = [Events(d.socket) for _ in range(8)]
    try:
        for s in streams:
            s.start()
        for s in streams:
            s.wait_for("system", timeout=25)
    finally:
        for s in streams:
            s.stop()
    d.assert_healthy()


@test(profile="standard", tags=("sse", "stress"))
def a_subscriber_that_never_reads_does_not_grow_the_daemon_without_bound(ctx):
    """`api_subs` is a list of unbounded senders pruned only when a send fails,
    so a subscriber that holds the socket open and never drains it queues 2s
    snapshots forever. This measures how much that actually costs."""
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    for i in range(4):
        d.http.new_process(ws, f"noise{i}", "sleep 600")

    with ProcSampler(d.pid, interval=0.5) as sampler:
        slow = Events(d.socket, read_delay=5.0)
        slow.start()
        try:
            end = time.monotonic() + 30 * ctx.scale
            while time.monotonic() < end:
                d.http.get("/v1/system")
                time.sleep(0.5)
        finally:
            slow.stop()

    stats = sampler.summary()
    ctx.metric("rss_growth", human_bytes((stats["rss_end_kb"] - stats["rss_start_kb"]) * 1024))
    ctx.metric("rss_peak", human_bytes(stats["rss_peak_kb"] * 1024))
    ctx.note(f"slow SSE subscriber: {sampler.describe()}")
    d.assert_healthy()
    growth_mb = (stats["rss_end_kb"] - stats["rss_start_kb"]) / 1024
    assert growth_mb < 200, f"RSS grew {growth_mb:.0f} MB behind a stalled subscriber"


@test(profile="standard", tags=("sse",))
def the_stream_survives_a_subscriber_hanging_up(ctx):
    """Half of all SSE clients are a browser tab that got closed."""
    d = ctx.daemon()
    for _ in range(5):
        s = Events(d.socket)
        s.start()
        s.wait_for("system", timeout=25)
        s.stop()
    with Events(d.socket) as final:
        final.wait_for("system", timeout=25)
    d.assert_healthy()
    assert not d.log_lines("panicked"), d.log()[-2000:]


@test(profile="standard", tags=("sse", "notifications", "agents"))
def an_agent_that_dies_badly_stays_visible_as_a_corpse(ctx):
    """A dead agent used to report `idle`, which made a corpse indistinguishable
    from a quiet live one. `exited` plus its code is the fix — and an agent that
    failed must stay in the rail so you can still read why."""
    # It works for a while before dying: notifications only fire once an agent
    # has been seeded by the sampler, so an agent that exits within the first
    # tick is legitimately silent.
    config = Config().agent(
        "dying",
        os.path.join(fakeagent_dir(), "fake-claude"),
        env={"FAKE_SCRIPT": "busy:8,exit:3"},
    )
    d = ctx.daemon(config=config)
    ctx.cover("agent:exited")
    ws = d.http.new_workspace(path=d.work)

    with Events(d.socket) as stream:
        stream.wait_for("system", timeout=20)
        d.http.spawn_agent(ws, "dying")
        agents = d.http.poll_until(
            f"/v1/workspaces/{ws}/agents",
            lambda a: any(x["state"] == "exited" for x in a),
            "the agent reported exited",
            timeout=45,
        )
        corpse = next(a for a in agents if a["state"] == "exited")
        assert corpse["exited"] == 3, f"expected exit code 3, got {corpse}"
        stream.wait_for(
            "notification",
            timeout=30,
            predicate=lambda data: data.get("kind") == "exited",
        )
    d.assert_healthy()
