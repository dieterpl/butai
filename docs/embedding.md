# Embedding butai

butai's daemon is the product; the bundled TUI is one client of it. This page is
about the case where **your** product is the client — where you ship the daemon
underneath an application of your own and keep your own UI, your own domain API,
and your own users. The daemon becomes the part that owns terminals, processes,
git and projects on disk, and you never write a VT emulator, a process
supervisor, or a git plumbing layer.

Two neighbouring documents own the questions this one does not:

| | |
| --- | --- |
| [building-a-client.md](building-a-client.md) | How to *write* a client against the API: every endpoint, the JSON data model, live updates, connection code, a UI storyboard. |
| [protocol.md](protocol.md) | The normative spec — framing, handshake, message types, the full `/v1` table. The file that must be updated when the wire changes. |

Read one of those for *what to call*. Read this one for **how to run the thing
you are calling**: deployment, supervision, relaying, isolation, and what you
may depend on across versions.

---

## What you get, and where it stops

### PTY-backed panes, rendered server-side

The daemon runs the terminals and renders each connection's viewport into a grid
of styled cells, streaming damage diffs. A client paints cell runs; it does not
parse escape sequences. That is why `web/`, the native clients and the bundled
TUI all exist without a VT parser, and it is the single largest thing you would
otherwise have to build. See [Frames](protocol.md#frames--how-pane-content-reaches-you).

The corollary is the rule the whole API turns on: **the daemon renders a screen
only when a program's bytes are on the other end of a PTY.** Everything else —
rails, git state, gauges, file trees — crosses as JSON and you draw it.

### Process supervision

`POST /v1/workspaces/{id}/processes` starts a named command in a workspace and
keeps it as a row with a status (`ok`, `run`, `done`, `FAIL(<code>)`, `...`).
`.../restart` restarts it. A workspace's `.butai.toml` declares processes to
bring up when the workspace opens, each with an optional `ready` substring that
flips the row to `ok` when it appears in the output — so "start the watcher when
this project is opened" needs no setup script on your side.

Honest boundaries: there is **no automatic restart policy**. A process that
exits stays on the list showing why; restarting it is an explicit call, and
`restart` allocates a **new pane id**. There is no dependency ordering and no
health check beyond the `ready` substring.

### The git surface

Branch, staged/unstaged/conflicted files with diffstat, diffs, whole-commit
patches, the commit graph, stashes, remotes, tags, worktrees, and the write
operations behind them. Index-only work (stage, unstage, discard, commit,
checkout, branch create/delete/rename, resolve, partial `apply`) is libgit2 and
answers synchronously; anything touching remotes or the sequencer shells out to
the real `git` binary so the user's credential helpers, hooks and config apply.
See [Git operations](protocol.md#git-operations) — in particular that a
`POST …/git/*` answers `200` *or* `202` for the same call on different days, and
that a failed operation is a `200` with `ok: false`.

### Workspaces on disk

A workspace is a directory. `POST /v1/workspaces {"path": "/abs/dir"}` opens one
(400 if the path is not a directory; omitting `path` uses the daemon process's
own cwd). The daemon reads `.butai.toml` from that directory, notices whether it
is a git repository, and persists the open set so a restart comes back to it.

### Where it stops

- **No authentication, no authorization, no multi-user.** Whoever can open the
  socket is the daemon's user. See [Security](#security).
- **No TCP listener.** One `AF_UNIX` socket, always. Reaching it from anywhere
  else is your relay's job or ssh's.
- **No CORS headers, no static assets, no cookies.** The daemon serves `/v1/*`
  and nothing else; an unmatched route is a JSON 404. A browser cannot talk to
  it directly, and every header a browser needs is yours to add.
- **No chrome.** No palette, no keymap, no layout — a client's settings are the
  client's, which is why there is no route to store them.
- **Creates do not return ids.** `POST …/agents` and `POST …/processes` answer
  `{"ok":true}`; the new pane id comes from re-reading `GET /v1/workspaces/{id}`
  (or from the pushed `workspace_detail`). Only `POST /v1/workspaces` answers
  `201 {"id"}`.
- **Unix only.** Unix sockets, `termios`, POSIX signals, `setsid`.

---

## Running the daemon headlessly

```sh
butai daemon
```

Foreground, no terminal required. It blocks until a termination signal arrives —
or, by default, until it has had a workspace and has none left. It logs to
`~/.butai/logs/daemon.log` (daily rotation), not to stdout.

### The socket, and choosing its path

`$BUTAI_SOCKET` if set, else `~/.butai/butai.sock`; with no home directory at
all, `/tmp/butai-<uid>/butai.sock`. Three constraints on a path you pick:

- The daemon **`chmod 700`s the socket's parent directory** when it binds, so
  that directory must be one you own. Bare `/tmp` is not — make a subdirectory.
- Keep it short. `AF_UNIX` paths have a hard kernel length limit of roughly a
  hundred bytes, and a deep per-tenant path exceeds it long before it looks
  unreasonable.
- It is the daemon's identity. `~/.butai/butai.lock` (the socket path with
  `.lock` substituted) holds an exclusive `flock` for the daemon's lifetime, so a
  second `butai daemon` on the same socket fails with *another butai daemon is
  already running* rather than racing. A stale socket file left by a crash is
  removed by whoever wins the lock.

### What it writes

Everything outside a project lives in one directory, `~/.butai`:

| path | what |
|---|---|
| `config.toml` | daemon settings (`[general]`, `[[agents]]`) and the client's half, ignored here |
| `logs/` | daemon logs, rotated daily |
| `session.json` | the open workspaces, restored on restart — `$BUTAI_SESSION_FILE` overrides |
| `panes/` | per-pane output dumps, replayed into fresh panes on restart; sits beside `session.json` |
| `scratch/` | files a client pasted in (`put_file`), per workspace, most recent 32 |
| `themes/` | user themes — the TUI's, not the daemon's |
| `butai.sock`, `butai.lock` | the socket and the spawn-race lock |

The directory is home-relative rather than `$XDG_RUNTIME_DIR`-relative on
purpose: that variable is set for a login shell but routinely absent from a
non-interactive `ssh host butai …`, so a path derived from it moves between the
two. **`$HOME` is therefore the isolation knob**, and `$BUTAI_SESSION_FILE`
(which also relocates `panes/`) is the finer one.

### The one setting an embedded daemon must change

```toml
# ~/.butai/config.toml
[general]
exit_when_empty = false
```

`exit_when_empty` defaults to **true**: a daemon that has had a workspace and
then loses its last one shuts itself down. That is right for a personal
multiplexer and wrong for a service your product dials — closing the last
project would take the engine with it. Turn it off for any daemon whose lifetime
is meant to be the container's.

Other `[general]` keys worth knowing: `default_shell` (else `$SHELL`, else
`/bin/sh`), `scrollback` (lines kept per pane, default 5000), `restore_bytes`
(raw PTY bytes kept per pane for restart replay, default 256 KiB; `0` disables
capture entirely). `[[agents]]` defines the agent types `GET /v1/agents` reports
and `POST …/agents` can spawn — name, command, args, resume args, env, and the
regexes that decide "blocked on you" and "still working".

### What the daemon needs from the machine

It shells out to **`git`** (everything beyond the index) and **`grep`**
(workspace search), and it needs a shell to spawn panes with. `docker` and
`nvidia-smi` are used for telemetry when present and simply absent from `SysDto`
when not. `SysDto.disks` comes from `/proc/self/mounts` on Linux and
`getfsstat(MNT_NOWAIT)` on macOS, plus a `statvfs` per mount either way, so in a
container it describes **the container's** mount namespace: `/` is the overlay,
the host's disks are not there unless you bind-mounted them, and a volume shows
up under the path it is mounted at. Each entry says which it is (`kind`), so a
client can draw the bind mount and skip the layer. Any other platform publishes
an empty list — the enumerator is the only per-platform part, so the rest of the
contract holds wherever there is one.

`testsuite/Dockerfile` is a worked runtime image and installs
`ca-certificates coreutils git grep procps ncurses-bin` plus the shell, and sets
`HOME`, `SHELL` and `TERM`.

### In a container

Two containers, and they are different jobs:

**The daemon's.** Copy in a static `musl` build (they run on Alpine, distroless
and scratch), install `git` and `grep`, run as a non-root user with a real
`$HOME`, and expose the socket by mounting the *directory* that holds it.
One gotcha is pinned by the test suite and worth building around: libgit2
refuses to open a repository owned by a different uid than the daemon runs as,
and butai swallows that error — so the symptom is not a message, it is
`GET …/changes` answering 404 and a project that silently has no git state. The
fix belongs in your image:

```dockerfile
RUN git config --global --add safe.directory '*'
```

**The relay's.** `web/Dockerfile` is the reference and is deliberately trivial —
`python:3.12-slim`, the source, `ENV BUTAI_SOCKET=/run/butai/butai.sock`, no
packages at all. `web/docker-compose.yml` shows the mount:

```yaml
volumes:
  - "${BUTAI_SOCKET_DIR:-$HOME/.butai}:/run/butai"
```

**Mount the directory, not the socket file.** A daemon restart recreates the
socket inode, and a container bound to the file itself stays pinned to the dead
one. Read-write, because spawning, staging and committing all POST through it —
this is the `/var/run/docker.sock` pattern, with the same consequences.

### Supervising it

The daemon is an ordinary foreground process: run it under systemd, s6, supervisord,
or as a container's PID 1. Three properties make that easy.

- **It exits cleanly on SIGTERM and SIGINT.** Both drain through the same
  shutdown path as `kill-server`, so a container stop is a clean stop.
- **Restarting is cheap and lossy in a bounded way.** `session.json` brings the
  workspaces back, `panes/` replays each pane's last `restore_bytes` of output
  into a fresh child, and an agent whose `[[agents]]` entry has `resume_args`
  is asked to reopen its conversation. Nothing survives but bytes and intent:
  the children are new processes.
