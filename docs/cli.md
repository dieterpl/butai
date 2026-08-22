# The command line

`butai` is one binary and it is three programs: the workbench you attach to, a
client of the daemon's REST API, and the daemon itself. This page is the whole
command tree — every subcommand, every flag, how a target string is resolved,
what each command prints under `--json`, and what every exit code means. The
keyboard inside the workbench is [keys.md](keys.md); the routes underneath these
commands are [protocol.md](protocol.md); the surface as an agent in a pane sees
it is [agents.md](agents.md).

## The invocation model

Commands fall into three groups, and the group decides what the binary does with
the socket.

| group | commands | transport |
|---|---|---|
| **attaching** | bare `butai`, `new`, `attach` | hands off to the client, which speaks the framed protocol and owns the terminal |
| **structured control** | `workspace`, `pane`, `agent`, `process`, `whoami` | HTTP over the same socket, one connection per request |
| **legacy one-shots** | `ls`, `kill-session`, `kill-server` | the framed control path — they predate the REST face and their output is a contract the test suite drives |
| **process modes** | `daemon`, `proxy`, `reset`, `standalone` | no daemon conversation at all; they *are* the process |

### Bare `butai`

```sh
butai
```

Attaches to the daemon at the resolved socket, spawning one if none is running,
and opens the workspace for the current directory if nothing is open yet. It is
`butai attach` with no target, plus one thing `attach` deliberately does not do —
see *The ssh handoff* below.

### When a daemon is auto-started

Every path that opens a socket goes through the same connect-or-spawn: try
`connect(2)`, and if that fails, `fork` this same executable as `butai daemon`
with `BUTAI_SOCKET` pointing at the requested path, `setsid`, stdio to
`/dev/null`, then retry the connect for up to about 40 attempts with a widening
backoff (≈50ms rising to ≈440ms).

That applies to **every** command that talks to a daemon, not just the attaching
ones: `butai ws ls` on a machine with no daemon running starts one. A shared
lock file beside the socket (`butai.lock`) is what keeps two racing clients from
both spawning — a live daemon holds it exclusively, so a client that cannot take
it shared knows one is already coming up and waits instead of forking a second.

The two commands that never spawn anything are `butai reset`, which touches only
your terminal, and `butai standalone`, which binds a socket of its own.

### The nesting guard

The daemon sets `$BUTAI` in every pane it spawns, to the socket path it actually
bound. An attaching command compares that against the socket it is about to
attach to, canonicalising both, and refuses if they are the same daemon:

```
already inside this butai (/home/you/.butai/butai.sock) — attach a different
--socket, unset BUTAI to force, or detach first
```

Attaching a *different* daemon from inside a pane is allowed on purpose — a
remote workbench opened from a local pane is a different daemon drawing a
different screen. `butai standalone` refuses any nesting at all, because it has
no socket identity to compare with.

### The ssh handoff

This is what separates bare `butai` from an explicit `attach`. When bare `butai`
runs with `$SSH_CONNECTION` set and both stdin and stdout are ttys, it writes a
Secondary DA query (`ESC[>c`) to the terminal and reads the reply. butai's own
terminal emulator answers with `98` (`b`) in the identifying field, so a reply
starting `ESC[>98;` means the far end of this ssh session is being displayed
inside a butai pane. In that case the far side writes a one-way APC naming
`user@<server address from $SSH_CONNECTION>` and its own socket path, prints

```
[butai: opened in your local butai — this machine's projects are in its tab bar]
```

and exits — the near daemon sees the APC in the pane's output and dials back, so
the remote machine's workspaces appear in the tab bar you already had.

Nothing is written before the gate passes, and the APC only ever follows a
confirmed butai reply, so a plain terminal sees one invisible DA2 query and
nothing else. Set `BUTAI_NO_HANDOFF` to a non-empty value other than `0` to skip
the probe entirely. `butai attach` and `butai new` never probe: asking for those
is asking for a workbench *here*.

## Global options

All four are `global = true`, so they are accepted at any depth — `butai --json
pane ls` and `butai pane ls --json` are the same command.

