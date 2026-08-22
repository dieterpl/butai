# Processes

The PROCESSES rail is the second thing a workspace has, and it is the reason
butai is a workbench rather than a terminal with agents in it. A **process** is
a long-running command the daemon owns: your dev server, a watcher, a test
loop, a `docker logs -f`, or a plain shell. It is started for you when the
workspace opens, it keeps running while you look at something else, and it says
in one word whether it came up.

Mechanically a process is a **terminal pane with a `ProcMeta` beside it** — a
name, the command, and an optional `ready` substring. The pane is exactly the
same kind of pane an agent gets: a PTY, a child, a server-side VT emulator. The
`ProcMeta` is what turns it into a supervised row.

> A process is not sandboxed, restarted on failure, or health-checked. Nothing
> here is a substitute for systemd. What butai gives you is a row that is
> honest about what the command is doing, in the same window as the agent that
> is editing the code the command is running.

## Declaring one

Four ways in, and they differ only in what the `ProcMeta` ends up holding.

| how | name | command | `ready` |
|---|---|---|---|
| `[[processes]]` in `.butai.toml` | `name` | `cmd` | `ready`, if given |
| `t` in the rail, or `[+ term]` on its separator | `shell` | the default shell | — (already `ok`) |
| `:process NAME COMMAND`, or a key bound to it | `NAME` | `COMMAND` | never |
| `butai process start NAME COMMAND…`, or `POST …/processes` | `name` | `command` | never |

`.butai.toml` is the only route that can set a `ready` marker. Everything
started by hand gets `ready = None`, which has a consequence worth knowing
before you read the status table: **a hand-started command never reaches `ok`.**

```toml
[[processes]]
name = "dev"
cmd  = "npm run dev"
ready = "Local:"
```

[configuration.md](configuration.md) owns the key table and the file's
lifecycle (read once at workspace creation, never watched, never rewritten).
What each field *does*:

- **`name`** is the rail label and nothing else — it is not an identifier, it is
  not deduplicated, and two rows may share one. The exception is the literal
  name `shell`, which is treated as "unnamed": the daemon relabels those rows
  with whatever the pane's tty currently has in the foreground, so six shells
  are not six identical rows. That probe is a `/proc` read cached for 500 ms,
  capped at 512 characters, and it deliberately answers *nothing* when the
  foreground process is a login shell sitting at its prompt with only flags —
  so an idle shell keeps reading `shell` rather than `-zsh`.
- **`cmd`** runs through `$SHELL -c`, in the workspace directory. The whole
  string becomes the pane's command label, not just its first word: a row
  reading `sudo` says far less than `sudo apt-get update -y`. One special case
  — if `cmd` is exactly the resolved default shell, the pane is spawned as a
  plain interactive shell instead of `shell -c shell`.
