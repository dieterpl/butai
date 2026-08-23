# Architecture

How butai is built, for someone about to change it. This page describes the
software as it is — including the parts that are awkward — rather than the
shape it is heading towards. [design.md](design.md) covers *why the interface
looks the way it does*; [protocol.md](protocol.md) is the normative wire spec
and this page links to it rather than restating message types.

## The four crates

```
                    ┌───────────────────┐
                    │  butai (binary)   │  entry points: attach, proxy,
                    │  crates/butai     │  standalone, daemon, the CLI
                    └─────────┬─────────┘
                       │             │
             ┌─────────┘             └─────────┐
             ▼                                 ▼
   ┌───────────────────┐             ┌───────────────────┐
   │   butai-client    │             │   butai-server    │
   │  the TUI, config, │             │  the daemon: core │
   │  keymap, theming  │             │  actor, panes,    │
   │                   │             │  PTYs, git, HTTP  │
   └─────────┬─────────┘             └─────────┬─────────┘
             │                                 │
             └──────────────┬──────────────────┘
                            ▼
                  ┌───────────────────┐
                  │  butai-protocol   │  wire types, framing,
                  │                   │  REST DTOs, paths, names
                  └───────────────────┘
```

| Crate | Owns |
| --- | --- |
| `butai-protocol` | Everything both sides must agree on: the framed message types, the length-prefixed codec, the REST request/reply DTOs, `~/.butai` path resolution, and the binary-name list used when re-executing butai on another machine. No I/O beyond reading environment variables. |
| `butai-server` | The daemon. The single-owner core actor, workspaces, panes, PTYs and the VT emulator, the git surface, process supervision, machine telemetry, search, the HTTP facade, and persistence. |
| `butai-client` | The terminal client and everything that draws: chrome geometry and rows, the keymap, themes, syntax highlighting, selection, the dial, and the REST/framed client code. |
| `butai` | The binary. Decides which role this invocation is (attach, proxy, standalone, daemon, one-shot CLI), resolves targets, and prints machine-readable output. |

`butai-protocol` depends on nothing else in the workspace. `butai-server` and
`butai-client` each depend on it and not on each other. The binary depends on
all three — which is the only reason a single artifact can be both ends of the
socket.

## One binary, three roles

The same executable is the daemon, the terminal client, and the scripting CLI.
Which one an invocation becomes is decided in `crates/butai/src/main.rs`; see
[cli.md](cli.md) for the user-facing side of that decision.

### The daemon

`butai daemon` runs `butai_server::daemon::run` in the foreground. It:

1. Creates `~/.butai/logs/` and installs a daily-rotating file logger. The
   level comes from `RUST_LOG`, defaulting to `info`. ANSI is off — the log is
   a file, not a terminal.
2. Builds a multi-threaded tokio runtime and, inside it, creates the socket's
   parent directory and `chmod`s it to `0700`.
3. Takes a non-blocking exclusive `flock` on `~/.butai/butai.lock` (the socket
   path with its extension replaced). Failing to take it means another daemon
   is already running, and this one exits rather than racing it.
4. Because it holds the lock, any socket file still on disk is stale by
   definition, so it is removed before `bind`.
5. Loads the config, logs each parse warning, and starts `serve`.

On the way out it removes the socket file and drops the lock. The socket file
is therefore only meaningful while a daemon holds the lock beside it — a
leftover socket after a crash is not a running daemon, and the next start
deletes it.

### The client

An attaching invocation dials the socket, and if nothing answers it spawns a
daemon and retries. The client is an ordinary API consumer: it reads structured
state over REST, subscribes to the event stream, and opens one framed
connection for the pane it is showing. It has no privileged channel.

### Standalone

`butai standalone` binds a real socket in a private temporary directory and
runs the normal client against it. It used to bridge the two halves with
in-memory channels; that stopped being honest once the client needed REST and
an event stream as well as a pane connection, so the mode now exercises exactly
the same code path as everything else. Nothing outside the process can reach
that socket, there is no session store, and it all goes away together.

