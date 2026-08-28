# Development

Working on butai itself: what is in the tree, what the toolchain is pinned to,
every check CI runs and the command that reproduces it locally, the three test
layers and how to add to each, how the branches and the two release tracks fit
together, how to run a second daemon without disturbing your own, and how a
release is cut.

[`../CONTRIBUTING.md`](../CONTRIBUTING.md) is the short front door — the layout
sketch and the three commands to run before a pull request. This page is the
long version of the same material. Where it goes further, it goes further; it
does not disagree.

## The tree

```
crates/                Rust workspace — four crates, one binary
  butai-protocol/        wire types, framing, paths, binary names
  butai-server/          the daemon: core actor, panes, VT emulation, git, HTTP
  butai-client/          the terminal client: chrome, keymap, theming, dialling
  butai/                 the `butai` binary: CLI, proxy, standalone, handoff
web/                   browser client (TypeScript/React/Vite) + its Bun bridge
testsuite/             the Docker suite: a real binary, real PTYs, real apps
docs/                  this manual
examples/              standalone samples: an API client, themes
scripts/               vet.sh, cut.sh, release.sh, install.sh, screenshot tooling
.github/workflows/     ci.yml, release.yml
```

Five crates, and the dependency direction is the point of the split:

| Crate | Holds | Depends on |
| --- | --- | --- |
| `butai-protocol` | `ClientMsg` / `ServerMsg`, `Command`, the REST DTOs, length-prefix framing, `~/.butai` path resolution, and `names::BINARIES` — every name this program has shipped under | nothing in the workspace |
| `butai-server` | the daemon. The core actor, terminal panes and their PTY threads, the VT emulator, cell-run rendering, the git status cache and `git` process execution, the HTTP facade, telemetry | `butai-protocol`, `butai-update` |
| `butai-update` | the self-updater. The GitHub release, the artifact this build is, `SHA256SUMS`, the swap and the exec. The only outbound network in the tree | nothing in the workspace |
| `butai-client` | the terminal client. Chrome geometry and row models, the keymap, palettes, the hit registry, ssh dialling, clipboard, terminal restore | `butai-protocol`, `butai-update` |
| `butai` | the binary. Clap command tree, exit codes, `proxy`, `standalone`, the ssh handoff | all four |

`crates/butai-server/tests/` holds the two end-to-end suites; every other test
lives in a `#[cfg(test)] mod tests` beside the code it covers. Roughly: 26 tests
in `butai-protocol`, 198 in `butai-server`, 512 in `butai-client`, 28 in
`butai`, 16 in `butai-update`, plus 39 e2e HTTP tests and 19 e2e socket tests.

[`architecture.md`](architecture.md) is the map of what those crates *do*; this
page is only about building and checking them.

## The toolchain

| File | Says |
| --- | --- |
| `rust-toolchain.toml` | channel `stable`, components `rustfmt` and `clippy`. No pinned date — CI runs `rustup show` and takes whatever stable is current |
| `Cargo.toml` `[workspace.package]` | `edition = "2021"`, `rust-version = "1.88"`, `license = "MPL-2.0"` |
| `Cargo.toml` `[workspace]` | `resolver = "2"`, four members |
| `Cargo.toml` `[workspace.lints.clippy]` | `dbg_macro = "warn"`, `todo = "warn"` — with CI's `-D warnings` both are errors there |
| `Cargo.toml` `[profile.release]` | `lto = "thin"`, `strip = true` |
| `rustfmt.toml` | `use_small_heuristics = "Max"` |

Two things about `rustfmt.toml` are worth knowing before you run the formatter.
The setting exists because the committed style keeps struct literals and call
chains on one line up to `max_width`, and without the file `cargo fmt` explodes
them — which is why the check had been red and contributors were hand-matching
the surrounding style instead. And the file's own comment records that it does
*not* make the tree clean: it was formatted with a mix of profiles over time,
and a tree-wide `cargo fmt --all` is meant to be its own deliberate commit taken
when no other work is in flight. Format the files you touched.

Workspace dependencies are declared once in `[workspace.dependencies]` and
inherited by each crate. The three internal crates carry **both** a `path` and a
`version` there: the path wins for workspace builds and the version is what
`cargo publish` rewrites the dependency to, and without it packaging fails with
*all dependencies must have a version requirement specified*. Bump them with
`workspace.package.version`.

`scripts/release.sh` reads `CARGO_TARGET_DIR` rather than hardcoding `target/`,
so a build directory pointed off-tree still packages. Anything else you run
follows cargo's own rules.

## What CI runs

`.github/workflows/ci.yml` has four jobs. They are deliberately independent of
each other — a lint failure must not hide an API regression, and a broken
testsuite must not hide a web client nobody can load.

Triggers: push to `main` or `develop`, every pull request, a nightly
`cron: "0 3 * * *"`, and `workflow_dispatch`. `RUSTFLAGS: "-D warnings"` is set for the whole workflow,
so warnings are errors in every job that compiles.

