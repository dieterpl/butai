# The butai client protocol (version 1)

Every butai client — the built-in TUI, `butai ls`, a Python script, an
Electron or Swift GUI — talks to the daemon through this one API. There is
no privileged path: anything the TUI can do, your client can do.

## Transport

* **Unix domain socket**, one daemon per user: `~/.butai/butai.sock`, overridable
  with the `BUTAI_SOCKET` environment variable. Authentication is the socket
  directory's `0700` permissions. (The path is home-relative rather than
  `$XDG_RUNTIME_DIR`-relative because that variable is set for a login shell but
  routinely absent from a non-interactive `ssh host butai ...`, so a remote
  client resolving it would miss the running daemon and spawn a second one.)
* **Remote access rides SSH** — the daemon never listens on TCP:
  * `ssh host butai proxy` bridges ssh's stdio to the daemon socket. Spawn it
    as a child process and speak the protocol over its stdin/stdout
    (the Docker CLI pattern). SSH keys are the authentication.
  * Alternatively forward the socket itself:
    `ssh -N -L /tmp/butai.sock:/home/<user>/.butai/butai.sock host`.

## Framing

Each message is one frame: a **4-byte big-endian length prefix** followed by
that many bytes of payload. The payload is JSON (UTF-8). Maximum frame size
is 32 MiB.

The client's first message must be `hello`, always JSON. Its `encoding`
field (`"json"` or `"msgpack"`) selects the encoding for **all frames after
each side's hello**. MessagePack (named-field maps, same structure) is an
optional optimization — JSON is the baseline every server accepts. Note that
"after each side's hello" is literal: it is the first frame *in each direction*
that is JSON, so if the handshake is rejected, the `error` is JSON and the
`detached` that follows is already in the negotiated encoding.

A connection's **first** frame must be under 16 MiB, not 32: the daemon decides
framed-vs-HTTP by peeking one byte, and a length prefix only has a zero top byte
below 16 MiB. Every real first frame is a small hello, so nothing legitimate
reaches this.

Enum variants without a payload are serialized by serde as **bare JSON strings**
— the wire carries `"detach"`, `"ok"` and `"bell"`, not `{"detach": null}`. The
`{...: null}` form is accepted on input, so a client may send either, but a
client's *parser* has to handle a bare string.

## Handshake

Client → server:

```json
{"hello": {"proto_version": 1, "encoding": "json", "cols": 120, "rows": 40,
           "target": {"new": {"name": "work", "layout": "ide"}},
           "cwd": "/home/me/project"}}
```

`target` selects what to attach to:

| target | meaning |
|---|---|
| `{"attach": {"name": "work"}}` | existing session by name — **no frames**, see below |
| `{"new": {"name": null, "layout": null}}` | create (name auto-assigned), **no frames**. `layout` is accepted and ignored — see below |
| `"default"` | most recent session, creating one if none exist, **no frames** |
| `"control"` | no session, no frames — for CLI one-shots and GUIs that only want structured state |
| `{"pane": {"pane": 7}}` | stream **one pane** full-bleed at this connection's size — the web client's "stage". Input routes straight to the pane; multiple viewers share it (the PTY holds one size, latest driver wins). Pair with the REST API to drive the surrounding UI. |

`layout` named a preset of pane splits. The workbench has fixed rails and no
free panes — which is why `apply_layout` is refused — so there has been nothing
for a preset to describe for some time. The field stays on the wire because
shipped clients send it, and an unknown key would be ignored anyway.

**Only a `pane` target receives frames.** The three session targets scope the
connection to a workspace so `butai new` and `butai attach` keep their meaning,
and so commands sent on it know which workspace they mean — but the daemon
draws no workbench, so there is nothing to send. A client builds its workbench
from `/v1/*` and one `pane` attach for the stage. That is what `web/`, the
macOS and iOS apps, and the bundled TUI all do.

Server → client (always JSON):

```json
{"hello": {"proto_version": 1,
           "session": {"id": 1, "name": "work", "windows": 1,
                        "attached_clients": 1, "cwd": "/home/me/project"},
           "server_version": "0.12.1"}}
```

A `proto_version` mismatch gets `{"error": "..."}` followed by
`{"detached": {"reason": "protocol mismatch"}}`.