| flag | type | default | effect |
|---|---|---|---|
| `--json` | bool | off | emit JSON instead of human text |
| `--socket <PATH>` | path | `$BUTAI_SOCKET`, else `~/.butai/butai.sock` | which daemon to talk to |
| `-w`, `--ws <WS>` | workspace id or name | `$BUTAI_WORKSPACE` | default workspace scope |
| `-q`, `--quiet` | bool | off | print nothing on success; the exit code is the answer |
| `-h`, `--help` | | | print help (`--help` is long form, `-h` the summary) |
| `-V`, `--version` | | | print the version — **top level only** |

`--quiet` beats `--json`: it suppresses success output whatever the format.
Errors are unaffected — they go to stderr regardless, because a silent failure
is not a quiet one.

Without a home directory to resolve, `~/.butai` becomes `/tmp/butai-<uid>`, so
the socket default follows. That is why `butai --json whoami` is the way to
learn a remote daemon's socket path rather than assuming one.

## Targets

Every command that addresses a pane takes a target with the same grammar:

```text
TARGET := [SCOPE ":"] LEAF
SCOPE  := <workspace id> | <workspace name>
LEAF   := <pane id> | <agent or process name> | "stage"
```

So `4`, `1:4`, `api:4`, `reviewer`, `api:reviewer` and `1:stage` are all valid.
The split is on the **first** colon, so a name may contain one (`api:build:web`
is workspace `api`, name `build:web`) while a scope may not. Surrounding
whitespace is trimmed on both halves; an empty half (`:4`, `1:`, `:`) is a usage
error.

### How each leaf resolves

| leaf | resolution |
|---|---|
| bare pane id (`7`) | scanned for across **every** open workspace — no scope needed, no scope consulted |
| scoped pane id (`1:7`) | an *assertion*: fails if pane 7 is not in workspace 1 |
| name (`reviewer`) | case-insensitive **substring** match against every agent title and process name in the scope |
| `stage` | whatever pane the scope's stage is showing |

Pane ids come from one daemon-wide counter, so a bare id is already unambiguous
— unlike a multiplexer that numbers panes per window. That is why `butai pane
read 7` works from anywhere, and why a scope alongside a numeric leaf is an
assertion rather than a lookup: a script that cached an id across a workspace
teardown gets an error instead of acting on whatever pane inherited the number.

### Where the scope comes from

For everything except a bare pane id, the scope is taken in this order:

1. the target's own `<workspace>:` prefix,
2. `-w` / `--ws`,
3. `$BUTAI_WORKSPACE`, which every pane carries.

With none of the three, the command fails with exit 64 and says so:

```
no workspace given for the pane list: pass --ws, write the target as
<workspace>:<leaf>, or run inside a butai pane
```

A numeric scope is taken at face value. A named one is matched
**case-sensitively** against the workspace list; no match is exit 2, more than
one match is exit 64 listing the colliding ids.

### Ambiguity and self-targeting

A name that matches more than one pane is an error, not a coin flip:

```
2 panes in workspace 1 match "claude"; use an id: 42 (claude), 45 (claude)
```

Sending a prompt to the wrong agent is not recoverable, so the CLI will not
guess. The same reasoning refuses a target that is the caller's own pane, for
`pane send`, `agent send`, `agent wait` and `agent kill`: typing into your own
pane appends to the prompt you are composing, and waiting on yourself can never
return, because you are `working` precisely because you are running the wait.
Both are exit 64.

Target resolution happens **client-side**. There is no `/v1/resolve` route today
— the CLI does the lookups itself against `/v1/workspaces` and
`/v1/workspaces/{id}`.

## Attaching

### `butai [new|attach]`

```sh
butai                       # attach; open the current directory if nothing is open
butai new [-s <SESSION>]
butai attach [-t <TARGET>]
```

| command | flag | type | default | effect |
|---|---|---|---|---|
| `new` | `-s`, `--session <SESSION>` | string | generated | the workspace name to create and attach to |
| `attach` | `-t`, `--target <TARGET>` | string | most recent | the workspace name to attach to |

"Session" here means workspace: the daemon's session list is derived from its
workspaces, one per project directory.

All three forms end up in the same client, which ensures a workspace exists
before drawing: a *named* target that is already open needs nothing; an unnamed
one is satisfied by any open workspace; otherwise the client posts
`/v1/workspaces` with the current directory (and the name, when one was given).
So `butai attach -t api` in a directory with no `api` workspace open **creates**
one here rather than failing.

On detach the client prints the reason and exits 0:

```
[butai: detached]
```

### `butai ls`

```sh
butai ls
```

