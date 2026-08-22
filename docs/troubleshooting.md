# Troubleshooting

Symptom, cause, fix — for the failures butai can actually produce. Every
message quoted here is one the binary prints; if you are looking at something
that is not on this page, the log is the next stop (see
[Logs](#logs-and-raising-the-level) at the end).

## Nothing runs

### `command not found: butai`

The install directory is not on your `PATH`. `scripts/install.sh` writes to
`/usr/local/bin` if it can, otherwise `~/.local/bin`, which many distributions
do not add to a non-login shell's `PATH`. Add it, or move the binary.

### `daemon did not come up on <path>`

The client forked a daemon and it never bound the socket. Every command that
talks to a daemon does this — `butai ws ls` on a machine with none running
starts one — so this can appear from a command that looks read-only.

Run the daemon in the foreground to see why:

```sh
butai daemon
```

The usual causes, in order:

| Cause | Sign | Fix |
| --- | --- | --- |
| The socket directory is not writable | `create <dir>` or `bind <path>` in the error | Fix ownership of `~/.butai`, or point `BUTAI_SOCKET` elsewhere |
| The socket path is too long | `bind` fails with a path-length error | See [Socket path too long](#socket-path-too-long) |
| Another daemon holds the lock | `another butai daemon is already running` | See below |
| A binary that cannot execute | nothing at all in the log | Check the architecture matches, and that `noexec` is not set on the filesystem |

### `another butai daemon is already running`

The daemon takes a non-blocking exclusive `flock` on `butai.lock` beside the
socket and holds it for its whole life. This message means the lock is held, so
a daemon really is up — the client should have connected instead of spawning.
That happens when the socket path resolves differently for the two of them.
Check what each is using:

```sh
butai whoami          # the socket this invocation would talk to
echo "$BUTAI_SOCKET"
```

### Socket path too long

Unix domain socket paths are capped by `SUN_LEN` — around 104 bytes on macOS,
108 on Linux. A `HOME` deep enough to push `~/.butai/butai.sock` past that
cannot bind. This bites in tests and containers far more than in real use.

Point the socket somewhere short:

```sh
BUTAI_SOCKET=/tmp/b.sock butai
```

### A socket file exists but nothing answers

A crash leaves the socket file behind. It is not a running daemon: the file is
only meaningful while a daemon holds the lock beside it, and the next daemon to
start removes any stale one before binding. So the fix is to start one:

```sh
butai daemon      # or just `butai`, which spawns one
```

Deleting the socket by hand is safe when no daemon is running and pointless
when one is.

## The terminal is wrong after a crash

### Mouse codes on screen, no echo, a wedged shell

butai puts the terminal in raw mode and enables mouse and bracketed-paste
reporting. It restores all of that on every ordinary exit and on nine signals,
but `SIGKILL` cannot be caught, and neither can a crash of the terminal
emulator itself.

```sh
butai reset
```

That writes the same restore sequence the normal teardown does — encodings off,
every tracking mode off, then the screen state, ending with the cursor shape put
back. It also unwedges a terminal an *older* butai left broken.

It must be run **on the terminal you want to fix**:

```
not a terminal (run `butai reset` from the terminal you want to fix)
```

means stdout was redirected. `butai reset` contacts no daemon and starts
nothing.

**Known hole:** `SIGTSTP`. Backgrounding butai leaves the terminal raw until it
is resumed. Job control cannot reach butai through raw mode's disabled `ISIG`,
so this only happens if you send `kill -TSTP` explicitly.

## Attaching

### `already inside this butai (<socket>) — attach a different --socket, unset BUTAI to force, or detach first`

You ran `butai` inside a butai pane. The daemon sets `$BUTAI` in every pane it
spawns to the socket it actually bound, and an attaching command refuses when
that resolves to the same daemon — nesting a workbench inside its own pane
gives you two clients fighting over one terminal.

Attaching a *different* daemon from inside a pane is allowed on purpose: a
remote workbench opened from a local pane is a different daemon drawing a
different screen. `butai standalone` refuses any nesting at all, because it has
no socket identity to compare against.

### A stale daemon serving old behaviour

Installing a new binary does **not** restart anything. The daemon you are
talking to is the one that was running when it started, which may be an older
build than the binary on your `PATH`. A "bug" that the source says cannot
happen is usually this.

```sh
butai --version                    # the binary
butai kill-server                  # stop the daemon; workspaces come back
butai                              # start again on the new build
```

`kill-server` snapshots the open workspaces and their pane output first, so this
is not destructive. `kill-server --clear` is the one that forgets them.

### The connection drops mid-session

`daemon closed the connection`, `connection closed before handshake`, or a stage
that goes blank. Either the daemon exited, or the framed stream desynchronised.

A daemon that exited leaves nothing listening; a reattach starts a new one and
restores. A protocol-level mismatch is more subtle: an undecodable frame is
skipped rather than fatal, precisely so a version-skewed peer does not cause a
reconnect loop, but sixteen undecodable frames in a row drop the connection. If
you see repeated reconnects, check both ends' versions —
[remote.md](remote.md) covers skew.

### A remote machine did not come back after the laptop slept

It should now come back on its own within a few seconds of the link being
noticed — the footer says `<host> went away — reconnecting`, then `<host> is
back` — because the client rebuilds the `ssh -L` rather than only retrying the
socket ssh took with it. Attempts back off from 5s to 5 minutes per machine, so
a machine that was off for an hour can take up to five minutes after it returns.

**If it stays down**, the dial is failing rather than not being attempted. The
footer carries ssh's own last line; the same dial by hand shows the whole thing:

```
ssh -o BatchMode=yes <your ssh_args> <host> butai ls
```

`BatchMode=yes` is the usual answer — a key needing a passphrase cannot be used
from inside the workbench, because there is nowhere to type it. Use an agent.

**On a butai older than this behaviour**, the machine never returned at all and
`alt-h` refused it with `<host> is already in the tab bar`. The workaround was
`alt-h` → disconnect it → `alt-h` → connect again, and failing that a restart.
If you still see that refusal, check the client's version.

## Keys

### `Alt` does nothing on macOS

Option is a compose key by default: Option-o types `ø` and no terminal reports
Alt at all. butai reads those characters back, so Option-o *is* `alt-o` and
nothing needs configuring.

Option-e and Option-n are **dead** keys — they emit nothing until the next
keystroke, so they cannot be recovered. Use `C-b n` to open a workspace and
`alt-o` for files, or set your terminal to send a real Alt:

```
Terminal.app    Settings › Profiles › Keyboard › Use Option as Meta Key
iTerm2          Profiles › Keys › Left Option Key › Esc+
Ghostty         macos-option-as-alt = true
kitty           macos_option_as_alt = yes
```

Inside tmux, keep `xterm-keys` on so it passes Alt through rather than eating
it.

### A key from `[keys]` does nothing

`[keys]` configures the **prefix layer only**. An entry names the key you press
*after* the prefix, so `F5 = "process build cargo build"` is `C-b F5`, not a
bare `F5`. The Alt layer is built in and not reconfigurable. See
[keys.md](keys.md#changing-them).

An entry that does not parse is a warning on the SETTINGS page, not a refusal
to start — and that page reports how many keys are bound and how many came from
your config, which is the question you have when a key does something you did
not expect.

### A bare key does nothing

Bare keys need the cursor **off** the stage. On the stage every key belongs to
the program, which is what makes it a terminal and not a preview. `alt-esc`
leaves the stage.

## Colours

### The theme did not load

```
theme "<name>" not found (built-ins: blueprint-dark, blueprint-light, …); using blueprint-dark
```

The name matched neither a built-in nor `~/.butai/themes/<name>.toml`. Names are
exact and the file extension is required on disk but not in the config.

A theme is the **client's**, read from `config.toml` at start rather than
switched at runtime — which is what lets one terminal be dark and another light
on the same daemon. A running client will not pick up an edited theme file.

### Colours are wrong, or your colorscheme is being overridden

Every theme except `terminal` sends 24-bit colour, which overrides your
terminal's palette by design. Set `terminal` to inherit your own colours
instead. [theming.md](theming.md) has the role list.

Pane *content* is never themed — a terminal's cells carry the program's own
colours, and butai passes exact RGB through without rounding it to a palette
entry.

## Agents

### An agent's status never changes

The rails read an agent's state off its own screen — the interrupt hint in its
footer, the shape of a confirmation prompt — not from any wrapper protocol. Two
failure shapes:

| Symptom | Cause |
| --- | --- |
| Always `[ ]` idle, even mid-turn | Its busy marker is not in the built-in table, and its output is not sustained enough to trigger the fallback |
| Stuck on `[~]` working, never finishes | Something matching a busy marker is pinned on screen, so the turn never appears to end — and a pane pinned to busy never fires its finished notification |

Both are fixed with per-agent overrides. Each pattern **replaces** the built-in
table for that one signal rather than adding to it, which is deliberate: an
additive pattern can only ever add matches, and taking back a false positive is
the harder half of the problem.

```toml
[[agents]]
name = "mycli"
command = "mycli"
busy_pattern = "esc to stop"
waiting_pattern = "\\[y/n\\]"
```

A pattern that does not compile is dropped with a warning in the daemon log and
the built-in markers stay in charge — falling back costs accuracy, while
refusing to start would cost you the agent.

Only the bottom eight rows of the pane are scanned, so a marker your agent
prints higher up will not be seen. See
[architecture.md](architecture.md#agent-status-detection) for exactly what is
matched.

### A restored agent died immediately

butai names each agent's conversation at launch and asks the CLI to reopen that
one on restore. A conversation does not exist until the agent is spoken to —
the CLIs create the transcript on the first user message — so reopening an
unwritten id fails and the process exits. butai tracks whether a pane was ever
typed into and restarts those fresh instead, and gives a failed resume one
fallback start within ten seconds. If an agent still dies on restore, its CLI's
resume flag or session-id spelling has probably changed; check the `[[agents]]`
block's `resume_args` against the CLI's current help.

## Processes

### A process never reaches `ok`

Without a `ready` marker a row reads `run` for as long as the command lives —
it never reaches `ok`. That is the right shape for a server. Add a `ready`
substring to anything whose startup you want to watch finish.

If you *have* set one and the row still says `run`:

- `ready` is matched **case-sensitively** against the raw output stream. A
  marker split by a colour escape sequence in the middle will never match, even
  though it looks right on screen.
- It is a latch with no timeout: once missed, there is no second chance short of
  a restart.

### A process will not start

`cmd` runs through a **non-interactive** shell, which sources none of the files
that put `~/.local/bin` or nvm on your `PATH`. butai puts back the directories a
login shell would have added — in front, and only the ones that exist — so
`npm run dev` usually works even when the daemon was started from a session
manager. A tool installed somewhere more exotic still needs an absolute path.

A malformed `.butai.toml` yields **zero** configured processes and the workspace
still opens with its shell; the parse warning goes to the daemon log only.
`[[processes]]` entries require both `name` and `cmd`, and a missing one fails
the whole file.

### Editing `.butai.toml` changed nothing

It is read **once**, when the workspace is created, and is not watched.
`:reload-config` does not touch it. Close the workspace and open it again.

A daemon restart does not read it either — restore replays the processes that
were actually running, so one you deleted from the file does not come back and
one you started by hand is not lost.

### A killed process is still running

Kill sends `SIGHUP` to the session leader, not `SIGKILL`. A process with a
handler, or a grandchild that was backgrounded away from the session, can
outlive the row.

## Git

### A git operation fails instead of prompting

butai runs git with prompts disabled — `GIT_TERMINAL_PROMPT=0`,
`GIT_ASKPASS=/bin/false`, `SSH_ASKPASS=/bin/false`, and `-c core.askPass=` to
override any `core.askPass` in your own config. This is deliberate: a daemon
that can be blocked on an invisible password prompt is a daemon that hangs.

Configure ssh-agent or a credential helper rather than expecting a prompt.

### `a git operation is already running: <kind>`

The write lock is held. It is keyed by **repository root**, not by workspace,
because two workspaces can be open on one worktree and letting them interleave
a rebase is how work gets lost. Wait for the running operation, or cancel it
from the git menu.

### `workspace <n> is not a git repository`

The workspace directory is not inside a repository, so there is no CHANGES rail
and the git routes refuse. butai re-probes periodically with a backoff from two
seconds up to a minute, so a `git init` is noticed without a restart — the
backoff exists because on a network mount every failed probe is a full
parent-directory stat walk.

### The CHANGES rail is showing something that is no longer true

Status is a cached off-thread scan, not a read-through. Mutations move their own
rows immediately and the authoritative rows arrive with the next scan, so a
stale rail means a scan is late or failed. `r` in the CHANGES rail forces a
refresh. On a network-mounted worktree, expect the scan to be the slow part.

## Workspaces

### A workspace's directory has moved

It is **not** dropped. A workspace whose directory does not resolve at startup
is kept in `session.json` and written back out by the next persist, so an
unmounted share does not silently erase your tab bar. Mount it and restart, or
close the workspace to forget it.

### `no workspace named "x" (open: …)` / `(none are open)`

The name is matched case-sensitively against the open workspaces. `butai ws ls`
lists them. Pane *names* are matched case-insensitively by substring; only the
workspace scope is exact.

## Clients other than the terminal

### The web client shows nothing

The browser cannot open a Unix socket, so the bridge in `web/server/` does it.
Check, in order:

1. A daemon is running and the bridge can reach its socket. The bridge prints
   every socket it is bridging on its startup line — read it.
2. The bridge is running and serving `/`. In development that is
   `bun run bridge` on 8080; the built client is `bun run build` first, because
   the bridge serves `dist/` and there is nothing to serve without it.
3. The bridge's `/api/*` relays to the daemon's `/v1/*` — `curl
   localhost:<port>/api/state` should return the whole-world snapshot.

`web/README.md` has the route table and the bridge's own diagnostics. If the
structural pages work and only the live pane is blank, the WebSocket relay is
the part that is broken — that is the framed protocol, not REST.

### The web client is a blank page, and the console says nothing useful

The client is bundled, so a syntax error cannot reach the browser: `bun run
build` fails first, and `bun run typecheck` fails before that. A blank page from
a *built* client is therefore a serving problem, not a parse problem.

The usual cause is a stale or missing `dist/`. The bridge falls through to a 404
when `dist/index.html` is not there, which reads as an empty page rather than as
a build you forgot to run. `bun run dev` avoids the question entirely — Vite
serves from source on 5173 and proxies `/api` and `/ws` to the bridge on 8080.

### `bun test` fails with a missing binary

The client's tests drive a **real daemon**, so they need a `butai` binary. They
default to `/var/tmp/butai-probe/butai`; `BUTAI_BIN=<path>` overrides it. Build
one with `cargo build -p butai` and copy it somewhere private — a shared
`CARGO_TARGET_DIR` holds one `debug/butai`, and another worktree relinking it
mid-test reproduces whatever you just fixed.

It is `BUTAI_BIN`, not `BUTAI`. A butai pane already exports `BUTAI`, set to the
**socket** path, so a `${BUTAI:-…}` default resolves to a socket and the harness
tries to execute it.

## Logs and raising the level

The daemon logs to `~/.butai/logs/daemon.log`, rotated daily, with ANSI off.
The default level is `info` and `RUST_LOG` overrides it:

```sh
butai kill-server
RUST_LOG=debug butai daemon        # foreground, so you see it live
```

The level is read when the daemon starts, so it cannot be raised on a running
one.

Two log lines worth recognising:

| Line | Means |
| --- | --- |
| `core loop blocked for <n>ms` | One pass of the event loop took over 50 ms. That loop owns every pane, so this freezes all of them at once. Blocking filesystem work on a network-mounted workspace is the usual cause. |
| `client N: skipping undecodable frame (n)` | The peer sent a message this build does not know. Expected across versions; a run of sixteen drops the connection. |

## Collecting a bug report

```sh
butai --version
butai whoami                         # the resolved socket, in or out of a pane
butai ls                             # the open workspaces
butai ws ls --json                   # the same, with counts, as the daemon sees it
tail -n 200 ~/.butai/logs/daemon.log
```

Include the terminal emulator and its version, whether you are inside tmux or
ssh, and — if the daemon has been up a while — whether the binary on `PATH` is
newer than the running daemon:

```sh
ls -l "$(command -v butai)"
```

Redact paths and workspace names if the repository is private; `butai ls` prints
directory paths verbatim.

## Where this lives

| Section | Source |
| --- | --- |
| Daemon startup, the lock, log file and level | `crates/butai-server/src/daemon.rs` |
| Socket path resolution and `~/.butai` layout | `crates/butai-protocol/src/paths.rs` |
| The nesting guard and daemon auto-spawn | `crates/butai/src/cli/mod.rs`, `crates/butai-client/src/conn.rs` |
| `butai reset` and the restore sequence | `crates/butai-client/src/term.rs` |
| Exit codes | `crates/butai/src/exit.rs` |
| Frame skipping and the bad-frame cap | `crates/butai-protocol/src/framing.rs`, `crates/butai-server/src/client_conn.rs` |
| Theme resolution and its warning | `crates/butai-client/src/theme.rs` |
| `[keys]` dispatch through the prefix | `crates/butai-client/src/keymap.rs`, `workbench.rs` (`handle_prefix`) |
| Agent markers and pattern overrides | `crates/butai-server/src/pane/terminal.rs` |
| Resume, `spoke`, and the retry window | `crates/butai-server/src/core.rs` |
| `ready` matching and process spawn `PATH` | `crates/butai-server/src/core.rs`, `pane/terminal.rs` |
| Git prompt suppression and the write lock | `crates/butai-server/src/git_op.rs`, `core.rs` |
| Repo re-probe backoff | `crates/butai-server/src/core.rs` (`attach_new_repos`) |
| Deferred workspaces | `crates/butai-server/src/core.rs` (`restore_session`) |
| The web bridge | `web/server/`, [`web/README.md`](../web/README.md) |
| The web client's build, dev server and tests | `web/package.json`, `web/vite.config.ts`, `web/test/` |