- **A second instance cannot start.** The `flock` makes "restart always" safe
  even when the old process is still shutting down.

Point your supervisor at the socket for readiness: if the file exists and
accepts a connection, the daemon is up. A one-line probe is
`curl --unix-socket $S http://localhost/v1/workspaces`.

### Shutting it down

```sh
butai --socket /run/butai/butai.sock kill-server           # workspaces come back next start
butai --socket /run/butai/butai.sock kill-server --clear   # forget them; next start is empty
```

or send SIGTERM. Both are graceful. **Never kill a daemon by process pattern** —
on a machine with more than one, `pkill -f "butai daemon"` reaches all of them.
The socket is the only precise handle.

---

## The two integration surfaces

| You want to… | Use |
|---|---|
| List, inspect, create, kill, stage, commit, browse, search | REST `/v1/*` |
| Follow state without polling | SSE `GET /v1/events` |
| Show a live terminal, or type into one | The framed protocol, `{"pane":{"pane":N}}` |
| Inject one keystroke without a live view | REST `POST …/panes/{pane}/input` |
| Read a pane's text without a live view | REST `GET …/panes/{pane}/output` |

Build on REST first and add a pane later. The framed path exists for exactly one
reason: a terminal's screen is the accumulated effect of every byte a program
wrote, and only a VT emulator holding that state can produce it. That state is
the daemon's; shipping it as cells is what lets your UI show a live agent without
implementing a terminal. Nothing else needs it, because everything else *is*
JSON already.

The two share one socket. A connection is HTTP when its first byte is an ASCII
method letter and framed when it is `0x00` — the top byte of a length prefix.
You do not choose a port or a mode; you just connect and start talking.

Note the asymmetry that catches people: **only a `pane` target streams frames.**
`"default"`, `"control"`, `{"attach":…}` and `{"new":…}` scope a connection to a
workspace and send no frames at all, because the daemon draws no workbench.

---

## Relaying: putting the daemon behind your own server

Your product almost certainly terminates HTTP itself — for auth, for its own
routes, for a browser that cannot open a Unix socket at all. `web/server/` is
the reference relay: ~1,500 lines of TypeScript on Bun, **with no dependencies
at all**, and it is the thing to read before writing your own.

That it has none is worth a sentence, because it is the shape to copy rather
than the runtime. Bun's `fetch` speaks to a Unix socket and `Bun.serve` speaks
WebSocket; between them they are the two things a relay actually needs, and a
relay is not a place that needs a framework. The version before this one made
the same argument in Python's standard library and came to 1,503 lines. Neither
parses a daemon payload.

| file | role |
| --- | --- |
| `index.ts` | the server: routing, the roster's two writes, and the fall-through order |
| `roster.ts` | which daemons this relay speaks for; key derivation and the socket allowlist |
| `routing.ts` | the qualified-id rule, and every refusal that keeps one machine's id off another |
| `proxy.ts` | one round trip, over a Unix socket |
| `snapshot.ts` | `/api/state`: the union across every daemon |
| `events.ts` | `/v1/events` → SSE |
| `ws.ts` | WebSocket ↔ the daemon's 4-byte length prefix |
| `static.ts` | the built client, with its traversal rules |

