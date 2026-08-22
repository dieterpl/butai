"""Worktrees, and the thing that makes them worth having here: a worktree is a
directory, a butai workspace is a directory, so one worktree is one workspace.

The `workspace` field on each row is what the whole feature turns on — without
it a client cannot tell "open this" from "go to the one already open", and
opening a second workspace on one worktree gives the same tree two changes
rails.
"""

import os

from suite import fixtures
from suite.runner import test


def _workspace(ctx, d, project):
    ws = d.http.new_workspace(path=project)
    d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: isinstance(c, dict) and "branch" in c,
        "the changes rail attached",
        timeout=30,
    )
    return ws


def _run_op(d, ws, method, route, body=None, timeout=60):
    """Drive a runner operation and return its final state, 200 or 202."""
    if method == "POST":
        reply = d.http.post(f"/v1/workspaces/{ws}/{route}", json_body=body or {})
    else:
        reply = d.http.delete(f"/v1/workspaces/{ws}/{route}")
    assert reply.status in (200, 202), f"{route} answered {reply.status}: {reply.text}"
    if reply.status == 200 and not reply.json().get("running", False):
        return reply.json()
    return d.http.poll_until(
        f"/v1/workspaces/{ws}/git/op",
        lambda o: isinstance(o, dict) and not o.get("running", True),
        f"{route} to finish",
        timeout=timeout,
    )


def _worktrees(d, ws):
    reply = d.http.get(f"/v1/workspaces/{ws}/git/worktrees")
    assert reply.status == 200, f"worktrees answered {reply.status}: {reply.text}"
    return reply.json()


@test(profile="standard", tags=("git",))
def a_worktree_is_added_listed_and_removed(ctx):
    project = os.path.join(ctx.tmp, "proj")
    fixtures.git_repo(project)
    d = ctx.daemon()
    ws = _workspace(ctx, d, project)

    # Only the main worktree, and it knows which workspace is on it.
    rows = _worktrees(d, ws)
    assert len(rows) == 1, rows
    assert rows[0]["is_main"] is True
    assert rows[0]["workspace"] == ws, f"the open workspace was not matched: {rows}"

    wt = os.path.join(ctx.tmp, "proj-feature")
    state = _run_op(
        d,
        ws,
        "POST",
        "git/worktree",
        {"path": wt, "branch": "feat/x", "new_branch": True},
    )
    assert state.get("ok") is True, f"worktree add failed: {state}"
    assert os.path.isdir(wt), "the worktree directory was not created"
    assert os.path.exists(os.path.join(wt, "README.md")), "nothing was checked out"

    rows = _worktrees(d, ws)
    added = next(r for r in rows if r["branch"] == "feat/x")
    assert added["is_main"] is False
    assert added["workspace"] is None, "nothing is open on it yet"

    # Open it, and the listing says so.
    other = d.http.new_workspace(path=wt)
    rows = _worktrees(d, ws)
    added = next(r for r in rows if r["branch"] == "feat/x")
    assert added["workspace"] == other, f"the new workspace was not matched: {rows}"

    state = _run_op(d, ws, "DELETE", f"git/worktree?path={wt}&force=true")
    assert state.get("ok") is True, f"worktree remove failed: {state}"
    assert not any(r["branch"] == "feat/x" for r in _worktrees(d, ws))


@test(profile="standard", tags=("git", "security"))
def a_worktree_path_that_git_would_read_as_a_flag_is_refused(ctx):
    """A path comes from a text prompt, so it gets the same treatment as a ref:
    refused before git runs, with nothing created."""
    project = os.path.join(ctx.tmp, "proj")
    fixtures.git_repo(project)
    d = ctx.daemon()
    ws = _workspace(ctx, d, project)

    for path in ("--git-dir=/etc", "-f", "relative/path", ""):
        reply = d.http.post(
            f"/v1/workspaces/{ws}/git/worktree",
            json_body={"path": path, "branch": "x", "new_branch": True},
        )
        assert reply.status == 400, f"{path!r} answered {reply.status}: {reply.text}"

    # A hostile branch name is refused for the same reason.
    reply = d.http.post(
        f"/v1/workspaces/{ws}/git/worktree",
        json_body={
            "path": os.path.join(ctx.tmp, "wt"),
            "branch": "--upload-pack=touch /tmp/pwned",
            "new_branch": True,
        },
    )
    assert reply.status == 400, reply.text
    assert not os.path.exists("/tmp/pwned"), "an injected command ran"
    assert len(_worktrees(d, ws)) == 1, "a refused add still created something"


@test(profile="standard", tags=("git", "security"))
def a_remote_url_outside_the_allowed_transports_is_refused(ctx):
    """`remote add` is the one route that takes a URL, and a URL is how git is
    made to run a program: `ext::sh -c …` dispatches to `git-remote-ext`."""
    project = os.path.join(ctx.tmp, "proj")
    fixtures.git_repo(project)
    d = ctx.daemon()
    ws = _workspace(ctx, d, project)

    for url in (
        "ext::sh -c whoami",
        "fd::17/foo",
        "transport::anything",
        "--upload-pack=touch /tmp/pwned",
        "just-a-word",
    ):
        reply = d.http.post(
            f"/v1/workspaces/{ws}/git/remote", json_body={"name": "evil", "url": url}
        )
        assert reply.status == 400, f"{url!r} answered {reply.status}: {reply.text}"

    remotes = d.http.get(f"/v1/workspaces/{ws}/git/remotes").json()
    assert remotes == [], f"a refused remote was configured anyway: {remotes}"

    # An ordinary one is accepted and shows up.
    upstream = fixtures.git_repo(os.path.join(ctx.tmp, "upstream"))
    reply = d.http.post(
        f"/v1/workspaces/{ws}/git/remote", json_body={"name": "origin", "url": upstream}
    )
    assert reply.status in (200, 202), reply.text
    d.http.poll_until(
        f"/v1/workspaces/{ws}/git/remotes",
        lambda r: any(x["name"] == "origin" for x in r),
        "the new remote",
        timeout=30,
    )

    reply = d.http.delete(f"/v1/workspaces/{ws}/git/remote?name=origin")
    assert reply.status in (200, 202), reply.text
    d.http.poll_until(
        f"/v1/workspaces/{ws}/git/remotes",
        lambda r: not any(x["name"] == "origin" for x in r),
        "the remote to go",
        timeout=30,
    )
