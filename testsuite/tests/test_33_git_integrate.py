"""Everything that rewrites history or gets stuck doing it.

Branch management, history, stash, tags, amend, reset — and merge, rebase and
conflict resolution, which are the reason the sequencer exists.

`fixtures.conflicting_branches` is the fixed starting point for every conflict
test: determinism comes from the fixture, not from timing, so "a conflict"
always means the same conflict on the same line with the same index stages.
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


def _run_op(d, ws, route, body=None, timeout=60):
    """POST a git operation and return its final state, 200 or 202."""
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
def branches_are_created_renamed_and_deleted(ctx):
    """Ref work is libgit2 and synchronous — a rename answering 202 would be
    absurd for something that cannot fail halfway."""
    d = ctx.daemon()
    ctx.cover(
        "POST /v1/workspaces/{id}/git/branch",
        "POST /v1/workspaces/{id}/git/branch/rename",
        "DELETE /v1/workspaces/{id}/git/branch",
    )
    project = fixtures.dirty_repo(os.path.join(d.work, "branch-ws"))
    ws = _workspace(ctx, d, project)

    d.http.ok("POST", f"/v1/workspaces/{ws}/git/branch", {"name": "topic"})
    assert "topic" in fixtures.git(project, "branch", "--list").stdout

    d.http.ok(
        "POST", f"/v1/workspaces/{ws}/git/branch/rename", {"from": "topic", "to": "topic-2"}
    )
    listed = fixtures.git(project, "branch", "--list").stdout
    assert "topic-2" in listed and "topic\n" not in listed, listed

    d.http.ok("DELETE", f"/v1/workspaces/{ws}/git/branch?name=topic-2")
    assert "topic-2" not in fixtures.git(project, "branch", "--list").stdout

    # The current branch cannot be deleted, and an unmerged one needs force —
    # both refusals exist so a keystroke cannot lose commits.
    current = fixtures.git(project, "branch", "--show-current").stdout.strip()
    refused = d.http.delete(f"/v1/workspaces/{ws}/git/branch?name={current}")
    assert refused.status == 400, refused.text

    fixtures.git(project, "checkout", "-q", "-b", "unmerged")
    fixtures.write(os.path.join(project, "only-here.txt"), "x\n")
    fixtures.git(project, "add", "only-here.txt")
    fixtures.git(project, "commit", "-q", "-m", "unmerged work")
    fixtures.git(project, "checkout", "-q", current)

    refused = d.http.delete(f"/v1/workspaces/{ws}/git/branch?name=unmerged")
    assert refused.status == 400 and "not merged" in refused.text, refused.text
    d.http.ok("DELETE", f"/v1/workspaces/{ws}/git/branch?name=unmerged&force=1")


@test(profile="standard", tags=("git",))
def history_pages(ctx):
    d = ctx.daemon()
    ctx.cover("GET /v1/workspaces/{id}/git/log")
    project = fixtures.dirty_repo(os.path.join(d.work, "log-ws"))
    for i in range(5):
        fixtures.write(os.path.join(project, f"c{i}.txt"), f"{i}\n")
        fixtures.git(project, "add", f"c{i}.txt")
        fixtures.git(project, "commit", "-q", "-m", f"commit {i}")
    ws = _workspace(ctx, d, project)

    page = d.http.json_at(f"/v1/workspaces/{ws}/git/log?limit=2")
    assert len(page["commits"]) == 2, page
    assert page["more"] is True, "a paged log must say another page exists"
    assert page["commits"][0]["summary"] == "commit 4", page
    assert page["commits"][0]["author"] and page["commits"][0]["date"], page

    second = d.http.json_at(f"/v1/workspaces/{ws}/git/log?limit=2&skip=2")
    assert second["commits"][0]["summary"] == "commit 2", second

    # One file's history, not the whole repo's.
    scoped = d.http.json_at(f"/v1/workspaces/{ws}/git/log?path=c1.txt")
    assert [c["summary"] for c in scoped["commits"]] == ["commit 1"], scoped


@test(profile="standard", tags=("git",))
def stash_round_trips(ctx):
    """`--include-untracked` is the whole point: without it a stash leaves new
    files behind, which is why "I stashed and it was still dirty" happens."""
    d = ctx.daemon()
    ctx.cover(
        "POST /v1/workspaces/{id}/git/stash",
        "GET /v1/workspaces/{id}/git/stashes",
        "POST /v1/workspaces/{id}/git/stash/apply",
        "DELETE /v1/workspaces/{id}/git/stash",
    )
    project = fixtures.dirty_repo(os.path.join(d.work, "stash-ws"))
    ws = _workspace(ctx, d, project)

    op = _run_op(d, ws, "git/stash", {"message": "wip", "include_untracked": True})
    assert op["ok"] is True, op
    assert fixtures.git(project, "status", "--porcelain").stdout.strip() == "", "tree not clean"

    stashes = d.http.json_at(f"/v1/workspaces/{ws}/git/stashes")
    assert len(stashes) == 1, stashes
    assert "wip" in stashes[0]["message"], stashes

    op = _run_op(d, ws, "git/stash/apply", {"index": 0, "pop": True})
    assert op["ok"] is True, op
    assert fixtures.git(project, "status", "--porcelain").stdout.strip() != "", "work not restored"
    assert d.http.json_at(f"/v1/workspaces/{ws}/git/stashes") == []

    # Drop the second one rather than restoring it.
    _run_op(d, ws, "git/stash", {"include_untracked": True})
    dropped = d.http.delete(f"/v1/workspaces/{ws}/git/stash?index=0")
    assert dropped.status in (200, 202), dropped.text
    d.http.poll_until(
        f"/v1/workspaces/{ws}/git/stashes",
        lambda s: s == [],
        "the stash to be dropped",
        timeout=30,
    )


@test(profile="standard", tags=("git",))
def amend_reset_and_tags(ctx):
    d = ctx.daemon()
    ctx.cover(
        "POST /v1/workspaces/{id}/git/amend",
        "POST /v1/workspaces/{id}/git/reset",
        "POST /v1/workspaces/{id}/git/tag",
        "GET /v1/workspaces/{id}/git/tags",
        "DELETE /v1/workspaces/{id}/git/tag",
    )
    project = fixtures.dirty_repo(os.path.join(d.work, "fixup-ws"), tracked_edits=0, untracked=0)
    ws = _workspace(ctx, d, project)

    op = _run_op(d, ws, "git/amend", {"message": "a better message"})
    assert op["ok"] is True, op
    assert "a better message" in fixtures.git(project, "log", "-1", "--format=%s").stdout

    # A tag, listed, then removed.
    assert _run_op(d, ws, "git/tag", {"name": "v1.0", "message": "release"})["ok"] is True
    assert d.http.json_at(f"/v1/workspaces/{ws}/git/tags") == ["v1.0"]
    dropped = d.http.delete(f"/v1/workspaces/{ws}/git/tag?name=v1.0")
    assert dropped.status in (200, 202), dropped.text
    d.http.poll_until(
        f"/v1/workspaces/{ws}/git/tags", lambda t: t == [], "the tag to go", timeout=30
    )

    # `reset --soft HEAD~1` puts the last commit's changes back in the index —
    # the "I committed too early" fix, and it must not touch the worktree.
    fixtures.write(os.path.join(project, "extra.txt"), "extra\n")
    fixtures.git(project, "add", "extra.txt")
    fixtures.git(project, "commit", "-q", "-m", "too early")
    op = _run_op(d, ws, "git/reset", {"rev": "HEAD~1", "mode": "soft"})
    assert op["ok"] is True, op
    assert os.path.exists(os.path.join(project, "extra.txt")), "a soft reset deleted the file"
    assert "A  extra.txt" in fixtures.git(project, "status", "--porcelain").stdout


@test(profile="standard", tags=("git",))
def a_merge_conflict_is_resolved_and_continued(ctx):
    """The whole conflict path: a merge stops, the rail says so, one side is
    taken, and the sequence carries on."""
    d = ctx.daemon()
    ctx.cover(
        "POST /v1/workspaces/{id}/git/merge",
        "POST /v1/workspaces/{id}/git/resolve",
        "POST /v1/workspaces/{id}/git/sequence",
        "GET /v1/workspaces/{id}/git/conflict",
    )
    project = fixtures.conflicting_branches(os.path.join(d.work, "merge-ws"))
    ws = _workspace(ctx, d, project)

    # A conflicting merge "fails" — which is the operation reporting honestly,
    # not the API erroring.
    op = _run_op(d, ws, "git/merge", {"branch": "feature"})
    assert op["ok"] is False, f"a conflicting merge reported success: {op}"

    changes = d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: c.get("state") == "merge" and c.get("conflicted"),
        "the rail to report the conflict",
        timeout=30,
    )
    assert [f["path"] for f in changes["conflicted"]] == ["conflict.txt"], changes
    # A conflicted file is never also listed as ordinary work.
    assert not any(f["path"] == "conflict.txt" for f in changes["unstaged"]), changes

    # All three sides are retrievable — the one thing a browser client cannot
    # reconstruct for itself.
    sides = d.http.json_at(f"/v1/workspaces/{ws}/git/conflict?path=conflict.txt")
    assert sides["base"].strip() == "base", sides
    assert sides["ours"].strip() == "ours", sides
    assert sides["theirs"].strip() == "theirs", sides

    # Take theirs, then carry on.
    d.http.ok(
        "POST", f"/v1/workspaces/{ws}/git/resolve", {"path": "conflict.txt", "take": "theirs"}
    )
    with open(os.path.join(project, "conflict.txt")) as f:
        assert f.read().strip() == "theirs", "the file did not take the chosen side"

    op = _run_op(d, ws, "git/sequence", {"action": "continue"})
    assert op["ok"] is True, op
    d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: c.get("state") == "clean" and not c.get("conflicted"),
        "the merge to finish",
        timeout=30,
    )


@test(profile="standard", tags=("git",))
def a_rebase_can_be_abandoned(ctx):
    """Abort is the way out of a stuck repository, and the reason the menu
    offers nothing else while one is stuck."""
    d = ctx.daemon()
    ctx.cover("POST /v1/workspaces/{id}/git/rebase")
    project = fixtures.conflicting_branches(os.path.join(d.work, "rebase-ws"))
    ws = _workspace(ctx, d, project)

    op = _run_op(d, ws, "git/rebase", {"onto": "feature"})
    assert op["ok"] is False, f"a conflicting rebase reported success: {op}"
    d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: c.get("state") == "rebase",
        "the rail to report the rebase",
        timeout=30,
    )

    op = _run_op(d, ws, "git/sequence", {"action": "abort"})
    assert op["ok"] is True, op
    d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: c.get("state") == "clean",
        "the rebase to be abandoned",
        timeout=30,
    )
    # Back where we started, with our own version intact.
    with open(os.path.join(project, "conflict.txt")) as f:
        assert f.read().strip() == "ours", "abort did not restore the branch"


@test(profile="standard", tags=("git",))
def revert_and_cherry_pick_move_commits_around(ctx):
    d = ctx.daemon()
    ctx.cover(
        "POST /v1/workspaces/{id}/git/revert", "POST /v1/workspaces/{id}/git/cherry-pick"
    )
    project = fixtures.dirty_repo(os.path.join(d.work, "pick-ws"), tracked_edits=0, untracked=0)
    ws = _workspace(ctx, d, project)

    fixtures.write(os.path.join(project, "feature.txt"), "feature\n")
    fixtures.git(project, "add", "feature.txt")
    fixtures.git(project, "commit", "-q", "-m", "add feature")
    head = fixtures.git(project, "rev-parse", "HEAD").stdout.strip()

    op = _run_op(d, ws, "git/revert", {"rev": head})
    assert op["ok"] is True, op
    assert not os.path.exists(os.path.join(project, "feature.txt")), "revert kept the file"

    # Cherry-pick the original commit back on.
    op = _run_op(d, ws, "git/cherry-pick", {"rev": head})
    assert op["ok"] is True, op
    assert os.path.exists(os.path.join(project, "feature.txt")), "cherry-pick lost the file"


@test(profile="standard", tags=("git",))
def remotes_are_listed(ctx):
    d = ctx.daemon()
    ctx.cover("GET /v1/workspaces/{id}/git/remotes")
    project = fixtures.repo_with_remote(
        os.path.join(d.work, "remotes-ws"), os.path.join(d.work, "remotes-origin.git")
    )
    ws = _workspace(ctx, d, project)

    remotes = d.http.json_at(f"/v1/workspaces/{ws}/git/remotes")
    # One entry per remote, not one per direction — `git remote -v` prints both.
    assert [r["name"] for r in remotes] == ["origin"], remotes
    assert remotes[0]["url"].endswith("remotes-origin.git"), remotes
