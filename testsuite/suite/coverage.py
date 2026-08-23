"""The daemon's public surface, enumerated.

Tests call `ctx.cover(...)` with these keys; the runner fails the run if
anything here was never exercised. That is the point of the file: a route added
to `http_conn.rs` without a test shows up as a red line rather than as silence.

Ground truth is the Rust source, not `docs/protocol.md`. **That is a rule about
where to look, not a claim that the docs are behind** — as of 2026-08-11 the
direction is the other way round, and this file is the one that lags.

What is missing, measured against the enums rather than remembered:

    HTTP_ROUTES   61 of the router's 70 arms. Absent: GET .../search,
                  POST .../git/apply, POST and DELETE .../git/remote,
                  GET .../git/worktrees, POST and DELETE .../git/worktree,
                  POST .../git/worktree/prune, DELETE .../file
    COMMANDS      26 of Command's 30. Absent: kill_server_clear,
                  set_default_agent, paste_image, put_file
    CLIENT_MSGS   5 of ClientMsg's 7. Absent: watch, notice
    SERVER_MSGS   9 of ServerMsg's 12. Absent: file_put, read_clipboard_image
                  (and theme_list, correctly — it has no construction site
                  anywhere in the daemon, so it is a tag nobody can receive)
    API_EVENTS    4 of ApiEvent's 6. Absent: workspace_detail, remote_announce

Everything on those lines is served, documented, and **unguarded**: it can break
without a red line anywhere. Note the shape of the fix — a name added here with
no test behind it *fails the run*, which is the whole point of the file, so
closing the gap is writing tests, not editing lists. `docs/protocol.md`'s
"What is actually covered, and by what" says the same from the other side, and
`web/check.py` counts the route half from the opposite end (it derives both the
denominator and the numerator from source, so neither can go stale).
"""

# crates/butai-server/src/http_conn.rs — the `match (&method, segs)` table.
HTTP_ROUTES = [
    "GET /v1/workspaces",
    "GET /v1/system",
    "POST /v1/update",
    "GET /v1/agents",
    "GET /v1/fs",
    "GET /v1/notifications",
    "GET /v1/events",
    "GET /v1/workspaces/{id}",
    "GET /v1/workspaces/{id}/agents",
    "GET /v1/workspaces/{id}/processes",
    "GET /v1/workspaces/{id}/changes",
    "GET /v1/workspaces/{id}/branches",
    "GET /v1/workspaces/{id}/tree",
    "GET /v1/workspaces/{id}/file",
    "GET /v1/workspaces/{id}/diff",
    "GET /v1/workspaces/{id}/show",
    "GET /v1/workspaces/{id}/download",
    "GET /v1/workspaces/{id}/panes/{pane}/output",
    "POST /v1/workspaces",
    "POST /v1/fs/mkdir",
    "DELETE /v1/workspaces/{id}",
    "POST /v1/workspaces/{id}/agents",
    "POST /v1/workspaces/{id}/processes",
    "POST /v1/workspaces/{id}/processes/{pane}/restart",
    "DELETE /v1/workspaces/{id}/panes/{pane}",
    "DELETE /v1/workspaces/{id}/processes/{pane}",  # legacy alias of the above
    "POST /v1/workspaces/{id}/panes/{pane}/input",
    "POST /v1/workspaces/{id}/panes/{pane}/ack",
    "POST /v1/workspaces/{id}/changes/stage",
    "POST /v1/workspaces/{id}/changes/unstage",
    "POST /v1/workspaces/{id}/changes/discard",
    "POST /v1/workspaces/{id}/changes/commit",
    "POST /v1/workspaces/{id}/changes/commit-all",
    "POST /v1/workspaces/{id}/checkout",
    "POST /v1/workspaces/{id}/git/fetch",
    "POST /v1/workspaces/{id}/git/pull",
    "POST /v1/workspaces/{id}/git/push",
    "GET /v1/workspaces/{id}/git/op",
    "DELETE /v1/workspaces/{id}/git/op",
    "GET /v1/workspaces/{id}/git/log",
    "GET /v1/workspaces/{id}/git/stashes",
    "GET /v1/workspaces/{id}/git/remotes",
    "GET /v1/workspaces/{id}/git/tags",
    "GET /v1/workspaces/{id}/git/conflict",
    "POST /v1/workspaces/{id}/git/stash",
    "POST /v1/workspaces/{id}/git/stash/apply",
    "DELETE /v1/workspaces/{id}/git/stash",
    "POST /v1/workspaces/{id}/git/amend",
    "POST /v1/workspaces/{id}/git/reset",
    "POST /v1/workspaces/{id}/git/revert",
    "POST /v1/workspaces/{id}/git/cherry-pick",
    "POST /v1/workspaces/{id}/git/merge",
    "POST /v1/workspaces/{id}/git/rebase",
    "POST /v1/workspaces/{id}/git/sequence",
    "POST /v1/workspaces/{id}/git/tag",
    "DELETE /v1/workspaces/{id}/git/tag",
    "POST /v1/workspaces/{id}/git/resolve",
    "POST /v1/workspaces/{id}/git/branch",
    "DELETE /v1/workspaces/{id}/git/branch",
    "POST /v1/workspaces/{id}/git/branch/rename",
    "POST /v1/workspaces/{id}/upload",
]

