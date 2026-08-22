"""The commit graph: parents, ref decoration, the `--all` walk, branch drift.

These are the four things a client cannot work out for itself. The relation
between commits lives in the object database, not in the page of commits a
client was handed, so a history without `parents` is a list rather than a tree
and no amount of clever drawing recovers it. Everything here is about the
daemon *sending* them, and sending them in an order the drawing can use.

The fixture is a real merge rather than a straight line, because every
interesting property — two parents, a lane that opens and closes, a
topological order that differs from date order — needs one to be observable at
all.
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


def _merged_repo(d, name="graph-ws"):
    """A repo with a merge, a tag, a second branch, a remote and a stash.

    Returns `(path, main_branch_name)`. The branch name is read back rather
    than assumed: `init.defaultBranch` differs between the container and a
    developer's machine, and a test that hard-codes `main` fails on one of them
    for a reason that has nothing to do with what it is testing.
    """
    path = os.path.join(d.work, name)
    # No initial commit from the fixture: this repo needs a *root* — a commit
    # with no parents — and `initial commit` would sit under it and make the
    # empty-parents assertion untestable.
    fixtures.git_repo(path, initial_commit=False)
    fixtures.write(os.path.join(path, "a.txt"), "root\n")
    fixtures.git(path, "add", "a.txt")
    fixtures.git(path, "commit", "-q", "-m", "root")
    fixtures.git(path, "tag", "v0.1")
    main = fixtures.git(path, "rev-parse", "--abbrev-ref", "HEAD").stdout.strip()

    fixtures.git(path, "checkout", "-q", "-b", "side")
    fixtures.write(os.path.join(path, "b.txt"), "side work\n")
    fixtures.git(path, "add", "b.txt")
    fixtures.git(path, "commit", "-q", "-m", "side work")

    fixtures.git(path, "checkout", "-q", main)
    fixtures.write(os.path.join(path, "a.txt"), "main moves on\n")
    fixtures.git(path, "commit", "-q", "-am", "main moves on")
    fixtures.git(path, "merge", "--no-ff", "-q", "side", "-m", "merge side")

    # A remote, so `entries` has a remote-tracking branch and an upstream to be
    # ahead of; and a stash, which must NOT appear as history.
    remote = fixtures.bare_remote(os.path.join(d.work, f"{name}-remote.git"))
    fixtures.git(path, "remote", "add", "origin", remote)
    fixtures.git(path, "push", "-q", "-u", "origin", main)
    fixtures.write(os.path.join(path, "a.txt"), "uncommitted\n")
    fixtures.git(path, "stash", "-q")
    return path, main


@test(profile="standard", tags=("git",))
def the_log_carries_parents_and_ref_decoration(ctx):
    d = ctx.daemon()
    ctx.cover("GET /v1/workspaces/{id}/git/log")
    project, main = _merged_repo(d)
    ws = _workspace(ctx, d, project)

    log = d.http.json_at(f"/v1/workspaces/{ws}/git/log?all=1&limit=20")
    commits = log["commits"]
    by_summary = {c["summary"]: c for c in commits}

    merge = by_summary["merge side"]
    assert len(merge["parents"]) == 2, f"a merge with one parent cannot be drawn: {merge}"
    root = by_summary["root"]
    assert root["parents"] == [], f"the root grew a parent: {root}"

    # Decoration is classified by `--decorate=full`'s prefixes, not guessed
    # from the shorthand — a tag and a branch may share a name.
    kinds = {(r["name"], r["kind"]) for r in root["refs"]}
    assert ("v0.1", "tag") in kinds, f"the tag is not on the root: {kinds}"
    tip = {(r["name"], r["kind"]) for r in merge["refs"]}
    assert (main, "branch") in tip, f"the branch tip is not decorated: {tip}"
    assert ("HEAD", "head") in tip, f"HEAD is not reported beside its branch: {tip}"
    assert (f"origin/{main}", "remote") in tip, f"the remote branch is missing: {tip}"


@test(profile="standard", tags=("git",))
def the_all_walk_is_topological_and_leaves_the_stash_out(ctx):
    """Two properties the drawing depends on absolutely.

    Lanes are assigned in one pass down the page, so a parent listed before its
    child breaks the graph outright — `--topo-order` is what prevents it, and
    date order does not, because a rebased or cherry-picked commit carries an
    old date. And `--all` is deliberately not `git log --all`: that includes
    `refs/stash`, whose two synthetic commits would sit in the history as if
    somebody had committed them.
    """
    d = ctx.daemon()
    project, _ = _merged_repo(d, "topo-ws")
    ws = _workspace(ctx, d, project)

    commits = d.http.json_at(f"/v1/workspaces/{ws}/git/log?all=1&limit=50")["commits"]
    assert commits, "the walk found nothing"

    seen = set()
    for c in commits:
        for p in c["parents"]:
            assert p not in seen, f"parent {p[:7]} was listed before its child {c['id'][:7]}"
        seen.add(c["id"])

    for c in commits:
        assert not c["summary"].startswith(("WIP on", "index on")), (
            f"a stash leaked into the history: {c['summary']}"
        )

    # The side branch is reachable only through the merge, so its presence is
    # what says the walk really covered every ref rather than just HEAD.
    assert any(c["summary"] == "side work" for c in commits), commits


@test(profile="standard", tags=("git",))
def the_log_refuses_all_and_rev_together(ctx):
    """They name different walks. Quietly preferring one would turn a client's
    bug into a daemon that answers a question nobody asked."""
    d = ctx.daemon()
    project, main = _merged_repo(d, "conflict-args-ws")
    ws = _workspace(ctx, d, project)

    bad = d.http.get(f"/v1/workspaces/{ws}/git/log?all=1&rev={main}")
    assert bad.status == 400, f"expected 400, got {bad.status}: {bad.text}"


@test(profile="standard", tags=("git",))
def branches_carry_upstream_and_drift(ctx):
    d = ctx.daemon()
    ctx.cover("GET /v1/workspaces/{id}/branches")
    project, main = _merged_repo(d, "branches-ws")
    ws = _workspace(ctx, d, project)

    dto = d.http.json_at(f"/v1/workspaces/{ws}/branches")
    # The old field and the new one describe the same local branches in the
    # same order: they are read by different clients — the picker takes names,
    # the GIT page takes entries — and a daemon where they disagree offers two
    # different repositories.
    locals_ = [e["name"] for e in dto["entries"] if not e["remote"]]
    assert dto["branches"] == locals_, f"{dto['branches']} != {locals_}"
    assert dto["branches"][0] == dto["current"], "the current branch is not first"

    entry = next(e for e in dto["entries"] if e["name"] == main)
    assert entry["upstream"] == f"origin/{main}", entry
    assert (entry["ahead"], entry["behind"]) == (0, 0), entry
    assert len(entry["tip"]) == 40, f"tip is not a full oid: {entry}"

    side = next(e for e in dto["entries"] if e["name"] == "side")
    assert side["upstream"] is None, f"invented an upstream: {side}"

    remotes = [e for e in dto["entries"] if e["remote"]]
    assert any(e["name"] == f"origin/{main}" for e in remotes), remotes
    # `origin/HEAD` is a symbolic ref onto another row in this same list.
    assert not any(e["name"].endswith("/HEAD") for e in remotes), remotes

    # Now commit locally: the drift is a real revwalk, not a placeholder, and
    # it is what the view rail's badge reads.
    fixtures.write(os.path.join(project, "c.txt"), "ahead\n")
    fixtures.git(project, "add", "c.txt")
    fixtures.git(project, "commit", "-q", "-m", "one ahead")
    after = d.http.json_at(f"/v1/workspaces/{ws}/branches")
    entry = next(e for e in after["entries"] if e["name"] == main)
    assert entry["ahead"] == 1, f"ahead did not move: {entry}"


@test(profile="standard", tags=("git",))
def a_merge_shows_what_it_brought_in(ctx):
    """`git show` diffs a merge against every parent, and a clean merge differs
    from none of them — so the endpoint answered a header and no patch, on
    exactly the commits people go looking for."""
    d = ctx.daemon()
    ctx.cover("GET /v1/workspaces/{id}/show")
    project, _ = _merged_repo(d, "merge-show-ws")
    ws = _workspace(ctx, d, project)

    commits = d.http.json_at(f"/v1/workspaces/{ws}/git/log?all=1&limit=20")["commits"]
    merge = next(c for c in commits if len(c["parents"]) == 2)
    patch = d.http.json_at(f"/v1/workspaces/{ws}/show?id={merge['id']}")["patch"]
    assert "diff --git" in patch and "+side work" in patch, patch


@test(profile="standard", tags=("git",))
def a_stash_can_be_shown_like_any_other_revision(ctx):
    """`stash@{0}` is a revision, and a stash list whose rows cannot show a
    diff is a list of rows that do nothing. `show` used to reject the `@{}`
    forms outright, having drifted from the validator every other route uses."""
    d = ctx.daemon()
    project, _ = _merged_repo(d, "stash-show-ws")
    ws = _workspace(ctx, d, project)

    stashes = d.http.json_at(f"/v1/workspaces/{ws}/git/stashes")
    assert stashes, "the fixture left no stash to show"
    shown = d.http.json_at(f"/v1/workspaces/{ws}/show?id=stash@{{0}}")
    assert "uncommitted" in shown["patch"], shown["patch"][:400]

    # `:` stays refused: `<rev>:<path>` reads a file out of a tree, which is a
    # different endpoint's job and a wider read than this one promises.
    bad = d.http.get(f"/v1/workspaces/{ws}/show?id=HEAD:a.txt")
    assert bad.status == 400, f"expected 400, got {bad.status}: {bad.text}"