- **`ready`** is a substring. See [The `ready` marker](#the-ready-marker).

There is no `env`, no `cwd` and no `restart` policy field. `[[agents]] env`
exists; `[[processes]]` has no equivalent, so put the variables in the command
(`FOO=1 npm run dev`) — it is going through a shell anyway.

## Status

One token per row, and it is computed fresh every time the process list is
built, from two inputs: the child's exit code, and whether the `ready` marker
has been seen.

| token | colour | what it means | what computes it |
|---|---|---|---|
| `ok` | ok | the marker was seen, or the row is a shell | `ready_seen`, and the child is alive |
| `...` | attention | output is actively streaming | `Attention::Working`, and not yet `ok` |
| `run` | ink | alive, and nothing has said otherwise | the fallback |
| `done` | ink | exited 0 | the exit code |
| `FAIL(n)` | danger | exited non-zero | the exit code |

The order in that table is the order the match runs in, and two things follow
from it:

- **`ok` outranks `...`.** Once a process has signalled ready it never goes back
  to showing activity, however much it prints. The marker is about startup, and
  a row that flickered between `ok` and `...` all afternoon would be reporting
  noise.
- **A row with no marker can never leave `run`/`...`** until it exits. That is
  the right shape for a server — it does not "finish" — and it is why a `ready`
  is worth writing for anything whose startup you want to watch complete.

`...` is the same "working" signal the AGENTS rail uses: output arriving within
the last two seconds **and** streaming for at least one second. The one-second
floor exists because opening a pane from another client resizes it and the
child answers the `SIGWINCH` with a full repaint; on raw recency that reads as
work. A process also inherits the built-in busy markers agents use, so a
program that prints `Ctrl+C to stop` in the bottom eight rows of its screen
will read `...` for as long as that line is visible.

Collapsed (`alt-z`), the same three states become one glyph in the left gutter:
`P✓` for `ok` or `done`, `P✗` for `FAIL`, `P·` for everything else.

No client shows a rolled-up "3 ok, 1 failed" count. The one aggregate that
exists is an exit code: `butai process status` exits non-zero if any row has
failed, which is what makes it usable in a condition —
`butai process status -q || butai agent send 7 "the dev server died"`. Flags are
[cli.md](cli.md)'s.

## The `ready` marker

`ready` is a **case-sensitive substring, matched against the raw output
stream** — not against the rendered screen. The scan runs in the daemon's
output loop, once per coalesced burst, before the bytes reach the VT emulator.

Three consequences, in descending order of how likely they are to surprise you:

1. **It must be contiguous in the bytes.** A marker the program paints in two
   colours has an escape sequence in the middle of it and will not match. Pick
   a run of plain text: `Local:` rather than the whole decorated banner.
2. **It never un-matches.** `ready_seen` is a latch. Once set, the scan stops
   running for that pane entirely.
3. **It is not lowercased.** The agent detection markers are matched
   case-insensitively; this one is not.

### `ready_carry`, and the boundary problem it solves

Output arrives coalesced per drain, and a server's startup banner is routinely
written in more than one `write` — or lands across a 64 KiB read boundary. A
naive "does this burst contain the marker?" check misses it in exactly that
case, and the row then stays `run` **forever** even though the marker was
printed, because the latch above means there is no second chance.

So `ProcMeta` keeps `ready_carry`: the tail of the previous burst, prepended to
the next one before the search. It is bounded by the marker's own length minus
one — just enough to complete a split match, and nothing to tune.

### When the marker is never seen

Nothing happens. The row stays `run` (or `...` while output arrives) until the
process exits, and then reports `done` or `FAIL(n)` like any other. There is no
timeout, no warning, and no "readiness failed" state — a marker that never
appears is indistinguishable from a server that is still starting, and butai
does not guess between them. If a row you expected to go green never does,
suspect the escape-sequence case above first.

## Starting, stopping, restarting

**Start** puts the new row at the end of the rail. A process from `.butai.toml`
does not take the stage — the workspace's opening shell keeps it, regardless of
how many processes and autostart agents come up behind it. A process *you*
start does take the stage, because you asked for it.

**Stop** is `x` on the row, `DELETE …/panes/{pane}`, or the row menu's *Close*.
It is immediate and leaves nothing behind: the pane is forgotten first and the
child killed as it is dropped, so there is no corpse row and no exit code to
read. If you want to see how a process died, let it die on its own.

Dropping the pane closes the master side of the PTY and sends the child a
`SIGHUP`. The child is a session leader with the pane as its controlling
terminal, so the hangup reaches its foreground process group — but a program
that installs a `SIGHUP` handler, or a grandchild the shell put in the
background, can outlive the row.

**Restart** is `r`, the row menu's *Restart*, or
`POST …/processes/{pane}/restart`. It kills the old child, spawns a new one
with the same name, command and `ready`, and resets `ready_seen` and
`ready_carry` so the marker has to be seen again. Three details that are
visible:

- **Output does not survive it.** The new pane starts blank. Restore replays a
  saved screen; restart deliberately does not — you asked for a fresh run, and
  the previous one's log above it would read as part of it.
- **The pane id changes.** A client holding the old id has to let go; the TUI
  clears the stage when the restarted pane was on it.
- **The row moves to the bottom of the rail**, because the restart re-appends
  it. There is no way to hold a position.

A process that exits **cleanly on its own** (code 0) is removed from the rail
automatically. A process that **fails** stays as a red `FAIL(n)` corpse so its
last output can still be read, and `x` dismisses it. Any row named `shell` — the
one a workspace opens with, and every one `t` adds — is the exception: it is
removed however it exited, because a shell that failed is you mistyping `exit`.
And when such a row is the *only* thing left in the workspace — no agents, no
other processes — leaving it closes the workspace.

Processes never produce notifications. The notification feed is agents only, so
a build that fails while you are looking at another tab is a red row you find,
not an alert that finds you.

## Across a daemon restart

Two stores, and processes are in both.

`~/.butai/session.json` holds each workspace's process list — name, command,
`ready`, and the dump file that goes with it. It is rewritten synchronously
whenever a workspace opens or closes and whenever the roster changes.

`~/.butai/panes/<slug>-<hash>/proc-<i>.bin` holds that row's recent output. Each
file starts `butai-dump 1 <cols> <rows>\n` and then carries the raw byte
stream, so the geometry needed to replay it travels with it rather than
depending on a second file staying in step. Dumps are sampled on the ~2 second
telemetry tick and once more as the daemon goes down; a hard crash therefore
costs the last couple of seconds, which is the bound the workspace list already
had.

On the way back up, each saved process is **spawned again** — the command runs
from the top — and its dump is replayed into the fresh emulator first, at the
size it was recorded at, then resized. So the pane you return to reads as the
pane you left, with the previous run's log above the new one's first line.
Nothing else survives: the child is new, its pid is new, and anything the old
run held in memory is gone.

**Restore replays the persisted list, not `.butai.toml`'s block.** This is the
rule that makes the two agree instead of fighting: the saved list already
contains the workspace file's processes — they were spawned from it and then
recorded — plus anything you started by hand, minus anything you closed.
Replaying the file on top would duplicate the first group and lose the other
two. So a process you deleted from `.butai.toml` does not come back, and a
process you started with `t` does.

Two edges worth knowing:

- A workspace whose directory does not resolve at startup is **deferred, not
  dropped** — an unmounted share reads exactly like a deleted folder, and this
  runs when mounts are least likely to be up. Its entry and its dumps are kept
  and written back out, so the next start can rebuild it.
- **A restored row without a `ready` marker comes back as `ok`**, where before
  the restart it read `run`. Rows *with* a marker restart at `run` and have to
  earn `ok` again, which is correct; the marker-less case takes the same
  "nothing to wait for" branch a shell does. It is cosmetic — nothing reads
  `ok` except the rail — but it means the token is not stable across a daemon
  restart.

Set `[general] restore_bytes = 0` to switch the capture off entirely. Panes
then come back blank, which is what they did before any of this existed.

## Working directory and environment

Every process runs in the **workspace's directory** — the project root, or
whichever path the workspace was opened on. There is no per-process `cwd`; `cd`
in the command if you need one (`cd web && npm run dev`), which is what the
docker log followers do.

Every pane the daemon spawns — process, agent or shell — carries:

| variable | value |
|---|---|
| `TERM` | `xterm-256color` |
| `COLORTERM` | `truecolor` |
| `BUTAI` | the socket this daemon bound; the nesting guard reads it |
| `BUTAI_SOCKET` | the same path, for `--socket` |
| `BUTAI_PANE` | this pane's id |
| `BUTAI_WORKSPACE` | the workspace it belongs to |

`BUTAI_SOCKET` and `BUTAI_WORKSPACE` are what make `butai` inside a pane act on
its own daemon and its own workspace without being told. [agents.md](agents.md)
is the surface built on that.

**`PATH` is repaired.** `$SHELL -c` is a *non-interactive* shell: it sources
none of the files that put `~/.local/bin`, `~/.bun/bin` or nvm's node on the
path, and the daemon itself was probably started by a session manager rather
than your login shell. So the daemon prepends the directories a login shell
would have added — only the ones that exist, only the ones not already present,
and nvm's version directories all-or-nothing, because a `PATH` that already
names one was written by your own hook and adding the others would put an older
node in front of it. A daemon started from a login shell gets its `PATH` back
byte for byte.

That is why `npm run dev` in a `.butai.toml` finds `npm`, and why the failure it
prevents is confusing rather than obviously environmental: the same line works
when you type it into a pane, where the shell is interactive and has read its
rc file.

Everything else is inherited from the daemon's own environment, which is
whatever started it.

## Output

A process pane is a terminal pane, so all of it applies: it is a real PTY with
a real VT emulator behind it, and it is **interactive**. Stage it and type —
`Ctrl-C` interrupts the build, `q` quits the pager, arrow keys reach the
program. The daemon answers cursor-position queries on the child's behalf so
programs that ask and then block do not stall.

| what | how much | where |
|---|---|---|
| scrollback | `[general] scrollback` lines, default 5000 | in memory, per pane |
| restart capture | `[general] restore_bytes` bytes, default 256 KiB | `~/.butai/panes/<key>/proc-<i>.bin` |

The two budgets are counted differently on purpose. Scrollback is what you
scroll through, so lines are the unit. The capture is the untouched byte stream
— a pane redrawing a full-screen TUI spends far more per line than one printing
log text — so bytes are what actually bounds the cost. 256 KiB is a few screens
of a redraw-heavy program and well over a thousand lines of plain output.

Dump files are keyed by **position** (`proc-0.bin`, `proc-1.bin`, …), written by
the same walk that builds the persisted list so the two cannot drift. Files
belonging to rows that have since closed are pruned on each pass, and so are
whole directories for closed workspaces. The directory name is a readable slug
of the project directory *plus* a hash of its full path: the slug alone would
collide across the several `.../src` directories you have open at once and
those workspaces would replay each other's output, and the hash alone would be
unreadable in a directory you are expected to be able to delete by hand.

To read a process's output without attaching to it — from a script, or from an
agent in the next pane — use `butai pane read <id>`, or
`GET …/panes/{pane}/output`. It is a query: it does not resize the pane or
clear its bell.

## Docker

Containers are **telemetry, not panes**. Every ~2 seconds the daemon runs
`docker ps -a` and reads each container's name, state, compose project and
compose working directory, capped at 64 rows. Docker missing, failing or taking
longer than two seconds yields nothing at all, which is why the SYSTEM rail
simply has no container line on a machine without it.

Those rows are grouped into **stacks**: one per compose project, plus one
single-member stack per standalone container. A stack with no running container
is dropped from the list entirely. A stack is "yours" when its compose working
directory is at, under, or over the workspace's directory, and yours sort
first — the DOCKER space shows only those, falling back to every started stack
when none match, so the page is never mysteriously empty.

**Following a container's logs is an ordinary process pane.** The client posts
`docker logs -f --tail 200 <name>` (or `cd <workdir> && docker compose --ansi
always logs -f --tail 200` for a stack) to `POST …/processes` and streams the
resulting pane like any other. There is no docker-logs message on the wire and
there does not need to be one: following a log is running a program and
watching its output, which the protocol has always been able to express. The
same is true of the other verbs — restart is a pane running `docker restart`,
and `s` opens a pane running `docker exec -it <name> sh`.

This is the PTY-versus-JSON rule doing its job. Docker logs are bytes from a
program, so they take the path bytes take; nothing about them justifies a
second one. It is also why every client gets the feature for free — the browser
client and the Mac client do exactly this, and neither has a VT parser.

The cost is that followers are real rows in a real rail, and they have to be
cleaned up. Both clients kill theirs when you leave the page and when the
client goes away; the browser also fires the kill on `pagehide` with
`fetch(…, {keepalive: true})`, since the kill is a `DELETE` and `sendBeacon` is
POST-only. Without that, a `docker logs -f` outlives every detach and the
PROCESSES rail fills with followers nobody asked for. The browser hides its
`logs:` panes from the rail; the terminal names them `logs <label>` and shows
them, so you can see the one that is running.

(The daemon's `Workspace` still carries a `docker_logs` slot from when the
follower was special-cased. Nothing assigns it — the followers are in
`processes` with everything else.)

## Processes, agents and terminals

All three are PTY-backed panes rendered by the same emulator. What differs is
the metadata beside them and what the daemon does with it.

| | process | agent | plain shell |
|---|---|---|---|
| declared in | `.butai.toml` `[[processes]]`, or by hand | `~/.butai/config.toml` `[[agents]]` | nothing — `t` |
| launched via | `$SHELL -c <cmd>` | `command` + `args`, resolved on `PATH` | the shell, interactively |
| lives in | the PROCESSES rail | the AGENTS rail | the PROCESSES rail |
| status comes from | exit code + `ready` | the footer band of its own screen | the same as a process (always `ok`) |
| status values | `ok` `run` `...` `done` `FAIL(n)` | `waiting` `working` `finished` `idle` `exited` | `ok`, then `done`/`FAIL(n)` |
| configurable detection | — | `waiting_pattern`, `busy_pattern` | — |
| configurable env | — | `[[agents]] env` | — |
| clean exit (0) | row removed | row removed | row removed |
| non-zero exit | red corpse row | red corpse row | row removed |
| notification on exit | no | yes | no |
| restart verb | `r` | — (kill and spawn again) | `r` |
| restored with its output | yes | yes | yes |
| conversation reopened | n/a | yes, by minted id | n/a |
| row label | its `name`, or the foreground command when named `shell` | its live OSC title | the foreground command |

The one asymmetry that catches people: **there is no `r` for an agent.** A
process is a command you can run again; an agent is a conversation, and
restarting one means deciding what happens to that conversation — which is what
the restore path does with `resume_args` and a named session id, and which is
not a thing to do by reflex on a keystroke.

## Where this lives

| section | file |
|---|---|
| `ProcMeta`, `ready_carry`, the workspace's process list, `forget_pane` | `crates/butai-server/src/workbench.rs` |
| `[[processes]]` parsing, `ProcDef`, `scrollback`, `restore_bytes` | `crates/butai-server/src/config.rs` |
| the `ready` scan and its burst-boundary carry | `crates/butai-server/src/core.rs` (`on_pty_output`) |
| spawning, staging, the shell special case | `crates/butai-server/src/core.rs` (`new_process`, `spawn_process`, `spawn_terminal_replaying`) |
| the status token and the shell relabel | `crates/butai-server/src/core.rs` (`build_processes`) |
| restart, kill, and the API arms behind them | `crates/butai-server/src/core.rs` (`api_restart_process`, `KillPane`, `drop_pane`) |
| exit handling, corpses, the lone-shell rule | `crates/butai-server/src/core.rs` (`on_pane_exited`) |
| `session.json`, dump files, pruning, restore | `crates/butai-server/src/core.rs` (`persist_session`, `capture_panes`, `restore_session`, `rebuild_workspace_panes`) |
| the PTY, the child, `SIGHUP` on drop, exit codes | `crates/butai-server/src/pane/terminal.rs` |
| the environment every pane gets, and the `PATH` repair | `crates/butai-server/src/pane/terminal.rs` (`child_path`, `login_bin_dirs`) |
| the output ring and the dump header | `crates/butai-server/src/pane/terminal.rs` (`OutputHistory`, `encode_dump`, `decode_dump`) |
| the foreground-command probe behind a `shell` row's label | `crates/butai-server/src/pane/terminal.rs` (`foreground_cmdline`, `display_argv`) |
| the `...` signal it shares with the agents rail | `crates/butai-server/src/pane/terminal.rs` (`attention`, `sustained_output`) |
| `docker ps` sampling, container fields, the timeout | `crates/butai-server/src/sys.rs` (`read_docker`) |
| compose grouping, `mine`, the docker row model | `crates/butai-server/src/workbench.rs` (`SysStats::stacks`, `docker_rows`) |
| routes, bodies and status codes | `crates/butai-server/src/http_conn.rs`, [protocol.md](protocol.md) |
| the rail: rows, the status token, the zen glyphs | `crates/butai-client/src/chrome/mod.rs` (`proc_status`, `draw_left_rail`, `draw_left_zen`) |
| `t` / `r` / `x` / `m`, and the flows behind them | `crates/butai-client/src/workbench.rs` (`run_process`, `restart_process`, `kill_process`), `crates/butai-client/src/verbs.rs` |
| the docker space: commands, the follower, its cleanup | `crates/butai-client/src/workbench.rs` (`docker_command`, `Flow::RunProcess`) |
| `butai process ls` / `status` / `start` | `crates/butai/src/cli/process.rs`, [cli.md](cli.md) |
| the browser client's port of the docker page | `web/butai-docker.js`, `web/butai-app.js`, [`web/README.md`](../web/README.md) |