# crates/butai-protocol/src/lib.rs
CLIENT_MSGS = ["hello", "input", "resize", "command", "detach"]

SERVER_MSGS = [
    "hello",
    "frame",
    "session_list",
    "agent_list",
    "ok",
    "detached",
    "error",
    "set_clipboard",
    "bell",
]

ATTACH_TARGETS = ["attach", "new", "default", "control", "pane"]

COMMANDS = [
    "split_pane",
    "close_pane",
    "focus_dir",
    "focus_pane",
    "zoom_toggle",
    "resize_pane",
    "scroll_page",
    "new_window",
    "next_window",
    "prev_window",
    "select_window",
    "rename_window",
    "new_session",
    "kill_session",
    "list_sessions",
    "kill_server",
    "apply_layout",
    "open_file",
    "list_agents",
    "spawn_agent",
    "new_process",
    "reload_config",
    "set_theme",
    "list_themes",
    "toggle_all_agents",
    "git_menu",
]

# Answered with an error on purpose. That is a contract a GUI client depends
# on, not a gap. Three reasons, one list: the first nine ask for free panes in a
# workbench that has fixed rails; the next three ask the daemon to change a
# screen it does not keep — menus, zoom and the ALL AGENTS panel are each
# client's own view now, composed from `/v1/*`; and the last three ask it to
# colour or fill a screen it does not draw at all.
COMMANDS_REJECTED = [
    "split_pane",
    "focus_dir",
    "focus_pane",
    "resize_pane",
    "new_window",
    "next_window",
    "prev_window",
    "select_window",
    "apply_layout",
    "git_menu",
    "zoom_toggle",
    "toggle_all_agents",
    "set_theme",
    "list_themes",
    "open_file",
    "set_default_agent",
]

INPUT_EVENTS = [
    "key",
    "paste",
    "mouse_down",
    "mouse_drag",
    "mouse_up",
    "scroll_up",
    "scroll_down",
]

KEY_CODES = [
    "char",
    "enter",
    "esc",
    "backspace",
    "tab",
    "back_tab",
    "left",
    "right",
    "up",
    "down",
    "home",
    "end",
    "page_up",
    "page_down",
    "delete",
    "insert",
    "f",
]

PANE_KINDS = ["terminal", "editor", "file_tree", "git", "diff", "agent"]

API_EVENTS = ["system", "workspaces", "notification", "git_op"]

AGENT_STATES = ["waiting", "working", "finished", "idle", "exited"]

ENCODINGS = ["json", "msgpack"]

# crates/butai/src/main.rs
CLI_COMMANDS = [
    "new",
    "attach",
    "ls",
    "workspace",
    "kill-session",
    "kill-server",
    "daemon",
    "proxy",
    "reset",
    "standalone",
]


def _keyed(prefix, names):
    return [f"{prefix}:{name}" for name in names]


def expected():
    """Every coverage key the suite should touch, grouped for the report."""
    return {
        "http routes": list(HTTP_ROUTES),
        "client messages": _keyed("client", CLIENT_MSGS),
        "server messages": _keyed("server", SERVER_MSGS),
        "attach targets": _keyed("target", ATTACH_TARGETS),
        "commands": _keyed("cmd", COMMANDS),
        "input events": _keyed("input", INPUT_EVENTS),
        "key codes": _keyed("key", KEY_CODES),
        "api events": _keyed("event", API_EVENTS),
        "agent states": _keyed("agent", AGENT_STATES),
        "encodings": _keyed("encoding", ENCODINGS),
        "cli commands": _keyed("cli", CLI_COMMANDS),
    }


def all_keys():
    keys = set()
    for group in expected().values():
        keys |= set(group)
    return keys
