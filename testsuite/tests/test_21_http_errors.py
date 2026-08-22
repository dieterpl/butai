"""Malformed requests.

An API is only as good as what it says when you get it wrong, and these are the
paths a hand-written client hits first.
"""

from suite.runner import test


@test(profile="smoke", tags=("http", "errors"))
def an_unknown_route_404s_with_the_route_it_did_not_find(ctx):
    d = ctx.daemon()
    res = d.http.get("/v1/nope")
    assert res.status == 404, res.status
    assert "no route" in res.json()["error"], res.text
    assert "/v1/nope" in res.json()["error"], res.text

    wrong_method = d.http.request("PUT", "/v1/workspaces")
    assert wrong_method.status == 404, wrong_method.status


@test(profile="smoke", tags=("http", "errors"))
def an_unknown_workspace_404s_and_a_malformed_id_400s(ctx):
    """The distinction matters: 404 means "ask again later", 400 means "your
    client built the URL wrong"."""
    d = ctx.daemon()
    missing = d.http.get("/v1/workspaces/999999")
    assert missing.status == 404, f"{missing.status}: {missing.text}"

    malformed = d.http.get("/v1/workspaces/not-a-number")
    assert malformed.status == 400, f"{malformed.status}: {malformed.text}"
    assert "workspace id" in malformed.json()["error"], malformed.text

    bad_pane = d.http.delete("/v1/workspaces/1/panes/not-a-pane")
    assert bad_pane.status == 400, f"{bad_pane.status}: {bad_pane.text}"


@test(profile="standard", tags=("http", "errors"))
def endpoints_that_need_a_path_say_so(ctx):
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    for route in ("file", "diff", "download"):
        res = d.http.get(f"/v1/workspaces/{ws}/{route}")
        assert res.status == 400, f"{route}: {res.status} {res.text[:200]}"
        assert "?path=" in res.json()["error"], f"{route}: {res.text[:200]}"

    show = d.http.get(f"/v1/workspaces/{ws}/show")
    assert show.status == 400 and "?id=" in show.json()["error"], show.text[:200]

    upload = d.http.post(f"/v1/workspaces/{ws}/upload", raw=b"x")
    assert upload.status == 400, upload.text[:200]


@test(profile="standard", tags=("http", "errors"))
def a_valueless_query_key_is_an_empty_string_not_an_absent_one(ctx):
    """`query_get` is hand-rolled: `?path` with no `=` yields `Some("")`. That
    is a real difference from "absent" and a client can trip on it, so the only
    thing that must hold is that it never becomes a 500."""
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    res = d.http.get(f"/v1/workspaces/{ws}/file?path")
    assert res.status != 500, f"valueless ?path 500'd: {res.text[:300]}"
    ctx.note(f"GET file?path (no '=') answers {res.status}")


@test(profile="standard", tags=("http", "errors"))
def malformed_json_bodies_are_rejected_not_ignored(ctx):
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    cases = [
        ("POST", f"/v1/workspaces/{ws}/agents", b"{not json"),
        # Wrong *type*, not a missing field: `command` is optional on purpose
        # (see below), so leaving it out is a documented request, not garbage.
        ("POST", f"/v1/workspaces/{ws}/processes", b'{"name": "x", "command": 7}'),
        ("POST", f"/v1/workspaces/{ws}/changes/stage", b"[]"),
        ("POST", f"/v1/workspaces/{ws}/changes/commit", b"{}"),  # missing message
        ("POST", f"/v1/workspaces/{ws}/panes/1/input", b'{"nonsense": true}'),
    ]
    for method, path, body in cases:
        res = d.http.request(method, path, raw=body, content_type="application/json")
        assert res.status == 400, f"{method} {path} with {body!r} -> {res.status}: {res.text[:200]}"
    d.assert_healthy()


@test(profile="standard", tags=("http", "processes"))
def a_process_with_no_command_gets_the_default_shell(ctx):
    """"Start me a process" with nothing to run has one sensible reading, and
    it is the one the `[+ term]` button relies on — the button asks for a
    terminal, not for a program."""
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    before = {p["pane"] for p in d.http.processes(ws)}
    for body in ({"name": "bare"}, {"name": "blank", "command": "  "}):
        res = d.http.post(f"/v1/workspaces/{ws}/processes", json_body=body)
        assert res.status == 200, f"{body} -> {res.status}: {res.text[:200]}"
    procs = d.http.poll_until(
        f"/v1/workspaces/{ws}/processes",
        lambda ps: len({p["pane"] for p in ps} - before) == 2,
        "both shells started",
        timeout=20,
    )
    for name in ("bare", "blank"):
        row = next(p for p in procs if p["name"] == name)
        assert row["exited"] is None, f"{name} died instead of opening a shell: {row}"
    d.assert_healthy()


@test(profile="standard", tags=("http", "errors"))
def an_empty_body_is_fine_where_every_field_is_optional(ctx):
    """`POST /v1/workspaces` has no required fields, so a bare POST must work —
    that is how "new workspace here" is spelled."""
    d = ctx.daemon()
    res = d.http.post("/v1/workspaces")
    assert res.status == 201, f"{res.status}: {res.text[:200]}"
    assert isinstance(res.json()["id"], int)


@test(profile="standard", tags=("http", "errors"))
def trailing_and_doubled_slashes_are_harmless(ctx):
    """Path segments are filtered for emptiness, so these normalize away. Worth
    a test: a client that naively joins URLs produces them constantly."""
    d = ctx.daemon()
    ws = d.http.new_workspace(path=d.work)
    for path in (
        "/v1/workspaces/",
        f"/v1/workspaces/{ws}/",
        f"//v1//workspaces//{ws}",
    ):
        res = d.http.get(path)
        assert res.status == 200, f"{path} -> {res.status}: {res.text[:200]}"


@test(profile="standard", tags=("http", "errors"))
def acting_on_a_pane_in_the_wrong_workspace_is_refused(ctx):
    """Pane ids are global but every action validates against the workspace's
    own pane set — otherwise one client could kill another project's agent."""
    d = ctx.daemon()
    first = d.http.new_workspace(path=d.work)
    second = d.http.new_workspace(path=d.work)
    pane = d.http.detail(first)["processes"][0]["pane"]

    res = d.http.delete(f"/v1/workspaces/{second}/panes/{pane}")
    assert res.status in (400, 404), f"cross-workspace kill answered {res.status}"
    still_there = [p["pane"] for p in d.http.processes(first)]
    assert pane in still_there, "the pane was killed from the wrong workspace"