### What it forwards verbatim

`/api/<path>` → `/v1/<path>` for GET, POST and DELETE. The reply's **bytes** go
back untouched, along with the daemon's `content-type` and any
`content-disposition` — which is what makes file download work through the relay
without special-casing it. Nothing about the payload is parsed. That is the
contract worth copying: a relay that understands the daemon's DTOs is a relay
that needs editing every time one grows a field.

**Content-encoding is the one header you must not forward.** Bun's `fetch` sends
`Accept-Encoding: gzip, deflate, br, zstd` on its own, so the daemon compresses
the hop the relay reads — which is exactly the hop that goes over ssh when the
socket is forwarded, and where `/v1/system` shrinks better than 6:1 — and then
`fetch` inflates it before the relay ever sees a byte. The bytes in hand are
therefore *decoded*, and passing the daemon's `content-encoding: gzip` along with
them tells the browser to inflate something already inflated. The relay forwards
`content-type` and `content-disposition` and nothing else, which is why this
costs it no code; write your own header pass-through as an allowlist and you
inherit the same safety. Do the same for `/v1/events`, whose gzip stream your
HTTP client is also likely to be decoding for you.

### What it rewrites

Only what it must. The relay speaks to several daemons at once, so it rewrites
**ids**: every id crossing to the browser is written `<daemon-key>:<n>`, and the
path segment is rewritten back to the bare integer the daemon understands
(`/api/workspaces/gpu:1/panes/gpu:5/ack` → `gpu`'s
`/v1/workspaces/1/panes/5/ack`). Four refusals fall out — an unknown key, two
different daemons named in one path, a bare id where several daemons are
configured, and a stream with no `?daemon=`. Each is a wrong-machine bug that
would otherwise be silent. If you relay exactly one daemon you need none of
this; if you ever relay two, take the scheme rather than inventing one.

It also **adds** two routes that are not the daemon's: `/api/daemons` (the
roster, contacting nobody) and `/api/state` (a fanned-out snapshot across every
daemon, used as the baseline before deltas and as a poll fallback).
`/api/daemons` takes writes too — `POST` to add a machine, `DELETE /api/daemons/{key}`
to drop one — so a socket forwarded after the relay started can join without a
restart. A `POST` **dials the socket before it joins the roster**, and that
ordering is the feature: an entry that has never answered draws exactly like a
machine that was fine and has just gone down, and the difference matters most
while somebody is still typing the path.

### The WebSocket bridge

One browser text frame is one `ClientMsg` JSON, length-prefixed onto the daemon
socket. One length-prefixed daemon frame is one browser text frame. That is the
whole mapping — the bridge translates framing and understands neither side's
semantics. The handshake, the input encoding and the application of `frame`
diffs all live in the browser.

One consequence is worth stating because it looks like a design flaw until you
see it: the bridge is told **which daemon on the URL** (`/ws?daemon=<key>`),
never by reading the pane id out of the attach message. Reading the payload would
mean parsing a `ClientMsg`, and the relay's whole contract is that it does not.

### Traps

- **A streaming response cannot be read to EOF.** The relay's ordinary
  request helper reads until the socket closes, which is right for a reply that
  ends and structurally wrong for `GET /v1/events`, which never does. The relay
  keeps a *second* path for the stream, which reads only as far as the blank
  line after the headers and hands back the unread socket. Getting this wrong is
  why the push channel was unreachable from a browser for a long time.
- **The event stream is chunked.** hyper streams `/v1/events` with no
  content-length, so it goes out `Transfer-Encoding: chunked` — and a chunk
  boundary is not an event boundary. Decode chunking, then forward the daemon's
  `data: {...}\n\n` bytes untouched. An HTTP client that can dial a Unix socket
  does this for you and the trap disappears; a hand-rolled socket read is where
  it bites. Choosing a runtime whose `fetch` takes a socket path is worth about
  150 lines of relay on its own.
- **Subscribe before you snapshot.** The daemon's stream carries no history: a
  subscriber sees only what happens after it subscribes. Open the subscription
  first and build the baseline second, so anything that changes in between
  arrives as a harmless repeat instead of a silent loss.
- **Request bodies are bytes, not JSON.** `POST …/upload` writes the raw request
  body to `?path=`. A relay that decodes and re-encodes bodies corrupts every
  binary upload.
