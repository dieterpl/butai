# `web` — the butai browser client and its bridge

The browser client, being rewritten in TypeScript. **Not yet the default**: the
clients that serve today are the vanilla one at `/` and the React preview at
`/ui/`, both still under `web/`, and both keep working until this reaches parity.

Today this directory holds the **bridge** and the **generated protocol types**.
The app itself lands on top of them.

## Running it

```sh
bun install
bun run bridge          # or `bun run dev:bridge` to reload on save
```

It serves the two existing clients unchanged, so it is a drop-in for
`python3 server.py` and can be checked against one by swapping the command.

| | |
| --- | --- |
| `BUTAI_SOCKET` | the primary daemon; on its own it is the whole configuration |
| `BUTAI_SOCKETS` | any others, `name=/path.sock` or a bare path, comma-separated |
| `BUTAI_SOCKET_NAME` | renames the primary (`local` by default) |
| `BUTAI_SOCKET_DIRS` | where a **runtime-added** socket may live |
| `PORT` | 8080 |

## Why a bridge exists at all

A browser cannot open an AF_UNIX socket, and the daemon listens on nothing else
(`crates/butai-server/src/daemon.rs`). So this translates — and only translates:
`/api/*` to the daemon's `/v1/*`, `/ws` to its framed protocol, `/api/events` to
its SSE push channel. It parses neither payload, which is what lets a new
`ApiEvent` tag arrive without a line changing here.

| file | role |
| --- | --- |
| `server/roster.ts` | which daemons this bridge speaks for; key derivation and the socket allowlist |
| `server/routing.ts` | the qualified-id rule — `<key>:<n>` in, bare `<n>` out, and every refusal that keeps an id from one machine off another |
| `server/proxy.ts` | one round trip, over a Unix socket |
| `server/snapshot.ts` | `/api/state`: the union across every daemon |
| `server/events.ts` | `/v1/events` → SSE |
| `server/ws.ts` | WebSocket ↔ the daemon's 4-byte length prefix |
| `server/static.ts` | the two legacy clients, with their traversal rules |
| `src/protocol/generated/` | 79 wire types, from `butai-protocol` — see `docs/development.md` |

## Checking it

```sh
bun run typecheck
bun test                                  # needs a butai binary; see below
bash test/compare-bridges.sh              # this bridge vs server.py, diffed
```

`test/compare-bridges.sh` starts **one** isolated daemon, points both bridges at
it and compares every answer — replies rather than assertions, because
assertions would encode what the author *believed* `server.py` does, which is
the thing under test. It found two real differences on its first run, one of them
in the socket allowlist. It is a migration-time tool and goes when `server.py`
does.

Both need a butai binary. They default to `/var/tmp/butai-probe/butai`;
`BUTAI_BIN=<path>` overrides it. Build one with
`cargo build -p butai` and **copy it somewhere private** — a shared
`CARGO_TARGET_DIR` has one `debug/butai`, and a concurrent worktree relinking it
mid-test reproduces the old behaviour of whatever you just fixed.

> `BUTAI_BIN`, not `BUTAI`. A butai pane exports `BUTAI` already, set to the
> *socket* path — so `${BUTAI:-<a binary>}` resolves to a socket and the harness
> tries to execute it. The tests clear the whole family for the same reason: a
> daemon that inherits `BUTAI_SOCKET` is not isolated, it is the user's own, and
> it will restore their session and spawn their agents.

## Not done yet

Lint (ESLint flat config) arrives with the React code in the next step, where the
rules that matter are the ones about hooks. Typecheck and the tests are the gate
until then.
