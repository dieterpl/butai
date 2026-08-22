# butai documentation

The complete manual. One page per subject; each ends with a **Where this
lives** table mapping its sections to the source files behind them, so a page
can be checked against the code rather than trusted.

New here? Start with [getting-started.md](getting-started.md) — install, open a
project, put an agent to work, review the diff, commit, detach. Twenty minutes,
and every stage links to the page that owns the detail.

The [project README](../README.md) is the summary; where it and a page here
disagree, the page wins.

## Using butai

| | |
| --- | --- |
| **[getting-started.md](getting-started.md)** | **Start here.** One session end to end: install, first attach, an agent, your dev server and tests, review and commit, detach and come back. |
| [workbench.md](workbench.md) | Every surface of the terminal client — the three-column frame, each rail and row type, the stage, and every page, overlay and dialog: what it shows, how to get in and out, and what you can do there. |
| [keys.md](keys.md) | Every shortcut, by surface. The two layers, the per-page verbs, and the rule they follow: nothing is reachable by pointer alone, and nothing is bound that cannot be found. Also how to rebind any of it, and what to do about `Option` on a Mac. |
| [cli.md](cli.md) | The command line in full: every command and subcommand, every flag, target resolution, machine-readable output, exit codes, and the environment butai reads and sets. |
| [configuration.md](configuration.md) | `~/.butai/config.toml` and `.butai.toml` key by key — type, default, effect, and which side reads it. Plus file locations, precedence, what the workbench writes back, and what an invalid value does. |
| [git.md](git.md) | The git surface: the working-tree model, staging by file and by hunk, diffs, the commit flow, branches, worktrees, remotes, integrate, the commit graph — and what butai deliberately leaves to a shell. |
| [processes.md](processes.md) | Process supervision: declaring one, its lifecycle and status markers, the `ready` marker, restart semantics, what survives a daemon restart, and how docker containers are surfaced. |
| [agents.md](agents.md) | Driving butai from *inside* a pane. Identity, targets, reading and sending, waiting on an agent, exit codes — for scripts, plugins and agents that want to steer the workbench around them. |
| [remote.md](remote.md) | Reaching a daemon that is not on this machine: SSH, forwarded sockets, `[[remote]]` config, the fleet and qualified ids, version skew, and the security posture. |
| [theming.md](theming.md) | The chrome palette: the eight built-in themes, every colour role and what it draws, and how to write your own in `~/.butai/themes/`. |
| [troubleshooting.md](troubleshooting.md) | Symptom, cause, fix — for the failure modes the code can actually produce. Plus how to raise the log level, where the log lands, and what to collect for a bug report. |

## Building on butai

butai's daemon is the product; the bundled TUI is just its first client. Every
client — terminal, browser, native, or another product embedding the daemon —
speaks the same API.

| | |
| --- | --- |
| **[building-a-client.md](building-a-client.md)** | **Start here to write a client.** A guided, self-contained tour: how to reach the daemon (local, SSH, forwarded socket), the HTTP API with `curl` examples, the JSON data model, live updates over SSE, connection code, and a full UI storyboard. You can build a working client from this page alone, without reading butai's source. |
| [protocol.md](protocol.md) | The normative spec. Framing, handshake, message types, cell-run frames, and the REST surface — terse, exhaustive, and the file that must be updated whenever the wire format changes. |
| [embedding.md](embedding.md) | Running the daemon as the engine underneath your own product: headless startup, containers, relaying `/v1` behind your own server, multi-tenancy, the security consequences, and what you may depend on across versions. |

Two worked implementations ship in this repo:

- [`web/`](../web/README.md) — a zero-build browser client that draws the whole
  workbench from the REST API and streams one live pane over a WebSocket.
- [`examples/api-client.py`](../examples/api-client.py) — ~100 lines, stdlib
  only, enough to list workspaces and spawn an agent.

## Understanding and changing butai

| | |
| --- | --- |
| [architecture.md](architecture.md) | How it works inside: the four crates, the one binary's three roles, the PTY-versus-JSON rule the whole design turns on, two protocols on one socket, the single-owner core actor and its run loop, panes and the render pipeline, agent status detection, git, persistence and restore. |
| [design.md](design.md) | Why the interface is shaped the way it is: the fixed three-column chrome, `Alt`-for-chrome keybinding layers, mouse and truecolor policy, and the trade-offs behind each. Rationale, not reference. |
| [development.md](development.md) | Working on butai: the pinned toolchain, every check CI runs and the local command that reproduces it, the three test layers and how to add to each, running a daemon in isolation, and the release process. |

## This folder also holds

- [`index.html`](index.html) — a standalone landing page for the project.
  Self-contained (no build, no external requests), so it works three ways:
  open the file directly, drop it in a GitHub Gist, or enable GitHub Pages with
  **Settings → Pages → Deploy from a branch → `main` / `/docs`** and it serves
  at `dieterpl.github.io/butai`.
- [`images/`](images/) — screenshots referenced by the README and this manual.
  They are real captures written straight out of the rendered cell grid as SVG;
  [`images/README.md`](images/README.md) says what each one shows and how to
  re-shoot them.

## Keeping this manual true

The rule, and the reason the **Where this lives** tables exist: a change that
alters behaviour is not finished until the page that owns that behaviour says
so. The routing table — which change updates which page — is in
[`../CLAUDE.md`](../CLAUDE.md), along with the rules for re-shooting a
screenshot when the chrome changes.

A new page is not discoverable until it is listed on this index.

## Elsewhere

Other clients — GUI apps, or products embedding the daemon — live in their own
repositories and speak this same protocol over a socket. Nothing in them is
required to use butai, and nothing here depends on them.
