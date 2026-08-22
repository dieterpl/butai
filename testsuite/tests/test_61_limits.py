"""Container-limit lanes.

These need constraints that cannot be changed from inside a running container,
so `run.sh` launches them in their own `docker run` with the limit applied and
sets `BUTAI_LANE` to say which one. Outside those lanes every test here skips.
"""

import os
import time

from suite import fixtures
from suite.metrics import ProcSampler, human_bytes, rss_kb, thread_count
from suite.runner import test

LANE = os.environ.get("BUTAI_LANE", "")


def _spawn_panes_until_failure(ctx, d, ws, cap):
    """Add process panes until the daemon refuses; returns how many landed."""
    landed = 0
    for i in range(cap):
        try:
            res = d.http.post(
                f"/v1/workspaces/{ws}/processes",
                json_body={"name": f"p{i}", "command": "sleep 900"},
                timeout=10,
            )
        except Exception as e:
            ctx.note(f"pane {i}: the daemon stopped answering ({type(e).__name__}: {e})")
            break
        if res.status != 200:
            ctx.note(f"pane {i}: refused with HTTP {res.status} — {res.text[:120]}")
            break
        landed += 1
        if i % 5 == 0 and not d.alive():
            break
    return landed


@test(profile="standard", tags=("limits", "stress"), timeout=600)
def hitting_the_container_pid_limit_only_fails_that_pane(ctx):
    """Every PTY pane costs two OS threads, so a container's pid limit is a
    real ceiling on how many panes fit. Reaching it must fail the one pane
    being created — not the daemon's core actor, which would take every other
    workspace, agent and client with it."""
    ctx.require(LANE == "pids", "not running in the --pids-limit lane")
    limit = int(os.environ.get("BUTAI_PIDS_LIMIT", "0"))
    ctx.require(limit, "BUTAI_PIDS_LIMIT is not set")

    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    before = thread_count(d.pid)

    landed = _spawn_panes_until_failure(ctx, d, ws, cap=limit)
    ctx.metric("pid_limit", limit)
    ctx.metric("panes_created", landed)
    ctx.metric("daemon_threads_at_failure", thread_count(d.pid))
    ctx.note(f"created {landed} panes under a pid limit of {limit} (threads from {before})")

    assert d.alive(), (
        f"the daemon process died after {landed} panes; stderr tail:\n{d.stderr()[-2000:]}"
    )
    assert not d.panics(), "the daemon panicked:\n" + "\n".join(d.panics()[:5])
    still_served = d.http.get("/v1/workspaces", timeout=10)
    assert still_served.status == 200, (
        f"the API answers {still_served.status} after a pane failed to spawn — the core "
        f"actor is gone: {still_served.text[:200]}"
    )
    assert d.http.detail(ws)["processes"], "the workspace lost every pane"


@test(profile="standard", tags=("limits", "stress"), timeout=420)
def a_memory_capped_container_survives_a_flood(ctx):
    """butai's output channel is bounded so a flooding pane throttles itself.
    Under a hard cgroup cap that promise is load-bearing: if it were unbounded,
    the daemon would be OOM-killed instead of slowed down."""
    ctx.require(LANE == "memory", "not running in the --memory lane")
    d = ctx.daemon()
    flood = fixtures.probe(d.work, "flood")
    ws = d.http.new_workspace(path=d.work)
    for i in range(6):
        d.http.new_process(ws, f"flood{i}", flood)

    with ProcSampler(d.pid, interval=0.5) as sampler:
        end = time.monotonic() + max(20, int(45 * ctx.scale))
        while time.monotonic() < end:
            assert d.alive(), (
                "the daemon was killed under a memory cap — peak RSS "
                f"{human_bytes(sampler.peak_rss_kb() * 1024)}"
            )
            d.http.get("/v1/workspaces")
            time.sleep(0.5)

    ctx.metric("peak_rss", human_bytes(sampler.peak_rss_kb() * 1024))
    ctx.metric("cgroup_limit", os.environ.get("BUTAI_MEMORY_LIMIT", "unknown"))
    ctx.note(f"under a memory cap: {sampler.describe()}")
    d.assert_healthy()


@test(profile="standard", tags=("limits", "stress"), timeout=420)
def a_cpu_capped_container_still_answers_control_requests(ctx):
    """A single-core deployment — a Raspberry Pi, a shared runner — is where
    the "control events are polled first" scheduling actually matters."""
    ctx.require(LANE == "cpu", "not running in the --cpus lane")
    from suite.metrics import Latency

    d = ctx.daemon()
    flood = fixtures.probe(d.work, "flood")
    ws = d.http.new_workspace(path=d.work)
    for i in range(4):
        d.http.new_process(ws, f"flood{i}", flood)

    poll = Latency("GET /v1/workspaces on a capped CPU")
    end = time.monotonic() + max(15, int(30 * ctx.scale))
    while time.monotonic() < end:
        poll.time(d.http.get, "/v1/workspaces")
        time.sleep(0.1)

    ctx.metric("cpu_limit", os.environ.get("BUTAI_CPU_LIMIT", "unknown"))
    ctx.metric("poll_on_capped_cpu", poll.summary())
    ctx.note(poll.describe())
    d.assert_healthy()
    assert poll.summary()["p99_ms"] < 5000, poll.describe()


@test(profile="standard", tags=("limits",))
def the_daemon_reports_its_own_resource_use_honestly(ctx):
    """`/proc/stat` and `/proc/meminfo` are not namespaced, so the SYSTEM rail
    shows the *host's* numbers and ignores `--cpus`/`--memory` entirely. That is
    a real limitation for anyone rendering those gauges in a container; it is
    documented here rather than asserted away."""
    d = ctx.daemon()
    gauges = d.http.json_at("/v1/system")
    own_rss_mb = (rss_kb(d.pid) or 0) / 1024
    ctx.metric("daemon_rss_mb", round(own_rss_mb, 1))
    ctx.metric("reported_ram_total_gb", gauges["ram_total_gb"])
    ctx.note(
        f"SYSTEM rail reports {gauges['ram_total_gb']:.1f} GB total RAM "
        f"(host-wide, not the cgroup); the daemon itself holds "
        f"{own_rss_mb:.0f} MB"
    )
    if os.environ.get("BUTAI_MEMORY_LIMIT"):
        ctx.note(
            f"container memory limit is {os.environ['BUTAI_MEMORY_LIMIT']} but the gauge "
            "does not reflect it — reading /sys/fs/cgroup would be needed for that"
        )
    assert gauges["ram_total_gb"] > 0
