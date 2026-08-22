"""Remote sync: fetch, pull, push, and the operation runner underneath them.

Every test here runs against a **local bare repository**, never a network. That
is not a shortcut — a local remote drives exactly the daemon code that a real
one does (spawn, progress, the per-repository write lock, completion, the
`git_op` event), and the suite has to be runnable offline and deterministic.

The one thing a local remote cannot exercise is a hang, which is the failure
this runner exists to prevent; `an_operation_that_cannot_authenticate_fails_fast`
covers that with an unreachable host instead.
"""

import os

from suite import fixtures
from suite.butai import Events
from suite.runner import test


def _remote_workspace(ctx, d, name="sync-ws", behind=0):
    """A repo tracking a local bare `origin`, with a workspace that has scanned it."""
    project = fixtures.repo_with_remote(
        os.path.join(d.work, name), os.path.join(d.work, f"{name}-origin.git"), behind=behind
    )
    ws = d.http.new_workspace(path=project)
    d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: isinstance(c, dict) and "branch" in c,
        "the changes rail attached",
        timeout=30,
    )
    return project, ws


def _commit_a_file(d, ws, project, name, message):
    """Write a file and commit it through the API.

    `commit-all` stages what the *rail* currently lists, and the rail is
    refreshed off-thread, so committing the instant the file is written races
    the scan and gets "nothing to commit". Wait for the rail to see it.
    """
    fixtures.write(os.path.join(project, name), f"{name}\n")
    d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: any(f["path"] == name for f in c.get("unstaged", [])),
        f"the rail to notice {name}",
        timeout=30,
    )
    d.http.ok("POST", f"/v1/workspaces/{ws}/changes/commit-all", {"message": message})


def _run_op(d, ws, route, body=None, timeout=60):
    """POST a git operation and return its final state.

    Absorbs the 200-vs-202 split: an operation that finishes inside the grace
    window answers with its result, and one that does not answers "accepted" and
    has to be polled. A client must handle both, so this helper does — and every
    test below is two lines because of it.
    """
    reply = d.http.post(f"/v1/workspaces/{ws}/{route}", json_body=body or {})
    assert reply.status in (200, 202), f"{route} answered {reply.status}: {reply.text}"
    if reply.status == 200 and not reply.json().get("running", False):
        return reply.json()
    return d.http.poll_until(
        f"/v1/workspaces/{ws}/git/op",
        lambda o: isinstance(o, dict) and not o.get("running", True),
        f"{route} to finish",
        timeout=timeout,
    )


@test(profile="standard", tags=("git",))
def push_sends_commits_to_the_remote(ctx):
    d = ctx.daemon()
    ctx.cover("POST /v1/workspaces/{id}/git/push", "GET /v1/workspaces/{id}/git/op")
    project, ws = _remote_workspace(ctx, d)
    origin = os.path.join(d.work, "sync-ws-origin.git")

    _commit_a_file(d, ws, project, "new.txt", "api: a new commit")

    op = _run_op(d, ws, "git/push")
    assert op["ok"] is True, op
    assert op["kind"] == "push", op
    # The commit is really in the bare repo, not merely reported as sent.
    log = fixtures.git(origin, "log", "--oneline", "-1", "main").stdout
    assert "a new commit" in log, log


@test(profile="standard", tags=("git",))
def fetch_updates_the_behind_count_without_touching_the_worktree(ctx):
    d = ctx.daemon()
    ctx.cover("POST /v1/workspaces/{id}/git/fetch")
    project, ws = _remote_workspace(ctx, d, name="fetch-ws", behind=2)

    op = _run_op(d, ws, "git/fetch", {"prune": True})
    assert op["ok"] is True, op

    changes = d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: c.get("behind", 0) == 2,
        "the rail to notice it is two commits behind",
        timeout=30,
    )
    assert changes["upstream"] == "origin/main", changes
    assert changes["ahead"] == 0, changes
    # Fetch moves no files.
    assert not os.path.exists(os.path.join(project, "upstream0.txt"))


@test(profile="standard", tags=("git",))
def pull_brings_the_commits_into_the_worktree(ctx):
    d = ctx.daemon()
    ctx.cover("POST /v1/workspaces/{id}/git/pull")
    project, ws = _remote_workspace(ctx, d, name="pull-ws", behind=1)

    op = _run_op(d, ws, "git/pull", {"rebase": True})
    assert op["ok"] is True, op
    assert os.path.exists(os.path.join(project, "upstream0.txt")), "pull did not update the worktree"

    d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: c.get("behind", 0) == 0,
        "the rail to catch up",
        timeout=30,
    )


