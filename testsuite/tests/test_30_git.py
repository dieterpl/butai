"""The CHANGES rail: status, staging, diffs, commits, branches.

Plus the two container-specific failure modes that never show up on a
developer's laptop and account for most "butai looks broken in Docker" reports:
a repo owned by a different uid, and a container with no git identity.
"""

import os
import urllib.parse

from suite import fixtures
from suite.runner import test

FOREIGN_REPO = os.environ.get("BUTAI_FOREIGN_REPO", "/opt/butai-testsuite/foreign-repo")


def _q(value):
    return urllib.parse.quote(str(value), safe="")


def _repo_workspace(ctx, d, name="repo-ws", **kw):
    """A dirty repo plus a workspace whose rail has caught up with it.

    Status is recomputed off-thread on the ~2s sampler tick, so a test that acts
    on a file the instant the workspace exists is racing the first scan.
    """
    project = fixtures.dirty_repo(os.path.join(d.work, name), **kw)
    ws = d.http.new_workspace(path=project)
    expect_changes = (
        kw.get("tracked_edits", 1) or kw.get("untracked", 1) or kw.get("staged", 0)
    )
    d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: isinstance(c, dict)
        and "branch" in c
        and (not expect_changes or c["staged"] or c["unstaged"]),
        "the changes rail attached and scanned the working tree",
        timeout=30,
    )
    return project, ws


@test(profile="smoke", tags=("git",))
def the_changes_rail_reports_the_working_tree(ctx):
    d = ctx.daemon()
    ctx.cover("GET /v1/workspaces/{id}/changes")
    _, ws = _repo_workspace(ctx, d, tracked_edits=2, untracked=1, staged=1)

    changes = d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: c["unstaged"] and c["staged"],
        "both staged and unstaged entries appeared",
        timeout=30,
    )
    assert changes["branch"], changes
    for entry in changes["unstaged"] + changes["staged"]:
        for field in ("path", "code", "added", "deleted"):
            assert field in entry, f"{field} missing from FileChange: {entry}"
    paths = {e["path"] for e in changes["unstaged"]}
    assert "tracked0.txt" in paths, paths
    assert "untracked0.txt" in paths, paths
    assert isinstance(changes["recent_commits"], list)


@test(profile="smoke", tags=("git",))
def staging_and_unstaging_move_a_file_between_the_lists(ctx):
    d = ctx.daemon()
    ctx.cover(
        "POST /v1/workspaces/{id}/changes/stage",
        "POST /v1/workspaces/{id}/changes/unstage",
    )
    _, ws = _repo_workspace(ctx, d, name="stage-ws")

    d.http.ok("POST", f"/v1/workspaces/{ws}/changes/stage", {"path": "tracked0.txt"})
    d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: any(e["path"] == "tracked0.txt" for e in c["staged"]),
        "the file moved to staged",
        timeout=30,
    )

    d.http.ok("POST", f"/v1/workspaces/{ws}/changes/unstage", {"path": "tracked0.txt"})
    d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: any(e["path"] == "tracked0.txt" for e in c["unstaged"]),
        "the file moved back to unstaged",
        timeout=30,
    )


@test(profile="standard", tags=("git",))
def diffs_are_served_for_both_sides_of_the_index(ctx):
    d = ctx.daemon()
    ctx.cover("GET /v1/workspaces/{id}/diff")
    project, ws = _repo_workspace(ctx, d, name="diff-ws")

    unstaged = d.http.json_at(f"/v1/workspaces/{ws}/diff?path={_q('tracked0.txt')}")
    assert unstaged["staged"] is False, unstaged
    assert "modified 0" in unstaged["patch"], unstaged["patch"][:400]
    assert "@@" in unstaged["patch"], "no hunk header in the patch"

    d.http.ok("POST", f"/v1/workspaces/{ws}/changes/stage", {"path": "tracked0.txt"})
    staged = d.http.poll_until(
        f"/v1/workspaces/{ws}/diff?path={_q('tracked0.txt')}&kind=staged",
        lambda diff: "modified 0" in diff["patch"],
        "the staged diff carries the change",
        timeout=30,
    )
    assert staged["staged"] is True, staged

    legacy = d.http.json_at(f"/v1/workspaces/{ws}/diff?path={_q('tracked0.txt')}&staged=true")
    assert legacy["staged"] is True, "the ?staged=true spelling is part of the API too"

    # A file with no diff must answer cleanly rather than 500.
    fixtures.write(os.path.join(project, "quiet.txt"), "untouched\n")
    quiet = d.http.get(f"/v1/workspaces/{ws}/diff?path={_q('quiet.txt')}")
    assert quiet.status in (200, 404), f"{quiet.status}: {quiet.text[:200]}"


