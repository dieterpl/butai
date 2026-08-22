"""Stress and performance.

These produce numbers, not just verdicts. butai's whole design bet is that a
flooding pane throttles itself — PTY output rides a bounded channel so a busy
reader parks and backpressures the child, while control events stay on an
unbounded one and are polled first. This is where that bet gets measured.

Every test scales with `--scale`, so the same file is a 30-second check in
smoke and a real load test in standard.
"""

import os
import threading
import time

from suite import fixtures
from suite.butai import Events, Framed, Http, Target
from suite.daemon import Config
from suite.metrics import Latency, ProcSampler, human_bytes
from suite.runner import test


def _flood_workspace(d, panes):
    """A workspace with `panes` processes each writing as fast as the PTY takes."""
    flood = fixtures.probe(d.work, "flood")
    ws = d.http.new_workspace(path=d.work)
    for i in range(panes):
        d.http.new_process(ws, f"flood{i}", flood)
    d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: len([x for x in p if x["name"].startswith("flood")]) >= panes,
        "every flood pane started",
        timeout=60,
    )
    return ws


@test(profile="standard", tags=("stress",), timeout=420)
def the_control_plane_stays_responsive_under_a_pty_flood(ctx):
    """The headline claim. If this degrades, every client — TUI, browser, phone
    — goes unresponsive whenever one pane gets chatty."""
    panes = max(4, int(8 * ctx.scale))
    seconds = max(15, int(30 * ctx.scale))
    d = ctx.daemon()
    ws = _flood_workspace(d, panes)

    rest = Latency("GET /v1/workspaces during flood")
    framed = Latency("framed list_sessions during flood")

    with ProcSampler(d.pid, interval=0.25) as sampler:
        with Framed(d.socket) as control:
            control.hello(Target.control(), cwd=d.work)
            end = time.monotonic() + seconds
            while time.monotonic() < end:
                rest.time(d.http.get, "/v1/workspaces")
                start = time.perf_counter()
                control.command("list_sessions")
                control.wait_msg("session_list", timeout=10)
                framed.record((time.perf_counter() - start) * 1000)
                time.sleep(0.05)

    for meter in (rest, framed):
        ctx.metric(meter.label, meter.summary())
        ctx.note(meter.describe())
    ctx.note(f"{panes} flooding panes: {sampler.describe()}")

    stalls = d.slow_loops()
    if stalls:
        ctx.note(
            f"the daemon logged {len(stalls)} core-loop stalls, worst {d.slowest_loop_ms()}ms"
        )
    ctx.metric("core_loop_stalls", len(stalls))
    ctx.metric("peak_rss", human_bytes(sampler.peak_rss_kb() * 1024))

    d.assert_healthy()
    assert rest.summary()["p99_ms"] < 2000, (
        f"REST p99 was {rest.summary()['p99_ms']}ms under {panes} flooding panes"
    )
    assert framed.summary()["p99_ms"] < 2000, (
        f"control p99 was {framed.summary()['p99_ms']}ms under {panes} flooding panes"
    )
    # The bounded output channel is what keeps this true.
    assert sampler.peak_rss_kb() < 2 * 1024 * 1024, (
        f"daemon reached {human_bytes(sampler.peak_rss_kb() * 1024)} under flood"
    )


@test(profile="standard", tags=("stress",), timeout=420)
def kill_server_still_lands_while_panes_are_flooding(ctx):
    """Shutting down has to work on the worst day, not the best one."""
    d = ctx.daemon()
    _flood_workspace(d, max(4, int(6 * ctx.scale)))
    time.sleep(3)

    start = time.perf_counter()
    d.cli("kill-server", timeout=30)
    code = d.wait_dead(timeout=20)
    elapsed = time.perf_counter() - start

    ctx.metric("kill_server_seconds", round(elapsed, 2))
    assert code is not None, "kill-server did not stop a flooding daemon"
    assert elapsed < 10, f"kill-server took {elapsed:.1f}s under flood"