@test(profile="standard", tags=("git",))
def a_second_operation_is_refused_while_one_is_running(ctx):
    """One writer per repository.

    Two operations interleaving index writes is how work gets lost, so the
    second is refused outright rather than queued — a queue would hide the
    contention instead of reporting it.
    """
    d = ctx.daemon()
    project, ws = _remote_workspace(ctx, d, name="busy-ws", behind=3)

    # Start something, then immediately try to start something else. Whether the
    # first is still running by the time the second arrives is a race we cannot
    # win reliably, so accept either outcome and only insist that two never run.
    first = d.http.post(f"/v1/workspaces/{ws}/git/fetch", json_body={})
    second = d.http.post(f"/v1/workspaces/{ws}/git/pull", json_body={})
    assert first.status in (200, 202), first.text
    assert second.status in (200, 202, 409), second.text
    if first.status == 202:
        assert second.status == 409, f"a second op ran alongside the first: {second.text}"
        assert "already running" in second.text, second.text

    d.http.poll_until(
        f"/v1/workspaces/{ws}/git/op",
        lambda o: not o.get("running", True),
        "the first operation to finish",
        timeout=60,
    )


@test(profile="standard", tags=("git",))
def a_hostile_remote_or_branch_never_becomes_a_command_line(ctx):
    """`git fetch 'ext::sh -c ...'` is remote code execution.

    These must be refused with a 400 *before* anything is spawned, so the check
    is both the status code and the absence of the file the payload would have
    created.
    """
    d = ctx.daemon()
    _, ws = _remote_workspace(ctx, d, name="evil-ws")
    marker = os.path.join(d.work, "pwned")

    hostile = [
        ("git/fetch", {"remote": f"ext::sh -c touch {marker}"}),
        ("git/fetch", {"remote": f"--upload-pack=touch {marker}"}),
        ("git/pull", {"remote": "ssh://evil.invalid/repo"}),
        ("git/pull", {"remote": "user@evil.invalid:repo"}),
        ("git/push", {"remote": "origin", "branch": f"--exec=touch {marker}"}),
        ("git/push", {"remote": "origin", "branch": "a..b"}),
        ("git/push", {"remote": "origin", "branch": "x.lock"}),
        ("git/push", {"remote": "origin", "branch": "has space"}),
    ]
    for route, body in hostile:
        reply = d.http.post(f"/v1/workspaces/{ws}/{route}", json_body=body)
        assert reply.status == 400, f"{route} {body} answered {reply.status}: {reply.text}"
    assert not os.path.exists(marker), "a hostile value reached a command line"


@test(profile="standard", tags=("git",))
def an_operation_that_cannot_authenticate_fails_fast(ctx):
    """The failure this runner exists to prevent.

    Before it, `git push` ran on a blocking-pool thread with no timeout and no
    `GIT_TERMINAL_PROMPT=0`: a credential prompt parked that thread forever with
    no way to cancel. It must now fail on its own, quickly, with a reason.
    """
    d = ctx.daemon()
    project, ws = _remote_workspace(ctx, d, name="auth-ws")
    # A host that does not resolve: git fails rather than prompting, provided
    # nothing is waiting for an answer that can never come.
    fixtures.git(project, "remote", "add", "nope", "https://butai.invalid/repo.git")

    op = _run_op(d, ws, "git/fetch", {"remote": "nope"}, timeout=60)
    assert op["ok"] is False, f"a fetch from an unreachable host reported success: {op}"
    assert op["summary"], "failed with no reason given"


@test(profile="standard", tags=("git",))
def a_running_operation_can_be_cancelled(ctx):
    d = ctx.daemon()
    ctx.cover("DELETE /v1/workspaces/{id}/git/op")
    project, ws = _remote_workspace(ctx, d, name="cancel-ws")
    fixtures.git(project, "remote", "add", "slow", "https://butai.invalid/repo.git")

    started = d.http.post(f"/v1/workspaces/{ws}/git/fetch", json_body={"remote": "slow"})
    assert started.status in (200, 202), started.text
    # If it already finished, cancelling has nothing to do and answers 404 —
    # which is the honest answer, not a failure.
    cancelled = d.http.delete(f"/v1/workspaces/{ws}/git/op")
    assert cancelled.status in (200, 404), cancelled.text

    d.http.poll_until(
        f"/v1/workspaces/{ws}/git/op",
        lambda o: not o.get("running", True),
        "the operation to stop",
        timeout=60,
    )


@test(profile="standard", tags=("git",))
def an_operation_is_announced_on_the_event_stream(ctx):
    """Every attached client learns about an operation, not just whoever started
    it — which is the whole reason completion is an event and not just an HTTP
    response."""
    d = ctx.daemon()
    ctx.cover("event:git_op")
    project, ws = _remote_workspace(ctx, d, name="event-ws")

    with Events(d.socket) as stream:
        stream.wait_for("system", timeout=20)
        _commit_a_file(d, ws, project, "evt.txt", "for the event")
        d.http.post(f"/v1/workspaces/{ws}/git/push", json_body={})
        seen = stream.wait_for("git_op", timeout=60, predicate=lambda data: data["kind"] == "push")

    # The workspace id is on the event because the stream is unfiltered: without
    # it a subscriber could not tell which workspace it belongs to.
    assert seen["data"]["ws"] == ws, seen