No flags of its own. One line per workspace, in the daemon's own order:

```
my-app: 1 window (1 clients) [/home/you/Projects/my-app]
api: 1 window (0 clients) [/home/you/Projects/api]
```

`no sessions` when there are none. The window count is always 1 — the workbench
has fixed rails and one stage, so there are no windows to count; the field
survives because it is part of the framed reply shape.

### `butai kill-session -t <TARGET>`

```sh
butai kill-session -t work
```

`-t`, `--target <TARGET>` is **required** and is a workspace *name*, not the
target grammar above and not an id. Kills the workspace and everything in it.
An unknown name is a daemon-side error, reported as exit 1.

### `butai kill-server [--clear]`

```sh
butai kill-server           # workspaces are remembered
butai kill-server --clear   # ...and forgotten, so the next start comes up empty
```

Detaches every client, kills every workspace, and stops the daemon. Without
`--clear` the open workspaces and their per-pane output dumps are snapshotted
first, so the next start comes back to them. `--clear` removes both halves of
that restore state *before* shutting down, so a daemon killed mid-exit still
comes up empty.

## `butai workspace` (alias `ws`)

The `/v1/workspaces` routes on the command line. Nothing here is CLI-only.

| command | signature | notes |
|---|---|---|
| `ls` (alias `list`) | `butai ws ls` | every workspace with its counts |
| `show` | `butai ws show [TARGET]` | one workspace's agents, processes and changes |
| `create` (alias `new`) | `butai ws create [--cwd PATH] [--name NAME] [--layout LAYOUT]` | prints the new id |
| `rm` (alias `kill`) | `butai ws rm [TARGET]` | closes it and kills everything in it |

`[TARGET]` on `show` and `rm` is a workspace id or name and **overrides** `--ws`
when given; with neither, the command fails.

`create` flags:

| flag | type | default | effect |
|---|---|---|---|
| `--cwd <PATH>` | path | the current directory | directory to open |
| `--name <NAME>` | string | the directory's basename | workspace name |
| `--layout <LAYOUT>` | string | none | **accepted and ignored** |

`--layout` reaches the daemon and is discarded there. Layout presets described
pane splits, and the workbench has fixed rails; the parameter survives on the
wire because shipped clients still send it. Do not build on it.

`create` prints the bare new id on stdout, so it assigns straight into a
variable, and `rm` prints `killed workspace 3`.

`ls` output is one tab-separated row per workspace, mentioning only the agent
states that are actually populated:

```
1	my-app	3 agents (1 working, 1 waiting), 2 processes, 4 changes	[/home/you/Projects/my-app]
```

`show` prints a header line then `AGENTS`, `PROCESSES` and `CHANGES` blocks,
skipping any that are empty. Staged files come first in `CHANGES` and are marked
`staged`, so `butai ws show` answers "what would a commit take?" without a second
look at git.

## `butai pane`

| command | signature |
|---|---|
| `ls` (alias `list`) | `butai pane ls [TARGET]` |
| `read` | `butai pane read <TARGET> [-l N] [--source S] [--format F]` |
| `send` | `butai pane send <TARGET> [TEXT...] [--key KEY] [--no-enter]` |

`pane ls` takes a **workspace** id or name (overriding `--ws`); `read` and `send`
take a pane target.

### `pane ls`

One tab-separated row per addressable pane — agents first, then processes:

```
42	agent	working	⠐ Refactor help button to separate screen
71	agent	working	⠂ Create comprehensive documentation <- you
106	agent	idle	✳ Claude Code [stage]
```

Columns are pane id, kind (`agent` or `process`), status, then the label with
` [stage]` appended when that pane is on the stage and ` <- you` when it is the
caller's own. `no panes in workspace N` when there are none.

### `pane read`

| flag | type | default | effect |
|---|---|---|---|
| `-l`, `--lines <LINES>` | usize | `200` | maximum rows, counting back from the live screen |
| `--source <SOURCE>` | `scrollback` \| `screen` \| `footer` | `scrollback` | which band to read |
| `--format <FORMAT>` | `text` \| `ansi` | `text` | `ansi` keeps colors as SGR sequences |

- `scrollback` — recent history ending at the live screen.
- `screen` — exactly the visible viewport, blank rows and all.
- `footer` — the band the agent-state detector scans, which makes "why does butai
  think this agent is working?" answerable from outside the daemon.