## The rule the whole design turns on

> The daemon renders a pane's screen only when a program's bytes are on the
> other end of a PTY. Everything else crosses the wire as JSON and the client
> draws it.

This is not a rendering preference, it is where the data is. What is on a
terminal's screen is the accumulated effect of every byte a program has written
to it, and reconstructing that needs a VT emulator. A rail of agent rows is not
like that: it is a list, and a list is JSON.

The rule is enforced structurally rather than by convention. `PaneState`
(`crates/butai-server/src/pane/mod.rs`) has exactly two variants and the
dispatch methods make the split explicit:

- `render` returns `true` only for a terminal. A git pane returns `false`, so a
  client asking to stream one is told there is nothing to stream rather than
  shown an empty grid.
- `handle_input` is a no-op for anything but a terminal. A key pressed over a
  git row is the client's to interpret; staging a file is
  `POST .../changes/stage`, not a `Space` keypress the daemon reads.
- `resize` is a no-op for anything but a terminal. How many rows of a status
  list fit on a screen is a question about a window, and two clients can
  disagree.

`crates/butai-server/src/render.rs` is what is left of a file that used to
compose an entire workbench. It carries no theme, because a terminal's cells
hold the program's own colours and the palette around them belongs to whoever
is painting the frame.

What was removed by this rule, and where it went: the editor, the diff view and
the file tree were each a cursor sitting in text the daemon happened to have
read. They are now the client's, against `GET .../file`, `GET .../diff` and
`GET .../tree`.

## One socket, two protocols

Both the framed protocol and the HTTP API are served on the same Unix socket.
A connection is classified by peeking at its first byte without consuming it
(`MSG_PEEK`, because `UnixStream` has no async peek):

| First byte | Meaning | Handler |
| --- | --- | --- |
| `0x00` | The top byte of a 4-byte big-endian length prefix — a framed hello | `client_conn::handle_connection` |
| Anything else | An ASCII HTTP method letter | `http_conn::handle` |

This works because a framed message would have to exceed 16 MB before its
length prefix's top byte were non-zero, and `MAX_FRAME_LEN` is 32 MB.

### The framed connection

The first frame in each direction is always JSON. The client's `Hello` carries
the encoding it wants; every frame after each side's `Hello` uses it. JSON is
the baseline and MessagePack (named-field) is the option — a third-party client
can ignore msgpack entirely.

Two failure policies, deliberately different:

- **An undecodable frame is skipped, not fatal.** The versioning rule is that
  additive changes do not bump `proto_version`, which only works if the side
  that has never heard of a new message ignores it. Dropping the connection
  instead turns "this daemon is one release behind" into a reconnect loop — a
  real session was caught doing that 25 times, and it presented as the stage
  blanking rather than as anything version-shaped. A counter caps this at
  `MAX_CONSECUTIVE_BAD_FRAMES` (16) so a genuinely desynchronised stream still
  ends.
- **A framing error is always fatal.** A bad length prefix means the next frame
  boundary is unknown and there is nothing to resynchronise to.

### The HTTP facade

`http_conn` owns no state. It translates HTTP into `Event::Api` or
`Event::ApiSubscribe` and round-trips through the core actor — a oneshot
channel for queries and actions, an mpsc stream for `GET /v1/events`. That is
why adding a route is a change in two places (the route table and the core's
`ApiRequest` handler) and never a change to how state is locked.

The full route list, request shapes and reply shapes are in
[protocol.md](protocol.md); [building-a-client.md](building-a-client.md) is the
guided version.

## The core actor

`ServerCore` (`crates/butai-server/src/core.rs`) is the single owner of all
mutable daemon state. Every mutation flows through one event loop. There are no
locks around the workspace graph because there is only one thread that touches
it.

### Two channels, on purpose