| Job | Runs | Reproduce locally |
| --- | --- | --- |
| `rust` — format | `cargo fmt --all --check` | `cargo fmt --all --check` |
| `rust` — clippy | `cargo clippy --all-targets --all-features` under `RUSTFLAGS=-D warnings` | `RUSTFLAGS="-D warnings" cargo clippy --all-targets --all-features` |
| `rust` — test | `cargo test --all --all-features` | `cargo test --all --all-features` |
| `rust` — bindings | `git diff --exit-code web/src/protocol/generated/` after the test step | `cargo test -p butai-protocol --features ts && git diff --stat web/src/protocol/generated/` |
| `web` | `bun install --frozen-lockfile`, then `bun run typecheck`, `bun test`, `bun run build`, from `web/` | `cd web && bun install && bun run typecheck && bun test && bun run build` |
| `docker` | `./testsuite/run.sh <profile>` | `./testsuite/run.sh smoke` (what a PR gets) |
| `targets` | `cargo`/`cross check --release --target <t> -p butai`, seven targets | `TARGETS="x86_64-unknown-linux-musl" scripts/release.sh`, or the `check` by hand |

The `docker` job picks its profile from the event: `smoke` on a pull request,
`standard` on a push to a branch, `soak --minutes 25` on the nightly schedule. It
uploads `testsuite/out/` as an artifact whatever the outcome.

The `targets` job runs **only** on the schedule or a manual dispatch. Its
comment says why: the release matrix rots quietly — a dependency picks up a C
library, a `cfg` gate assumes glibc — and nobody finds out until a tag is cut.
It is `check`, not `build`, because the point is catching a target that stopped
compiling, not linking seven binaries.

The `web` job installs with `--frozen-lockfile`, so CI builds what the repo pins
rather than whatever resolves today; the image build asserts the same thing. It
is independent of the `rust` job for the reason it always was — a lint failure
elsewhere must not hide a client nobody can load.

Three steps, and the order is deliberate: `typecheck` reads every file in under
a second, `bun test` runs the logic and the bridge against a real daemon, and
`build` proves the thing also *links*. Together they replace the old
`check.py --static-only`, which existed because nothing read `web/`'s code until
a browser did. The compiler is a stronger check than the one it replaces, and
for a specific reason: the DTOs it checks against are generated from
`butai-protocol`, so a daemon field the client never learned about fails here
rather than surfacing as a value silently ignored in a browser.

## The three test layers