- **`Connection: close` per request.** Both surfaces on the socket are served
  per-connection; the reference relay opens a socket per request and never
  reuses one. That is not a performance disaster on a Unix socket, and it removes
  keep-alive parsing from your relay entirely.
- **Route on the path alone.** A query string must not turn a known route into a
  404, or drop an aggregate through to the proxy where it becomes a `/v1` path
  the daemon does not serve.
- **A dead socket is a fact, not an exception.** Report it as a body
  (`{"error": …, "daemon": …, "socket": …}` with a 502) rather than letting the
  handler die with no reply — an empty response reads as an unexplained network
  error on the client, at exactly the moment your snapshot is already reporting
  the reason.
- **Every browser-facing header is yours.** Add CORS, cache policy, compression
  and auth in the relay. The daemon sets `content-type`, `content-disposition`
  and the SSE `cache-control`, and nothing else.

---

## `web/` as an embedded client

`web/` is the one most often embedded, because it is a browser client and a
bridge in one container: a React client built by Vite, and a Bun server that
translates the daemon's socket into `/api/*`, `/ws` and Server-Sent Events. The
daemon socket is bind-mounted in — the same pattern as mounting
`/var/run/docker.sock`.

What an embedder depends on there:

| | |
|---|---|
| `GET /` | The client. Stable. A single-page app, so anything under it that is not a file is `index.html` and the client routes it. |
| `GET /ws?daemon=<key>` | One pane's framed stream, relayed. |
| `GET /api/state`, `GET /api/daemons`, `GET /api/events` | The whole-world snapshot, the roster, the push stream. |
| `POST /api/daemons`, `DELETE /api/daemons/{key}` | Add or drop a machine at runtime, bounded by `BUTAI_SOCKET_DIRS`. |
| `GET/POST/DELETE /api/*` | Proxied to `/v1/*` on the daemon the qualified ids name. |

**The asset file names are not part of that contract.** They are content-hashed
by the bundler and change on every build. `GET /` is what you link to, and the
bundle's `base` is relative so the client also works mounted under a prefix of
your own.

The one piece worth copying rather than depending on is the pane renderer,
`web/src/stage/`: `Screen.ts` applies `frame` damage to a DPI-aware `<canvas>`
and turns keyboard, mouse, paste, wheel and resize back into protocol messages,
and `Stage.tsx` owns the socket and the canvas around it. It is a plain class
with a React wrapper rather than a custom element, so it ports to any view layer.
Downstream clients have copied this pattern; it is what holds the boundary that
the daemon renders a screen and the client draws everything else.

**Frames never reach React.** A PTY at full rate is thousands of updates a
second, and each one goes straight to the canvas. The only thing in the stage
that moves component state is the status pill. Whatever framework you wrap this
in, keep that split — routing frames through a reactive layer is the one
performance mistake this design exists to avoid.

## The image runs with no internet

A container that needs registry access at runtime is a regression, and an
embedder shipping it into an air-gapped environment is the reason. It is a
guarantee rather than an intention, and it now costs nothing to keep: **a
bundler resolves imports for a living**, so `bun install --frozen-lockfile` and
`bun run build` produce a `dist/` that references nothing outside the image.

That used to be three build stages. The previous client imported React, htm and
Radix from esm.sh through an import map, which a browser resolves at *load*
time — so one stage walked the esm.sh module graph into `vendor/`, a second
rewrote every import map to point at those files, and a probe checked that no
bare specifier had been missed. `web/tools/vendor.py`, `web/vendor.txt` and the
offline check are all **gone**, and so is `GET /vendor/*`.

Two things an embedder can rely on:

- **`--frozen-lockfile` makes the lockfile a fact rather than a hint.** An image
  whose dependencies drift from the repo's is not reproducible; CI asserts the
  same thing on every run.
- **The bridge ships with no dependencies of its own.** Bun executes TypeScript
  directly, so `server/` runs from source: no second bundle to keep in step with
  the first, and no `node_modules` in the final stage.

`web/README.md` is the reference for the build itself.

## Theming, for an embedder

A theme is a property of the client, not of the daemon — the daemon has no
palette. The browser client's palettes live in `web/src/logic/settings.ts`, the
choice lives in the browser's `localStorage`, and applying one is CSS custom
properties written onto `<html>` and nothing else — `web/src/theme.ts` is the
only code that writes them. There are no colour literals anywhere under
`web/src/`. [theming.md](theming.md) has the roles and the loading path.