| Channel | Bound | Carries |
| --- | --- | --- |
| `Event` (`events_tx`) | unbounded | Everything: client connect/disconnect, client messages, pane exits, git scan results, telemetry, ticks, API requests, shutdown |
| `(PaneId, Vec<u8>)` (`output_tx`) | bounded, `OUTPUT_CHANNEL_CAP` = 256 | PTY output only |

Keeping high-volume output off the control channel is what makes a flooding
process throttle *itself* rather than bury control events behind an unbounded
backlog. When the output channel is full, reader threads block on send, which
stalls PTY draining, fills the kernel pipe buffer, and applies backpressure to
the child — capping CPU and memory under a flood.

### The run loop

```
restore_session()
loop {
    drain_ready()            // all control events first, then ≤128 KiB of output
    if shutdown || should_exit { break }
    if dirty && now >= last_render + 16ms {
        render_all()
        broadcast_ws_details()
    }
    if saturated { park until the next frame deadline, biased on control }
    else         { block on control | output | frame deadline, biased on control }
}
```

Four properties worth knowing before you change it:

- **The frame clock is 16 ms** (`FRAME_INTERVAL`). Rendering is coalesced to
  it, and so is the workspace-detail broadcast — a client drawing rails needs
  them to change when the pane beside them does, not on the next telemetry
  tick.
- **Control events are `biased` first** in every `select!`, so a kill-server or
  a keystroke wins any tie against a flood of output.
- **Output drains are capped at `OUTPUT_DRAIN_BYTES`** (128 KiB per pass).
  Feeding the emulator is synchronous, so this bounds how long the loop can go
  without checking the control channel. Leftover output stays queued.
- **A slow iteration is logged.** Past `SLOW_ITERATION` (50 ms) the loop warns
  with the elapsed time. This loop is the only thread that owns panes, drains
  output and renders, so one slow pass freezes every pane and every client at
  once. Blocking filesystem work on a network-mounted workspace is the usual
  cause, and without the log it reads as "butai is slow".

Anything that can block for an unbounded time runs off the actor: git status
scans, git operations, workspace search, directory probes and uploads all
finish by posting an `Event` back.

## The state model

```
ServerCore
├── workspaces: HashMap<SessionId, Workspace>   + order: Vec<SessionId>
│   └── Workspace
│       ├── cwd, name
│       ├── agents:    Vec<PaneId>  + agent_meta: HashMap<PaneId, AgentMeta>
│       ├── processes: Vec<PaneId>  + proc_meta:  HashMap<PaneId, ProcMeta>
│       ├── docker_logs: Option<PaneId>
│       ├── changes:     Option<PaneId>      (a GitPane; None outside a repo)
│       ├── stage:       Option<PaneId>
│       └── stage_size:  Option<(rows, cols)>
├── panes:   HashMap<PaneId, PaneState>       (flat; workspaces hold ids)
└── clients: HashMap<ClientId, ClientState>
```

A **workspace** is one project directory. It has fixed roles rather than a
free-form window tree: agents, processes and system on the left, changes on the
right, one stage in the middle.

`stage_size` is the interior of the stage as the last client to draw this
workspace reported it. A pane's size belongs to whoever is looking at it — the
client decides how wide its rails are, so only the client knows how big the
hole in the middle is. It is kept so the *next* pane can be born the right size
instead of being born small and reflowed the moment it is staged; a program
that reads its window size once, at startup, only gets one chance.

### Clients

`ClientState` is deliberately small: where to send messages, what the
connection is looking at, how big that is, and the last frame it was sent so
the next one can be a diff. A connection has a *subject* — a session **or** a
pane, never both.

It used to carry a whole terminal interface — pickers, a prompt, a commit
buffer, a help-modal scroll offset, an armed confirmation, a selection anchor —
because the daemon drew one interface per client. All of that is the client's
now.

### Identifiers

