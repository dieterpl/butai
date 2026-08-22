"""The REST API's happy paths — every route in `http_conn.rs`.

Ten of these routes are absent from `docs/protocol.md`; they are tested here
against the code, which is the contract clients actually meet.
"""

import os
import urllib.parse

from suite import fixtures
from suite.daemon import Config
from suite.runner import test


def _q(value):
    return urllib.parse.quote(str(value), safe="")


@test(profile="smoke", tags=("http",))
def workspaces_are_created_listed_and_killed(ctx):
    d = ctx.daemon()
    ctx.cover(
        "GET /v1/workspaces",
        "POST /v1/workspaces",
        "GET /v1/workspaces/{id}",
        "DELETE /v1/workspaces/{id}",
    )
    project = fixtures.workspace(d.work, "alpha", files={"main.py": "print('hi')\n"})

    created = d.http.post("/v1/workspaces", json_body={"path": project, "name": "alpha"})
    assert created.status == 201, f"{created.status}: {created.text}"
    ws = created.json()["id"]

    listed = d.http.workspaces()
    assert len(listed) == 1, listed
    summary = listed[0]
    for field in (
        "id",
        "name",
        "cwd",
        "agents",
        "processes",
        "changes",
        "attached_clients",
    ):
        assert field in summary, f"{field} missing from WorkspaceSummary: {summary}"
    assert summary["name"] == "alpha"
    # realpath on both sides: the daemon canonicalises the workspace path, and
    # on macOS /tmp is a symlink to /private/tmp.
    assert os.path.realpath(summary["cwd"]) == os.path.realpath(project), summary

    detail = d.http.detail(ws)
    assert detail["id"] == ws
    assert isinstance(detail["agents"], list)
    assert detail["processes"], "a new workspace always gets its shell row"
    assert "changes" in detail and "stage" in detail

    assert d.http.delete(f"/v1/workspaces/{ws}").status == 200
    d.http.poll_until("/v1/workspaces", lambda w: w == [], "the workspace disappeared")


@test(profile="smoke", tags=("http",))
def only_workspace_creation_answers_201(ctx):
    """A small contract that is easy to get wrong in a client: creating a
    workspace is 201 + `{id}`; every other action is 200 + `{"ok":true}`."""
    d = ctx.daemon(config=Config().shell_agent("sh"))
    ws = d.http.new_workspace(path=d.work)

    agent = d.http.post(f"/v1/workspaces/{ws}/agents", json_body={"type": "sh"})
    assert agent.status == 200 and agent.json() == {"ok": True}, agent.text

    proc = d.http.post(
        f"/v1/workspaces/{ws}/processes", json_body={"name": "p", "command": "sleep 60"}
    )
    assert proc.status == 200 and proc.json() == {"ok": True}, proc.text
    ctx.cover("POST /v1/workspaces/{id}/agents", "POST /v1/workspaces/{id}/processes")


@test(profile="smoke", tags=("http",))
def system_gauges_are_well_formed(ctx):
    """In a container /proc/stat and /proc/meminfo are the *host's* — not the
    cgroup's — so the numbers are wrong on purpose. What must hold is that they
    are present, typed and non-fatal."""
    d = ctx.daemon()
    ctx.cover("GET /v1/system")
    sys_info = d.http.json_at("/v1/system")
    for field in ("cpu_pct", "ram_used_gb", "ram_total_gb", "gpus", "containers", "stacks"):
        assert field in sys_info, f"{field} missing from SysDto: {sys_info}"
    assert 0 <= sys_info["cpu_pct"] <= 100, sys_info["cpu_pct"]
    assert sys_info["ram_total_gb"] > 0, "no RAM total — /proc/meminfo unreadable?"
    assert isinstance(sys_info["gpus"], list)
    ctx.note(
        f"gauges report cpu={sys_info['cpu_pct']:.1f}% "
        f"ram={sys_info['ram_used_gb']:.1f}/{sys_info['ram_total_gb']:.1f}GB "
        f"(host-wide: /proc is not namespaced, so --cpus/--memory are ignored)"
    )


@test(profile="smoke", tags=("http", "agents"))
def agent_types_come_from_config(ctx):
    d = ctx.daemon(config=Config().shell_agent("sh").agent("other", "/bin/sh"))
    ctx.cover("GET /v1/agents")
    assert d.http.json_at("/v1/agents") == ["sh", "other"]


@test(profile="standard", tags=("http",))
def agents_and_processes_have_their_own_endpoints(ctx):
    d = ctx.daemon(config=Config().shell_agent("sh"))
    ctx.cover("GET /v1/workspaces/{id}/agents", "GET /v1/workspaces/{id}/processes")
    ws = d.http.new_workspace(path=d.work)
    d.http.spawn_agent(ws, "sh")

    agents = d.http.poll_until(
        f"/v1/workspaces/{ws}/agents", lambda a: len(a) == 1, "the agent row appeared"
    )
    for field in ("pane", "title", "state"):
        assert field in agents[0], f"{field} missing from AgentDto: {agents[0]}"

    d.http.new_process(ws, "worker", "sleep 120")
    procs = d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: any(x["name"] == "worker" for x in p),
        "the process row appeared",
    )
    worker = next(p for p in procs if p["name"] == "worker")
    for field in ("pane", "name", "command", "status"):
        assert field in worker, f"{field} missing from ProcessDto: {worker}"
    assert worker["command"] == "sleep 120"