Human output is the lines and nothing else — no header, no pane id, no color —
so `butai pane read 7 | grep …` behaves like reading a file. The read is a
*query*: it does not resize the pane, take the stage, or acknowledge its bell.

### `pane send`

| flag | type | default | effect |
|---|---|---|---|
| `--key <KEY>` | key name | none | send one named key instead of text |
| `--no-enter` | bool | off | do not press Enter after the text |

`[TEXT]...` is joined with single spaces and delivered as **one paste**, not a
keystroke per character: it is a single round-trip, and it arrives inside the
agent's bracketed-paste guard the way a real paste would rather than looking like
implausibly fast typing. Enter follows unless `--no-enter`.

`--key` and text are mutually exclusive (exit 64 either way — both given, or
neither). Key names are trimmed and lowercased before matching:

| accepted |
|---|
| `enter`, `esc`, `tab`, `backspace`, `delete` |
| `up`, `down`, `left`, `right` |
| `home`, `end`, `page_up`, `page_down` |
| `ctrl-<char>` or `ctrl+<char>`, a single character |

Anything else is exit 64 naming what is accepted, rather than a silent no-op
keystroke.

`send` prints nothing on success, under `--json` too.

## `butai agent`

| command | signature |
|---|---|
| `ls` (alias `list`) | `butai agent ls [TARGET]` |
| `spawn` | `butai agent spawn <KIND> [--background] [--prompt TEXT] [--wait] [--timeout MS]` |
| `send` | `butai agent send <TARGET> [TEXT...] [--wait] [--until SET] [--timeout MS]` |
| `read` | `butai agent read <TARGET> [-l N] [--source S] [--format F]` |
| `wait` | `butai agent wait <TARGET> [--until SET] [--timeout MS] [--since-seq N]` |
| `kill` | `butai agent kill <TARGET>` |

`agent ls` and `agent read` are the same code as their `pane` counterparts —
`agent ls` therefore lists **processes as well as agents**, exactly as `pane ls`
does. `GET /v1/agents` is the route that lists configured agent *types*; the CLI
has no verb for it.

### `agent spawn`

| flag | type | default | effect |
|---|---|---|---|
| `--background` | bool | off | do not take the stage — leave the human's view where it is |
| `--prompt <TEXT>` | string | none | send this prompt once the agent is up |
| `--wait` | bool | off | with `--prompt`, block until the agent finishes it |
| `--timeout <MS>` | u64 | `300000` | give up after this many milliseconds when waiting |

`<KIND>` is an agent type as configured under `[[agents]]` — see
[configuration.md](configuration.md). The workspace comes from `--ws` or
`$BUTAI_WORKSPACE` only; there is no positional workspace argument.

**stdout is the bare pane id and nothing else**, so `P=$(butai agent spawn
claude)` works. Under `--json` it is `{"pane":42}`. Everything the spawn has to
say afterwards goes to stderr.

The spawn route answers `{"ok":true}` rather than the new id, so the CLI recovers
it by listing the agents before and after and taking the highest id that was not
there before — unambiguous, because pane ids come from one daemon-wide counter.

`--prompt` does not type the instant the pane exists. The route returns as soon
as the PTY is there, but an agent CLI needs about a second more before it is
reading input, and what is typed into that gap does not queue: the paste lands in
the box and the Enter after it is dropped with the rest of the buffered startup
input, so the turn never begins. So `--prompt` first polls the pane's footer
every 100ms for up to **15 seconds**, waiting for a non-blank row — a TUI paints
because its input loop is running. If it never draws, the prompt is sent anyway
and a warning goes to stderr:

```
pane 42 drew nothing in 15000ms; sending the prompt anyway
```

`--wait` has effect only alongside `--prompt`; on its own it is silently a no-op,
as is `--timeout` with no wait. The wait reads the notification feed's head
*before* posting the spawn, so it is edge-correct by construction, and it uses
the default `--until` set (`finished,exited`) — `spawn` has no `--until` of its
own. Its outcome reaches you through the exit code, plus a stderr line if it
timed out.

### `agent send`

| flag | type | default | effect |
|---|---|---|---|
| `--wait` | bool | off | block until the agent reaches `--until` |
| `--until <SET>` | state set | `finished,exited` | states that end the wait |
| `--timeout <MS>` | u64 | `300000` | give up after this many milliseconds |