Panes, workspaces and clients are monotonic counters, which is right for ids
whose whole life is one daemon run. `crates/butai-server/src/ids.rs` exists for
the other kind: an id handed to a *foreign* program that must still be nameable
after a restart. Today that is agent conversation ids, minted as RFC 4122
version 4 UUIDs because the CLIs that accept one validate the shape.

## Terminal panes

A `TerminalPane` is a PTY child plus a server-side VT emulator grid.

### Threads

`portable-pty`'s I/O is blocking, so each pane owns two OS threads:

- a **reader thread**, reading up to `READ_CHUNK` (64 KiB) per wake and
  forwarding chunks on the bounded output channel;
- a **waiter thread**, which reports the child's exit as `Event::PaneExited`.

Bigger reads coalesce bursts, which is why output arrives at the emulator in
chunks rather than per-write — and why anything that scans output has to cope
with a marker split across two chunks.

### The emulator

`pane/term_emu.rs` is a trait with a `vt100` implementation behind it. The
trait exists so `alacritty_terminal` can slot in if vt100's correctness becomes
a limit; nothing else in the tree names vt100 directly.

### From cells to the wire

```
program bytes → PTY → reader thread → output channel → emulator → cell grid
      → ratatui::Buffer → Buffer::diff → CellRun[] → FrameUpdate → client
```

`render::diff_to_frame` takes the previous buffer this client saw and the next
one, and emits runs of contiguous cells. `prev = None` or a size change forces
a full repaint. Runs are extended while the next cell is exactly where the last
one ended, accounting for double-width glyphs.

Colours are mapped once, and the mapping is a contract: `Color::Reset` becomes
`Default` ("your own"), the sixteen named colours become their conventional
indices, `Indexed` passes through, and `Rgb` stays exact — a client must not
round an exact colour to a palette entry.

### Resize

The client reports the stage interior when it points at a pane or resizes one.
That measurement is the only one there is; the daemon stopped computing
rectangles when it stopped drawing rails. A pane spawned into a workspace no
client has drawn gets `UNWATCHED_PANE_SIZE` — 24×80, a conventional terminal,
because the real size is a fact about a window and there is no window yet.

### Input

`input/encode.rs` turns protocol key events into the xterm-style byte
sequences a program on the PTY slave expects. It is a pure function over key +
modifiers, which is what makes it testable without a PTY. Paste has its own
encoder so bracketed paste is emitted correctly.

## Agent status detection

This is the feature that makes the rails worth scanning, and it is the most
heuristic code in the tree. It lives in `pane/terminal.rs` and is debounced in
`core.rs`.

### The raw signals

`Attention` has three values — `Waiting`, `Working`, `Idle` — computed per tick
from the pane's grid and its output history.

| Signal | What it is |
| --- | --- |
| Busy markers | Substrings that only appear in an agent's status line while a turn is live, matched case-insensitively in the bottom `FOOTER_SCAN_ROWS` (8) rows: `esc to interrupt`, `ctrl+c to cancel`, and eight more. |
| Busy line starts | Phrases matched only at the start of a footer line, after its box gutter — currently `running in the background`. |
| Prompt markers | Confirmation *chrome* that never occurs in prose: `(y/n)`, `(y)es/(n)o`, `press enter to continue`, `enter to select`, `tab to amend`, and others. A positive "needs you now" signal. |
| Question markers | Sentences a decision dialog asks in words (`do you want to`, `proceed?`). Weak alone, so they only count with a numbered option list under them and never while a busy marker is up. |
| Sustained output | Output within the last `WORKING_WINDOW` (2 s) that has been streaming for at least `WORKING_MIN_SPAN` (1 s). |
| The bell | A `BEL` since the last look means the agent wants you. |

Every one of these boundaries was drawn around a specific false positive, and
the comments in the source say which:

- Bare verbs ("to interrupt", "to stop") are not markers, because an agent
  writes those in ordinary prose and prose scrolls through the footer band. A
  match there pins the pane to busy forever, and a pane pinned to busy never
  fires its finished notification. Every entry is anchored to the key you would
  press.