@test(profile="standard", tags=("git",))
def committing_clears_the_rail_and_lands_in_the_log(ctx):
    d = ctx.daemon()
    ctx.cover("POST /v1/workspaces/{id}/changes/commit")
    project, ws = _repo_workspace(ctx, d, name="commit-ws")

    d.http.ok("POST", f"/v1/workspaces/{ws}/changes/stage", {"path": "tracked0.txt"})
    d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: any(e["path"] == "tracked0.txt" for e in c["staged"]),
        "the file is staged",
        timeout=30,
    )
    d.http.ok("POST", f"/v1/workspaces/{ws}/changes/commit", {"message": "api: commit one file"})

    changes = d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: any(x["summary"] == "api: commit one file" for x in c["recent_commits"]),
        "the commit appears in recent_commits",
        timeout=30,
    )
    assert not any(e["path"] == "tracked0.txt" for e in changes["staged"]), changes["staged"]
    log = fixtures.git(project, "log", "--oneline", "-1").stdout
    assert "api: commit one file" in log, log


@test(profile="standard", tags=("git",))
def commit_all_stages_everything_first(ctx):
    """The "commit all my work" case, in one call instead of one stage per file."""
    d = ctx.daemon()
    ctx.cover("POST /v1/workspaces/{id}/changes/commit-all")
    project, ws = _repo_workspace(ctx, d, name="commit-all-ws", tracked_edits=3, untracked=2)

    d.http.ok("POST", f"/v1/workspaces/{ws}/changes/commit-all", {"message": "api: commit all"})
    d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: not c["staged"] and not c["unstaged"],
        "the tree is clean",
        timeout=30,
    )
    files = fixtures.git(project, "show", "--name-only", "--format=", "HEAD").stdout.split()
    assert len(files) == 5, files

    nothing = d.http.post(
        f"/v1/workspaces/{ws}/changes/commit-all", json_body={"message": "again"}
    )
    assert nothing.status == 400, f"committing a clean tree answered {nothing.status}"
    assert "nothing to commit" in nothing.json()["error"], nothing.text


@test(profile="standard", tags=("git",))
def discard_throws_away_unstaged_work_only(ctx):
    """Destructive, and deliberately narrow: a staged file has to be unstaged
    first, so one mis-click cannot delete work you had already indexed."""
    d = ctx.daemon()
    ctx.cover("POST /v1/workspaces/{id}/changes/discard")
    project, ws = _repo_workspace(ctx, d, name="discard-ws", untracked=1)

    d.http.ok("POST", f"/v1/workspaces/{ws}/changes/discard", {"path": "tracked0.txt"})
    with open(os.path.join(project, "tracked0.txt")) as fh:
        assert fh.read() == "original 0\n", "discard did not restore the committed content"

    d.http.ok("POST", f"/v1/workspaces/{ws}/changes/discard", {"path": "untracked0.txt"})
    assert not os.path.exists(os.path.join(project, "untracked0.txt")), (
        "discarding an untracked file should delete it"
    )

    fixtures.write(os.path.join(project, "tracked1.txt"), "edited again\n")
    fixtures.git(project, "add", "tracked1.txt")
    d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: any(e["path"] == "tracked1.txt" for e in c["staged"]),
        "the file is staged",
        timeout=30,
    )
    refused = d.http.post(
        f"/v1/workspaces/{ws}/changes/discard", json_body={"path": "tracked1.txt"}
    )
    assert refused.status == 400, f"discarding a staged file answered {refused.status}"


@test(profile="standard", tags=("git",))
def branches_are_listed_created_and_checked_out(ctx):
    d = ctx.daemon()
    ctx.cover("GET /v1/workspaces/{id}/branches", "POST /v1/workspaces/{id}/checkout")
    project, ws = _repo_workspace(ctx, d, name="branch-ws", tracked_edits=0, untracked=0)

    listed = d.http.json_at(f"/v1/workspaces/{ws}/branches")
    assert listed["current"] == "main", listed
    assert "main" in listed["branches"], listed

    d.http.ok("POST", f"/v1/workspaces/{ws}/checkout", {"branch": "feature", "create": True})
    d.http.poll_until(
        f"/v1/workspaces/{ws}/branches",
        lambda b: b["current"] == "feature",
        "the new branch is checked out",
        timeout=30,
    )
    assert "feature" in fixtures.git(project, "branch", "--show-current").stdout

    d.http.ok("POST", f"/v1/workspaces/{ws}/checkout", {"branch": "main"})
    d.http.poll_until(
        f"/v1/workspaces/{ws}/branches",
        lambda b: b["current"] == "main",
        "switched back to main",
        timeout=30,
    )

    missing = d.http.post(f"/v1/workspaces/{ws}/checkout", json_body={"branch": "no-such-branch"})
    assert missing.status in (400, 404), f"checking out a missing branch answered {missing.status}"


@test(profile="standard", tags=("git",))
def show_renders_a_past_commit(ctx):
    d = ctx.daemon()
    ctx.cover("GET /v1/workspaces/{id}/show")
    project, ws = _repo_workspace(ctx, d, name="show-ws")
    rev = fixtures.git(project, "rev-parse", "HEAD").stdout.strip()

    shown = d.http.json_at(f"/v1/workspaces/{ws}/show?id={_q(rev)}")
    assert "add tracked files" in shown["patch"], shown["patch"][:400]

    aliased = d.http.get(f"/v1/workspaces/{ws}/show?rev={_q(rev)}")
    assert aliased.status == 200, "?rev= is an accepted alias for ?id="


