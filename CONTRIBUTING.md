# Contributing to butai

Thanks for your interest in butai. This document covers the layout of the
repository and the checks your change is expected to pass.

## Repository layout

```
crates/            Rust workspace (the daemon, protocol, and TUI client)
  butai-protocol/    wire format: framing + JSON/msgpack API types
  butai-server/      the daemon: panes, terminal emulation, rendering, git
  butai-client/      the terminal (TUI) client, config, keymap, theming
  butai/             the `butai` binary (standalone / proxy entry points)
web/               browser client (TypeScript, React, Bun) + its bridge
docs/              design notes, protocol spec, client handoff docs
examples/          small standalone samples (e.g. an API client)
```

## Branches

Work on a feature branch and open a pull request against **`develop`**.
`develop` is what gets merged to `main`, and `main` moves only when a stable
release is cut — the README's install line fetches `scripts/install.sh` from
`main` by raw URL, so whatever is there is what a stranger's `curl | sh` runs.

`scripts/vet.sh` runs every check CI runs, against a branch or against your
working tree, and `--run` then starts a daemon on that build under
`BUTAI_HOME=~/.butai-dev` so you can try it against real work without touching
the butai you use. See [`docs/development.md`](docs/development.md#branches-and-the-two-release-tracks).

## Rust

The workspace pins a toolchain in `rust-toolchain.toml` (stable, with
`rustfmt` and `clippy`). Before opening a pull request:

```sh
cargo fmt --all
cargo clippy --all-targets --all-features
cargo test --all
```

CI runs the same three checks with warnings treated as errors, so a clean
local run should match CI.

## Web client

`web/` is TypeScript: a React client built by Vite, and a bridge that runs on
Bun and turns the daemon's Unix socket into `/api/*`, `/ws` and Server-Sent
Events. [Bun](https://bun.sh) is the only prerequisite. Before opening a pull
request:

```sh
cd web
bun install
bun run typecheck                  # tsc --noEmit
bun test                           # needs a butai binary; see web/README.md
bun run build                      # proves it links, not just that it parses
```

CI runs those same three after `bun install --frozen-lockfile`, so commit
`bun.lock` with any dependency change.

The client's wire types are **generated from the Rust** by ts-rs and live in
`web/src/protocol/generated/protocol.ts`. Never hand-edit them: change the DTO in
`crates/butai-protocol`, run `cargo test -p butai-protocol --features ts`, and
commit both sides. CI diffs the checked-in file and fails if it is stale. See
`web/README.md` and [`docs/development.md`](docs/development.md).

## Other clients

This repository builds exactly one artifact: the `butai` binary, which carries
both the daemon and its terminal client. Every other client — a GUI, a browser
tab, or another product embedding the daemon — is a separate codebase that
speaks the documented protocol over a socket. See
[`docs/protocol.md`](docs/protocol.md) and
[`docs/building-a-client.md`](docs/building-a-client.md) to build one.

A client's own source is never covered by butai's license. MPL-2.0 is
file-level: it reaches butai's files, not yours, and linking is not what
triggers it — so a client carries whatever license you choose. If you also
distribute butai's binary, §3.2 asks you to tell your users where to obtain
butai's source. That is the whole of it.

## Releases

Two tracks, one workflow, and the tag decides which:

| Tag | Cut on | Published as |
| --- | --- | --- |
| `v1.3.0` | `main` | a stable release |
| `v1.3.0-dev.1` | `develop` | a **prerelease** |

A prerelease is kept out of GitHub's `releases/latest`, which is the only
endpoint the self-updater and the installer read — so a dev build is never
offered to anyone who did not ask for it by name
(`BUTAI_VERSION=v1.3.0-dev.1`). Use `scripts/cut.sh <version>` to set the
version; it rewrites all four places it appears in `Cargo.toml` and refreshes
the lockfile, and leaves the commit and tag to you.

Tagging `v*` runs [`.github/workflows/release.yml`](.github/workflows/release.yml),
which builds the binary for every supported target and uploads one tarball per
target plus a `SHA256SUMS` file. To reproduce a release locally, run
[`scripts/release.sh`](scripts/release.sh) — same target list, same tarball
layout. Linux targets cross-compile through
[`cross`](https://github.com/cross-rs/cross); macOS targets build natively.

## Documentation

[`docs/`](docs/README.md) is a complete manual, one page per subject, and each
page ends in a **Where this lives** table mapping its sections to the source
files behind them. A change that alters behaviour is not finished until the page
that owns that behaviour says so — find the owner from the table below rather
than by searching, so the same fact does not end up written twice.

| If you change | Update |
| --- | --- |
| A command, flag, exit code, or `--help` output | [`docs/cli.md`](docs/cli.md) |
| A config key, default, file path, or precedence | [`docs/configuration.md`](docs/configuration.md) |
| A key binding, verb table, or footer | [`docs/keys.md`](docs/keys.md) |
| A screen, rail, page, overlay, or status marker | [`docs/workbench.md`](docs/workbench.md) |
| The wire format, a message, or a REST route | [`docs/protocol.md`](docs/protocol.md) **and** [`docs/building-a-client.md`](docs/building-a-client.md) |
| Pane kinds, the core loop, persistence, agent detection | [`docs/architecture.md`](docs/architecture.md) |
| Staging, diffs, commits, branches, worktrees, remotes | [`docs/git.md`](docs/git.md) |
| Process supervision, `ready`, restart, docker panes | [`docs/processes.md`](docs/processes.md) |
| SSH, forwarded sockets, the fleet, qualified ids | [`docs/remote.md`](docs/remote.md) |
| Anything a downstream embedder depends on | [`docs/embedding.md`](docs/embedding.md) |
| Build, test, lint, CI, branches, or the release matrix | [`docs/development.md`](docs/development.md) and this file |
| An error message a user can hit | [`docs/troubleshooting.md`](docs/troubleshooting.md) |
| Colour roles or theme loading | [`docs/theming.md`](docs/theming.md) |

Rationale belongs in [`docs/design.md`](docs/design.md), not in a reference
page. A new page is not discoverable until it is listed in
[`docs/README.md`](docs/README.md). If you move or rename a source file, fix
every **Where this lives** table that names it.

**Screenshots.** [`docs/images/`](docs/images/README.md) holds real captures,
not mockups, so a change to the frame, the rails, the status markers or the
palette means re-shooting: `scripts/shoot.py`. Read
[`docs/images/README.md`](docs/images/README.md) first — it stands up an
isolated daemon, and the isolation rules there are not optional.

## Commits & pull requests

- Branch off `develop`; open the pull request against `develop`.
- Keep commits focused; write imperative subject lines (`add`, `fix`, `move`).
- Describe the user-visible effect in the PR body, and note protocol changes.
- Any change to the wire protocol must update [`docs/protocol.md`](docs/protocol.md).
- Update the documentation page that owns what you changed, in the same commit.