- The footer band exists so an agent echoing "esc to interrupt" in its own
  answer does not count.
- Raw output recency alone is not enough: opening a pane from another client
  resizes it, the child answers the `SIGWINCH` with a full repaint, and on
  recency that burst reads as a whole turn. Hence `WORKING_MIN_SPAN`.
- A bare `>` or `❯` input box is deliberately not a prompt marker. Idle is not
  a question.

### Debouncing

`AgentTrack` holds the *published* state, separate from the per-tick raw
signal, so a lull inside a turn does not masquerade as the agent finishing.

| Constant | Value | Effect |
| --- | --- | --- |
| `AGENT_SETTLE` | 3 s | Quiet time after the busy marker disappears before `working → finished`. |
| `MIN_TURN` | 3 s | A working run that never showed a busy marker must have lasted this long to count as a turn worth notifying. Runs *with* a marker notify however short they were. |
| `AGENT_CHECK_MIN` / `MAX` | 1 s / 30 s | Adaptive re-check interval. Busy or settling agents are rescanned every tick; a stable one doubles its backoff, so long-idle panes cost almost nothing. |

The first observation seeds a baseline and never notifies, so connecting to an
already-finished agent is silent.

### Notifications

Transitions land in a bounded ring (`NOTIF_HISTORY` = 256) with monotonic
sequence numbers. Clients drain it with `GET /v1/notifications?since=`. The
daemon also tracks which panes it has already rung the bell for, so each
transition rings once.

### Per-agent overrides

An `[[agents]]` block can set `waiting_pattern` and `busy_pattern`. Each
*replaces* the built-in table for that one signal rather than adding to it: an
additive pattern could only ever add matches, and the harder half of the
problem is taking back a false positive. A pattern that does not compile is
dropped with a warning and the built-in markers stay in charge — falling back
costs accuracy, while refusing to start costs the user their agent. See
[configuration.md](configuration.md).

## Git

`GitPane` caches a worktree's status: conflicted, unstaged and staged sections
plus recent commits. It is *not* a rail — it was one, and every client draws
that column for itself now from the `ChangesDto` this produces.

**Why anything is cached.** Every field a DTO reads is filled by an off-thread
scan and never touched again until the next one. Resolving the branch, the
upstream or the worktree root on demand meant a `Repository::discover` per
read, per client, which on a network-mounted worktree was the difference
between a workbench and a frozen one.

**Scan scheduling.** `git_refreshing` dedupes in-flight scans so the ~2 s
sampler tick cannot pile them up on a slow repo. `git_refresh_again` records a
request that arrived *while* a scan was running — dropping it instead loses the
mutation that asked for it, because the in-flight scan started before the
commit and nothing else would schedule another. The rail then shows files as
staged that are already committed until something unrelated triggers a refresh.

**Mutations move their own rows** before returning, so a DTO built immediately
after a stage is already right; the authoritative rows arrive with the rescan.

**Writes shell out to `git`.** `git_op.rs` runs the real binary for everything
beyond the index, because the user's remotes, `push.default`, credential
helpers, ssh-agent, hooks, signing config and sequencer state all belong to it —
and libgit2 is built here with `default-features = false`, so no SSH or HTTPS
transport is compiled in and it *cannot* reach a network. Two details make this
different from shelling out naively:

- It is `tokio::process`, not `spawn_blocking` + `Command::output()`. A
  blocking-pool thread parked in `wait()` cannot be cancelled or timed out, so
  one credential prompt used to park it forever. An async child can be raced
  against a timer and killed.
- Argument construction (`argv`) is a pure function that never spawns anything,
  so the whole injection surface is unit-testable. No shell is involved, so
  quoting and `$()` are not the threat; a value git parses as an *option* is,
  and it is handled by validating before argv is built.