`[TEXT]...` is the prompt, joined with spaces; empty is exit 64. Delivery is the
same paste-then-Enter as `pane send`, with no `--no-enter` escape — this verb
submits a turn.

`--until` is validated even without `--wait`, so a typo is caught either way.
With `--wait`, the notification feed's head is read *before* the prompt is
injected, which is what makes the wait edge-correct rather than level-triggered
(see below).

### `agent wait`

| flag | type | default | effect |
|---|---|---|---|
| `--until <SET>` | state set | `finished,exited` | states that end the wait |
| `--timeout <MS>` | u64 | `300000` | give up after this many milliseconds |
| `--since-seq <N>` | u64 | none | only accept a state reached after this notification sequence |

The state set is comma-separated, trimmed, case-insensitive and deduplicated.
Five names are the daemon's own, two are aliases that exist only in the CLI:

| word | expands to |
|---|---|
| `waiting` | blocked on you mid-task |
| `working` | recent output |
| `finished` | finished its turn and settled at the prompt |
| `idle` | quiet, nothing pending |
| `exited` | the process is gone |
| `done` | `finished`, `idle`, `exited` |
| `attention` | `waiting`, `finished`, `exited` |

An unknown word, or a `--until` that names no states at all, is exit 64 listing
the real ones. The default is deliberately **not** `idle`: a freshly spawned
agent starts out idle, so waiting for it would return immediately.

`wait` polls `GET /v1/workspaces/{ws}/agents` — 400ms, doubling to a 1s ceiling.
It is not the event stream for two reasons: `ApiEvent::Workspaces` carries only
per-workspace *counts*, so a subscriber would still have to follow every event
with a GET; and a clean agent exit removes the row and emits no notification at
all, so an edge-only waiter would hang on the most ordinary success. A vanished
row *is* the evidence, and that is what `wait` reports as `exited`. Agent state
is recomputed on the daemon's ~2s sampler tick, so polling faster buys nothing.

**The level-vs-edge trap.** `butai agent send 7 …` followed by a separate `butai
agent wait 7 --until finished` can return immediately on the **previous** turn's
`finished`, because the daemon may not have noticed the new prompt yet.
`--since-seq N` fixes it: a matching state then counts only once the daemon has
emitted a notification for this pane past sequence `N`, or the state has changed
since the first poll (which covers `idle`, the one state that never notifies).
`agent send --wait` and `agent spawn --prompt --wait` do this for you.

A bare numeric target that resolves to nothing is reported as `exited` (code 4)
rather than not-found (code 2): an agent that has already gone is `exited`, and a
wait that spanned the exit has always said so, so starting half a second later on
the same situation must not report a different code. Only a bare id gets this —
`1:7`, `stage` and a name can each fail for reasons of their own, and stay 404s.

Output is one line, or the outcome object under `--json`:

```
pane 42 finished
pane 42 still working after 300000ms
```

### `agent kill`

Deletes the pane. Refuses your own pane. Prints nothing.

## `butai process` (alias `proc`)

| command | signature |
|---|---|
| `ls` (alias `list`) | `butai process ls [TARGET]` |
| `status` | `butai process status [TARGET]` |
| `start` | `butai process start <NAME> [COMMAND...]` |

`[TARGET]` is a workspace id or name, overriding `--ws`. Rows are tab-separated
— pane, status, name, command:

```
44	ok	dev	npm run dev
59	FAIL(1)	build	cargo build
```

`status` prints the same rows and exists to be used in a condition: it exits 4 if
any process's status starts with `FAIL` or carries a non-zero exit code, so

```sh
butai process status -q || butai agent send 7 "the dev server died"
```

works without parsing anything.

`start` takes a label for the process rail and the command, joined with spaces.
An empty command is exit 64 from the CLI — note that the route itself treats an
empty command as "the workspace's default shell", so `POST
/v1/workspaces/{id}/processes` can do something the CLI will not. `start` prints
nothing on success.

## `butai whoami`

```sh
butai whoami
# pane 71
# workspace 1
# socket /home/you/.butai/butai.sock
```

Answers "where am I?" for a program running inside a pane, and is the first thing
a caller should run before issuing any control command. Outside a pane:

```
not inside a butai pane ($BUTAI_PANE is unset)
socket /home/you/.butai/butai.sock
```