---

## One daemon, or many

The socket path is the whole boundary. Two daemons on two sockets share nothing
except the machine.

**One daemon per user** is the shape butai is built for. All of that user's
projects are workspaces in one daemon, the tab bar spans them, `session.json`
remembers them, and pane ids are unique daemon-wide.

**One daemon per project** is the shape an embedded product usually wants, and it
works, but a socket path alone does not isolate anything. `~/.butai` is resolved
from `$HOME`, so two daemons started by the same user share one `config.toml`,
one `session.json`, one `panes/` directory and one log file — and the session
store will happily restore *the other one's* workspaces. Give each daemon its own
`$HOME`, or at minimum its own `$BUTAI_SESSION_FILE` (which relocates `panes/`
with it).

**Isolation between tenants is a uid or a container, never a socket path.** The
daemon runs every pane as its own user with that user's full filesystem access;
two daemons under one uid are one security domain with two front doors.

What the daemon assumes about the filesystem it runs on:

- A POSIX filesystem with working PTYs. A container needs `/dev/pts` available.
- `$HOME` exists and is writable — everything in the table above is created
  under it. Without a resolvable home it falls back to `/tmp/butai-<uid>`, which
  a reboot wipes.
- The socket's parent directory is one it may `chmod 700`.
- A workspace's directory exists at open time; the path is canonicalized and a
  non-directory is a 400.
- Repositories are readable **and owned by the daemon's uid**, or `safe.directory`
  says otherwise.
- Every workspace-scoped `path` is joined against the workspace root and refused
  with `400 path escapes workspace` if it would escape — percent-decoding
  happens first. `GET /v1/fs` is *not* workspace-scoped and browses anywhere the
  daemon's user can read.

---

## Security

**The daemon trusts anything that can open its socket.** There is no token, no
user, no role. Authentication is the `0700` on the socket's parent directory,
and it answers exactly one question: are you the uid that started it.

Spell out what a connection therefore grants, because "it is only a socket"
undersells it:

- **Arbitrary process execution.** `POST /v1/workspaces/{id}/processes` runs a
  command of the caller's choosing. So does spawning an agent — and the built-in
  agent launchers pass each CLI's auto-approve flag, by design, because agents
  run unattended in rail panes.
- **Typing into anything already running.** `POST …/panes/{pane}/input` injects
  keystrokes into a live program without attaching, and the framed pane path
  carries a full keyboard.
- **Filesystem read and write.** `GET /v1/fs` browses any directory the user can
  read; `download`, `file` and `tree` read within a workspace; `upload` writes
  within one; `put_file` writes into `~/.butai/scratch/`.
- **Git writes.** Commit, reset `--hard`, discard, branch delete, worktree
  remove, push. `POST …/git/remote` is the only route that accepts a URL, and it
  is validated against an allowlist of transports rather than a denylist, because
  `git fetch 'ext::sh -c …'` is remote code execution. Everywhere else a remote
  is *named*, and any name containing `:` is a 400.

The deployment rules that follow are short and non-negotiable:

1. **Never expose the socket, or anything that relays it, to an untrusted
   network.** The reference bridge binds `0.0.0.0` with no authentication of any
   kind; publishing that port publishes a shell. That is stated plainly in
   [`web/README.md`](../web/README.md#security) and it is not a shortfall of the
   bridge, it is the daemon's model showing through.
2. **Authenticate in your relay, before the proxy.** It is the only layer that
   can. If your product has sessions, the relay is where a request becomes a
   permitted one.
3. **One security domain per uid.** Run the daemon as a dedicated unprivileged
   user in its own container, with only the projects it should see mounted.
4. **Treat the daemon's user as the blast radius.** Anything that user can read,
   a client can read; anything it can run, a client can run.
5. **Remote access rides ssh, not TCP.** `ssh -N -L /tmp/remote.sock:…` turns a
   far daemon into a local socket path, which is exactly the unit a relay takes.
   ssh keys are the authentication that the socket does not have.

---

## Compatibility

**`proto_version` is a single integer and it is `1`.** Additive changes — new
commands, new routes, new optional fields, new SSE tags — do **not** bump it.
Breaking changes do, and the daemon rejects a mismatched client at hello with an
`error` followed by a `detached`.

What that means for your code, in both directions:

- **Ignore unknown JSON fields.** Decode leniently; fields get added.
- **Ignore unknown SSE tags.** The set grows. `workspace_detail` was added this
  way, without a version bump.
- **Unknown *messages* are ignored too**, not just unknown fields. An
  undecodable frame is logged and skipped in both directions and the connection
  survives — which is what makes the additive rule true rather than merely
  stated. Sixteen undecodable frames in a row does end the connection. A
  malformed *length prefix* is always fatal, because the next frame boundary is
  then unknown.

Which surfaces you may build on:

| surface | stability |
|---|---|
| `/v1/*` paths and DTO field names | stable; grows additively |
| The framed protocol at `proto_version: 1` | stable; grows additively |
| Status-code semantics (200/201/202/400/404/409/500) | stable — including `200` + `ok:false` for a git operation that ran and failed |
| Ids | per-daemon integers, **not** stable across restarts. `restart` allocates a new pane id |
| `~/.butai` layout, log format, `session.json` | internal. Read it and you are coupled to a version |

### Detecting the version at runtime

`server_version` is the daemon's own build string, and it rides the **framed
hello** — there is no REST route that reports it. The cheapest probe is a
`"control"` connection, which creates no session and streams nothing:

```jsonc
// →
{"hello":{"proto_version":1,"encoding":"json","cols":80,"rows":24,
          "target":"control","cwd":"/"}}
// ←
{"hello":{"proto_version":1,"session":null,"server_version":"0.12.1"}}
```

It is optional and omitted when unset, **and its absence is itself
informative**: a daemon that does not send it predates the field, and so is older
than any client able to look. `proto_version` cannot do this job — by the rule
above it stays at `1` across every additive change, so a daemon and a client many
releases apart both report `1` and the handshake sees nothing wrong.

### What to do on skew

Say so, in words, where the user will see it. The TUI puts *daemon is 0.8.0,
client is 0.9.0 — restart it* in its footer, and that message exists because of a
real incident: `watch` was added additively and correctly, but a daemon one
release older could not decode it and **closed the connection**. The client
re-dialled and sent another at the next pane change, so a one-release gap
presented as the view blanking repeatedly, with nothing anywhere naming a
version. Every symptom pointed at a feature; none pointed at the stale process.

The practical advice for an embedder: **ship the daemon binary with your
application and restart it on upgrade.** A long-lived daemon left running across
a deploy is the single most likely source of bugs that do not exist.

---

## A minimal embed, end to end

Start an isolated daemon on a short, owned path:

```sh
mkdir -p /tmp/embed && chmod 700 /tmp/embed
HOME=/tmp/embed BUTAI_SOCKET=/tmp/embed/butai.sock butai daemon &
S=/tmp/embed/butai.sock
```

Open a project as a workspace, and note its id:

```sh
curl -s --unix-socket $S -X POST http://localhost/v1/workspaces \
     -H 'content-type: application/json' \
     -d '{"name":"demo","path":"/srv/projects/demo"}'
# {"id":1}
```

Start a supervised process in it:

```sh
curl -s --unix-socket $S -X POST http://localhost/v1/workspaces/1/processes \
     -H 'content-type: application/json' \
     -d '{"name":"web","command":"python3 -m http.server 9000"}'
# {"ok":true}
```

Read the state back. This is also how you learn the new pane id, since the create
above answered `ok` rather than an id:

```sh
curl -s --unix-socket $S http://localhost/v1/workspaces/1
# {"id":1,"name":"demo","cwd":"/srv/projects/demo",
#  "agents":[],"processes":[{"pane":2,"name":"web","command":"python3 -m http.server 9000",
#                            "status":"run","exited":null}, ...],
#  "changes":{...},"stage":1}
```

Follow it without polling — one long-lived request, one `data:` record per change:

```sh
curl -N -s --unix-socket $S http://localhost/v1/events
```

And stream that pane. This is the only part that needs the framed protocol:

```python
import json, socket, struct

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect("/tmp/embed/butai.sock")

def send(msg):
    payload = json.dumps(msg).encode()
    sock.sendall(struct.pack(">I", len(payload)) + payload)

def recv():
    head = b""
    while len(head) < 4:
        head += sock.recv(4 - len(head))
    (n,) = struct.unpack(">I", head)
    body = b""
    while len(body) < n:
        body += sock.recv(n - len(body))
    return json.loads(body)

send({"hello": {"proto_version": 1, "encoding": "json", "cols": 100, "rows": 30,
                "target": {"pane": {"pane": 2}}, "cwd": "/"}})
print(recv())                       # the server hello, carrying server_version

while True:
    msg = recv()
    if "frame" in msg:
        # frame["full"] means clear first; frame["cells"] are runs of styled
        # cells at (x, y); frame["cursor"] is [x, y] or null — draw it.
        # Advance by each grapheme's display width, not one column per cell.
        print(msg["frame"]["full"], len(msg["frame"]["cells"]))
```

Send `{"input":{"key":{"code":{"char":"x"},"mods":{}}}}` to type into it,
`{"resize":{"cols":C,"rows":R}}` when your view changes size, and
`{"watch":{"pane":N}}` to re-point the same connection at a different pane rather
than dialling a new one. `{"detach":null}` closes it politely.

[`examples/api-client.py`](../examples/api-client.py) is the complete version of
this in ~100 lines, including a correct cell-grid reader.

Then tear it down:

```sh
butai --socket $S kill-server
```

---

## Where this lives

| section | source |
|---|---|
| Capabilities, DTO shapes | `crates/butai-protocol/src/api.rs`, [`protocol.md`](protocol.md) |
| Frames and why they exist | `crates/butai-server/src/render.rs`, [`protocol.md`](protocol.md#frames--how-pane-content-reaches-you) |
| Process supervision, `.butai.toml` | `crates/butai-server/src/config.rs` (`WorkspaceFile`, `ProcDef`), `crates/butai-server/src/core.rs` |
| The git surface | `crates/butai-server/src/git_op.rs`, `crates/butai-server/src/pane/git.rs` |
| `butai daemon`, signals, the flock | `crates/butai/src/cli/mod.rs`, `crates/butai-server/src/daemon.rs` |
| `exit_when_empty` and the other knobs | `crates/butai-server/src/config.rs`, `crates/butai-server/src/core.rs` (`should_exit`) |
| Socket path, `~/.butai` layout, `$BUTAI_SESSION_FILE` | `crates/butai-protocol/src/paths.rs` |
| An in-process daemon on a private socket | `crates/butai/src/standalone.rs` |
| The HTTP surface, headers, status codes | `crates/butai-server/src/http_conn.rs` |
| First-byte routing between HTTP and framed | `crates/butai-server/src/client_conn.rs` |
| The reference relay | [`web/server/`](../web/server), [`web/README.md`](../web/README.md) |
| Relay container and socket mount | [`web/Dockerfile`](../web/Dockerfile), [`web/docker-compose.yml`](../web/docker-compose.yml) |
| A daemon runtime image, and the `safe.directory` trap | `testsuite/Dockerfile`, `testsuite/tests/test_30_git.py` |
| Security posture | [`web/README.md`](../web/README.md#security), [`protocol.md`](protocol.md#transport) |
| Versioning and `server_version` | [`protocol.md`](protocol.md#versioning), `crates/butai-client/src/workbench.rs` (`skew_notice`) |
| The executable contract | [`crates/butai-server/tests/e2e_http.rs`](../crates/butai-server/tests/e2e_http.rs) |
| TypeScript for the DTOs, generated from them | [`web/src/protocol/generated/protocol.ts`](../web/src/protocol/generated/protocol.ts), [`development.md`](development.md) |
| The pane renderer downstream copied | [`web/src/stage/Screen.ts`](../web/src/stage/Screen.ts), [`web/src/stage/Stage.tsx`](../web/src/stage/Stage.tsx) |
| The client's own layers: logic, kit, pages, shell | [`web/src/`](../web/src), [`web/README.md`](../web/README.md) |
| The image and what it needs at build time | [`web/Dockerfile`](../web/Dockerfile), [`web/README.md`](../web/README.md) |
| The offline guarantee: the lockfile and the bundle | [`web/bun.lock`](../web/bun.lock), [`web/vite.config.ts`](../web/vite.config.ts) |
| Palettes, and how a client applies one | [`web/src/logic/settings.ts`](../web/src/logic/settings.ts), [`web/src/theme.ts`](../web/src/theme.ts), [`theming.md`](theming.md) |
| The end-to-end walkthrough | [`examples/api-client.py`](../examples/api-client.py), `crates/butai-server/tests/e2e_http.rs` |