**The write lock is per repository root**, not per workspace, because the thing
that must not have two writers is the repository — two workspaces can be open
on one worktree and interleaving a rebase there is how work gets lost. A
refused operation reports as `NoRepo`, `Busy` or `Invalid`, which the REST layer
maps to distinct status codes and the TUI shows as the same sentence.

`git_worktree.rs` treats a worktree as a workspace: opening one gives it its own
agents, processes, changes rail and branch, with no stashing and no switching.
Nothing in that file touches a repository — listing parses
`git worktree list --porcelain` and the writes are argv built by `git_op::argv`,
so both halves are tested as text.

User-facing detail is in [git.md](git.md).

## Process supervision

Processes are terminal panes with a `ProcMeta` beside them: a name, the command,
and an optional `ready` substring that flips the row to "ok" when it appears in
output. `ready_carry` keeps the tail of the previous output burst so a marker
split across two bursts still matches — output arrives coalesced, and a
server's startup banner is routinely written in more than one syscall or lands
on a 64 KiB read boundary. Without it the row stays "run" forever even though
the marker was printed.

See [processes.md](processes.md).

## Background services

| Task | Cadence | Purpose |
| --- | --- | --- |
| `sys::spawn_sampler` | ~2 s | CPU, RAM, swap, temperature, GPU and per-interface network telemetry for the SYSTEM rail, delivered as `Event::Sys`. The machine's static identity — CPU model and core counts — is read once when the task starts and carried on every sample. |
| `sys::spawn_ticker` | animation | Marquee phase for scrolling rail text. Only repaints while something is actually scrolling. |
| `sys::spawn_fast_ticker` | animation | Sprite phase for the ALL AGENTS panel. Gated on `wants_fast_anim`, so the extra clock costs nothing until that panel is open with an agent working. |
| `search` | on demand | Fuzzy filename matching (nucleo) plus content grep, on the blocking pool — walking and grepping a tree is exactly the filesystem work that freezes the daemon when the directory is on a share that has gone away. |
| repo probe | 2 s → 60 s | Re-checks whether a non-repository workspace has become one. Backs off, because on a network mount each failed probe is a full parent-directory stat walk. |

## Persistence and restore

Two stores, on different schedules:

| Path | Written | Holds |
| --- | --- | --- |
| `~/.butai/session.json` | synchronously, whenever a workspace opens or closes | Which project directories were open, in what order, their agents and processes, and which pane held the stage |
| `~/.butai/panes/<slug>-<hash>/` | continuously | Per-pane output dumps, replayed into fresh panes on restart |

Terminal output is deliberately not in `session.json`: it is bulk binary that
turns over every few seconds, while that file is rewritten synchronously on
every workspace change.

The dump directory name is a readable slug of the project directory *plus* a
hash of its full path. The slug alone collides across the several `.../src` or
`.../web` directories a person has open at once, and those workspaces would
replay each other's output. The hash alone would be unreadable in a directory a
user is expected to be able to delete by hand.

Three details that make restore correct rather than approximate:

- **Agents are stored by their `[[agents]]` name**, not by the command they
  resolved to. Command, args and detection patterns are config, so an agent
  whose launcher has been edited comes back under the new definition rather
  than a stale copy. The pane's *label* is not usable for this: agents rewrite
  their OSC title continuously to show the current task.
- **Conversations are named.** butai mints a UUID, passes it to the CLI at
  launch, and persists it, so a restored agent reopens *its own* conversation.
  The obvious resume flags — `claude --continue`, `gemini --resume latest` —
  all mean "the most recent conversation in this directory", which is ambiguous
  exactly when a workspace runs two agents: both would reopen the same
  transcript and interleave into it. A launcher with no session concept
  restores repainted but fresh.
- **`spoke` gates the resume.** A conversation does not exist until the agent is
  spoken to — the CLIs create the transcript on the first user message, not at
  launch — so asking to reopen an unwritten id fails and exits 1. Panes that
  were never typed into are restarted fresh instead. A pane whose resume fails
  anyway gets one fallback start within `RESUME_RETRY_WINDOW` (10 s).