@test(profile="standard", tags=("http",))
def the_file_tree_and_file_reader_walk_the_workspace(ctx):
    d = ctx.daemon()
    ctx.cover("GET /v1/workspaces/{id}/tree", "GET /v1/workspaces/{id}/file")
    project = fixtures.workspace(
        d.work,
        "tree-ws",
        files={"src/lib.rs": "pub fn hi() {}\n", "README.md": "# tree\n"},
    )
    ws = d.http.new_workspace(path=project)

    root = d.http.json_at(f"/v1/workspaces/{ws}/tree")
    names = {e["name"] for e in root["entries"]}
    assert {"src", "README.md"} <= names, names
    src = next(e for e in root["entries"] if e["name"] == "src")
    assert src["is_dir"] is True
    for field in ("name", "path", "is_dir", "changed", "size"):
        assert field in src, f"{field} missing from TreeEntry: {src}"

    nested = d.http.json_at(f"/v1/workspaces/{ws}/tree?path={_q('src')}")
    assert [e["name"] for e in nested["entries"]] == ["lib.rs"], nested

    body = d.http.json_at(f"/v1/workspaces/{ws}/file?path={_q('src/lib.rs')}")
    assert body["text"] == "pub fn hi() {}\n", body
    assert body["truncated"] is False


@test(profile="standard", tags=("http",))
def a_large_file_is_truncated_rather_than_streamed(ctx):
    """The reader caps at 512 KiB and says so, instead of shipping a 200 MB
    JSON string to a GUI that only wanted a preview."""
    d = ctx.daemon()
    project = fixtures.workspace(d.work, "big-file-ws")
    with open(os.path.join(project, "huge.txt"), "w") as fh:
        fh.write("x" * (2 * 1024 * 1024))
    ws = d.http.new_workspace(path=project)
    body = d.http.json_at(f"/v1/workspaces/{ws}/file?path={_q('huge.txt')}")
    assert body["truncated"] is True, "a 2 MiB file should report truncated"
    assert len(body["text"]) < 2 * 1024 * 1024, len(body["text"])
    ctx.note(f"file reader capped a 2 MiB file at {len(body['text'])} bytes")


@test(profile="standard", tags=("http",))
def download_and_upload_move_bytes_both_ways(ctx):
    d = ctx.daemon()
    ctx.cover("GET /v1/workspaces/{id}/download", "POST /v1/workspaces/{id}/upload")
    project = fixtures.workspace(d.work, "transfer-ws")
    payload = bytes(range(256)) * 64
    with open(os.path.join(project, "blob.bin"), "wb") as fh:
        fh.write(payload)
    ws = d.http.new_workspace(path=project)

    got = d.http.get(f"/v1/workspaces/{ws}/download?path={_q('blob.bin')}")
    assert got.status == 200, got.text[:200]
    assert got.body == payload, f"{len(got.body)} bytes back, wanted {len(payload)}"
    assert "attachment" in got.headers.get("content-disposition", ""), got.headers

    sent = b"uploaded-through-the-api\n"
    up = d.http.post(
        f"/v1/workspaces/{ws}/upload?path={_q('uploads/new.txt')}",
        raw=sent,
    )
    assert up.status == 200, up.text[:200]
    with open(os.path.join(project, "uploads", "new.txt"), "rb") as fh:
        assert fh.read() == sent


@test(profile="standard", tags=("http",))
def the_filesystem_browser_lists_and_creates_directories(ctx):
    """`/v1/fs` is how a GUI's "open a project" picker works — it is outside any
    workspace, so it is the one path-taking endpoint with no workspace root."""
    d = ctx.daemon()
    ctx.cover("GET /v1/fs", "POST /v1/fs/mkdir")
    base = fixtures.workspace(d.work, "browse-root", files={"a/keep.txt": "x"})

    listing = d.http.json_at(f"/v1/fs?path={_q(base)}")
    assert os.path.realpath(listing["path"]) == os.path.realpath(base), listing
    assert "parent" in listing
    assert any(e["name"] == "a" and e["is_dir"] for e in listing["entries"]), listing

    made = d.http.post("/v1/fs/mkdir", json_body={"path": base, "name": "fresh"})
    assert made.status == 200, made.text[:200]
    assert os.path.isdir(os.path.join(base, "fresh"))

    default = d.http.get("/v1/fs")
    assert default.status == 200, default.text[:200]