It contacts no daemon. The socket it reports is the *resolved* socket this
invocation would talk to, not the raw `$BUTAI_SOCKET`, so it is always answered —
which makes `ssh host butai --json whoami` the way to learn a remote daemon's
socket path. `ssh -L` needs that path and cannot guess it: it forwards the path
verbatim without shell expansion, and `~/.butai/butai.sock` is not guaranteed
anyway. See [remote.md](remote.md).

## Process modes

### `butai daemon`

Runs the daemon in the foreground on the resolved socket. Normally spawned
automatically; run it by hand to watch it, or under a supervisor.

It creates the socket's parent directory and chmods it `0700`, takes an exclusive
`flock` on `butai.lock` as its single-instance guard — a second one exits with
`another butai daemon is already running` — removes any stale socket file, binds,
and loads `~/.butai/config.toml`, printing each config warning as a log line.
Logs go to `~/.butai/logs/daemon.log`, rotated daily, never to the terminal;
`RUST_LOG` sets the filter and defaults to `info`. On exit it removes the socket
and releases the lock.

### `butai proxy`

```sh
ssh host butai proxy
```

Bridges stdin/stdout to the daemon socket, connect-or-spawning it first. This is
the remote-access path: ssh provides both the transport and the authentication,
and the daemon never listens on TCP. Both protocols ride it — the length-prefixed
framed protocol and HTTP — because the daemon tells them apart by their first
byte.

When stdin ends the socket is half-closed rather than torn down, so a one-shot
`butai proxy < request` still gets to read its reply. The bridge ends when the
daemon closes: for HTTP that is the response being complete, for the framed
protocol it is the session ending.

### `butai reset`

Puts a terminal left in mouse or raw mode by a killed or crashed butai back to
normal — the fix when your shell is spewing mouse codes. It writes the restore
sequences first (so the mouse goes quiet even if the termios work fails), applies
the equivalent of `stty sane` in place, and flushes the input queue of mouse
reports already sitting in it. It uses `TCSANOW` rather than `TCSADRAIN`, so it
cannot hang on a wedged terminal.

It stands alone on purpose: no nesting guard, no daemon, no socket — so it works
from whatever shell the wedged terminal left you in. It needs a tty, and says so
if it does not have one.

### `butai standalone`

A daemon and a workbench in one process lifetime, on a socket nobody else can
find. No detach support, and the session is deliberately not persisted, so the
next one starts empty rather than reopening whatever this one had.

The socket lives in a `0700` directory named for the process id, under
`$XDG_RUNTIME_DIR` when set and the system temp directory otherwise, and the
directory is removed on the way out. The workspace it opens is named
`standalone`. It ignores `--socket`.

### `butai help`

`butai help [COMMAND]` is clap's own, equivalent to `--help` on that command.
`--help` and `--version` exit 0; any other parse failure exits 64 rather than
clap's own 2, so it matches the rest of the CLI.

## Machine-readable output

`--json` has two behaviours, and which one you get depends on where the data came
from.

**Passed through verbatim.** For commands whose answer is a daemon response
body, `--json` re-emits those bytes unmodified — not re-serialized from a parsed
struct — so the CLI's JSON *is* the REST API's JSON and the two cannot drift as
DTOs gain fields. A trailing newline is added if the body lacks one.

| command | body |
|---|---|
| `ws ls` | `WorkspaceSummary[]` |
| `ws show` | `WorkspaceDetail` |
| `ws create` | `{"id":3}` |
| `ws rm` | `{"ok":true}` |
| `pane read`, `agent read` | `PaneOutputDto` |
| `process ls`, `process status` | `ProcessDto[]` |

**Serialized by the CLI.** These have no route behind them, so the CLI owns the
shape:

| command | shape |
|---|---|
| `ls` | `SessionInfo[]` — `{id, name, windows, attached_clients, cwd}` |
| `whoami` | `{inside_butai, pane, workspace, socket}` |
| `pane ls`, `agent ls` | `{pane, kind, label, status, staged}[]` |
| `agent spawn` | `{"pane":42}` |
| `agent wait`, `agent send --wait` | `{pane, state, exited, timed_out, waited_ms}` |

The wait outcome is shaped so a future server-side blocking route can return
exactly this and the `--json` output does not change when it switches over.