Processes restore from the persisted list rather than from `.butai.toml`'s
autostart block, so a process removed from the workspace file since does not
come back and one started by hand is not lost.

Workspaces whose directory does not resolve at startup are **deferred**, not
dropped: they are carried in memory and written back out by the next persist,
so an unmounted share does not silently erase your tab bar.

Every field past `cwd` is `#[serde(default)]`, which is what lets a
`session.json` written before any of this existed still load.

## Shutdown

| Trigger | Path |
| --- | --- |
| `SIGINT` / `SIGTERM` | The signal handler sends `Event::Shutdown` and awaits the core task. |
| `kill-server` | Same event, from a client message. |
| Last workspace closes | `should_exit` in daemon mode, if configured. |
| Last client goes | `should_exit` in standalone mode. |

The accept task is aborted after the core finishes. The socket file is removed
and the flock released on the way out.

A client disconnecting is an ordinary `Event::ClientGone`: its `ClientState` is
dropped and its panes keep running, which is the entire point of a daemon.

## If you are changing X, read Y first

| Changing | Read first |
| --- | --- |
| Anything on the wire | [protocol.md](protocol.md) — it is normative and has an external consumer. Additive messages must not bump `proto_version`, and both sides must keep skipping what they cannot decode. |
| The core loop | The `saturated` / `biased` comments in `core.rs::run`. Both exist because of specific pathologies, and removing either brings back a hang under flood. |
| Agent detection | The marker tables in `pane/terminal.rs`. Every entry has a comment naming the false positive it was drawn around; widening one is how a pane gets pinned to busy and stops notifying. |
| Git writes | `git_op.rs`'s module docs. The rules are: pure `argv`, async child, per-root lock, no libgit2 network. |
| Anything that touches the filesystem | Whether it can run on the actor. If it walks a directory, it cannot — a network mount that has gone away will freeze every pane at once. |
| Restore | The `#[serde(default)]` policy in `core.rs`. An old `session.json` must still load. |

## Where this lives

| Section | Source |
| --- | --- |
| Crate map | `Cargo.toml`, `crates/*/Cargo.toml` |
| Daemon lifecycle, flock, logging | `crates/butai-server/src/daemon.rs` |
| Standalone mode | `crates/butai/src/standalone.rs` |
| Socket routing, framed connections | `crates/butai-server/src/client_conn.rs` |
| Framing, encodings, frame limits | `crates/butai-protocol/src/framing.rs` |
| HTTP facade and the event stream | `crates/butai-server/src/http_conn.rs` |
| The core actor, run loop, events | `crates/butai-server/src/core.rs` |
| Workspace and pane metadata | `crates/butai-server/src/workbench.rs` |
| Pane dispatch and the PTY/JSON split | `crates/butai-server/src/pane/mod.rs` |
| PTY panes, threads, agent detection | `crates/butai-server/src/pane/terminal.rs` |
| VT emulator abstraction | `crates/butai-server/src/pane/term_emu.rs` |
| Cell-run diffing and wire colours | `crates/butai-server/src/render.rs` |
| Key encoding | `crates/butai-server/src/input/encode.rs` |
| Git status cache and mutations | `crates/butai-server/src/pane/git.rs` |
| Git process execution and argv | `crates/butai-server/src/git_op.rs` |
| Worktrees | `crates/butai-server/src/git_worktree.rs` |
| Telemetry and animation clocks | `crates/butai-server/src/sys.rs` |
| Search | `crates/butai-server/src/search.rs` |
| Conversation ids | `crates/butai-server/src/ids.rs` |
| Paths and the `~/.butai` layout | `crates/butai-protocol/src/paths.rs` |
| Persistence and restore | `crates/butai-server/src/core.rs` (`SessionState`, `restore_session`, `persist_session`) |
| Updating butai in place | `crates/butai-client/src/update.rs`, `crates/butai-client/build.rs` (the target triple it asks the release for) |
