"""The soak profile: does the daemon drift?

Everything else in this suite runs for seconds. The daemon is meant to run for
weeks — that is its entire reason to exist — so the failure modes that matter
most are the ones only visible over time: a slow leak, threads that are created
per pane and never reaped, descriptors left behind by clients that came and
went.

`run.sh soak --minutes N` sets the duration; the default is 30 minutes.
"""

import os
import time

from suite import fixtures
from suite.butai import Events, Framed, Target
from suite.daemon import Config, fakeagent_dir
from suite.metrics import ProcSampler, human_bytes, slope
from suite.runner import test

WORKSPACES = 3


def _steady_state(d):
    """A workspace mix that keeps every subsystem busy: PTY output, git status,
    agent tracking, and the render loop."""
    heartbeat = fixtures.probe(d.work, "heartbeat")
    made = []
    for i in range(WORKSPACES):
        project = fixtures.dirty_repo(os.path.join(d.work, f"soak{i}"))
        ws = d.http.new_workspace(path=project, name=f"soak{i}")
        d.http.new_process(ws, "chatter", heartbeat)
        made.append((ws, f"soak{i}"))
    return made


@test(profile="soak", tags=("soak", "stress"), timeout=7200)
def the_daemon_does_not_drift_under_a_steady_workload(ctx):
    duration = ctx.soak_seconds
    config = Config().agent(
        "worker",
        os.path.join(fakeagent_dir(), "fake-claude"),
        env={"FAKE_SCRIPT": "busy:20,idle:20,question:20,idle:10"},
    )
    d = ctx.daemon(config=config)
    workspaces = _steady_state(d)
    ctx.note(f"soaking for {duration / 60:.0f} minutes across {len(workspaces)} workspaces")

    sampler = ProcSampler(d.pid, interval=2.0)
    sampler.start()
    stream = Events(d.socket)
    stream.start()

    cycles = 0
    started = time.monotonic()
    try:
        end = started + duration
        while time.monotonic() < end:
            cycles += 1
            ws, name = workspaces[cycles % len(workspaces)]

            # A client attaches, looks around, and leaves.
            with Framed(d.socket) as client:
                client.hello(Target.attach(name), cols=110 + (cycles % 9), rows=32, cwd=d.work)
                client.pump(0.5)
                client.detach()
                client.wait_msg("detached", timeout=15)

            # A GUI's polling cycle.
            d.http.workspaces()
            d.http.detail(ws)
            d.http.get(f"/v1/workspaces/{ws}/changes")
            d.http.get("/v1/system")
            d.http.get("/v1/notifications?since=0")

            # An agent's whole life, repeatedly: the tracking state machine is
            # where per-pane state would accumulate if it accumulated anywhere.
            d.http.spawn_agent(ws, "worker")
            agents = d.http.poll_until(
                f"/v1/workspaces/{ws}/agents",
                lambda a: len(a) >= 1,
                "an agent appeared",
                timeout=30,
            )
            time.sleep(3)
            for agent in agents:
                d.http.delete(f"/v1/workspaces/{ws}/panes/{agent['pane']}")

            # A file changes, so the git rail has real work every cycle.
            project = d.http.detail(ws)["cwd"]
            fixtures.write(os.path.join(project, "churn.txt"), f"cycle {cycles}\n")

            assert d.alive(), f"daemon died after {cycles} cycles:\n{d.stderr()[-3000:]}"
            assert not d.panics(), "daemon panicked:\n" + "\n".join(d.panics()[:5])
    finally:
        stream.stop()
        sampler.stop()

    stats = sampler.summary()
    elapsed = time.monotonic() - started
    rss_slope = slope(sampler.rss_series) * 60  # kB per minute

    ctx.metric("cycles", cycles)
    ctx.metric("minutes", round(elapsed / 60, 1))
    ctx.metric("rss_start", human_bytes(stats["rss_start_kb"] * 1024))
    ctx.metric("rss_end", human_bytes(stats["rss_end_kb"] * 1024))
    ctx.metric("rss_peak", human_bytes(stats["rss_peak_kb"] * 1024))
    ctx.metric("rss_slope_kb_per_min", round(rss_slope, 1))
    ctx.metric("threads", f"{stats['threads_start']} -> {stats['threads_end']}")
    ctx.metric("fds", f"{stats['fds_start']} -> {stats['fds_end']}")
    ctx.note(f"{cycles} cycles over {elapsed / 60:.1f} min: {sampler.describe()}")
    ctx.row(
        "soak drift",
        metric="rss",
        start=human_bytes(stats["rss_start_kb"] * 1024),
        end=human_bytes(stats["rss_end_kb"] * 1024),
        slope=f"{rss_slope:.0f} kB/min",
    )
    ctx.row(
        "soak drift",
        metric="threads",
        start=stats["threads_start"],
        end=stats["threads_end"],
        slope=f"{slope([(s[0], s[2]) for s in sampler.samples]) * 60:.2f} /min",
    )
    ctx.row(
        "soak drift",
        metric="fds",
        start=stats["fds_start"],
        end=stats["fds_end"],
        slope=f"{slope([(s[0], s[3]) for s in sampler.samples]) * 60:.2f} /min",
    )

    d.assert_healthy()

    # Growth is judged against the run's own baseline: a bigger machine starts
    # higher, and the question is drift, not absolute size.
    allowed_kb = stats["rss_start_kb"] * 0.5 + 150 * 1024
    growth_kb = stats["rss_end_kb"] - stats["rss_start_kb"]
    assert growth_kb < allowed_kb, (
        f"RSS grew {human_bytes(growth_kb * 1024)} over {elapsed / 60:.0f} min "
        f"({rss_slope:.0f} kB/min) — allowed {human_bytes(allowed_kb * 1024)}"
    )
    assert stats["threads_end"] <= stats["threads_start"] + 12, (
        f"threads drifted {stats['threads_start']} -> {stats['threads_end']} "
        f"over {cycles} agent lifecycles"
    )
    assert stats["fds_end"] <= stats["fds_start"] + 24, (
        f"descriptors drifted {stats['fds_start']} -> {stats['fds_end']} "
        f"over {cycles} attach/detach cycles"
    )
    assert stream.error is None, f"the SSE stream failed mid-soak: {stream.error}"
    assert len(stream.of("system")) > elapsed / 10, (
        f"the sampler produced only {len(stream.of('system'))} ticks in {elapsed:.0f}s"
    )