These three cover the daemon and its binary. The browser client has a fourth of
its own, `bun test` in `web/`, described under [The web client](#the-web-client)
— it is a separate toolchain and a separate CI job, and it is the layer that
proves the *client* half of the boundary rather than the daemon's.

### 1. Rust unit tests

In-crate, in a `#[cfg(test)] mod tests` next to the code. They are the bulk of
the suite and the fastest loop:

```sh
cargo test -p butai-client                 # one crate
cargo test -p butai-server config::        # one module's tests
cargo test --all --all-features            # what CI runs
```

**Adding one:** put it beside the thing it covers, name it as a sentence about
the behaviour (`a_machine_still_carrying_the_old_name_is_reachable`,
`every_builtin_auto_approves_and_names_the_conversation_it_resumes`), and make
the doc comment say which real failure it pins. That is the house style
throughout the tree, and it is what makes a failing name legible in CI output.

Prefer a test that can fail. Break the code, confirm the test goes red, put it
back — an assertion that passes either way is decoration.

### 2. Rust end-to-end tests

`crates/butai-server/tests/e2e_http.rs` (33 tests) and `e2e_socket.rs` (16)
drive a real daemon **in-process**: each helper binds a `UnixListener` on a
`tempfile::TempDir` socket and `tokio::spawn`s `butai_server::daemon::serve`.
No binary, no subprocess.

| File | Drives | Shape |
| --- | --- | --- |
| `e2e_http.rs` | the REST facade on the socket, exactly as `curl --unix-socket` would — including the first-byte sniff that separates HTTP from framed connections | one `HTTP/1.1` request with `Connection: close` per call, read to EOF |
| `e2e_socket.rs` | the accept loop, JSON framing, handshake, detach/reattach, kill-server, restore | `tokio_util` `Framed` with the length codec, `ClientMsg` in, `ServerMsg` out |

Both share conventions worth copying:

- **`start_daemon(tmp)`** returns the socket path. Variants exist for the cases
  a test needs: `start_daemon_with_shell_agent` configures one agent named `sh`
  whose command is `/bin/sh`, because the real agent CLIs are not installed in
  CI and a shell is how you get genuine PTY output and real agent-state
  transitions. `start_daemon_with_store` takes an explicit session-store path so
  a test can restart the daemon and exercise restore.
- **`poll_until(socket, path, what, pred)`** — never assert immediately.
  Workspace scans run off the core loop, agent state is recomputed on the ~2 s
  sampler tick, and `finished` waits a further settle window. The helper polls
  for ~6 s and panics with the last body it saw.
- **`Screen`** in `e2e_socket.rs` applies `FrameUpdate` cell runs to a text grid,
  so a test can assert on what a pane says. Anything that reads a screen reads
  it through an `AttachTarget::Pane` connection, because a pane is the only
  thing the daemon draws.

**Adding one:** a `#[tokio::test]`, a temp dir, one of the `start_daemon`
helpers, then `http(...)` or `send`/`recv`. If it asserts on screen content, go
through `Screen` and `await_text`, not raw bytes.

### 3. The Python testsuite

`testsuite/` runs a **real `butai daemon` binary** in a container and drives it
the way a client does: the framed socket protocol and the REST API, with real
PTYs running real terminal applications. It is the half the crate tests cannot
reach — a Linux userspace, `/proc`, container limits, git's ownership rules, and
the agent CLIs butai exists to host. Docker is the only requirement; the
harness is standard library only, no pip.

```sh
./testsuite/run.sh smoke                  # ~1 min   API + protocol gate
./testsuite/run.sh standard               # ~10 min  the default
./testsuite/run.sh soak --minutes 30      #          drift detection
./testsuite/run.sh standard --real-agents # adds the real CLIs
./testsuite/run.sh standard --filter http --keep
./testsuite/run.sh standard --list        # what would run, then exit
```

Profiles are cumulative — `standard` runs `smoke` too, `soak` runs all three.
`standard` and `soak` additionally run three short passes in separate
containers with `--pids-limit 120`, `--memory 1g` and `--cpus 1`, because those
limits cannot be changed from inside a running container. `--no-lanes` skips
them; they are also skipped whenever `--filter` is in play.

Other flags: `--scale N` (stress load multiplier), `--no-build` (reuse the
image), `--platform`, and `--keep`, which sets `BUTAI_KEEP_TMP=1` so the
container's temp directories survive for inspection.

Reports land in `testsuite/out/<lane>/` as `results.json` and a self-contained
`report.html`. Set `BUTAI_TEST_OUT` to move them, which `run.sh`'s own comment
recommends when the working copy sits on a network mount.

**The run fails if a route was never exercised.** `suite/coverage.py` enumerates
the daemon's public surface — HTTP routes, `Command` variants, `ClientMsg` and
`ServerMsg` tags, attach targets, input events, key codes, API events, agent
states, encodings, CLI commands — and `Runner.summary()` fails the run when
anything in it is missing. Coverage is only enforced when the whole suite ran:
a `smoke` run or any `--filter` legitimately touches a subset, and the report
says so.

The harness modules:

| Module | Role |
| --- | --- |
| `runner.py` | registration, profiles, per-test timeouts, `xfail`, coverage, reporting |
| `daemon.py` | daemon lifecycle, one isolated `HOME` per daemon, the `Config` builder |
| `butai.py` | framed, HTTP and SSE clients |
| `screen.py` | applies `FrameUpdate` damage to a grid, the way a real client does |
| `tty.py` | a real PTY plus a reconstructed screen (see below) |
| `fixtures.py` | workspaces, git repos, terminal probe scripts |
| `metrics.py` | latency percentiles, `/proc` sampling, leak slopes |
| `msgpack.py` | just enough MessagePack for `rmp-serde`'s named maps |
| `coverage.py` | the enumerated surface the run is checked against |
| `report.py` | the HTML report |

`tests/` is numbered by area: `00_daemon`, `10_protocol`, `11_commands`,
`12_input`, `20_http`, `21_http_errors`, `22_security`, `23_events`,
`24_pane_output`, `30_git`, `31_processes`, `32_git_remote`,
`33_git_integrate`, `34_git_graph`, `35_git_hunks`, `36_git_worktree`,
`40_agents`, `41_agents_real`, `50_apps`, `60_stress`, `61_limits`, `70_soak`.

**Adding one:**

```python
from suite.runner import test

@test(profile="standard", tags=("http",))
def a_workspace_can_be_created(ctx):
    d = ctx.daemon()                       # own HOME, own socket, torn down for you
    ctx.cover("POST /v1/workspaces")       # counts toward the coverage assertion
    ws = d.http.new_workspace(path=d.work)
    assert d.http.detail(ws)["processes"]
    d.assert_healthy()                     # no panics, still alive
```

`ctx` also gives you `note()` (free text in the report), `metric()`, `row()`
(a matrix row), `require()` / `require_tool()` / `skip()`, and `scale` /
`soak_seconds` for load-dependent tests. `@test` takes `timeout` (default 180 s,
enforced with `SIGALRM` so a wedged PTY fails a test rather than hanging the
run) and `xfail`.

`xfail` is a **sentence explaining behaviour butai gets wrong today**. The test
still runs; failing is expected and reported under KNOWN GAPS, while passing is
flagged `XPASS` — the signal to delete the annotation. `test_40_agents.py`'s
`a_marker_above_a_tall_footer_is_still_seen` is the model.

Two suite defaults differ from a user's, and `test_00_daemon.py` pins the
shipped behaviour of both: `exit_when_empty` is off (the shipped default stops
the daemon with its last workspace, which is right for a person and fatal for a
test that asks another question afterwards), and agents point at the doubles.

## The fake agents

Agent status is a heuristic: butai re-renders the pane and scans the bottom
eight rendered rows for marker strings. There is no protocol, so compatibility
is a property of what each CLI **draws** — which is why `testsuite/fakeagents/`
holds scripted doubles that reproduce each real CLI's drawing. The whole state
machine is then tested deterministically with no API keys.

| Double | Simulates |
| --- | --- |
| `fake-claude` | Claude Code: a spinner line whose `esc to interrupt` is the busy marker, a boxed permission dialog whose highlighted `❯ 1. Yes` reads as "needs you", and the much taller AskUserQuestion dialog, where the highlighted option is thirteen rows up and only the hint line gives it away |
| `fake-codex` | codex's drawing |
| `fake-gemini` | gemini's drawing |
| `fake-aider` | aider: no recognised status line, so it exercises the output-recency fallback, plus the `(Y)es/(N)o/(A)ll/(S)kip all [Yes]:` confirmation, which is chrome no prose contains |
| `fake-tallfooter` | the negative shape of the 8-row band: a perfectly good `esc to interrupt` marker sitting *above* a context/model/shortcuts block, so butai never sees it. This is the `xfail` |

`_lib.sh` is the shared driver. An agent script sources it, overrides
`emit_banner` / `emit_busy` / `emit_question` / `emit_choice` / `emit_prose`,
and calls `run_script`. `FAKE_SCRIPT` drives the phases as a comma-separated
list of `phase:seconds`:

| Phase | Draws |
| --- | --- |
| `busy` | the working marker |
| `question` | a confirmation dialog |
| `choice` | a multiple-choice dialog |
| `prose` | the negative control — "to interrupt" and "do you want to" in ordinary sentences |
| `noisy` | sustained output with no marker at all |
| `idle` | scrolls every marker out of the footer band, back to a bare prompt |
| `bell` | a `BEL` |
| `exit` | die with that exit code |

Three properties of the driver exist because of how detection works, and a new
double has to keep them:

- **Each phase draws once, then goes silent.** butai has two independent
  working signals — a footer marker and raw output recency — and an agent that
  redrew continuously would trip both, so a marker test could not tell which one
  fired.
- **`scroll_to_bottom` first.** Status is read from the last eight rendered
  rows, and a real agent's status line is at the bottom because its transcript
  fills the screen above it. A double printing from the top would leave its
  marker near row 1 and test nothing.
- **Repaint on resize.** butai resizes every pane in a workspace whenever any
  client resizes; a real TUI answers `SIGWINCH` with a full redraw. `nap` polls
  `tput` as well as trapping `WINCH`, because the first resize can land before
  the trap is installed. A double whose "working" state is a scrolling
  transcript sets `REPAINTS=0`, since repainting would keep its output
  artificially recent.

`--real-agents` builds `testsuite/real-agents/Dockerfile` on top of the base
image and runs the same assertions against the actual CLIs. That lane is what
catches an upstream reword; without it the fakes could agree with butai and both
be wrong. Credentials (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`,
`GOOGLE_API_KEY`) are forwarded only when set, so tests skip with a reason
rather than fail.

## Driving a real pty, and the trap in reading it

`butai`, `butai standalone` and `butai reset` all talk to a terminal, so testing
them from `subprocess.run` proves nothing. `suite/tty.py`'s `PtyProcess` gives
them one: `pty.openpty()`, `TIOCSWINSZ` for the size, `Popen` with the slave on
all three descriptors and `start_new_session=True`.

It keeps a **screen**, not just a byte log, and that distinction is the whole
point:

> The client draws cell by cell — each run of matching style gets its own cursor
> move — so **a word on screen is almost never a contiguous run of bytes in the
> stream**. Grepping the raw capture always says NO.

That has looked like a broken feature more than once. `wait_output(needle)`
therefore searches the reconstructed grid, and `text()` is what you assert on;
`raw()` exists only for a failure message that needs it.

`_Screen` models just enough terminal to answer *what is on screen*: absolute
positioning, erase, newline and backspace, which is all a ratatui client emits
because it moves the cursor for every run it draws. Styling is dropped — these
tests ask what a screen *says*, which also means a list cursor drawn as a
background colour is invisible to it. One subtlety is baked into the regex: a
CSI can carry **intermediate bytes** between its parameters and its final
letter, and butai emits one — `CSI 0 SP q`, the cursor shape that says whether
the stage is listening. Matching only `[0-9;?]*` missed it and typed the
leftovers into the grid as text.

`suite/screen.py` is the other grid: it applies the daemon's `FrameUpdate`
damage the way a real client does, honours `full` frames, keeps per-cell style
so the SGR and colour probes can assert on attributes, and advances by each
grapheme's **display width** — the daemon sends no filler cell for the second
column of a wide character, so advancing one column per cell shifts everything
after the first CJK or emoji glyph on a line. Its `footer(rows=8)` is the band
butai scans for agent status.

When you probe a PTY, wait on the *effect*, not on the text you typed —
`wait_output` matches your own echo and will pass before anything happened.
`test_00_daemon.py`'s `caret_at(app, label, prev=...)` is the pattern: sample
until the value **changes**.

## Running a second daemon

A daemon on the default paths restores the user's real session and will spawn
their agents and processes. There are two reasons to stand up another one, and
they want different tools — the difference is whether you mean to *use* it.

### A butai you mean to use: `BUTAI_HOME`

Trying a build against real work is the case `BUTAI_HOME` exists for. It moves
`~/.butai` and nothing else:

```sh
BUTAI_HOME=~/.butai-dev target/release/butai
```

That daemon gets its own socket, lock, config, themes, logs, `session.json` and
`panes/`, and keeps your real `$HOME` — ssh config, shell profile, git identity,
and the repositories you actually work in. Your own butai keeps running beside
it; the two never meet, because they are not looking at the same socket.

It is read in one place, [`paths::butai_dir`](../crates/butai-protocol/src/paths.rs),
so every path in the module follows it at once. That is the point of putting it
there: no combination of variables can leave a daemon with one butai's socket
and another's session store. Panes inherit it — a pane starts from a snapshot of
the daemon's environment — so `butai` shelled out inside a dev pane reaches the
dev daemon, not the real one.

Empty is not an override. `BUTAI_HOME=` from a profile that exports it
unconditionally means "no opinion", not "the working directory".

`scripts/vet.sh --run` does all of this for you, including seeding
`~/.butai-dev` with a **copy** of your real `config.toml` and `themes/` — a copy
and not a symlink, because the client writes back to `config.toml` and a build
you are still vetting should not be able to edit the one your real butai reads.

### A daemon that must touch nothing: `HOME`

A test daemon is the other case, and there `BUTAI_HOME` is not enough: the point
is a machine-shaped sandbox, not just butai's share of one. Four rules.

**1. Give it its own `HOME`.** Everything butai reads or writes outside a
project lives under `~/.butai` — socket, lock, config, logs, session store, pane
dumps — so one environment variable isolates all of it, along with everything
*else* the process might reach for. That is exactly what `suite/daemon.py` does,
and it is why a dozen differently-configured daemons can run side by side in one
container.

**2. Set `BUTAI_SOCKET` explicitly.** This is the one that bites. `BUTAI_SOCKET`
is exported into **every pane the daemon creates**, so a shell you are running
*inside butai* already has it set — pointing at the real daemon. A test that
sets only `HOME` inherits that value and aims itself at the daemon you are
trying to protect. `Daemon.env()` in the suite sets `HOME` and `BUTAI_SOCKET`
together for this reason, and also clears `BUTAI` so the nesting guard is not
confused by a stale value.

```sh
sandbox=/tmp/bt
mkdir -p "$sandbox/.butai"
HOME="$sandbox" BUTAI_SOCKET="$sandbox/.butai/butai.sock" \
  butai daemon &
```

**3. Keep the socket path short.** `sockaddr_un.sun_path` is 104 bytes on macOS
and 108 on Linux, so a daemon whose `HOME` sits under a deep temp directory
cannot bind at all. The suite budgets 100 bytes and fails with an actionable
message; `temp_base()` prefers `/tmp` over `tempfile.gettempdir()` because the
macOS default (`/var/folders/<hash>/<hash>/T`) is most of the budget on its own.
`BUTAI_TEST_TMP` overrides it. `vet.sh` budgets the same 100 bytes before it
starts a dev daemon.

**4. Kill it by socket, never by pattern.**

```sh
butai --socket /tmp/bt/.butai/butai.sock kill-server   # a sandbox
BUTAI_HOME=~/.butai-dev butai kill-server              # a dev butai
```

`pkill -f "butai daemon"` matches the user's real daemon too.

Two more knobs are worth knowing. `BUTAI_SESSION_FILE` overrides
`~/.butai/session.json` on its own and takes `panes/` and `scratch/` with it —
deliberately *not* keyed off `BUTAI_SOCKET`, so a second daemon on a custom
socket shares the real session store unless you set it, or set `BUTAI_HOME`,
which takes it along. And `exit_when_empty` ships **on**: closing the last
workspace stops the daemon, which is right for a person and surprising in a
test, so the suite's `Config` turns it off.

The web bridge has the same trap in a different shape: `BUTAI_SOCKETS` on its
own is not isolation, because `BUTAI_SOCKET` has a default and the bridge will
add your real daemon as `local` alongside whatever you named. Set both.

## TypeScript for the browser client, generated from the DTOs

`butai-protocol`'s types are the wire format, so the browser client's types are
compiled from them rather than written twice:

```sh
cargo test -p butai-protocol --features ts   # writes web/src/protocol/generated/
```

79 types — every REST DTO and the whole framed message set — land in one
`protocol.ts`, carrying the Rust doc comments through as JSDoc. The output
directory is set once in `.cargo/config.toml` (`TS_RS_EXPORT_DIR`), so no
attribute names a path.

The `ts` feature is **off by default**, deliberately: it is a code generator,
and a crate on crates.io should not make every downstream build compile one.
CI's `cargo test --all --all-features` turns it on anyway, so the freshness
check is a `git diff` rather than a second build — a DTO that grows a field and
a client that never learned about it becomes a red step naming the line.

**Never hand-edit `protocol.ts`.** Change the Rust, rerun the command above,
commit both. And note what the generator is *not* doing: it reads the same
`serde` attributes serde does, so it cannot change a byte on the wire. If a
diff here implies one, something else is wrong.

**`TS_RS_EXPORT_DIR` has to name the directory the client imports from**, and
the freshness check cannot tell you when it does not. It kept pointing at
`web/app/src/...` after the TypeScript cutover moved the client to `web/src/...`:
the export wrote a whole `protocol.ts` into a directory git does not track, the
`git diff` found the real one unchanged, and the step went green. `SysDto` grew
`disks`, the browser never saw the field, and nothing anywhere said so. If you
move the client, move that path — and check that a deliberate DTO change *does*
turn this step red before trusting it again.

## The web client

`web/` is a TypeScript client and its bridge, and [Bun](https://bun.sh) is the
only prerequisite — it is the package manager, the test runner and the runtime
the bridge executes on. Vite builds the client; nothing else in the repo needs
node.

```sh
cd web
bun install
bun run dev            # Vite on 5173, proxying /api and /ws to the bridge
bun run bridge         # the bridge on 8080 (`dev:bridge` reloads on save)
bun run build          # a hashed bundle into dist/
bun run typecheck      # tsc --noEmit
bun test               # needs a butai binary; see below
```

The two servers are separate on purpose and only in development. `bun run
bridge` owns the daemon sockets; the dev server owns only the assets, and
proxies `/api` and `/ws` at it so `vite dev` behaves like the shipped thing. In
production there is one origin: the bridge serves `dist/`.

**The build step is the first check, and it is a real one.** The previous client
had none, which was a constraint rather than a convenience — nothing read that
code until a browser did, and a browser reports a module that fails to parse as
one console line and a blank page, with the importing elements silently never
defined. That shipped once and stood for six days. `tsc` now reads every file
before anything runs, and it reads them against DTOs generated from
`butai-protocol`, so the class of bug that check could never reach — a field the
daemon renamed and the client still spells the old way — is a compile error
naming the line.

`bun test` is the layer above it. Two kinds of test live there:

| | |
| --- | --- |
| `test/verbs.test.ts`, `test/settings-docs.test.ts`, `test/fleet.test.ts`, `test/graph.test.ts` | the DOM-free logic layer, executed. These are the assertions the old `check.py` ran under node, ported: the verb tables, the settings store and its palettes, the fleet's row model, the commit graph's lane assignment |
| `test/ws.test.ts` | the bridge's `/ws` relay, against a **real daemon**. A WebSocket is not a request, and the daemon's 4-byte length prefix is hand-written on both sides — exactly where a byte-order slip would live, and it would present as a stage that never paints |

Anything that starts a daemon needs a `butai` binary. It defaults to
`/var/tmp/butai-probe/butai` and `BUTAI_BIN=<path>` overrides it. Build one with
`cargo build -p butai` and **copy it somewhere private**: a shared
`CARGO_TARGET_DIR` has one `debug/butai`, and a concurrent worktree relinking it
mid-test reproduces the old behaviour of whatever you just fixed.

> `BUTAI_BIN`, not `BUTAI`. A butai pane exports `BUTAI` already, set to the
> *socket* path, so `${BUTAI:-<a binary>}` resolves to a socket and the harness
> tries to execute it. The tests clear the whole family for the same reason a
> Rust test does — a daemon that inherits `BUTAI_SOCKET` is not isolated, it is
> the user's own, and it will restore their session and spawn their agents.

There is **no lint step yet**. An ESLint flat config is the outstanding piece,
and the rules that will matter are the ones about hooks; until it lands, the
compiler and the tests are the whole gate. CI's `web` job runs `typecheck`,
`test` and `build` after `bun install --frozen-lockfile`, and nothing else.

## Branches, and the two release tracks

Work happens on feature branches. They land on `develop`, and `develop` lands on
`main` when it is worth a stable release.

```
feat/x ──PR──▶ develop ──tag v1.3.0-dev.1──▶ prerelease   (dev track)
                  │
                  └──merge──▶ main ──tag v1.3.0──▶ release (stable track)
```

`main` moves **only** for a stable release, and that is not tidiness. The
install line in the README fetches `scripts/install.sh` from `main` by raw URL,
so whatever is on `main` is what a stranger's `curl | sh` runs today. A branch
you can break is a branch that cannot also be that.

**One tag shape decides the track**, and it decides it by itself: a prerelease
identifier — the `-` in `1.3.0-dev.1`, as semver defines it. `release.yml` reads
the tag rather than the branch, because a tag is a commit and by publish time
there is nothing left to ask about where it was cut. A prerelease is published
with `--prerelease`, and that one flag is the whole separation:

- GitHub keeps a prerelease out of `releases/latest`.
- `releases/latest` is the only endpoint `crates/butai-update` asks
  ([`check`](../crates/butai-update/src/lib.rs)) and the one
  `scripts/install.sh` reads.

So a dev tag is invisible to every stable install without either of them
filtering anything, and no stable user is ever offered a build off `develop`.
Reaching one is deliberate.

### Installing the dev track

Two things, and they are separate: which build gets installed, and which track
that build then follows.

```sh
# a second butai, on the dev track, beside the one you use
BUTAI_CHANNEL=dev \
BUTAI_INSTALL_DIR=~/.butai-dev/bin \
BUTAI_NO_RESTART=1 \
  curl -fsSL https://raw.githubusercontent.com/dieterpl/butai/main/scripts/install.sh | sh

# ~/.butai-dev/config.toml
[update]
channel = "dev"

BUTAI_HOME=~/.butai-dev ~/.butai-dev/bin/butai
```

`BUTAI_CHANNEL=dev` reads the release *list* instead of `releases/latest` and
takes the newest prerelease; `BUTAI_VERSION=v1.3.0-dev.1` still names one
exactly. `BUTAI_NO_RESTART=1` matters when the install is a second butai: the
installer stops the daemon it is replacing, and without `BUTAI_HOME` set in that
same shell the daemon it would stop is your real one.

`[update] channel` is what keeps it there. It is read by **both** halves — the
client for the binary a person runs, the daemon for `POST /v1/update` — and it
lives in the config of whichever `BUTAI_HOME` the install uses, which is what
makes it per-install rather than per-machine. `dev` compares prereleases
properly (`1.3.0-dev.10` is ahead of `1.3.0-dev.9`, and `1.3.0` is ahead of
both), so a dev butai keeps up with `develop` on its own. The stable install
beside it never sees any of it.

A dev install is also how a *remote* machine gets one — the useful case, since a
workbench attached over `ssh host butai proxy` is talking to a daemon on the far
side, and the far side is the one that has to be running the code you are
testing. `butai update --daemon` then follows the channel configured **there**,
which is the point: the daemon is the thing being replaced.

For iterating on your own commits, none of this is needed — `scripts/vet.sh
--run` builds the working tree and runs it on `~/.butai-dev` with no release in
sight. The dev track is for builds that have to reach a machine you are not
building on.

### Vetting a branch: `scripts/vet.sh`

```sh
scripts/vet.sh                 # this tree, uncommitted changes and all
scripts/vet.sh feat/x          # a branch, in a worktree of its own
scripts/vet.sh feat/x --run    # ...and leave a daemon up on it to drive
scripts/vet.sh --full          # the standard testsuite instead of smoke
```

It runs every check CI runs — `cargo fmt --check`, `clippy` and `test` under
`-D warnings`, the generated-bindings diff, the four `bun` steps, and
`testsuite/run.sh` — reporting each as passed, failed or skipped, and skipping
cleanly when a tool is absent rather than failing on it. A named branch is
checked out `--detach` into a throwaway worktree; no argument means this tree,
which is the case worth optimising for, since what you most want to vet is
usually what you have not committed yet. `CARGO_TARGET_DIR` is shared across
runs so the second one is incremental.

`--run` is the step CI cannot do for you: it builds the branch and starts a
daemon on it under `BUTAI_HOME=~/.butai-dev`, seeded once with a copy of your
real `config.toml` and `themes/`. See
[Running a second daemon](#running-a-second-daemon) for what that isolates and
what it deliberately does not.

### Cutting one: `scripts/cut.sh`

The version lives in four places in the root `Cargo.toml` — `[workspace.package]
version` and the three internal `butai-*` pins under `[workspace.dependencies]`,
which carry a `version` beside their `path` so `cargo publish` has something to
rewrite. Four strings that must agree, edited by hand, is how a release goes out
with a crate still pinned to the last one.

```sh
scripts/cut.sh 1.3.0-dev.1     # on develop
scripts/cut.sh 1.3.0           # on main
```

It rewrites all four, runs `cargo check` to refresh `Cargo.lock`, and stops
there — committing and tagging are yours, because they are the two steps that
are hard to take back. It prints the exact three commands to finish the job, and
warns (without refusing) if the version's track and the branch you are on
disagree.

## Cutting a release

Both tracks build the same seven artifacts from the same workflow; only the
label differs. See [Branches, and the two release tracks](#branches-and-the-two-release-tracks)
for which tag goes where, and use `scripts/cut.sh` to set the version rather
than editing four strings by hand.

The version lives in one place as far as the *build* is concerned,
`[workspace.package] version` in the root `Cargo.toml`, and both build paths
read it from `cargo metadata`. A prerelease version — `1.3.0-dev.1` — flows
through unchanged: the tarball is `butai-1.3.0-dev.1-<triple>.tar.gz`, which is
also what `butai_update::asset_name` asks for, since it is built from
`CARGO_PKG_VERSION`. The two halves cannot disagree as long as the tag and the
manifest say the same thing, which is what `cut.sh` is for.

**The target matrix** — seven targets, kept in sync between
`.github/workflows/release.yml`, `scripts/release.sh` and the install table in
the project README:

| Target | Built by | Runner | Notes |
| --- | --- | --- | --- |
| `x86_64-unknown-linux-gnu` | `cross` | ubuntu-latest | glibc floor is `cross`'s container, not the runner's |
| `aarch64-unknown-linux-gnu` | `cross` | ubuntu-latest | |
| `armv7-unknown-linux-gnueabihf` | `cross` | ubuntu-latest | |
| `x86_64-unknown-linux-musl` | `cross` | ubuntu-latest | static: Alpine, distroless, scratch |
| `aarch64-unknown-linux-musl` | `cross` | ubuntu-latest | static |
| `aarch64-apple-darwin` | `cargo` | macos-14 | native on purpose |
| `x86_64-apple-darwin` | `cargo` | macos-13 | native on purpose |

macOS builds natively because the linker ad-hoc signs the binary as a side
effect, and **arm64 macOS refuses to exec an unsigned binary** (`Killed: 9`).
`cross` has no darwin image at all; `scripts/release.sh` refuses a darwin target
on a non-Mac host with that explanation rather than producing an artifact nobody
can run.

**The artifact shape** is one per target and identical across platforms — there
is no app bundle: `butai-<version>-<target>.tar.gz`, holding `butai`, `README.md`
and `LICENSE`. Plus one `SHA256SUMS` over every tarball.

**Locally:**

```sh
scripts/release.sh                                       # every target
TARGETS="x86_64-unknown-linux-musl" scripts/release.sh   # a subset
```

It wipes and rebuilds `dist/`, picks `cargo` for the host triple (and for either
Apple target when on a Mac) and `cross` for everything else, verifies the binary
landed under `$CARGO_TARGET_DIR`, stages, tars, and writes `SHA256SUMS` with
`sha256sum` or `shasum -a 256`. `cross` needs Docker running and
`cargo install cross --locked`.

**In CI:** pushing a tag `vX.Y.Z` runs `release.yml`. Each matrix leg builds,
packages, and — where the runner can execute what it just built
(`x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `aarch64-apple-darwin`)
— runs `butai --version` as a smoke test, which is what catches a binary that
links but dies on startup. The `publish` job downloads every artifact, computes
`SHA256SUMS`, and runs `gh release create` with `--generate-notes`. It is gated
on `startsWith(github.ref, 'refs/tags/v')`, so a `workflow_dispatch` run builds
the whole matrix and publishes nothing — that is the dry run.

A prerelease tag takes two different turns in the `publish` job. Its notes come
from `## [Unreleased]` rather than from a section named for the version, and a
thin one is not fatal — a dev tag is a build you cut to try something, and
failing the publish over its notes would only mean the binaries you wanted never
got built. A stable tag with no changelog section still fails, deliberately:
that one is an announcement, and shipping it with nothing written down is a
mistake worth stopping for.

**What `scripts/install.sh` expects of a release**, since it is the consumer:

- Assets named `butai-<bare-version>-<triple>.tar.gz` at
  `https://github.com/dieterpl/butai/releases/download/<tag>/`. A bare
  `butai-<triple>` binary is tried second so tags from before the tarballs still
  install.
- A `SHA256SUMS` whose lines name the asset. Absent, or with no sha256 tool on
  the box, verification is skipped with a message rather than failed.
- The tarball must contain an executable named `butai` somewhere inside it.

It maps `uname` to a triple, prefers the musl build wherever `ldd --version`
reports musl, honours `BUTAI_VERSION` and `BUTAI_INSTALL_DIR`, installs to
`/usr/local/bin` when writable and `~/.local/bin` otherwise, and tells you if
that directory is not on `PATH`. Windows is refused with the reason — Unix
domain sockets and termios — and WSL2 is pointed at instead.

## How this manual is written

Four conventions, and they are enforced by habit rather than by a linter:

1. **One page per subject.** A page owns its subject completely; another page
   links to it rather than restating it. [`README.md`](README.md) is the index,
   and a new page is not discoverable until it is listed there.
2. **Every page ends with a `## Where this lives` table** mapping its sections
   to the source files behind them, so a page can be checked against the code
   rather than trusted.
3. **A behaviour change is not finished until the page that owns it says so.**
   Any change to the wire protocol must update
   [`protocol.md`](protocol.md) — it is the normative spec, and `web/` and every
   external client read it.
4. **Prose is declarative and second person.** Say what happens and why the
   boundary is where it is; name the failure a rule was drawn around. No
   marketing, no emoji.

## Where this lives

| Section | Source |
| --- | --- |
| The tree, crate boundaries and dependency direction | `Cargo.toml`, `crates/*/src/lib.rs` |
| Toolchain pin, components | `rust-toolchain.toml` |
| Edition, MSRV, workspace deps, clippy lints, release profile | `Cargo.toml` |
| Formatting style and its caveat | `rustfmt.toml` |
| Every CI job, its trigger and its exact command | `.github/workflows/ci.yml` |
| Rust end-to-end tests, in-process daemon, `Screen`, `poll_until` | `crates/butai-server/tests/e2e_http.rs`, `e2e_socket.rs` |
| Suite entry point, profiles, lanes, flags, report location | `testsuite/run.sh` |
| Test image, terminal apps, the foreign-owned repo fixture | `testsuite/Dockerfile` |
| One-shot and interactive compose services | `testsuite/docker-compose.yml` |
| Registration, `xfail`/`XPASS`, timeouts, `ctx` methods, coverage enforcement | `testsuite/suite/runner.py` |
| The enumerated API surface a run is checked against | `testsuite/suite/coverage.py` |
| Isolated `HOME`, `BUTAI_SOCKET`, socket-path budget, `Config` defaults | `testsuite/suite/daemon.py` |
| `BUTAI_HOME` reaching panes by inheritance | `crates/butai-server/src/pane/terminal.rs` |
| Real PTY, the reconstructed screen, the contiguous-bytes trap | `testsuite/suite/tty.py` |
| Client-side frame application and display width | `testsuite/suite/screen.py` |
| Fake agent driver, phases, repaint rules | `testsuite/fakeagents/_lib.sh` |
| What each double draws | `testsuite/fakeagents/fake-claude`, `fake-aider`, `fake-tallfooter`, … |
| The real-agent layer | `testsuite/real-agents/Dockerfile` |
| The client's commands, dependencies and lockfile | `web/package.json`, `web/bun.lock` |
| Its compiler settings, and why each strict flag is on | `web/tsconfig.json` |
| The build, the dev proxy, and the relative `base` | `web/vite.config.ts` |
| The client's tests | `web/test/`, `web/README.md` |
| The bridge | `web/server/`, `web/README.md` |
| The `ts` feature, the derives, and where the bindings are written | `crates/butai-protocol/Cargo.toml`, `crates/butai-protocol/src/api.rs`, `.cargo/config.toml` |
| The generated TypeScript itself | `web/src/protocol/generated/protocol.ts` |
| Release matrix, packaging, publishing, the prerelease turn | `.github/workflows/release.yml` |
| Local release builds, builder selection, checksums | `scripts/release.sh` |
| Asset names, triple detection, install directory | `scripts/install.sh` |
| The branch gate, its steps and the dev daemon it starts | `scripts/vet.sh` |
| Setting the version across the manifest | `scripts/cut.sh` |
| `BUTAI_HOME` and every path derived from it | `crates/butai-protocol/src/paths.rs` |
| Which release the updater asks for, and how versions compare | `crates/butai-update/src/lib.rs` |
| Documentation conventions and the page index | `docs/README.md` |
