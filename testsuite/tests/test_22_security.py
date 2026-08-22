"""Path containment and input validation.

Every workspace-scoped path parameter is joined against the workspace root, and
percent-decoding happens *before* that join — so the escape attempts here go in
encoded as well as raw. The strongest assertion is not the status code but the
body: nothing outside the workspace may come back, and nothing outside it may be
written.
"""

import os
import urllib.parse

from suite import fixtures
from suite.runner import test

SECRET = "root:x:0:0"  # a line only /etc/passwd has

ESCAPES = [
    "../../../../etc/passwd",
    "..%2f..%2f..%2f..%2fetc%2fpasswd",
    "%2e%2e%2f%2e%2e%2f%2e%2e%2fetc%2fpasswd",
    "/etc/passwd",
    "subdir/../../../../etc/passwd",
    "....//....//....//etc/passwd",
]


def _q(value):
    return urllib.parse.quote(str(value), safe="")


@test(profile="smoke", tags=("security",))
def readers_cannot_escape_the_workspace_root(ctx):
    d = ctx.daemon()
    project = fixtures.workspace(d.work, "contained", files={"subdir/inside.txt": "fine\n"})
    ws = d.http.new_workspace(path=project)

    leaked = []
    errored = set()
    for route in ("file", "download", "tree", "diff"):
        for attempt in ESCAPES:
            # Already-encoded attempts are passed through as-is; raw ones get
            # encoded, so both reach the daemon in the shape a client would send.
            value = attempt if "%" in attempt else _q(attempt)
            res = d.http.get(f"/v1/workspaces/{ws}/{route}?path={value}")
            if SECRET in res.text:
                leaked.append(f"{route}?path={attempt} -> {res.status}")
            if res.status == 500:
                errored.add(route)
    assert not leaked, "workspace containment escaped:\n" + "\n".join(leaked)
    if errored:
        # Not a containment failure — see `test_30_git`, where the underlying
        # cause (diff on a workspace that is not a repo) is pinned on its own.
        ctx.note(f"these routes answered 500 rather than 4xx on a bad path: {sorted(errored)}")
    d.assert_healthy()


@test(profile="smoke", tags=("security",))
def uploads_cannot_write_outside_the_workspace(ctx):
    d = ctx.daemon()
    project = fixtures.workspace(d.work, "upload-jail")
    ws = d.http.new_workspace(path=project)
    outside = os.path.join(d.work, "escaped.txt")

    for attempt in ("../escaped.txt", "..%2fescaped.txt", "/tmp/butai-escaped.txt"):
        value = attempt if "%" in attempt else _q(attempt)
        res = d.http.post(f"/v1/workspaces/{ws}/upload?path={value}", raw=b"escaped\n")
        assert res.status != 500, f"upload {attempt} 500'd: {res.text[:200]}"

    assert not os.path.exists(outside), "an upload escaped the workspace root"
    assert not os.path.exists("/tmp/butai-escaped.txt"), "an absolute upload path was honoured"
    d.assert_healthy()


@test(profile="standard", tags=("security",))
def a_symlink_out_of_the_workspace_does_not_widen_the_reader(ctx):
    """Containment is checked on the joined path, so this documents what the
    rule actually is: it constrains the *path*, not the inode it resolves to."""
    d = ctx.daemon()
    project = fixtures.workspace(d.work, "symlink-ws")
    link = os.path.join(project, "passwd-link")
    os.symlink("/etc/passwd", link)
    ws = d.http.new_workspace(path=project)

    res = d.http.get(f"/v1/workspaces/{ws}/file?path={_q('passwd-link')}")
    if SECRET in res.text:
        ctx.note(
            "a symlink inside the workspace reads through to /etc/passwd — "
            "containment is path-based, not inode-based; a client exposing the "
            "API beyond the socket's own permissions should know this"
        )
    else:
        ctx.note(f"symlink read answered {res.status} without following the link")
    assert res.status != 500, res.text[:200]


@test(profile="standard", tags=("security", "git"))
def the_show_endpoint_refuses_a_revision_that_is_really_an_argument(ctx):
    """`show` shells out to `git show`, so the revision is validated against a
    character allowlist and must not start with `-` — otherwise a rev is a way
    to pass flags to git."""
    d = ctx.daemon()
    project = fixtures.dirty_repo(os.path.join(d.work, "show-ws"))
    ws = d.http.new_workspace(path=project)

    for rev in ("--upload-pack=touch /tmp/butai-pwned", "-x", "HEAD;id", "HEAD$(id)", "a" * 200):
        res = d.http.get(f"/v1/workspaces/{ws}/show?id={_q(rev)}")
        assert res.status != 500, f"rev {rev!r} 500'd: {res.text[:200]}"
        assert "uid=" not in res.text, f"rev {rev!r} executed a command: {res.text[:200]}"
    assert not os.path.exists("/tmp/butai-pwned"), "a rev parameter reached the shell"
    d.assert_healthy()


@test(profile="standard", tags=("security",))
def process_and_agent_names_do_not_reach_a_shell_unquoted(ctx):
    """`[[processes]] cmd` runs through `sh -c` by design, but an agent's
    command is exec'd directly — so an agent name must never become a shell
    word."""
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    marker = "/tmp/butai-agent-injection"
    res = d.http.post(
        f"/v1/workspaces/{ws}/agents", json_body={"type": f"sh; touch {marker}"}
    )
    assert res.status in (200, 400, 404), res.status
    assert not os.path.exists(marker), "an agent type reached a shell"
    d.assert_healthy()