# Below this, filling three panes' scrollback rings dominates the sample and the
# half-slopes swing either way run to run. Measured at three minutes the same
# workload both accelerated and decelerated on consecutive runs, so a verdict
# from a short run would be noise wearing a pass/fail badge.
CONCLUSIVE_FLOOD_SECONDS = 15 * 60


@test(profile="soak", tags=("soak", "stress"), timeout=7200)
def a_pane_flooding_for_the_whole_soak_stays_bounded(ctx):
    """The bounded-channel promise, held for the full duration rather than 30
    seconds: a pane writing continuously must reach a plateau, not a slope.

    The numbers are always reported; the assertion only runs once the sample is
    long enough to mean something (see `CONCLUSIVE_FLOOD_SECONDS`).
    """
    duration = min(ctx.soak_seconds, 1800)
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    for i in range(3):
        d.http.new_process(ws, f"flood{i}", fixtures.probe(d.work, "flood"))

    sampler = ProcSampler(d.pid, interval=2.0)
    sampler.start()
    try:
        end = time.monotonic() + duration
        while time.monotonic() < end:
            res = d.http.get("/v1/workspaces")
            assert res.status == 200, f"the API stopped answering: {res.status}"
            time.sleep(2.0)
    finally:
        sampler.stop()

    stats = sampler.summary()
    series = sampler.rss_series
    overall = slope(series) * 60

    # Judge the *shape*, not an absolute rate. Filling three panes' scrollback
    # rings is real work that takes as long as it takes, so a fixed kB/min
    # threshold only measures how long the run was. What distinguishes bounded
    # from leaking is deceleration: a ring that is filling grows more slowly as
    # it approaches its cap, a leak does not.
    half = len(series) // 2
    early = slope(series[:half]) * 60 if half > 5 else overall
    late = slope(series[half:]) * 60 if half > 5 else overall

    ctx.metric("rss_peak", human_bytes(stats["rss_peak_kb"] * 1024))
    ctx.metric("rss_slope_first_half_kb_per_min", round(early, 1))
    ctx.metric("rss_slope_second_half_kb_per_min", round(late, 1))
    ctx.note(f"sustained flood for {duration / 60:.0f} min: {sampler.describe()}")
    ctx.note(
        f"growth decelerated from {early:.0f} to {late:.0f} kB/min across the run"
        if late < early
        else f"growth did NOT decelerate ({early:.0f} -> {late:.0f} kB/min)"
    )
    ctx.note(
        "scrollback is capped in lines, not bytes, so a process emitting very long "
        "lines costs proportionally more memory than this workload does"
    )
    d.assert_healthy()

    if duration < CONCLUSIVE_FLOOD_SECONDS:
        ctx.note(
            f"ran for {duration / 60:.0f} min — too short to judge whether this plateaus; "
            f"the numbers above are reported, but the assertion needs "
            f"--minutes {CONCLUSIVE_FLOOD_SECONDS // 60} or more"
        )
        return

    # Either it is already flat, or it is still filling and slowing down.
    assert late < 1024 or late < early * 0.8, (
        f"a continuously flooding pane grew RSS at {late:.0f} kB/min in the second half "
        f"of the run versus {early:.0f} in the first — that is not a scrollback ring "
        "filling up, it is unbounded growth"
    )