`server_version` is the daemon's own build string. It is optional — omitted when
unset, and absent entirely from daemons older than the field — and exists so a
client can tell a *stale daemon* from a broken one. See
[Versioning](#versioning).

## Client → server messages

```json
{"input": {"key": {"code": {"char": "x"}, "mods": {"ctrl": true}}}}
{"input": {"paste": "some text"}}
{"input": {"scroll_up": {"x": 10, "y": 4}}}
{"input": {"mouse_down": {"x": 10, "y": 4}}}
{"input": {"mouse_down": {"x": 10, "y": 4, "button": "right"}}}
{"resize": {"cols": 100, "rows": 30}}
{"command": {"split_pane": {"dir": "horizontal", "kind": {"terminal": {"command": null}}}}}
{"watch": {"pane": 7}}                    // re-point a `pane` connection
{"notice": "no image on the clipboard"}   // something only the client could know
{"detach": null}
```

`watch` changes which pane a `{"pane": ...}` connection is streaming, without
reconnecting. You get a full frame for the new pane, exactly as if you had
attached to it — so clear the screen before applying it, the way you would on
attach. A client that shows one pane at a time has to change which one, and the
alternative is tearing the connection down and dialling again: a visible stall
on any link with latency, for what is bookkeeping on the daemon's side.

Sent on any other kind of connection, or naming a pane that does not exist, it
answers `error` and **keeps streaming whatever it was streaming**. A pane can
exit between you deciding to watch it and the daemon reading the message, so a
refusal is ordinary rather than a client bug — and losing the pane you already
had would be the wrong answer to it.

Added in 0.6. `PROTOCOL_VERSION` is unchanged: a client that never sends one
behaves exactly as before.

Key codes: `{"char": "a"}`, `"enter"`, `"esc"`, `"backspace"`, `"tab"`,
`"back_tab"`, `"left"`, `"right"`, `"up"`, `"down"`, `"home"`, `"end"`,
`"page_up"`, `"page_down"`, `"delete"`, `"insert"`, `{"f": 5}`.
Mods: `{"ctrl": true, "alt": true, "shift": true}` (all optional, default false).

`mouse_down` carries an optional `button`, `"left"` (default) or `"right"`.
It is omitted when left, so a left click is byte-identical to what clients sent
before right-click existed. `"right"` is a client's cue to open its own context
menu; a `pane` connection has no chrome to hang one off, so the daemon drops it
rather than starting a selection with it. `mouse_up` and `mouse_drag`
deliberately carry no `button`: only the left button drags, so a right press has
no matching release on the wire.

`scroll_up` / `scroll_down` are **one notch of a wheel**, and the daemon decides
where a notch goes: to the program, if it has enabled mouse reporting, as the
SGR sequence it is waiting for; otherwise three lines of the pane's own
scrollback. Only the daemon can make that call — whether the program wants the
mouse is a fact about the bytes it is writing, which no client sees — so send
the notch and let it. A client that scrolls its own picture instead will do
nothing at all over `vim`, `less` or an agent CLI: those draw on the alternate
screen, where there is no scrollback to move.

For a *page* — what `page_up` is for, and what a scrollbar drags — send the
`scroll_page` command below instead. Same direction convention, different unit.

### Commands

Commands are the shared vocabulary of keybindings, the palette, and API
clients. Snake-cased enum on the wire:

```json
{"split_pane": {"dir": "horizontal|vertical", "kind": <pane_kind>}}
{"close_pane": null}
{"focus_dir": "left|right|up|down"}
{"focus_pane": 7}
{"zoom_toggle": null}
{"resize_pane": {"dir": "left", "cells": 2}}
{"scroll_page": -1}            // scrollback: negative back, positive forward
{"new_window": null}  {"next_window": null}  {"prev_window": null}
{"select_window": 1}
{"rename_window": "build"}
{"new_session": {"name": "x", "layout": "ide"}}
{"kill_session": "x"}
{"list_sessions": null}
{"kill_server": null}          // stops the daemon, keeps the session
{"kill_server_clear": null}    // stops it and forgets the session
{"apply_layout": "ide"}
{"open_file": "/path/to/file"}
{"list_agents": null}
{"spawn_agent": "claude"}
{"set_default_agent": "claude"}   // null clears the pin
{"new_process": {"name": "web", "command": "npm run dev"}}
{"reload_config": null}
{"set_theme": "tokyonight"}
{"list_themes": null}
{"toggle_all_agents": null}
{"git_menu": null}
{"paste_image": null}
{"put_file": {"name": "clipboard.png", "data": "<base64>"}}
```

Pane kinds: `{"terminal": {"command": null}}`, `{"editor": {"path": null}}`,
`"file_tree"`, `"git"`, `"diff"`, `{"agent": {"name": "claude"}}`.

**Sixteen of these are answered with an `error` on purpose**, for four reasons.

Nine ask for free panes in a workbench that has fixed rails: `split_pane`,
`focus_dir`, `focus_pane`, `resize_pane`, `new_window`, `next_window`,
`prev_window`, `select_window` and `apply_layout` reply with a message pointing
at the alternatives instead of rearranging the chrome.

Three — `git_menu`, `zoom_toggle` and `toggle_all_agents` — ask the daemon to
change a screen it does not keep. Every client composes its own workbench from
`/v1/*` and decides for itself what is open, folded or zoomed, so obeying these
would move every viewer at once. They are refused rather than quietly ignored,
because silence would read as "done".

Three — `set_theme`, `list_themes` and `open_file` — ask it to colour or fill a
screen it does not draw. A theme colours a screen, and the only screen the
daemon draws is a program's own cells, which carry the program's own colours;
every client picks its palette from its own config, which is what lets one
terminal be dark and another light while both watch the same workspace.
`open_file` is the same shape: the file is at
`GET /v1/workspaces/{id}/file?path=`, and the editor that shows it is the
client's.

`theme_list` therefore no longer appears on the wire. The `ServerMsg::ThemeList`
variant is still in `butai-protocol` and has **no construction site anywhere** —
dropping it from the enum is a source-breaking change for anything that matches
on `ServerMsg`, and nothing has needed one enough to make it. It is listed here
rather than under [Server → client messages](#server--client-messages) because
that section is what you can receive, and this is a tag you cannot: a parser
should accept it (unknown messages are ignored anyway) and never expect it.

One — `set_default_agent` — asked it to write a *client's* config file. The pin
is what your own `[+ agent]` spawns without being asked; it lives in your
`[general] default_agent`, the daemon never reads it, and the names to validate
against are at `GET /v1/agents`. It was the last command that reached across
that line.

All sixteen remain in the vocabulary so a client built against a freer layout, or
against the daemon that used to draw one, gets a reason rather than silence.

Every connection gets the same replies: a bare `ok` for a command that worked
and `error` for one that did not. There used to be a second path — an
interactive target had its text written into the footer the daemon was drawing
for it — and there is no such footer now, so there is one answer and the client
decides where to put it.

**Which pane a command acts on** follows the connection. Most act on the
workspace's staged pane, but a `pane` target has no workspace and does have an
obvious subject — the pane it is streaming — so `scroll_page` and `put_file`
use that. Scrollback is therefore reachable from a `pane` attach without a
second connection: send `{"scroll_page": -1}` down the same socket the frames
arrive on. New output snaps the view back to live, as in any terminal.

### `put_file` — pasting an image (or any file) into a pane

The daemon writes `data` to `~/.butai/scratch/<workspace>/` and pastes the
resulting **absolute path** into the pane your input would have reached, which
is the form agent CLIs accept an image in. It answers with `file_put`.

`data` is base64 (standard alphabet; padding optional) rather than a byte array,
so JSON and MessagePack clients send the identical structure. Files are capped
at **8 MiB decoded** — frames go to 32 MiB, so the limit is there to refuse a
40-megapixel photo with a readable error instead of a rejected frame. `name` is
advisory: only its basename survives, non-alphanumerics become `-`, and the
daemon prefixes a counter, so a client cannot choose where the file lands. Each
workspace keeps its most recent 32.

Deliberately **not** the same thing as `POST /v1/workspaces/{id}/upload`. That
route writes into the project and the file appears in the changes rail, which is
right for a file you meant to add and wrong for a pasted screenshot. It is a
command rather than a second REST route because a remote TUI has one
`ssh host butai proxy` channel already open, and using HTTP would cost it another
one per paste.

### `paste_image` — and why the daemon asks you back

A clipboard belongs to the machine the *client* runs on. Over
`ssh host butai proxy` that is not the machine the daemon runs on, so the daemon
cannot read it and does not try:

```
client                          daemon
  {"command": "paste_image"}  →            (a keybinding, the `:` prompt, a button)
                             ←  "read_clipboard_image"
  {"command": {"put_file": …}} →           (or {"notice": "…"} if there is none)
                             ←  {"file_put": {"path": …}}
```

`paste_image` is in the command vocabulary rather than being a client-side
keybinding so that the keymap, `:paste-image`, and the help overlay all describe
it in one place, and a client that has no clipboard just ignores the request.

`notice` is how a *client-side* handler reports back through the same path as a
daemon-side one: "no image on the clipboard" is known only by the side that
looked, and this puts it wherever the client shows `error`. Truncated at 200
characters.

## Server → client messages

```json
{"frame": { ... }}                    // see Frames below
{"session_list": [<session_info>]}    // reply to list_sessions
{"agent_list": ["claude", "aider"]}   // reply to list_agents
"ok"                                  // success ack for control commands
{"detached": {"reason": "detached"}}  // always last; the server then closes — see below
{"error": "human-readable message"}
{"set_clipboard": "selected text"}    // put this on the system clipboard
"bell"                                // ring the terminal bell
{"file_put": {"path": "/home/me/.butai/scratch/proj-1a2b/000001-clipboard.png"}}
"read_clipboard_image"                // read your clipboard, reply with put_file
```

`set_clipboard` follows a mouse selection on a `pane` connection: the daemon
owns that pane's grid, so it does the extraction and hands back the text to
copy — which is how a client with no VT parser supports selection at all. A
client that composes its own screen selects against that instead and never sees
this message.

`bell` is sent to every interactive client viewing a workspace when one of its
agents first needs attention. "Viewing" includes streaming any *pane* of the
workspace, not only the agent's own and not only a session connection: a
workbench is one pane attach plus `/v1/*`, and the bell's whole purpose is to
find you when you are looking at something else. `file_put` answers `put_file`: the path is
already in the pane, and this is so a client with no footer — a `pane` target —
can still say where the file went.

`"ok"`, `"bell"` and `"read_clipboard_image"` carry no payload and are therefore
**bare strings** on the wire, not one-key objects. So is `paste_image` in the
other direction: `{"command": "paste_image"}`.

### `detached` — one reason is not like the others

Every `detached` ends the connection, but they do not all mean the same thing,
and the reason is how you tell:

| `reason` | What it means |
| --- | --- |
| `"server shutting down"` | **The daemon is going, the pane is not.** |
| everything else | The thing you were watching is gone: `"pane closed"`, `"workspace closed"`, `"detached"`, `"no such pane"`, `"pane has no screen"`, `"protocol mismatch"` |

The distinction is worth drawing because the two call for opposite screens. A
pane that closed should leave an empty stage — there is nothing to show. A
daemon that is shutting down has not touched your pane: `kill-server` snapshots
every workspace and restores it on the next start, so the program is coming
back, and clearing the screen tells the user their agent died when it did not.
The TUI keeps its last frame, dims it and says so; **a client that ignores the
distinction is still correct**, it just cannot draw the difference.

The same silence with no `detached` in front of it — the socket simply closing —
is the *other* half of this and means the same thing: a daemon that was killed
outright, or a forwarded socket whose `ssh` went away, never gets to send a
reason. Treat end-of-stream as `"server shutting down"` rather than as a closed
pane. See [remote.md](remote.md#when-a-machine-goes-away).

## Frames — how pane content reaches you

The daemon renders exactly one thing: a terminal's screen. A pane holds a PTY,
so what is on it is the accumulated effect of every byte the program has
written, and reconstructing that needs a VT emulator — which is why this crosses
the wire as cells rather than as JSON, and why no client needs a VT parser of
its own. Everything else about a workspace is state, and state goes out over
`/v1/*` for the client to draw.

Frames therefore only ever arrive on a `pane` connection, and always cover just
that pane, full-bleed at the connection's size. Clients receive damage diffs:

```json
{"frame": {
  "full": true,
  "cells": [
    {"x": 0, "y": 0, "cells": [
      {"ch": "$"},
      {"ch": " "},
      {"ch": "v", "fg": {"indexed": 2}, "mods": {"bold": true}}
    ]}
  ],
  "cursor": [2, 0]
}}
```

* `full: true` (attach, resize) means clear the screen, then apply.
  Otherwise apply the runs over what you have.
* `wants_mouse: true` means the program in this pane has asked for mouse
  reporting, so a drag over it belongs to *it* rather than to your own text
  selection. Only the daemon can know this — it is parsing the program's output
  — and only you can act on it. Absent means false, so a client that ignores it
  simply always selects, which is what every client did before it existed.
* Each run is a row-contiguous span starting at `(x, y)`.
* Cell: `ch` is the grapheme (may be multi-codepoint). `fg`/`bg` are
  `"default"`, `{"indexed": 0-255}`, or `{"rgb": [r, g, b]}`. `mods` has
  optional booleans `bold`, `dim`, `italic`, `underline`, `reverse`,
  `crossed_out`. Absent fields mean default.
* **Advance by each grapheme's display width, not by one per cell.** A run is a
  sequence of consecutive graphemes with **no filler cell** for the second
  column of a wide character, so `CJK:日本語` arrives as seven cells covering ten
  columns. A client that advances one column per cell will shift everything
  after the first wide character on the line. Both
  [`examples/api-client.py`](../examples/api-client.py) and
  `testsuite/suite/screen.py` show a correct reader.
* `cursor` is `[x, y]` absolute in the viewport, or `null` when hidden — also
  `null` for a pane scrolled back and for a command that has exited. **Draw it.**
  The daemon consumed the escape sequences that would have moved a real
  terminal's cursor, so nothing else will put one on your screen: a client that
  ignores this field shows a shell with no caret to type against.
* `cursor_shape` is `"block"`, `"underline"` or `"bar"`, absent meaning `block`.
  Today it is *always* `block` — the emulator does not track `DECSCUSR` — so a
  client drawing into a real terminal should leave the shape alone rather than
  force one, and a client drawing its own cursor should treat anything but the
  three names as `block`.

Updates are coalesced server-side (≤ ~60 fps) and state-based, so a slow
client just sees fewer, larger diffs — never a backlog of stale output.

A pane is sized by whoever is streaming it — its `cols`/`rows` at attach, and
every `resize` after that, latest wins. A pane nobody is streaming holds the
size of the last client that did, or a conventional 24x80 if there has never
been one. The daemon does not compute a size for it: how big a stage is depends
on how wide the client made its rails, which is not a fact this side has.

## Minimal session example

See [`examples/api-client.py`](../examples/api-client.py) — a dependency-free
Python client that creates a session, runs a command, and prints the pane
content, in ~100 lines.

## HTTP/REST API (same socket, Docker-style)

The daemon also speaks **HTTP/1.1 on the very same socket**, for tools that
want structured state without the framed streaming protocol — the pattern of
`curl --unix-socket /var/run/docker.sock`. A connection is routed to the HTTP
handler when its first byte is an ASCII method letter; a framed hello always
begins with `0x00` (the top byte of its length prefix), so the two never
collide. Auth is unchanged: the socket directory's `0700` permissions.

```sh
S=~/.butai/butai.sock
curl --unix-socket $S http://localhost/v1/workspaces
curl --unix-socket $S http://localhost/v1/system
curl --unix-socket $S -X POST http://localhost/v1/workspaces/1/agents \
     -H 'content-type: application/json' -d '{"type":"claude"}'
curl -N --unix-socket $S http://localhost/v1/events    # SSE state stream
```

Queries (GET) return JSON DTOs; actions (POST/DELETE) return `{"ok":true}`,
`{"id":<n>}` (201), or `{"error":"..."}` (4xx/5xx).

### Compression

Send `Accept-Encoding: gzip` and any JSON reply over 1 KiB comes back
`content-encoding: gzip`, as does the `/v1/events` stream. Ask for nothing and
you get exactly the bytes every earlier daemon sent — this is negotiated, never
volunteered, so no existing client changes.

It is worth asking for. `/v1/system` is the largest thing served and the one a
live client reads most, and it compresses better than 6:1; the event stream,
which is ~98% `system`, measured **9× smaller** over a 20-second window. On a
Unix socket that saving is invisible. Over `ssh host butai proxy` or a forwarded
socket it is most of what the connection costs.

Three details worth knowing:

- **Only `application/json`.** `/v1/workspaces/{id}/download` serves arbitrary
  bytes — often a PNG or a tarball, which gzip would only make bigger — so it is
  never compressed.
- **Under 1 KiB is left alone**, because gzip's own header and trailer eat most
  of the saving on a short reply. The response still carries
  `vary: accept-encoding`.
- **`gzip;q=0` is honoured as the refusal it is**, and `*` is accepted as an
  offer.

The event stream is compressed as one long gzip stream with a flush after every
record, so records arrive exactly when they did before — a client decoding it
must use a *streaming* inflater (`zlib.decompressobj(31)`, `DecompressionStream`,
`flate2::read::GzDecoder`), not one that waits for a complete member. Browsers,
`curl --compressed` and Bun's `fetch` all do this already. The stream ends when
the connection does, so it has no gzip trailer; that is normal for a stream that
is aborted rather than completed, and no decoder in the list above minds.

| method | path | purpose |
|---|---|---|
| GET | `/v1/workspaces` | list workspaces + agent/process/change counts |
| GET | `/v1/workspaces/{id}` | full detail: agents, processes, changes |
| GET | `/v1/workspaces/{id}/agents` | agent rows (`state`: `waiting`/`working`/`finished`/`idle`/`exited`; `exited` also carries the code; `unread` = reached a your-move state and not looked at since, cleared by staging/input/`ack`) |
| GET | `/v1/workspaces/{id}/processes` | process rows (`status`: `ok`/`run`/`done`/`FAIL(n)`/`...`) |
| GET | `/v1/workspaces/{id}/changes` | git branch, staged/unstaged/**conflicted** files, recent commits, upstream + ahead/behind, and `state` (see [Git operations](#git-operations)) |
| GET | `/v1/workspaces/{id}/tree?path=&filter=` | one directory listing (lazy file tree); `path` relative to cwd, `""`=root. `filter` is `all` (default, and what this route has always answered) or `docs` — markdown, READMEs, and every directory but `target` and `node_modules`. **It decides the rows *and* their `changed` markers**, which is the whole reason it is a parameter and not a client's own filter: a directory's `changed` is "something under here changed", so a client that filtered the reply afterwards kept directories marked for files it had just dropped, and following one down landed on an empty listing. Any other value is a `400` rather than a silently unfiltered listing |
| GET | `/v1/workspaces/{id}/file?path=` | a file's text content (`truncated` when over the read cap) |
| GET | `/v1/workspaces/{id}/diff?path=&kind=` | unified diff for one file; `kind=staged` (index vs HEAD) or `unstaged` (worktree vs index, default). `path` is repo-root relative, as `/changes` reports it; **empty or omitted is the whole section**. An **untracked** file (a `?` row) is diffed against `/dev/null` on the unstaged side, so a brand new file answers with the same `new file mode` patch it will have once staged rather than with nothing — up to 200 of them in one reply, ignored paths excluded |
| GET | `/v1/workspaces/{id}/search?q=` | fuzzy filename matches then content matches, both capped → `SearchDto` |
| GET | `/v1/workspaces/{id}/show?id=` | whole-commit diff (`git show --first-parent`) for a revision `id` — first-parent so a **merge** answers with what it brought onto the branch instead of the empty patch a clean merge has against all its parents (returns the diff DTO, `path`=rev). `?rev=` is an accepted alias. Reflog forms are revisions too — `stash@{0}`, `main@{upstream}`, `HEAD@{2}` — which is how a stash list shows a diff. `:` is refused: `<rev>:<path>` reads a file out of a tree, and that is `/file`'s job, not this one's |
| GET | `/v1/workspaces/{id}/branches` | local branches + the current one → `{current, branches, entries}`. `branches` is the local names, current first, and is unchanged. **`entries`** adds remote-tracking branches and the detail a branch list wants: `[{name,remote,upstream,ahead,behind,tip}]`, locals first (same order as `branches`) then remotes, `origin/HEAD` omitted. `ahead`/`behind` are capped like `changes`; both `0` without an upstream. `tip` is the full oid — for its summary or date ask `git/log?rev=<name>&limit=1`, so the wire has one spelling of a date |
| GET | `/v1/workspaces/{id}/download?path=` | one file's raw bytes (`application/octet-stream` + `content-disposition`) |
| GET | `/v1/workspaces/{id}/panes/{pane}/output` | a pane's rendered output **as text**. `?lines=` (default 200), `?source=scrollback\|screen\|footer` (default `scrollback`), `?format=text\|ansi` (default `text`). A query: it does *not* resize the pane or clear its bell, unlike a framed `pane` attach |
| GET | `/v1/system` | cpu / ram / swap / gpu / network / disk / docker telemetry, plus the machine's static identity (`cpu_model`, `cpu_cores`, `cpu_threads`, each GPU's `name`). `net` is **every** interface with its `kind`, `carrier`, `default_route`, live rates and — for the kinds that are not double-counted — a history. `disks` is every mount with a capacity, largest first, with its `kind` and a `stale` flag for one that missed the daemon's deadline; it carries no history, because a filesystem does not move across the window. Filtering either list is the client's call, see [`SysDto`](building-a-client.md#5-data-model-json-schemas) |
| GET | `/v1/agents` | configured agent type names |
| GET | `/v1/usage` | every configured CLI's account standing → `{clis,sampled_ms}`. Machine-scoped, so it takes no workspace id — an account limit is not a property of a project. See [Account standing](#account-standing) for what the numbers are and are not |
| GET | `/v1/fs?path=` | browse *any* directory (the "open a project" picker); `path` defaults to `$HOME` |
| GET | `/v1/notifications?since=` | agent transitions after sequence `since` → `{head, items}` |
| GET | `/v1/events` | Server-Sent Events: `system`, `workspaces`, `workspace_detail`, `notification`, `git_op`, `remote_announce`. **Ignore tags you do not know** — more will be added. Gzipped when you send `Accept-Encoding: gzip` — ~9× smaller, see [Compression](#compression) |
| GET | `/v1/workspaces/{id}/git/log?limit=&skip=&rev=&path=&all=` | a page of history → `{commits:[{id,summary,author,date,parents,refs}], more}`. `path` narrows it to one file. **`parents`** is the commit's parent ids, first parent first — empty for a root, two or more for a merge — and **`refs`** is `[{name,kind}]` with `kind` one of `head`/`branch`/`remote`/`tag`, usually empty since only tips are decorated. **`all=1`** walks every branch, tag and remote-tracking branch (plus `HEAD`, so a detached checkout still appears) instead of only HEAD; it is **not** `git log --all` — `refs/stash` and `refs/notes` stay out, because a stash is two synthetic commits that are not history. `all` and `rev` together are a 400: they name different walks. The walk is always `--topo-order`, so a parent never precedes its child and lanes can be assigned in one pass |
| GET | `/v1/workspaces/{id}/git/stashes` | stash entries, newest first |
| GET | `/v1/workspaces/{id}/git/remotes` | configured remotes → `[{name,url}]`, one entry per remote |
| GET | `/v1/workspaces/{id}/git/tags` | tag names, newest first |
| GET | `/v1/workspaces/{id}/git/worktrees` | every checkout of this repository → `[{path,branch,head,is_main,detached,locked,prunable,workspace}]`. `workspace` is the id of the butai workspace already open on that path, or `null` — so a client can offer "go there" rather than "open it again" |
| GET | `/v1/workspaces/{id}/git/conflict?path=` | the three sides of one conflicted file → `{path,base,ours,theirs}`; an absent stage is `""` |
| GET | `/v1/workspaces/{id}/git/op` | the running (or last) git operation; 404 if none has run |
| DELETE | `/v1/workspaces/{id}/git/op` | kill the running operation |
| POST | `/v1/workspaces` | `{name?,layout?,path?}` create → `201 {"id"}`; an empty body is valid |
| POST | `/v1/fs/mkdir` | `{path?,name}` create a directory, returns the new listing |
| DELETE | `/v1/workspaces/{id}` | kill workspace |
| POST | `/v1/workspaces/{id}/agents` | `{type}` spawn an agent (`name`/`agent` accepted as aliases). `{background:true}` leaves the stage and focus alone, for a helper spawned by another agent |
| POST | `/v1/workspaces/{id}/processes` | `{name,command}` start a process. An omitted or empty `command` starts the workspace's default shell — what a `[+ term]` button asks for |
| POST | `/v1/workspaces/{id}/processes/{pane}/restart` | restart a process (**allocates a new pane id**) |
| DELETE | `/v1/workspaces/{id}/panes/{pane}` | kill any pane — agent, process, editor, tree |
| DELETE | `/v1/workspaces/{id}/processes/{pane}` | legacy alias of the above, kept for older clients |
| POST | `/v1/workspaces/{id}/panes/{pane}/input` | inject one `InputEvent` (e.g. `{"key":{"code":"enter"}}`) without attaching |
| POST | `/v1/workspaces/{id}/panes/{pane}/ack` | dismiss a pane's pending bell — without this a non-TUI client can never clear `waiting` |
| POST | `/v1/workspaces/{id}/checkout` | `{branch,create?}` switch branches |
| POST | `/v1/workspaces/{id}/upload?path=` | write the **raw request body** to `path` (not JSON) |
| DELETE | `/v1/workspaces/{id}/file?path=` | delete one file. **Destructive and unrecoverable** — no trash, no index copy, nothing to restore from — so confirm with the user first, more firmly than for `changes/discard`, which is bounded by what git already has. `path` is relative to the workspace cwd and takes `?path=` rather than a body, like the `download`/`upload` pair it sits between. **Files only:** a directory is a `400` rather than a recursive removal, so one confirmed keystroke cannot take out `src`. A symlink is removed as the link, not followed. A path that is already gone is a `404`, not a silent success — that case is a client working from a stale listing, and answering `ok` would have it report a deletion something else did |
| POST | `/v1/workspaces/{id}/changes/stage` | `{path}` stage a file |
| POST | `/v1/workspaces/{id}/changes/unstage` | `{path}` unstage a file |
| POST | `/v1/workspaces/{id}/changes/discard` | `{path}` discard a file's worktree changes — restores from the index, deletes if untracked. Destructive and unrecoverable: confirm with the user first. Unstaged files only (400 otherwise) |
| POST | `/v1/workspaces/{id}/changes/commit` | `{message}` commit the index |
| POST | `/v1/workspaces/{id}/changes/commit-all` | `{message}` stage every change, then commit the index (400 if there is nothing to commit, or if anything is conflicted) |
| POST | `/v1/workspaces/{id}/git/resolve` | `{path, take:"ours"\|"theirs"\|"resolved"}` settle one conflicted file. Synchronous |
| POST | `/v1/workspaces/{id}/git/branch` | `{name, from?}` create a branch without switching. Synchronous |
| DELETE | `/v1/workspaces/{id}/git/branch?name=&force=` | delete a branch; 400 if it is current, or unmerged without `force` |
| POST | `/v1/workspaces/{id}/git/branch/rename` | `{from?, to}` rename a branch (`from` defaults to the current one) |
| POST | `/v1/workspaces/{id}/git/apply` | `{patch, target:"index"\|"worktree", reverse?}` apply a unified diff — **partial staging**. Send back the hunks or lines you want out of what `GET .../diff` returned. `target:"index"` stages without touching the worktree; `reverse` inverts it, so unstage is `index`+`reverse` and discard-a-hunk is `worktree`+`reverse`. Synchronous (libgit2, no hooks, no refs); 400 if the patch does not apply |
| POST | `/v1/workspaces/{id}/git/worktree` | `{path, branch?, new_branch?}` add a checkout at `path` (**absolute**). `new_branch` creates `branch` rather than requiring it |
| DELETE | `/v1/workspaces/{id}/git/worktree?path=&force=` | remove a worktree; `force` is needed when it is dirty or locked |
| POST | `/v1/workspaces/{id}/git/worktree/prune` | forget worktrees whose directories are gone |
| POST | `/v1/workspaces/{id}/git/remote` | `{name, url}` configure a remote. **The only route that accepts a URL.** Allowed transports: `https`, `http`, `ssh`, `git`, `file`, `git+ssh`, an absolute path, and scp-style `user@host:path`. Everything else is a 400 — in particular any `<helper>::<rest>` form, which would make git run `git-remote-<helper>` |
| DELETE | `/v1/workspaces/{id}/git/remote?name=` | remove a remote. **Long broken, now fixed**: the daemon built `git remote remove -- <name>`, and `git remote remove` rejects the `--` that `git remote add` accepts (`usage: git remote remove <name>`, exit 129), so it answered `200` with `ok: false` and left the remote in place. A client that carries a workaround for that can drop it. Naming a remote that does not exist still answers `200` with `ok: false` and git's own `No such remote` — the operation ran and failed, so there is no 4xx to notice, which is why the "**check `ok`**" rule below is not pedantry |
| POST | `/v1/workspaces/{id}/git/fetch` | `{remote?, all?, prune?}` |
| POST | `/v1/workspaces/{id}/git/pull` | `{remote?, branch?, rebase?, ff_only?}` |
| POST | `/v1/workspaces/{id}/git/push` | `{remote?, branch?, set_upstream?, force_with_lease?}` |
| POST | `/v1/workspaces/{id}/git/stash` | `{message?, include_untracked?}` |
| POST | `/v1/workspaces/{id}/git/stash/apply` | `{index?, pop?}` restore a stash entry |
| DELETE | `/v1/workspaces/{id}/git/stash?index=` | drop a stash entry |
| POST | `/v1/workspaces/{id}/git/amend` | `{message?}` replace the last commit; without a message git keeps the old one |
| POST | `/v1/workspaces/{id}/git/reset` | `{rev?, mode:"soft"\|"mixed"\|"hard"}`. `hard` discards the worktree |
| POST | `/v1/workspaces/{id}/git/revert` | `{rev}` undo a commit with a new one |
| POST | `/v1/workspaces/{id}/git/cherry-pick` | `{rev}` copy a commit onto this branch |
| POST | `/v1/workspaces/{id}/git/merge` | `{branch, no_ff?}` |
| POST | `/v1/workspaces/{id}/git/rebase` | `{onto}`. Interactive rebase is refused — there is no editor to drive the todo list |
| POST | `/v1/workspaces/{id}/git/sequence` | `{action:"continue"\|"abort"\|"skip"}` drive whatever merge/rebase/cherry-pick/revert is in progress |
| POST | `/v1/workspaces/{id}/git/tag` | `{name, rev?, message?}`; a message makes it annotated |
| DELETE | `/v1/workspaces/{id}/git/tag?name=` | delete a tag |

### Account standing

`GET /v1/usage` answers *which of my agent accounts stops me first, and when
does it come back*. It is not a spend report: the numbers here are about
limits, and cost never appears.

```json
{ "clis": [ {
    "name": "claude", "command": "claude",
    "state": "metered",
    "version": "2.1.228 (Claude Code)",
    "account": "you@example.com", "plan": "max 5x",
    "windows": [
      { "label": "session", "used": 42, "of": 100,
        "unit": "percent", "resets_ms": 1786529265842 },
      { "label": "week · all models", "used": 56, "of": 100,
        "unit": "percent", "resets_ms": 1786888365842 },
      { "label": "week · opus", "used": 91, "of": 100,
        "unit": "percent", "resets_ms": 1786888365842 }
    ],
    "panes": [3, 9],
    "source": "published",
    "note": "published by claude, read 4m ago from ~/.claude.json"
  } ],
  "sampled_ms": 1786483248196 }
```

**`state` is the field to branch on**, and every one of its values is worth
drawing:

| state | meaning |
|---|---|
| `metered` | a ceiling is known, so `of` is set and the window is a proportion. `source` says whose ceiling — the provider's or the user's |
| `counted` | signed in, and the daemon can total each window — but **nothing published a ceiling**, so `of` is `null` |
| `unknown` | installed, probably signed in, and butai cannot read its usage at all |
| `no_account` | installed, and there is nothing to meter: it runs on your own API key |
| `absent` | configured in `[[agents]]`, and there is no binary where a pane would look for one. Still listed — "you have not installed this one" is an answer |

**No agent CLI reports its account limits through a subcommand**, and asking a
provider directly would mean authenticating as the user. But `claude` writes
the numbers its own `/usage` screen draws into `~/.claude.json`
(`cachedUsageUtilization`) and refreshes them while it runs, so for that CLI
the daemon serves the provider's real windows: a percentage, and the instant
each one resets. For everything else it reports only what it can stand behind —
whether the binary is there, its version, the account and plan where the CLI
records them in plain config, and what its own transcripts add up to over a
rolling window.

What each configured CLI can answer, and why:

| command | state | because |
|---|---|---|
| `claude` | `metered` | caches the provider's own limits in `~/.claude.json`. Falls back to `counted` from `~/.claude/projects/**.jsonl` before it has ever written them, or once that cache has outlived every window it names |
| `gemini` | `counted` | publishes no ceiling on disk, but every assistant turn in `~/.gemini/tmp/*/chats/*.json` carries its token counts |
| `agy` | `unknown` | **has** a quota and never writes it down — fetched per run into an in-memory cache — and its sessions record no per-turn cost, so there is nothing to total instead |
| `aider` | `no_account` | runs on your own API key; the provider bills you directly and there is no account limit to report |
| anything else | `unknown` | installed, and butai does not know where it keeps its numbers |

A CLI whose binary cannot be found is `absent` regardless of the above — and
"found" means what it means for a pane: `PATH` first, then `~/.local/bin`,
`~/.bun/bin`, `~/bin` and the newest nvm `bin`, exactly as
[configuration](configuration.md) describes for an `[[agents]]` `command`. The
daemon's own `PATH` is not a login shell's, so answering it alone would report
an npm- or cargo-installed CLI as missing on a machine where every pane
launches it.

`source` says where a window's numbers came from, and it is the field to read
before deciding how much to trust them:

| source | meaning |
|---|---|
| `published` | the provider's own limit, cached on disk by the CLI. The only windows here with a true reset instant |
| `transcripts` | summed from the CLI's transcripts on this machine. A total with no ceiling |
| `declared` | measured against a `[[budgets]]` number the user wrote |
| `none` | nothing was countable |

Four consequences a client must handle:

- **`of` may be `null`.** Draw the total, not a bar. Rendering `used/of`
  against a zero or invented denominator reports a limit nobody stated. `of` is
  set when the provider published a ceiling (`source: "published"`, and then
  `unit` is `percent` and `of` is `100`) or when the user declared a budget
  (`[[budgets]]`, see [configuration](configuration.md), and then `source` is
  `declared`).
- **`unit` travels with the number** (`tokens`, `requests`, `percent`) because
  the providers do not agree on one. Format from the unit rather than assuming
  a percentage — and do not render a percentage as `56 / 100`.
- **`resets_ms` and `sampled_ms` are absolute epoch millis**, not countdowns —
  the client runs the clock, exactly as it does for `AgentDto.started_ms`. A
  rolling window has no reset instant and leaves it `null`.
- **A published window is only as fresh as the CLI's last run**, and one CLI's
  `windows` may therefore mix sources. A cached limit is a snapshot, not a feed:
  `claude` refreshes it only when it runs and decides to fetch, so it goes stale
  without bound — twenty hours is ordinary. The daemon **drops** a window the
  snapshot no longer describes, rather than serving a stale or invented number,
  and counts the transcripts to fill the hole. A window is dropped when its
  `resets_ms` has passed, or when the snapshot is older than the window's own
  span (five hours for `session`, seven days for the weekly rows) — so the same
  file can be authoritative for `week · all models` and worthless for `session`
  at the same instant. What comes back is the surviving published windows
  followed by counted ones, and the two are told apart by `unit` and `of`:
  `percent`/`100` for published, `tokens`/`null` for counted. `source` stays
  `published` when any published window survived; the `note` says both halves.
  A client that groups or sorts windows must not assume they share a unit.

A stale snapshot, as it arrives — the week still the provider's, the five hours
counted here because the cache is older than the window it claimed to describe:

```json
{ "state": "metered", "source": "published",
  "windows": [
    { "label": "week · all models", "used": 63, "of": 100,
      "unit": "percent", "resets_ms": 1786910399901 },
    { "label": "last 5h", "used": 1292722, "of": null,
      "unit": "tokens", "resets_ms": null },
    { "label": "last 7d", "used": 42449248, "of": null,
      "unit": "tokens", "resets_ms": null }
  ],
  "note": "published by claude 20h ago — the windows that snapshot has outlived are counted from this machine's transcripts instead" }
```

`panes` is the panes on that account **right now**, across every workspace this
daemon serves, as ids rather than a count so a client can offer to jump to one.
It is stitched in when the request is answered rather than when the sample was
taken, so a pane that started a second ago is already in it. A remote machine's
agents are on that machine's own roster; assembling the fleet's view is the
client's job, since only the client knows which daemons it is holding.

**A credential store is never opened.** The account, the plan and the published
limits all come from plain config the CLI already wrote (`~/.claude.json`);
`.credentials.json` sits beside it and is deliberately never read.
Authenticating to a provider as the user is a decision they have not made, and
the daemon does not make it.

### Git operations

Anything that writes the repository beyond the index runs the real `git` binary,
because remotes, `push.default`, credential helpers, ssh-agent, hooks, signing
and sequencer state all live in the user's git config — and the daemon's libgit2
is built without any network transport at all. Index-only work (stage, unstage,
discard, commit, checkout, branch create/delete/rename, resolve, **apply**) uses
libgit2 and answers synchronously.

**Partial staging is `POST …/git/apply`.** There is no "stage this hunk" verb,
because a hunk is not a thing the daemon can name back to you — the client sends
a *patch*, built from the diff it already has, and says which copy of the file it
lands on. That keeps one route covering stage-a-hunk, unstage-a-hunk,
stage-selected-lines and discard-a-hunk, and it means a client that can render a
diff can already do partial staging without any new vocabulary. The daemon
recomputes nothing: if the patch does not apply, it is a 400.

**Worktrees map onto workspaces.** A worktree is a directory and a butai workspace
is a directory, so `GET …/git/worktrees` reports the workspace already open on
each one. Opening a second workspace on the same worktree would give one tree two
changes rails; clients should switch to the existing id instead.

**Every `POST /v1/workspaces/{id}/git/*` action answers one of:**

| status | meaning |
|---|---|
| `200` + `GitOpDto` | finished. **Check `ok`** — a rejected push is a successful call reporting a failed operation, not a 4xx |
| `202` + `GitOpDto` | still running. Poll `GET …/git/op` or watch the `git_op` SSE event |
| `400` | refused before anything ran — a bad ref name, a URL where a remote name belongs, a relative worktree path, a remote URL outside the allowed transports |
| `404` | the workspace is not a git repository |
| `409` | another operation holds this repository's write lock |

A client **must handle both `200` and `202`**: which one it gets depends only on
whether the operation beat a short grace window, so the same call can answer
either way on different days. `ok` and `summary` are where the outcome lives in
both cases — there is no status code left to carry it once an operation outlives
the request.

**One writer per repository, keyed by worktree root** rather than by workspace,
because two workspaces can be open on one worktree and interleaving their index
writes loses work. While an operation runs, index mutations answer `409`; reads
and status refreshes carry on.

**Never hangs.** Operations run with `GIT_TERMINAL_PROMPT=0` and no askpass, so
anything needing a credential fails quickly instead of waiting for an answer that
cannot come; an operation silent for 120s, or running for 600s, is killed.

**Exactly one route accepts a remote URL, and it is
`POST …/git/remote`.** Everywhere else a remote is *named* and resolved through
the repository's own config: `valid_remote` refuses any string containing `:`,
so `{"remote": "ext::sh -c whoami"}` on `fetch`, `pull` or `push` is a 400
before a command line is built.

The asymmetry is the point. `git fetch 'ext::sh -c …'` is remote code
execution, so the one place a URL can enter the daemon validates it against an
**allowlist of transports** (`git_op::valid_remote_url`) rather than a denylist
of bad strings — the set of installed `git-remote-*` helpers is a property of
the machine, not something this side can enumerate. Any `<helper>::<rest>` form
is refused outright, and the caller additionally passes
`-c protocol.ext.allow=never`. The route's own row above lists the transports
that get through.

Adding a remote was not exposed at all until worktrees and remote management
were asked for, which is why older copies of this document said no route took a
URL. One now does.

`ChangesDto.state` is `clean` | `merge` | `rebase` | `cherry_pick` | `revert` |
`bisect` | `unknown`. Treat `unknown` as "something is in progress that this
client does not model" — never as clean.

### What a pane knows about itself

The daemon sets these in every pane it spawns — agent, process and shell alike —
so a program running inside one can drive butai without being told where it is:

| variable | value |
|---|---|
| `BUTAI_PANE` | this pane's id |
| `BUTAI_WORKSPACE` | the workspace it belongs to |
| `BUTAI_SOCKET` | the socket this daemon is bound to |
| `BUTAI` | unchanged: the nesting marker a client checks to refuse to run inside itself |

There is deliberately no separate "am I inside butai" flag. `BUTAI_PANE` is always
set by the spawner and is strictly more informative than a boolean, so **its
absence is the test**. The CLI leans on this: `--ws` defaults to
`$BUTAI_WORKSPACE` and `--socket` to `$BUTAI_SOCKET`, so a command run inside a
pane acts on its own workspace, on its own daemon, without arguments.

`skills/butai/SKILL.md` is the agent-facing version of this, and `docs/agents.md`
the human one.

Every workspace-scoped `path` is joined against the workspace root and rejected
with `400 path escapes workspace` if it would escape — percent-decoding happens
first, so encoded traversal is covered too.

Unlike every other response, an SSE record is **internally** tagged. All six
tags, in full — this is the whole vocabulary, not a sample:

```
data: {"event": "system",            "data": { ...SysDto... }}
data: {"event": "workspaces",        "data": [ ...WorkspaceSummary... ]}
data: {"event": "workspace_detail",  "data": { ...WorkspaceDetail... }}
data: {"event": "notification",      "data": { ...NotificationDto... }}
data: {"event": "git_op",            "data": { ...GitOpDto... }}
data: {"event": "remote_announce",   "data": { ...RemoteAnnounceDto... }}
```

`workspaces` carries **counts**, which is enough to badge a tab and not enough
to draw a rail; `workspace_detail` is one workspace's full rail contents and is
what replaced the 1–2s poll of `GET /v1/workspaces/{id}`. Assigning a summary
where a detail belongs is the one mistake this pair invites — `agents` is a
number in the first and an array in the second — and it is worth a type in your
client rather than a comment.

### `remote_announce` — another machine said where it is

Typing `butai` after `ssh` announces the far machine back through the pane's own
terminal (see `butai/src/handoff.rs` for the handshake). The daemon is the only
party that can *see* this — it parses every byte a pane writes — so it does the
detecting, and emits this event.

It does not act on it. Connecting a second machine is a **client** decision:
whose tab bar those projects appear in is a property of the client, and a daemon
dialling another daemon to answer it is a relay. A client that wants the far
machine forwards its socket (`ssh -L`, using `ssh_target`/`ssh_args`, which the
daemon recovered from the pane's own `ssh` process) and then treats it as one
more local socket — exactly what `[[remote]] socket = …` already is.

A client that ignores this tag loses nothing but the convenience.

The DTO shapes are defined in `butai-protocol`'s `api` module.

The browser client in [`web`](../web/) is **hybrid**: it draws its own chrome
(workspace tabs, the AGENTS / PROCESSES / SYSTEM / CHANGES rails, and the files,
git, docker, home, settings and docs pages) from this **REST API** and its SSE
stream, and streams only the center "stage" — one live pane — over the
**framed** protocol using a `{"pane": …}` attach target. A stdlib bridge
(`web/server.py`) proxies REST over the socket and relays the framed pane frames
over a WebSocket, since browsers cannot open an AF_UNIX socket. The daemon gains
nothing web-specific — it still speaks only framed + HTTP on the one socket, so
SSH socket-forwarding reaches the web client unchanged. See
[`web/README.md`](../web/README.md) for the architecture in full.

### What is actually covered, and by what

Two independent things check this table, and neither of them checks all of it.
Say so plainly rather than let a reader assume the surface is guarded:

* **[`testsuite/`](../testsuite/README.md)** drives a real daemon and fails the
  run if a route it *lists* is never called. The list is
  `testsuite/suite/coverage.py`'s `HTTP_ROUTES`, and it is maintained by hand —
  today it names 60 of the 69 routes above. The nine it does not name are
  `GET …/search`, `POST …/git/apply`, `POST`/`DELETE …/git/remote`,
  `GET …/git/worktrees`, `POST`/`DELETE …/git/worktree`,
  `POST …/git/worktree/prune` and `DELETE …/file`. Eight of those nine are
  unguarded: they can break without a red line anywhere. `DELETE …/file` is
  the exception — it has `e2e_http.rs`'s own test, which is a different harness
  from this one, so it is absent here without being uncovered.
* **[`web/check.py`](../web/check.py)** counts from the other end. It parses the
  router's `match` for the denominator and `web/`'s own sources for the
  numerator, so neither half is a copy that can go stale, and it prints
  `route coverage: n/69` on every run. It reaches 61 — a *different* set: it has
  callers for all the git routes above and none for `search`, `git/apply`,
  `panes/{pane}/input`, `panes/{pane}/output`, the three per-workspace
  aggregates the push channel supersedes, or the legacy `DELETE …/processes/{pane}`.
  Two of its 60 are declared in the client's `api.js` and bound to no verb, so
  they are asked for by source and by nobody; `web/README.md` names them.

The union leaves `GET …/search` and `POST …/git/apply` reached by nothing at
all. `DELETE …/git/remote` has a client and, for most of its life, did not work
at all — see the note on its row.

## Versioning

`proto_version` is a single integer. Additive changes (new commands, new
optional fields) do not bump it; unknown JSON fields are ignored. Breaking
changes bump it, and the server rejects mismatched clients at hello.

**Unknown *messages* are ignored too, not just unknown fields.** A frame that
will not decode is logged and skipped, in both directions; the connection
survives. This is what makes the additive rule above true rather than merely
stated — a new command reaches daemons that have never heard of it, and they
must shrug rather than hang up. Sixteen undecodable frames in a row does end the
connection, on the grounds that the stream has stopped making sense rather than
merely being newer. A malformed *length prefix* is unrelated and always fatal:
the next frame boundary is then unknown.

This was not always so, and the failure is worth knowing because it is invisible
from the inside. `watch` was added in 0.6 as an additive change — correctly, by
the rule above. But an older daemon meeting a newer client's `watch` could not
decode it and **closed the connection**; the client re-dialled and sent another
at the next stage change. A one-release gap therefore presented as the stage
blanking repeatedly, with nothing anywhere naming a version.

Which is why the server's hello also carries **`server_version`**, its own build
string:

```json
{"hello": {"proto_version": 1, "session": null, "server_version": "0.12.1"}}
```

It is optional and omitted when unset, so it costs nothing on the wire and older
clients ignore it. `proto_version` cannot do this job — by the rule above it
stays put across additive changes, so a daemon and a client many releases apart
both report `1` and the handshake sees nothing wrong. **Its absence is itself
informative**: a daemon that does not send it predates the field, and so is older
than any client able to look. Clients are encouraged to say so plainly; the TUI
puts "daemon is 0.8.0, client is 0.9.0 — restart it" in the footer, because the
alternative is the user hunting for five bugs that are all one stale process.

