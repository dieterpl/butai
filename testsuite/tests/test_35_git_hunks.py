"""Partial staging: taking part of a file, and leaving the rest exactly alone.

`POST .../git/apply` is one route doing four jobs — stage a hunk, unstage one,
stage selected lines, discard one — because the client sends a *patch* and says
which copy of the file it lands on. So the tests here are all the same shape:
ask for the diff, cut it down, send it back, and check both the index **and**
the file on disk. Checking only one of the two is how "staged" and "discarded"
get confused.
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


def _two_hunk_repo(path):
    """A file with two changes far enough apart that git emits two hunks."""
    fixtures.git_repo(path)
    base = "".join(f"line{i}\n" for i in range(1, 21))
    fixtures.write(os.path.join(path, "a.txt"), base)
    fixtures.git(path, "add", "a.txt")
    fixtures.git(path, "commit", "-q", "-m", "twenty lines")
    edited = "".join(
        {2: "EARLY\n", 18: "LATE\n"}.get(i, f"line{i}\n") for i in range(1, 21)
    )
    fixtures.write(os.path.join(path, "a.txt"), edited)
    return edited


def _diff(d, ws, path, staged=False):
    kind = "staged" if staged else "unstaged"
    reply = d.http.get(f"/v1/workspaces/{ws}/diff?path={path}&kind={kind}")
    assert reply.status == 200, f"diff answered {reply.status}: {reply.text}"
    return reply.json()["patch"]


def _hunks(patch):
    """Split a one-file unified diff into (header, [hunk-text]).

    Deliberately re-implemented here rather than shared with the daemon: this
    is the test's own idea of what a hunk is, so a bug in the daemon's parser
    cannot hide by agreeing with itself.
    """
    lines = patch.splitlines(keepends=True)
    starts = [i for i, l in enumerate(lines) if l.startswith("@@")]
    header = "".join(lines[: starts[0]])
    bounds = starts + [len(lines)]
    return header, ["".join(lines[a:b]) for a, b in zip(bounds, bounds[1:])]


def _apply(d, ws, patch, target="index", reverse=False):
    return d.http.post(
        f"/v1/workspaces/{ws}/git/apply",
        json_body={"patch": patch, "target": target, "reverse": reverse},
    )


def _staged_text(project, path):
    """What the index holds for `path` — read through git rather than libgit2,
    so the assertion does not share a cache with the thing it is checking."""
    return fixtures.git(project, "show", f":{path}").stdout


@test(profile="standard", tags=("git",))
def one_hunk_stages_without_taking_the_other(ctx):
    """The whole feature: a file with a finished change and a debug line no
    longer has to be committed whole."""
    project = os.path.join(ctx.tmp, "proj")
    edited = _two_hunk_repo(project)
    d = ctx.daemon()
    ws = _workspace(ctx, d, project)

    header, hunks = _hunks(_diff(d, ws, "a.txt"))
    assert len(hunks) == 2, f"expected two hunks, got {len(hunks)}"
    late = next(h for h in hunks if "LATE" in h)

    reply = _apply(d, ws, header + late)
    assert reply.status == 200, f"apply answered {reply.status}: {reply.text}"

    staged = _staged_text(project, "a.txt")
    assert "LATE" in staged, f"the chosen hunk was not staged:\n{staged}"
    assert "EARLY" not in staged, f"the other hunk came along:\n{staged}"
    assert "line2\n" in staged, f"the untouched line did not survive:\n{staged}"

    # Staging moves nothing on disk. Both edits are still in the worktree.
    with open(os.path.join(project, "a.txt")) as f:
        assert f.read() == edited, "the working tree was rewritten by a stage"


@test(profile="standard", tags=("git",))
def a_staged_hunk_comes_back_out_the_way_it_went_in(ctx):
    """Unstage is the same patch, reversed, against the index — and it must not
    touch the worktree, or it would be a discard wearing the wrong name."""
    project = os.path.join(ctx.tmp, "proj")
    edited = _two_hunk_repo(project)
    d = ctx.daemon()
    ws = _workspace(ctx, d, project)

    header, hunks = _hunks(_diff(d, ws, "a.txt"))
    early = next(h for h in hunks if "EARLY" in h)
    assert _apply(d, ws, header + early).status == 200
    assert "EARLY" in _staged_text(project, "a.txt")

    # Reverse the *staged* diff to take it back out.
    header, hunks = _hunks(_diff(d, ws, "a.txt", staged=True))
    reply = _apply(d, ws, header + hunks[0], reverse=True)
    assert reply.status == 200, f"unstage answered {reply.status}: {reply.text}"

    staged = _staged_text(project, "a.txt")
    assert "EARLY" not in staged, f"the hunk stayed in the index:\n{staged}"
    with open(os.path.join(project, "a.txt")) as f:
        assert f.read() == edited, "unstaging discarded the worktree change"


@test(profile="standard", tags=("git",))
def a_hunk_is_discarded_from_the_worktree_alone(ctx):
    """Reverse-applied to the worktree: the file loses the change and the index
    is not involved."""
    project = os.path.join(ctx.tmp, "proj")
    _two_hunk_repo(project)
    d = ctx.daemon()
    ws = _workspace(ctx, d, project)

    header, hunks = _hunks(_diff(d, ws, "a.txt"))
    early = next(h for h in hunks if "EARLY" in h)
    reply = _apply(d, ws, header + early, target="worktree", reverse=True)
    assert reply.status == 200, f"discard answered {reply.status}: {reply.text}"

    with open(os.path.join(project, "a.txt")) as f:
        text = f.read()
    assert "EARLY" not in text, "the discarded change is still on disk"
    assert "LATE" in text, "discarding one hunk took the other with it"


@test(profile="standard", tags=("git",))
def a_patch_that_does_not_apply_is_refused(ctx):
    """400, not a silent success — a client must be able to tell that its patch
    was stale rather than believe it staged something."""
    project = os.path.join(ctx.tmp, "proj")
    _two_hunk_repo(project)
    d = ctx.daemon()
    ws = _workspace(ctx, d, project)

    junk = (
        "diff --git a/a.txt b/a.txt\n"
        "--- a/a.txt\n"
        "+++ b/a.txt\n"
        "@@ -1,1 +1,1 @@\n"
        "-nothing like this line\n"
        "+replacement\n"
    )
    assert _apply(d, ws, junk).status == 400
    assert _apply(d, ws, "").status == 400
