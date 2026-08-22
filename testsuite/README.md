# butai test suite

Runs a **real `butai daemon`** in a container and drives it the way a client
does: over the framed socket protocol and the REST API, with real PTYs running
real terminal applications.

The crate tests (`cargo test --workspace`) call `daemon::serve()` in-process on
the developer's machine. This suite is the other half — the binary, a Linux
userspace, `/proc`, container limits, git's ownership rules, and the terminal
apps and agent CLIs butai exists to host.

```sh
./testsuite/run.sh smoke        # ~1 min   API + protocol gate
./testsuite/run.sh standard     # ~10 min  the default
./testsuite/run.sh soak --minutes 30
```

Docker is the only requirement. Everything else — the daemon build, the test
harness, the agent doubles — is in this directory.

## Profiles

Profiles are cumulative: `standard` runs `smoke` too, `soak` runs all three.

| profile | for | covers |
| --- | --- | --- |
| `smoke` | "did I break the API" | handshake, dispatch, core routes, error shapes, agent status basics |
| `standard` | CI and pre-commit | every route and protocol variant, git, processes, the agent and terminal-app matrices, stress with latency percentiles |
| `soak` | before a release | steady-state drift: RSS, threads and descriptors over 30+ minutes |

`standard` and `soak` also run three short extra passes in separate containers
with `--pids-limit`, `--memory` and `--cpus` applied, because those limits
cannot be changed from inside a running container.

## What it tests

**The API, exhaustively.** All 60 HTTP routes, all 26 `Command` variants, all
10 `ServerMsg` variants, both wire encodings, every `KeyCode`, every
`AttachTarget`, and the SSE stream. `suite/coverage.py` enumerates that surface
and the run **fails if anything in it was never exercised**, so a route added
without a test shows up as a red line rather than as silence.

Ground truth is the Rust source, not `docs/protocol.md` — the docs currently
lag the code by ten routes, three enum variants, and the encoding of unit
variants (serde writes them as bare strings, `"ok"`, not `{"ok":null}`).

**Agents.** Agent status is heuristic: butai re-renders the pane and scans the
bottom eight lines for markers like `esc to interrupt` or a highlighted `❯ 1.
Yes`. There is no protocol, so compatibility is a property of what each CLI
*draws*. `fakeagents/` holds doubles that reproduce each real CLI's drawing, so
the whole state machine is tested deterministically with no API keys — plus
negative controls, because "to interrupt" and "do you want to" also appear in
ordinary agent prose.

`--real-agents` builds a layer with the actual CLIs and runs the same
assertions against them. That lane is what catches an upstream reword; without
it the fakes could agree with butai and both be wrong.

**Terminal applications.** btop, htop, top, vim, nano, less, ncdu, mc and a
nested tmux are each spawned in a pane and read back *through the wire
protocol* — the same path a GUI client sees. Alongside them are probes for
SGR attributes, truecolor, wide characters, the alternate screen, SIGWINCH, and
the cursor-position and device-attribute queries the daemon answers on the
child's behalf.

**Stress.** Output floods, client fan-out, resize storms, restart storms, input
floods, a repository with tens of thousands of untracked files, and a hung
external tool on the sampler's path. These report percentiles and resource
timelines rather than a bare pass, because "it didn't crash" is not the
question.

## Reading a run

Each lane writes `results.json` and a self-contained `report.html` to
`testsuite/out/<lane>/` — set `BUTAI_TEST_OUT` to put them elsewhere, which is
worth doing if the working copy is on a network mount. Two sections matter most:

- **KNOWN GAPS** — tests marked `xfail`. These describe behaviour butai gets
  wrong today, with the reasoning, and they keep the suite green while staying
  visible. A gap that starts passing is reported as `XPASS`, which is the
  signal to delete the annotation.
- **The matrices** — agent compatibility, status detection by client size,
  terminal apps, SGR attributes. These are reported rather than asserted,
  because they document an envelope rather than a bug.

## Layout

```
run.sh              build the image, run a profile, drive the limit lanes
Dockerfile          multi-stage: cargo build -> Debian runtime with the apps
docker-compose.yml  one-shot runs, plus a daemon + web client to poke at by hand
suite/              the harness (standard library only)
  runner.py         registration, profiles, timeouts, xfail, reporting
  butai.py          framed + HTTP + SSE clients
  msgpack.py        just enough MessagePack for rmp-serde's named maps
  screen.py         applies FrameUpdate damage to a grid, like a real client
  daemon.py         daemon lifecycle, isolated per test in its own HOME
  fixtures.py       workspaces, git repos, terminal probe scripts
  metrics.py        latency percentiles, /proc sampling, leak slopes
  coverage.py       the enumerated API surface the run is checked against
tests/              the tests, numbered by area
fakeagents/         scripted agent CLIs that draw what the real ones draw
real-agents/        the opt-in layer that installs the real ones
```

## Writing a test

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

Useful `ctx` methods: `daemon()`, `cover()`, `note()` (free text in the report),
`metric()`, `row()` (a matrix row), `require()` / `skip()`, and `scale` /
`soak_seconds` for load-dependent tests.

Each daemon gets its own `HOME`, and `~/.butai` holds the socket, config, logs
and session store — so isolating a daemon is a single environment variable, and
a dozen differently-configured daemons can run side by side.

Two defaults differ from a user's: `exit_when_empty` is off (the shipped default
stops the daemon with its last workspace, which is right for a person and fatal
for a test that asks another question afterwards), and agents point at the
doubles. `test_00_daemon.py` pins the shipped behaviour of both.

## Notes for a container deployment

Things the suite measures that are easy to get wrong in Docker, and are
asserted here so they stay true:

- **Git ownership.** A bind-mounted repo owned by a different uid makes
  `Repository::discover` fail; butai swallows that error, so the symptom is a
  missing CHANGES rail and `changes: null` — not an error message. Fix with
  `git config --global --add safe.directory <path>`. Both halves are tested.
- **Git identity.** No `user.email` means every commit fails with a 400.
- **The socket's parent directory** is chmodded `0700` by the daemon, so it must
  be a directory the daemon's user owns. `~/.butai` is the default.
- **`exit_when_empty`** ships on: closing the last workspace stops the daemon.
- **System gauges read the host.** `/proc/stat` and `/proc/meminfo` are not
  namespaced, so the SYSTEM rail ignores `--cpus` and `--memory`.
- **Every pane costs two OS threads**, so `--pids-limit` is a real ceiling on
  how many panes a container can hold.