@test(profile="standard", tags=("git",))
def a_workspace_that_is_not_a_repo_has_no_rail(ctx):
    d = ctx.daemon()
    project = fixtures.workspace(d.work, "not-a-repo", files={"a.txt": "x"})
    ws = d.http.new_workspace(path=project)
    res = d.http.get(f"/v1/workspaces/{ws}/changes")
    assert res.status == 404, f"a non-repo answered {res.status}: {res.text[:200]}"
    assert d.http.detail(ws)["changes"] is None, "WorkspaceDetail.changes should be null"


@test(profile="standard", tags=("git", "errors"))
def diff_on_a_workspace_that_is_not_a_repo_is_a_404(ctx):
    """A project without a repo is an ordinary state, not a daemon failure, so
    `/diff` answers 404 like its `/changes` sibling rather than 500 with a page
    of git's own CLI usage."""
    d = ctx.daemon()
    project = fixtures.workspace(d.work, "diff-no-repo", files={"a.txt": "x\n"})
    ws = d.http.new_workspace(path=project)
    res = d.http.get(f"/v1/workspaces/{ws}/diff?path={_q('a.txt')}")
    assert res.status == 404, f"expected 404, got {res.status}: {res.text[:200]}"


@test(profile="standard", tags=("git", "container"))
def a_container_without_a_git_identity_fails_the_commit_with_a_reason(ctx):
    """`repo.signature()` is where a bare container falls over. It must surface
    as a 400 the user can act on, not a hang or a 500."""
    d = ctx.daemon(env={"GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_SYSTEM": "/dev/null"})
    project = fixtures.dirty_repo(os.path.join(d.work, "no-identity"))
    fixtures.git(project, "config", "--unset-all", "user.name", check=False)
    fixtures.git(project, "config", "--unset-all", "user.email", check=False)
    ws = d.http.new_workspace(path=project)
    d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: isinstance(c, dict) and any(e["path"] == "tracked0.txt" for e in c["unstaged"]),
        "the rail scanned the working tree",
        timeout=30,
    )

    d.http.ok("POST", f"/v1/workspaces/{ws}/changes/stage", {"path": "tracked0.txt"})
    res = d.http.post(
        f"/v1/workspaces/{ws}/changes/commit", json_body={"message": "should not land"}
    )
    assert res.status == 400, f"expected 400 without a git identity, got {res.status}: {res.text}"
    ctx.note(f"commit without user.email answers 400: {res.json().get('error')!r}")
    d.assert_healthy()


@test(profile="standard", tags=("git", "container"))
def a_repo_owned_by_another_uid_silently_loses_its_changes_rail(ctx):
    """The most likely Docker surprise, pinned.

    libgit2 refuses to open a repo owned by a different uid unless
    `safe.directory` allows it. butai swallows that error when it builds the
    workspace, so the symptom is not an error message — it is a CHANGES rail
    that is simply not there, and `changes: null` over the API.
    """
    ctx.require(
        os.path.isdir(FOREIGN_REPO),
        f"no foreign-owned fixture at {FOREIGN_REPO} (built into the image)",
    )
    ctx.require(
        os.stat(FOREIGN_REPO).st_uid != os.getuid(),
        "the fixture repo is owned by this user, so there is nothing to reproduce",
    )
    d = ctx.daemon()
    ws = d.http.new_workspace(path=FOREIGN_REPO)
    res = d.http.get(f"/v1/workspaces/{ws}/changes")
    assert res.status == 404, (
        f"expected the rail to be missing for a foreign-owned repo, got {res.status}"
    )
    ctx.note(
        "a repo owned by another uid gets no CHANGES rail and no error — "
        "`git config --global --add safe.directory <path>` in the container is the fix"
    )
    d.assert_healthy()


@test(profile="standard", tags=("git", "container"))
def safe_directory_restores_the_rail_for_a_foreign_repo(ctx):
    """The other half of the finding above: the fix works, and this is the
    exact configuration to put in a Dockerfile."""
    ctx.require(
        os.path.isdir(FOREIGN_REPO),
        f"no foreign-owned fixture at {FOREIGN_REPO} (built into the image)",
    )
    ctx.require(
        os.stat(FOREIGN_REPO).st_uid != os.getuid(),
        "the fixture repo is owned by this user, so there is nothing to reproduce",
    )
    d = ctx.daemon(start=False)
    os.makedirs(d.home, exist_ok=True)
    with open(os.path.join(d.home, ".gitconfig"), "w") as fh:
        fh.write("[safe]\n\tdirectory = *\n")
    d.start()

    ws = d.http.new_workspace(path=FOREIGN_REPO)
    changes = d.http.poll_until(
        f"/v1/workspaces/{ws}/changes",
        lambda c: isinstance(c, dict) and "branch" in c,
        "the rail attached once safe.directory allowed the repo",
        timeout=40,
    )
    assert changes["branch"], changes