Commands that print nothing on success print nothing under `--json` either:
`pane send`, `agent kill`, `process start`, `kill-session`, `kill-server`.

The DTO field-by-field reference is [protocol.md](protocol.md); a worked client
is [../web/README.md](../web/README.md).

## Exit codes

The exit code is the interface. Under `--quiet` nothing is printed at all, so
`butai agent wait 7 -q && ./deploy.sh` has to be able to tell "it finished" from
"it timed out" from "there is no pane 7" — which is why failures are not all
collapsed to 1.

| code | name | meaning |
|---|---|---|
| 0 | `OK` | success; for `wait`, the target reached the state |
| 1 | `FAILED` | generic failure — daemon unreachable, a 5xx, an unexpected reply |
| 2 | `NOT_FOUND` | no such workspace, pane, or agent |
| 3 | `TIMED_OUT` | `wait` timed out; the target is still running |
| 4 | `EXITED` | the target exited, or `process status` found a failure |
| 64 | `USAGE` | bad flag, bad target, ambiguous name, self-target (`EX_USAGE`) |

Codes 2 and 64 come straight from the daemon's own 404 and 400 when the failure
happened there, and are produced client-side with the same meanings when the CLI
resolved the target itself — so `butai pane read 9999` and `butai pane read
1:9999` agree. The mapping walks the whole error chain, so a status wrapped in
context still reports what the daemon said.

`exited` is code 4 **even when you asked to wait for it**. It is in the default
`--until` set so the wait terminates, not because a dead agent is a success:
`butai agent wait 7 -q && ./deploy.sh` must not deploy because the agent's
process fell over.

## Environment

### Read by the binary

| variable | read by | effect |
|---|---|---|
| `BUTAI_SOCKET` | `--socket` default, and the socket-path helper | which daemon to talk to |
| `BUTAI_WORKSPACE` | `--ws` default, `whoami` | default workspace scope |
| `BUTAI_PANE` | target resolution, `whoami` | the caller's own pane; its **absence** is the test for "not inside butai" |
| `BUTAI` | the nesting guard | socket of the daemon this pane belongs to |
| `BUTAI_NO_HANDOFF` | bare `butai` | non-empty and not `0` disables the ssh handoff probe |
| `BUTAI_SESSION_FILE` | the daemon | overrides `~/.butai/session.json`, and moves the per-pane dumps beside it |
| `BUTAI_THEME_DIR` | the client | overrides `~/.butai/themes` |
| `SSH_CONNECTION` | the handoff | gates the probe, and its third field is the dial-back address |
| `USER` | the handoff | the user half of the `user@host` dial-back hint |
| `XDG_RUNTIME_DIR` | `standalone` | where the private socket directory goes; falls back to the temp dir |
| `SHELL` | the daemon | default shell for panes, when config does not name one |
| `RUST_LOG` | the daemon | log filter; defaults to `info` |
| `TMUX` | the client | send clipboard writes through tmux's DCS passthrough as well |
| `DISPLAY`, `WAYLAND_DISPLAY` | the client, Linux only | absent means there is no clipboard to read |
| `HOME` | everything | resolves `~/.butai`; without it, `/tmp/butai-<uid>` |

### Set by the daemon in every pane

Agent, process and plain shell alike:

| variable | value |
|---|---|
| `BUTAI` | the socket this daemon bound |
| `BUTAI_SOCKET` | the same path |
| `BUTAI_PANE` | this pane's id |
| `BUTAI_WORKSPACE` | the workspace it belongs to |
| `TERM` | `xterm-256color` |
| `COLORTERM` | `truecolor` |
| `PATH` | repaired when the daemon's inherited one is missing the usual directories |

`[[agents]] env` entries are applied *after* these, so an agent's own config can
override any of them. Because `--ws` defaults to `$BUTAI_WORKSPACE` and
`--socket` to `$BUTAI_SOCKET`, a command run inside a pane already acts on its own
workspace, on its own daemon, with nothing configured.

### Set on the spawned daemon

An auto-spawned daemon is started with `BUTAI_SOCKET` set to the socket the
client asked for, which is how a non-default socket propagates without a flag.

## Recipes

Spawn a helper, hand it a task, block, read the answer:

```sh
P=$(butai agent spawn claude --background)
butai agent send "$P" "summarise the failures in ./logs" --wait --timeout 120000
butai pane read "$P" --lines 40
```