@test(profile="standard", tags=("http", "panes"))
def panes_take_input_acks_and_deletion(ctx):
    """The list-UI counterpart to attaching: answer an agent's prompt, dismiss
    its bell, or kill it, all without opening a stream."""
    d = ctx.daemon(config=Config().shell_agent("sh"))
    ctx.cover(
        "POST /v1/workspaces/{id}/panes/{pane}/input",
        "POST /v1/workspaces/{id}/panes/{pane}/ack",
        "DELETE /v1/workspaces/{id}/panes/{pane}",
    )
    ws = d.http.new_workspace(path=d.work)
    d.http.spawn_agent(ws, "sh")
    agents = d.http.poll_until(
        f"/v1/workspaces/{ws}/agents", lambda a: len(a) == 1, "the agent appeared"
    )
    pane = agents[0]["pane"]

    typed = d.http.post(
        f"/v1/workspaces/{ws}/panes/{pane}/input",
        json_body={"paste": "echo PANE-INPUT-WORKED\n"},
    )
    assert typed.status == 200 and typed.json() == {"ok": True}, typed.text

    acked = d.http.post(f"/v1/workspaces/{ws}/panes/{pane}/ack")
    assert acked.status == 200, acked.text

    killed = d.http.delete(f"/v1/workspaces/{ws}/panes/{pane}")
    assert killed.status == 200, killed.text
    d.http.poll_until(
        f"/v1/workspaces/{ws}/agents",
        lambda a: all(x["pane"] != pane for x in a),
        "the killed agent left the rail",
    )


@test(profile="standard", tags=("http", "panes"))
def the_legacy_processes_delete_alias_still_kills_a_pane(ctx):
    """`DELETE .../processes/{pane}` predates the pane-generic route and is kept
    so a client built against an older daemon keeps working."""
    d = ctx.daemon()
    ctx.cover("DELETE /v1/workspaces/{id}/processes/{pane}")
    ws = d.http.new_workspace(path=d.work)
    d.http.new_process(ws, "doomed", "sleep 300")
    procs = d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: any(x["name"] == "doomed" for x in p),
        "the process appeared",
    )
    pane = next(p for p in procs if p["name"] == "doomed")["pane"]

    killed = d.http.delete(f"/v1/workspaces/{ws}/processes/{pane}")
    assert killed.status == 200, killed.text
    d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: all(x["pane"] != pane for x in p),
        "the pane is gone",
    )


@test(profile="standard", tags=("http", "notifications"))
def notifications_accumulate_behind_a_cursor(ctx):
    """A polling client asks for everything after the last `seq` it saw, so the
    feed has to be monotonic and replayable."""
    d = ctx.daemon(config=Config().fake_agents("fake-claude"))
    ctx.cover("GET /v1/notifications")
    ws = d.http.new_workspace(path=d.work)

    empty = d.http.json_at("/v1/notifications?since=0")
    assert "head" in empty and "items" in empty, empty
    assert empty["items"] == [], empty

    d.http.spawn_agent(ws, "fake-claude")
    feed = d.http.poll_until(
        "/v1/notifications?since=0",
        lambda f: len(f["items"]) >= 1,
        "an agent notification arrived",
        timeout=45,
    )
    item = feed["items"][0]
    for field in ("seq", "at_ms", "ws", "ws_name", "pane", "title", "kind"):
        assert field in item, f"{field} missing from NotificationDto: {item}"
    assert item["kind"] in ("waiting", "finished", "exited"), item["kind"]

    after = d.http.json_at(f"/v1/notifications?since={feed['head']}")
    assert after["items"] == [], f"since=head should be empty, got {after}"
    ctx.note(f"first notification was {item['kind']} for {item['title']!r}")


@test(profile="standard", tags=("http",))
def restarting_a_process_gives_it_a_new_pane(ctx):
    """Restart allocates a fresh PaneId, so a client holding the old one has to
    re-read — worth pinning, because a GUI that caches it silently breaks."""
    d = ctx.daemon()
    ctx.cover("POST /v1/workspaces/{id}/processes/{pane}/restart")
    project = fixtures.workspace(
        d.work,
        "restart-ws",
        butai_file=fixtures.butai_toml(processes=[("web", "echo SERVER-UP; sleep 300", "SERVER-UP")]),
    )
    ws = d.http.new_workspace(path=project)
    procs = d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: any(x["name"] == "web" and x["status"] == "ok" for x in p),
        "the ready marker flipped the row to ok",
        timeout=30,
    )
    before = next(p for p in procs if p["name"] == "web")["pane"]

    restarted = d.http.post(f"/v1/workspaces/{ws}/processes/{before}/restart")
    assert restarted.status == 200, restarted.text
    after = d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda p: any(x["name"] == "web" and x["pane"] != before for x in p),
        "the process came back on a new pane",
        timeout=30,
    )
    new_pane = next(p for p in after if p["name"] == "web")["pane"]
    assert new_pane != before
    stale = d.http.delete(f"/v1/workspaces/{ws}/panes/{before}")
    assert stale.status in (400, 404), f"a stale pane id should not resolve: {stale.status}"
