# Getting started

One session with butai, end to end: install it, open a project, put an agent to
work, bring your dev server and tests up beside it, review the diff, commit, and
walk away without stopping any of it. Each stage says what to run, what you
should see, and what just happened.

This is the walkthrough. The reference pages own the detail, and each stage
links to the one that does.

## 1. Install

```sh
curl -fsSL https://raw.githubusercontent.com/dieterpl/butai/main/scripts/install.sh | sh
```

It detects your platform, downloads the matching prebuilt binary, verifies it
against the release's `SHA256SUMS`, and puts `butai` in `/usr/local/bin` or
`~/.local/bin`. There is no runtime and no dependency — it is one binary.

Confirm it:

```sh
butai --version
```

Other routes — a release tarball, `cargo install`, pinning a version — are in
the [README](../README.md#install). Linux and macOS only; Windows is a
documented non-goal, though butai runs under WSL2 as an ordinary Linux binary.

> **Nothing happened when you ran it?** If `butai` is not found, the install
> directory is not on your `PATH`. See
> [troubleshooting.md](troubleshooting.md).

## 2. Open a project

```sh
cd ~/Projects/my-app
butai
```

That is the whole ceremony. Bare `butai` attaches to the latest session and
creates one if there is none, with your current directory as the first
workspace.

Behind that one word: a daemon started in the background if none was running,
and `~/.butai/` was created to hold everything it keeps — `config.toml`,
`themes/`, `logs/`, `session.json`, per-pane output dumps, and the socket
itself. **Nothing is written into your project directory.** A `.butai.toml`
there is read, never rewritten.

What you are looking at is one fixed frame:

```
┌ my-app ─────────────────────────────────────────────────┬────────────────┐
│ AGENTS                                                  │ CHANGES        │
│   (empty)                                               │   (empty)      │
│ PROCESSES                     ┌──────────────────────┐  │                │
│   shell                       │                      │  │                │
│ SYSTEM                        │        stage         │  │                │
│   cpu ▁▂▃  ram ▁▁▂            │                      │  │                │
└─────────────────────────────  └──────────────────────┘ ─┴────────────────┘
```

Three columns, and they never move: **AGENTS**, **PROCESSES** and **SYSTEM** on
the left, **CHANGES** (git) on the right, one **stage** in the middle. You learn
the geography once and stop thinking about layout.

The rails are status lists, not terminals. The stage is where a real program
lives — the agent you are steering, a process log, a file, a diff.

Each project is its own tab. [workbench.md](workbench.md) is the full tour of
every surface.

## 3. Move around

One rule covers the whole keymap:

> **`Alt` is chrome. Everything else goes to the focused thing.**

An `Alt` key butai does not bind falls through, so `alt-b` and `alt-f` still
move by words in readline. If your terminal eats `Alt`, the prefix layer
(`C-b` by default) reaches the same verbs.

Six keys get you everywhere:

| Key | Does |
| --- | --- |
| `alt-a` / `alt-p` / `alt-g` | Focus the AGENTS, PROCESSES or CHANGES rail |
| `j` `k` | Move within a rail (cursor off the stage) |
| `enter` | Put the selected row on the stage, and focus it |
| `alt-esc` | Leave the stage, back to the chrome |
| `?` | The full key reference, in the app |
| `alt-d` | Detach |

Bare keys like `j` and `a` only work with the cursor **off** the stage. On it,
every key belongs to the program — that is what makes it a terminal and not a
preview.

The whole list is [keys.md](keys.md), including how to rebind any of it and
what to do about `Option` on a Mac.

## 4. Put an agent to work

Focus the AGENTS rail and spawn one:

- `alt-a` to focus the rail, then
- `A` to choose from the configured agents, or `a` to spawn the pinned one.

The agent is an ordinary CLI in a PTY pane — no wrapper protocol, full TUI
fidelity. Press `enter` on its row to put it on the stage and type at it exactly
as you would in any terminal.

Which agents are offered comes from `[[agents]]` blocks in
`~/.butai/config.toml`. See [configuration.md](configuration.md#agents) for the
block's fields.

From a script or another pane, the same thing without the UI:

```sh
butai agent spawn claude --background
```

### Reading the rail

The point of the rail is that you can scan it instead of visiting each agent:

| Marker | Means |
| --- | --- |
| `[!]` | **Needs you.** A confirmation or a question is on screen, or it rang the bell. |
| `[~]` | **Working.** Its status line offers a way to interrupt the turn, or it is streaming output. |
| `[ ]` | **Idle.** Waiting at its prompt with nothing to do. |

An agent that needs you also badges its workspace tab, so you can see it from
another project.

These are read off the agent's own screen — the interrupt hint in its footer,
the shape of a confirmation prompt — and debounced, so a pause mid-turn does not
read as "finished". [architecture.md](architecture.md#agent-status-detection)
explains exactly what is matched, and
[configuration.md](configuration.md#agents) covers the `waiting_pattern` /
`busy_pattern` overrides for a CLI whose spelling butai does not know.

> **A status that never changes?** That is almost always a marker mismatch, not
> a stuck agent. [troubleshooting.md](troubleshooting.md) has the two patterns
> that fix it.

## 5. Bring up your dev server and tests

Put a `.butai.toml` in the project root:

```toml
[[processes]]
name = "dev"
cmd = "npm run dev"
ready = "Local:"

[[processes]]
name = "test"
cmd = "npm test -- --watch"

[agents]
autostart = ["claude"]
```

`cmd` runs through `$SHELL -c` in the workspace directory. `ready` is a
substring of the process's own output that flips its row from `run` to `ok` —
give one to anything whose startup you want to watch finish. Without it a row
reads `run` for as long as the command lives, which is the right shape for a
server.

The file is **read once, when the workspace is created**. Editing it does
nothing to a running workspace, so close this one and open it again:

- `alt-x` closes the workspace (it asks first),
- `alt-n` opens one.

Now `dev` comes up and goes `ok` when it prints `Local:`, `test` runs, and a
failing suite shows as `FAIL(2)` on its row — a count you can see without
staging anything. `enter` on the row puts its output on the stage; `r` restarts
it, `x` kills it.

[processes.md](processes.md) covers the lifecycle, the status markers and what
survives a restart. [configuration.md](configuration.md) is the key-by-key
reference for both config files.

## 6. Review and commit

The CHANGES rail is the git working tree, permanently on screen. `alt-g` focuses
it.

| Key | Does |
| --- | --- |
| `d` | Diff the selected file, on the stage, syntax-highlighted |
| `s` | Stage it |
| `u` | Unstage it |
| `x` | Discard it |
| `c` | Commit |
| `C` | Stage everything and commit |
| `p` | Push |
| `g` | The git menu — branches, worktrees, stashes, remotes |

Inside a diff, `]` and `[` walk hunks and `space` stages the one under the
cursor. `v` starts a line selection, `space` picks lines, `enter` applies them.

Commit, and the rail empties. You never left the workbench, and the agent kept
working while you read.

[git.md](git.md) is the whole model: hunk staging, worktrees, remotes,
integrate, the commit graph, and what butai deliberately leaves to a shell.

## 7. Walk away and come back

`alt-d` detaches. Close the terminal entirely if you like.

```sh
butai            # later, same machine
```

Everything is where you left it. What survives:

- **Agents**, reopened on their *own* conversation — butai names each one at
  launch, so two agents in the same directory do not resume into each other's
  transcript.
- **Processes**, including ones you started by hand. Restore replays what was
  actually running, not `.butai.toml`'s autostart list, so a process you removed
  from the file does not come back.
- **Pane output**, replayed into the fresh panes, so a restored pane is not
  blank.
- **Your open workspaces and their order**, and which pane held each stage.

What does not: a process's own in-memory state. A restarted dev server is a
restarted dev server. A workspace whose directory has disappeared is kept in the
session file rather than dropped, so an unmounted share does not erase your tab
bar.

```sh
butai ls                    # the sessions
butai kill-server           # stop everything; workspaces come back next time
butai kill-server --clear   # ...and forget them, so the next start is empty
```

Every command and flag is in [cli.md](cli.md).

> **Terminal spewing mouse codes after a crash?** `butai reset` fixes it.

## 8. From another machine

butai never listens on TCP. Remote access rides SSH, and authorization is
filesystem permissions on the socket:

```sh
ssh dev-box butai
```

A daemon on the far end is started if it is not running, and a remote host's
projects can sit in your tab bar next to the local ones. See
[remote.md](remote.md) for `[[remote]]` config, forwarded sockets, the fleet,
and the security posture that goes with all of it.

## 9. Driving it from a script

The daemon serves a JSON API on the same socket, and the CLI is a client of it —
the way `docker` is a client of dockerd:

```sh
butai ws ls --json | jq '.[] | select(.waiting > 0) | .name'
```

Inside any pane, `$BUTAI_PANE`, `$BUTAI_WORKSPACE` and `$BUTAI_SOCKET` are
already set, so a script or an agent can drive the workbench around it with no
configuration:

```sh
butai agent wait 7 -q && ./deploy.sh
```

[agents.md](agents.md) is the guide for programs running *inside* a pane;
[building-a-client.md](building-a-client.md) is for programs talking to the
daemon from outside.

## Where to go next

| Page | Answers |
| --- | --- |
| [workbench.md](workbench.md) | What is every screen, rail and overlay, and what can I do there? |
| [keys.md](keys.md) | What is bound, and how do I change it? |
| [configuration.md](configuration.md) | What can I set, where, and what wins? |
| [cli.md](cli.md) | What is every command, flag and exit code? |
| [git.md](git.md) | How does the git surface work? |
| [processes.md](processes.md) | How are processes supervised? |
| [agents.md](agents.md) | How does a program inside a pane drive butai? |
| [remote.md](remote.md) | How do I reach a daemon on another machine? |
| [theming.md](theming.md) | How do I change the colours? |
| [troubleshooting.md](troubleshooting.md) | Why is it doing that? |
| [architecture.md](architecture.md) | How does it work inside? |
| [protocol.md](protocol.md) / [building-a-client.md](building-a-client.md) | How do I write my own client? |
| [embedding.md](embedding.md) | How do I run the daemon underneath my own product? |
| [development.md](development.md) | How do I build, test and release it? |

## Where this lives

| Section | Source |
| --- | --- |
| Install script and its environment | `scripts/install.sh` |
| Bare `butai`, attach, daemon auto-spawn | `crates/butai/src/main.rs`, `crates/butai/src/cli/mod.rs` |
| `~/.butai` layout | `crates/butai-protocol/src/paths.rs` |
| The frame and the rails | `crates/butai-client/src/chrome/mod.rs` |
| Default keymap | `crates/butai-client/src/keymap.rs`, `verbs.rs` |
| Agent status markers | `crates/butai-server/src/pane/terminal.rs`, `core.rs` |
| `.butai.toml` parsing | `crates/butai-server/src/config.rs` |
| Restore | `crates/butai-server/src/core.rs` (`restore_session`) |
