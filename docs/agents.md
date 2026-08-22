# Driving butai from inside a pane

The daemon's REST API has always been able to spawn an agent, inspect its
siblings and inject input. What was missing was a way for a program *in* a pane
to find any of that: it had no idea which pane it was, no way to read a sibling's
output as text, no blocking `wait`, and nothing telling it the surface existed.

This is that surface. It is deliberately the same one a shell-out plugin would
use — there is no separate agent API.

## Identity

Every pane the daemon spawns — agent, process, or plain shell — carries:

| variable | value |
|---|---|
| `BUTAI_PANE` | this pane's id |
| `BUTAI_WORKSPACE` | the workspace it belongs to |
| `BUTAI_SOCKET` | the socket this daemon is bound to |

There is no separate "am I inside butai" flag: `BUTAI_PANE` is always set by the
spawner and says more than a boolean would, so its absence is the test.

Because `--ws` defaults to `$BUTAI_WORKSPACE` and `--socket` to `$BUTAI_SOCKET`, a
command run inside a pane already acts on its own workspace, on its own daemon.

```sh
butai whoami
# pane 7
# workspace 1
# socket /Users/you/.butai/butai.sock
```

## Targets

A target names a pane: an id (`7`), an agent name (`reviewer`), or `stage`.
Optionally scoped, as `<workspace>:<leaf>` — `1:7`, `api:reviewer`, `1:stage`.

Pane ids come from one daemon-wide counter, so a bare id is unambiguous and needs
no scope. When a scope *is* given alongside an id it is an assertion, not a
lookup: `1:7` fails if pane 7 is not in workspace 1, so a script holding an id
across a workspace teardown gets an error rather than acting on whatever pane
inherited the number.

## Reading

```sh
butai pane read 7 --lines 50
butai pane read 7 --source screen    # the viewport, padding included
butai pane read 7 --source footer    # the band the state detector scans
butai pane read 7 --format ansi      # keep the colours
```

Plain text on stdout and nothing else — no header, no id — so it pipes.

The daemon runs the VT emulator, so it resolves wide graphemes and trailing
blanks once, server-side, instead of every client reimplementing the cell-grid
rules the framed protocol requires. A read is a *query*: it does not resize the
pane or acknowledge its bell, which a transient framed attach would do.

`--source footer` shows exactly the rows butai scans to decide an agent's state,
which makes "why does butai think this agent is working?" answerable from outside
the daemon.

**Reach.** A read covers the visible screen plus about one screen of history,
whatever `[general] scrollback` is set to. The emulator's view is one screen
tall at any offset, so reaching further means walking the scrollback a screen at
a time — worth doing when something asks for it, and nothing does yet.
`"more": true` in the JSON reports the shortfall rather than pretending the rest
is not there. This is a bound on the *read*, not on the pane: a pane scrolls
back as far as `[general] scrollback` goes.

## Sending

```sh
butai pane send 7 "make test"       # typed, then Enter
butai pane send 7 "partial" --no-enter
butai pane send 7 --key ctrl-c
```

Text is delivered as a paste, so it arrives inside the agent's bracketed-paste
guard rather than looking like implausibly fast typing. Input never attaches,
resizes, or takes the stage.

## Waiting

```sh
butai agent send 7 "run the migration" --wait
butai agent wait 7 --until finished,exited
butai agent wait 7 --until attention --timeout 60000
```

States are the daemon's own: `waiting`, `working`, `finished`, `idle`, `exited`.
Two aliases exist only in the CLI: `done` (`finished,idle,exited`) and
`attention` (`waiting,finished,exited`). The default is `finished,exited` —
notably *not* `idle`, which is also a freshly spawned agent's initial state.

### The level-vs-edge trap

`butai agent send 7 …` followed by `butai agent wait 7 --until finished` can return
immediately on the **previous** turn's `finished`, because the daemon recomputes
agent state on a ~2s tick and may not have noticed the new prompt yet.

`butai agent send 7 … --wait` avoids this: it reads the notification feed's head
*before* it types, and then only accepts a state reached after that point. If you
need the two separate, pass the same position yourself via `--since-seq`.

Note that `finished` costs a 3-second settle window by construction, so a wait
that returns it has already paid for the quiet period. That is a feature.

### Why polling

`wait` polls `GET /v1/workspaces/{ws}/agents` rather than subscribing to
`/v1/events`. Two reasons:

- `ApiEvent::Workspaces` carries per-workspace **counts**, never per-pane state,
  so a subscriber would have to follow every event with a GET anyway.
- A *clean* agent exit removes the row and emits no notification at all, so an
  edge-only waiter would hang on the most ordinary success. A vanished row is the
  only evidence there is.

Agent state is recomputed on the daemon's ~2s sampler tick, so polling faster
than that buys nothing.

A server-side blocking route would let other embedders (Caliper, the mobile
clients) skip the loop. The CLI's contract is its exit codes and its `--json`
shape, both of which are already what such a route would return, so it can move
without anything downstream changing.

## Exit codes

`--quiet` prints nothing on success and leaves the code as the answer.

| code | meaning |
|---|---|
| 0 | success; for `wait`, the state was reached |
| 1 | generic failure — daemon unreachable, 5xx |
| 2 | no such workspace, pane, or agent |
| 3 | `wait` timed out; the target is still running |
| 4 | the target exited, or `process status` found a failure |
| 64 | usage — bad flag, bad target, self-target |

`exited` is code 4 even when you asked to wait for it: it is in the default set
so the wait *terminates*, not because a dead agent is a success.

```sh
butai agent wait 7 -q && ./deploy.sh         # only on a live finish
butai process status -q || echo "something died"
```

## Spawning helpers

```sh
P=$(butai agent spawn claude --background)
butai agent send $P "summarise the failures in ./logs" --wait
butai pane read $P --lines 40
```

`spawn` prints the bare pane id, so it assigns straight into a variable, and
`--background` leaves the stage where the human left it. Configured agent types
come from `[[agents]]` (`claude`, `codex`, `gemini`, `aider`, `agy` by default);
`butai --json agent ls` and `GET /v1/agents` both list them.

`--prompt` folds the first two lines into one, which is the shape an
orchestrator wants — spawn a worker, hand it its task, block until it answers:

```sh
P=$(butai agent spawn claude --background --prompt "summarise ./logs" --wait)
butai pane read $P --lines 40
```

The prompt is not typed the instant the pane exists. The spawn route returns as
soon as the PTY is there, but an agent CLI needs about a second more before it
is reading input, and what is typed into that gap does not simply queue: the
paste lands in the box, the Enter after it is dropped with the rest of the
buffered startup input, and the turn never begins. So `--prompt` waits for the
agent to draw its footer first. Still only stdout is the pane id — the wait
reports itself on stderr — so `P=$(…)` keeps working.

## There is no split

The daemon refuses layout commands by design — the frame is fixed, with rails and
one stage. You add an agent row or a process row and one of them takes the stage;
you never divide a pane. Anything looking for a `split` verb is looking for the
wrong tool.

## A worked example

```sh
# Build the CLI somewhere off the repo — an in-repo target/ corrupts on SMB.
CARGO_TARGET_DIR=/tmp/butai-target cargo build -p butai
BUTAI_SOCKET=/tmp/t/butai.sock /tmp/butai-target/debug/butai new -s smoke

# ...then, in the pane's own shell:
butai whoami && butai pane ls
P=$(butai agent spawn claude --background)
butai agent send $P "reply with the single word ready" --wait --timeout 120000
butai pane read $P --lines 5
butai agent wait $BUTAI_PANE    # refuses: exit 64
```