The same thing in one command — stdout is still only the pane id:

```sh
P=$(butai agent spawn claude --background --prompt "summarise ./logs" --wait)
butai pane read "$P" --lines 40
```

Gate a deployment on a live finish, not on a crash:

```sh
if butai agent wait 7 -q; then
  ./deploy.sh
else
  case $? in
    3) echo "still running" ;;
    4) echo "the agent died" ;;
    *) echo "could not wait on it" ;;
  esac
fi
```

Which projects need a human, as JSON:

```sh
butai ws ls --json | jq -r '.[] | select(.waiting > 0) | .name'
```

Watch for a failed process from inside a pane and tell an agent about it:

```sh
butai process status -q || butai agent send reviewer "the dev server died"
```

Read exactly what the state detector sees, when an agent's status looks wrong:

```sh
butai pane read 42 --source footer --lines 8
```

Interrupt a runaway program without attaching:

```sh
butai pane send 42 --key ctrl-c
```

Open a project without attaching to it, then start its dev server:

```sh
WS=$(butai ws create --cwd ~/Projects/api --name api)
butai --ws "$WS" process start dev npm run dev
```

Learn a remote daemon's socket path, then forward it:

```sh
SOCK=$(ssh host butai --json whoami | jq -r .socket)
ssh -N -L "/tmp/host.sock:$SOCK" host &
butai --socket /tmp/host.sock ws ls
```

Check whether you are inside butai at all before doing any of this:

```sh
if [ -n "$BUTAI_PANE" ]; then butai pane ls; fi
```

## What the CLI does not cover

The REST API is wider than the command tree. There is no verb for staging a pane
(`POST /v1/workspaces/{id}/show`), restarting a process (`POST
…/processes/{pane}/restart`), listing configured agent types (`GET /v1/agents`),
the notification feed (`GET /v1/notifications`), the event stream, the file tree,
or any of the git operations. Reach those with `curl --unix-socket`, or through a
client — [protocol.md](protocol.md) has the full route list.

There is also no `split`. The daemon refuses layout commands by design: the frame
is fixed, with rails and one stage. You add an agent row or a process row and one
of them takes the stage; you never divide a pane.

## Where this lives

| section | source |
|---|---|
| entry point, exit-code plumbing | `crates/butai/src/main.rs` |
| the command tree, global flags, `ls`, `kill-session`, `kill-server`, `whoami` | `crates/butai/src/cli/mod.rs` |
| target grammar and parsing | `crates/butai/src/target.rs` |
| target resolution, `pane ls` / `read` / `send`, key names, self-target refusal | `crates/butai/src/cli/pane.rs` |
| `agent` verbs, `--until`, the wait loop, spawn readiness | `crates/butai/src/cli/agent.rs` |
| `process` verbs and the `status` exit code | `crates/butai/src/cli/process.rs` |
| `workspace` verbs | `crates/butai/src/cli/workspace.rs` |
| exit codes and the error-to-code mapping | `crates/butai/src/exit.rs` |
| `--json` / `--quiet` rendering | `crates/butai/src/out.rs` |
| the ssh handoff and its DA2 handshake | `crates/butai/src/handoff.rs` |
| `proxy` | `crates/butai/src/proxy.rs` |
| `standalone` | `crates/butai/src/standalone.rs` |
| attaching, the nesting guard, workspace bootstrap | `crates/butai-client/src/lib.rs` |
| connect-or-spawn, the spawn lock, framed control requests | `crates/butai-client/src/conn.rs` |
| the HTTP client and `ApiError` | `crates/butai-client/src/api.rs` |
| `reset` | `crates/butai-client/src/term.rs` |
| socket, config, session and theme paths | `crates/butai-protocol/src/paths.rs` |
| the names this program answers to | `crates/butai-protocol/src/names.rs` |
| DTO shapes behind `--json` | `crates/butai-protocol/src/api.rs` |
| `SessionInfo`, `AttachTarget`, `Command` | `crates/butai-protocol/src/lib.rs` |
| daemon startup, the single-instance lock, logging | `crates/butai-server/src/daemon.rs` |
| route table and request bodies | `crates/butai-server/src/http_conn.rs` |
| `kill-server`, `kill-session`, `ListSessions` | `crates/butai-server/src/core.rs` |
| the environment a pane is spawned with | `crates/butai-server/src/pane/terminal.rs` |