@test(profile="standard", tags=("stress",), timeout=420)
def many_concurrent_clients_all_get_served(ctx):
    """Nothing caps client count, so this measures what a fleet of GUI clients
    (each polling REST, streaming SSE and attaching a viewport) actually costs."""
    viewers = max(4, int(12 * ctx.scale))
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    d.http.new_process(ws, "chatter", fixtures.probe(d.work, "heartbeat"))
    name = d.http.detail(ws)["name"]

    handshake = Latency("attach handshake")
    clients, streams = [], []
    try:
        with ProcSampler(d.pid, interval=0.5) as sampler:
            for _ in range(viewers):
                client = Framed(d.socket)
                handshake.time(client.hello, Target.attach(name), 100, 30, d.work)
                clients.append(client)
            for _ in range(max(2, viewers // 3)):
                stream = Events(d.socket)
                stream.start()
                streams.append(stream)

            polls = Latency("GET /v1/workspaces with many clients")
            end = time.monotonic() + max(10, int(20 * ctx.scale))
            while time.monotonic() < end:
                polls.time(d.http.get, "/v1/workspaces")
                for client in clients:
                    client.pump(0.0)
                time.sleep(0.1)

        detail = d.http.detail(ws)
        assert detail["id"] == ws
        assert d.http.workspaces()[0]["attached_clients"] >= viewers, (
            f"only {d.http.workspaces()[0]['attached_clients']} of {viewers} clients registered"
        )
    finally:
        for stream in streams:
            stream.stop()
        for client in clients:
            client.close()

    ctx.note(handshake.describe())
    ctx.note(polls.describe())
    ctx.note(f"{viewers} viewers + {len(streams)} SSE streams: {sampler.describe()}")
    ctx.metric("attach_handshake", handshake.summary())
    ctx.metric("poll_with_many_clients", polls.summary())
    d.assert_healthy()
    assert polls.summary()["p99_ms"] < 2000, polls.describe()


@test(profile="standard", tags=("stress", "resize"), timeout=300)
def two_clients_of_different_sizes_do_not_wedge_each_other(ctx):
    """A PTY holds one size, so two clients streaming one pane at different
    sizes SIGWINCH it at each other, repeatedly — active-client-wins, by
    design. The pane is allowed to thrash; the daemon is not."""
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    for i in range(3):
        d.http.new_process(ws, f"app{i}", fixtures.probe(d.work, "winsize"))
    pane = d.staged_pane(ws)

    rounds = max(10, int(30 * ctx.scale))
    with ProcSampler(d.pid, interval=0.25) as sampler:
        with Framed(d.socket) as small, Framed(d.socket) as large:
            small.hello(Target.pane(pane), cols=90, rows=25, cwd=d.work)
            large.hello(Target.pane(pane), cols=200, rows=55, cwd=d.work)
            for i in range(rounds):
                small.resize(90 + (i % 7), 25 + (i % 3))
                large.resize(200 - (i % 11), 55 - (i % 5))
                small.pump(0.05)
                large.pump(0.05)
            small.pump(1.0)
            large.pump(1.0)
            full_frames = small.screen.full_frames + large.screen.full_frames

    ctx.metric("full_repaints_for_resizes", full_frames)
    ctx.note(
        f"{rounds * 2} resizes produced {full_frames} full repaints across two clients; "
        f"{sampler.describe()}"
    )
    d.assert_healthy()
    assert d.http.detail(ws)["processes"], "the workspace lost its panes to a resize war"


@test(profile="standard", tags=("stress",), timeout=420)
def a_restart_storm_does_not_leak_panes_or_threads(ctx):
    """Each restart tears down a PTY and its two reader threads and builds new
    ones. Doing it in a loop is how a supervisor-shaped client behaves."""
    d = ctx.daemon()
    project = fixtures.workspace(
        d.work,
        "storm-ws",
        butai_file=fixtures.butai_toml(processes=[("dev", "sleep 600")]),
    )
    ws = d.http.new_workspace(path=project)
    procs = d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: any(x["name"] == "dev" for x in p),
        "dev started",
        timeout=30,
    )
    pane = next(p for p in procs if p["name"] == "dev")["pane"]

    rounds = max(5, int(25 * ctx.scale))
    restart = Latency("restart round trip")
    with ProcSampler(d.pid, interval=0.25) as sampler:
        for _ in range(rounds):
            restart.time(d.http.post, f"/v1/workspaces/{ws}/processes/{pane}/restart")
            procs = d.http.poll_until(
                f"/v1/workspaces/{ws}/processes",
                lambda p: any(x["name"] == "dev" and x["pane"] != pane for x in p),
                "dev came back",
                timeout=30,
            )
            pane = next(p for p in procs if p["name"] == "dev")["pane"]

    stats = sampler.summary()
    ctx.note(f"{rounds} restarts: {sampler.describe()}")
    ctx.note(restart.describe())
    ctx.metric("restart_latency", restart.summary())
    d.assert_healthy()
    assert stats["threads_end"] <= stats["threads_start"] + 8, (
        f"threads grew {stats['threads_start']} -> {stats['threads_end']} over {rounds} restarts"
    )
    assert stats["fds_end"] <= stats["fds_start"] + 16, (
        f"file descriptors grew {stats['fds_start']} -> {stats['fds_end']}"
    )


@test(profile="standard", tags=("stress",), timeout=300)
def an_input_flood_does_not_grow_the_daemon_without_bound(ctx):
    """Input rides the unbounded control channel with no rate limit, so a
    client that types faster than the PTY drains is worth measuring."""
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    pane = d.http.detail(ws)["processes"][0]["pane"]

    events = max(2000, int(20000 * ctx.scale))
    with ProcSampler(d.pid, interval=0.25) as sampler:
        with Framed(d.socket) as client:
            client.hello(Target.pane(pane), cols=100, rows=30, cwd=d.work)
            client.type_line("cat > /dev/null")
            time.sleep(0.5)
            start = time.perf_counter()
            for i in range(events):
                client.send({"input": {"key": {"code": {"char": "a"}}}})
            elapsed = time.perf_counter() - start
            client.pump(2.0)

    ctx.metric("input_events", events)
    ctx.metric("input_rate_per_s", round(events / elapsed))
    ctx.note(f"{events} keystrokes in {elapsed:.1f}s: {sampler.describe()}")
    d.assert_healthy()
    growth_mb = (sampler.summary()["rss_end_kb"] - sampler.summary()["rss_start_kb"]) / 1024
    assert growth_mb < 300, f"RSS grew {growth_mb:.0f} MB under an input flood"


@test(profile="standard", tags=("stress", "git"), timeout=600)
def a_repository_full_of_untracked_files_stays_usable(ctx):
    """Status runs every sampler tick with `recurse_untracked_dirs`, so this is
    the `node_modules` case — the one that decides whether butai is usable on a
    real front-end project."""
    d = ctx.daemon()
    count = max(4000, int(30000 * ctx.scale))
    project = fixtures.big_repo(os.path.join(d.work, "huge-repo"), files=count)
    ws = d.http.new_workspace(path=project)

    poll = Latency("GET /v1/workspaces with a huge repo")
    with ProcSampler(d.pid, interval=0.5) as sampler:
        d.http.poll_until(
            f"/v1/workspaces/{ws}/changes",
            lambda c: isinstance(c, dict),
            "the changes rail attached to a huge repo",
            timeout=180,
        )
        end = time.monotonic() + max(10, int(20 * ctx.scale))
        while time.monotonic() < end:
            poll.time(d.http.get, "/v1/workspaces")
            time.sleep(0.2)

    ctx.metric("untracked_files", count)
    ctx.metric("poll_with_huge_repo", poll.summary())
    ctx.note(f"{count} untracked files: {poll.describe()}")
    ctx.note(f"daemon under a huge repo: {sampler.describe()}")
    if d.slow_loops():
        ctx.note(f"worst core-loop stall was {d.slowest_loop_ms()}ms")
    d.assert_healthy()
    assert poll.summary()["p99_ms"] < 3000, poll.describe()


@test(profile="standard", tags=("stress",), timeout=300)
def a_slow_external_tool_does_not_stall_agent_status(ctx):
    """The sampler tick is sequential: it awaits `nvidia-smi`, then `docker ps`,
    then broadcasts. A hung tool therefore delays git refresh, agent status and
    SSE together. `docker ps` is capped at 2s, so this measures the real cost."""
    shim_dir = os.path.join(ctx.tmp, "slow-bin")
    for tool in ("docker", "nvidia-smi"):
        fixtures.write(os.path.join(shim_dir, tool), "#!/bin/sh\nsleep 30\n", mode=0o755)
    d = ctx.daemon(env={"PATH": shim_dir + os.pathsep + os.environ.get("PATH", "")})

    ws = d.http.new_workspace(path=d.work)
    with Events(d.socket) as stream:
        stream.wait_for("system", timeout=40)
        first = len(stream.of("system"))
        time.sleep(20)
        ticks = len(stream.of("system")) - first

    expected = 20 / 2.0  # the sampler nominally fires every 2s
    ctx.metric("system_ticks_in_20s", ticks)
    ctx.note(
        f"with hung docker/nvidia-smi shims the sampler produced {ticks} ticks in 20s "
        f"(~{expected:.0f} without); every tick also carries git refresh and agent status"
    )
    d.assert_healthy()
    assert ticks >= 1, "the sampler stopped entirely behind a hung external tool"
    assert d.http.detail(ws)["processes"], "the workspace became unreadable"


@test(profile="standard", tags=("stress",), timeout=420)
def detach_and_reattach_churn_does_not_accumulate_state(ctx):
    """Clients come and go constantly — a phone locking, an ssh session
    dropping, a browser tab closing."""
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    d.http.new_process(ws, "chatter", fixtures.probe(d.work, "heartbeat"))
    name = d.http.detail(ws)["name"]

    rounds = max(10, int(40 * ctx.scale))
    with ProcSampler(d.pid, interval=0.25) as sampler:
        for i in range(rounds):
            with Framed(d.socket) as client:
                client.hello(Target.attach(name), cols=100 + (i % 5), rows=30, cwd=d.work)
                client.pump(0.1)
                client.detach()
                client.wait_msg("detached", timeout=10)

    stats = sampler.summary()
    ctx.note(f"{rounds} attach/detach cycles: {sampler.describe()}")
    d.assert_healthy()
    assert d.http.workspaces()[0]["attached_clients"] == 0, (
        f"clients leaked: {d.http.workspaces()[0]['attached_clients']} still registered"
    )
    assert stats["fds_end"] <= stats["fds_start"] + 8, (
        f"file descriptors grew {stats['fds_start']} -> {stats['fds_end']} over {rounds} cycles"
    )


@test(profile="standard", tags=("stress",), timeout=420)
def many_workspaces_and_panes_stay_addressable(ctx):
    """Nothing caps workspaces or panes; the pane budget per workspace is
    already ~9 before a single agent. This is what a heavy user's day looks
    like."""
    workspaces = max(3, int(10 * ctx.scale))
    d = ctx.daemon(config=Config().shell_agent("sh"))

    created = []
    with ProcSampler(d.pid, interval=0.5) as sampler:
        for i in range(workspaces):
            project = fixtures.workspace(d.work, f"ws{i}")
            ws = d.http.new_workspace(path=project, name=f"ws{i}")
            d.http.new_process(ws, "dev", "sleep 900")
            d.http.spawn_agent(ws, "sh")
            created.append(ws)
        listing = d.http.poll_until(
            "/v1/workspaces",
            lambda w: len(w) == workspaces,
            "every workspace registered",
            timeout=120,
        )

    assert {w["id"] for w in listing} == set(created), listing
    for ws in created:
        assert d.http.detail(ws)["processes"], f"workspace {ws} lost its panes"

    ctx.metric("workspaces", workspaces)
    ctx.note(f"{workspaces} workspaces, {workspaces * 3} panes: {sampler.describe()}")
    ctx.note(f"peak threads {sampler.peak_threads()} (two per PTY pane)")
    d.assert_healthy()


@test(profile="standard", tags=("stress",), timeout=300)
def concurrent_api_calls_from_many_connections_are_serialized_safely(ctx):
    """Every REST call round-trips through the single core actor. Hammering it
    from many sockets at once is the cheapest way to find a lock-order or
    reply-channel bug."""
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    workers = max(4, int(16 * ctx.scale))
    seconds = max(8, int(15 * ctx.scale))
    errors = []
    latency = Latency("concurrent REST")
    lock = threading.Lock()

    def hammer():
        http = Http(d.socket)
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            for path in (f"/v1/workspaces/{ws}", "/v1/system", "/v1/workspaces"):
                start = time.perf_counter()
                try:
                    res = http.get(path)
                    if res.status != 200:
                        with lock:
                            errors.append(f"{path} -> {res.status}")
                except Exception as e:  # connection refused, timeouts, ...
                    with lock:
                        errors.append(f"{path} -> {type(e).__name__}: {e}")
                with lock:
                    latency.record((time.perf_counter() - start) * 1000)

    threads = [threading.Thread(target=hammer) for _ in range(workers)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    ctx.metric("concurrent_rest", latency.summary())
    ctx.note(f"{workers} concurrent clients: {latency.describe()}")
    d.assert_healthy()
    assert not errors, f"{len(errors)} failed requests, first few: {errors[:5]}"
