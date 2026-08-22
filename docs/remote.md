# Remote machines

The daemon never listens on TCP. It binds one `AF_UNIX` socket and speaks both
protocols on it — the framed pane stream and the `/v1/*` HTTP API — so reaching a
daemon that is not on this machine is always the same problem: **make its socket
a path you can open.** SSH is how that is done, and SSH is also the whole of the
authentication (see [Security](#security)).

This page is every way in, the `[[remote]]` block that automates them, how
several daemons share one tab bar, and what breaks when a machine goes away. The
config mechanics are [configuration.md](configuration.md), the wire is
[protocol.md](protocol.md), the flags are [cli.md](cli.md), and deploying a
daemon somebody else's product dials is [embedding.md](embedding.md).

## Every way to reach a daemon

| | What you run | What the far end needs | How it fails |
|---|---|---|---|
| **The local socket** | `butai` | nothing | Nothing to fail. `$BUTAI_SOCKET`, else `~/.butai/butai.sock`, else `/tmp/butai-<uid>/butai.sock` with no home directory. A socket that does not answer is **spawned**, not reported |
| **An explicit socket** | `butai --socket <path> …` | a daemon on that path | Same connect-or-spawn: a path that does not answer gets a *local* daemon bound to it. See [The auto-spawn hazard](#the-auto-spawn-hazard) |
| **A stdio bridge** | `ssh host butai proxy` | `butai` on `PATH` or in `~/.local/bin` | ssh's own errors on stderr. One stream, so this drives **one pane** — it cannot be a whole client |
| **A forwarded socket** | `ssh -N -L /tmp/host.sock:<far socket> host`, then `--socket /tmp/host.sock` | a daemon already running there | `ssh -L` refuses a path that is already bound; a dead tunnel leaves a socket file that answers nothing |
| **A `[[remote]] socket` block** | nothing — it is in `config.toml` | somebody else's forward already up | Connected before the first frame; an unreachable one is a startup warning, not a fatal error |
| **A `[[remote]] host` block** | nothing — the client runs `ssh` for you | `butai` findable, and a key that works **without a prompt** | Dialled after the first frame on its own task; failures land in the footer |
| **The host picker** | `alt-h`, choose a machine | same as above | Same, plus the machine is written into `config.toml` once it answers |
| **The ssh handoff** | `ssh host`, then type `butai` | `butai` on that machine | Silent no-op if the near terminal is not a butai pane. See [`remote_announce`](#remote_announce) |
| **A private daemon** | `butai standalone` | — | Not reachable at all, by design: a socket in a `0700` directory named for the pid, removed on exit |

Two of these are the whole architecture and the rest are conveniences:

- **`butai proxy` bridges stdio to the socket.** It is enough to stream one
  pane, and not enough to *be* a client: a client needs REST, an event stream and
  a pane connection at once, and HTTP down one pipe is one request at a time with
  nowhere to hold the event stream open beside them.
- **`ssh -L` produces a socket path**, which is exactly the unit everything else
  takes. That is why `[[remote]]`, the browser bridge and both native clients all
  take sockets rather than ssh commands, and why the tab bar can span machines
  with no daemon relaying another.

## What the far end must have

Reaching another machine means running *its* copy of this program. Three things
do that — the host picker asks it `--json whoami`, the proxy runs `proxy`, and
the handoff writes an APC the daemon recognises — and all three resolve the
binary the same way, with this shell fragment:

```sh
for n in <every name this program has shipped under>; do
  [ -x "$HOME/.local/bin/$n" ] && BUTAI="$HOME/.local/bin/$n" && break
  BUTAI="$(command -v "$n" 2>/dev/null)"; [ -n "$BUTAI" ] && break
done
```

`~/.local/bin` first because that is where `cargo install` lands and it is
routinely missing from a non-interactive ssh's `PATH`; `command -v` second for a
system install.

**Why it is a list and not a name.** `butai_protocol::names::BINARIES` holds
every name this program has shipped under, most recent first, and the project has
been renamed once already. A rename that lands only in this repository leaves
every machine you have not upgraded unreachable — and says so in the one way that
reads as your fault, "not installed", about a machine carrying a perfectly good
install under the old name. So the far side is searched for all of them, the
current name wins on a half-upgraded machine, and the APC parser accepts any of
them too. Removing a name from the tail is a decision to stop talking to machines
that still have it. The current list is in
`crates/butai-protocol/src/names.rs`.

A machine with none of them fails with the names it looked for:

```
no butai … there — install it on that machine (~/.local/bin/butai or on its PATH)
```

## The socket path is never guessed

`~/.butai/butai.sock` is not guaranteed — without a home directory to resolve, the
daemon lives under `/tmp/butai-<uid>` — and `ssh -L` forwards the path
**verbatim**, with no shell expansion to save you. So the path comes from the far
side itself, two ways:

- **A handoff arrives with it attached.** The far `butai` announced its own
  socket; nothing needs configuring.
- **A machine you picked has announced nothing**, so it is asked:

  ```sh
  butai ls >/dev/null 2>&1; butai --json whoami
  ```

  `whoami` contacts no daemon and reports the socket *this invocation would talk
  to*, which is why it answers outside a pane. The `ls` in front of it is what
  makes the daemon exist: connecting to a machine that is not running butai yet
  has always started one.

The reply is parsed tolerantly — anything printed before the first `{` is
skipped, because a login shell's rc files write to stdout more often than their
authors think, and one `echo` would otherwise make a machine unreachable.

## `[[remote]]`

Machines whose workspaces join this tab bar, connected at start so they are there
without a gesture every morning. The full field reference is in
[configuration.md](configuration.md#remote); this is what each one *does*.

| Key | Type | Effect |
|---|---|---|
| `host` | string | An ssh destination: an alias from `~/.ssh/config`, or `user@host`. The client dials it |
| `ssh_args` | list | Extra ssh arguments, placed **before** the destination — `["-p","2222"]`, `["-J","bastion"]` |
| `socket` | string | A socket already reachable from here. Used *instead of* dialling ssh |
| `socket_path` | string | Where the **far** daemon listens. Normally unset, so the far `butai` resolves its own default and finds the daemon already running there rather than starting a second one on a path nothing else uses |
| `name` | string | The badge this machine's tabs carry |

**Naming.** A `host` block is badged `name`, else the destination. A `socket`
block is badged `name`, else `host` if the block also set one, else the last
`/`-separated component of the socket path — `fwd.sock`, extension included.

**`host` and `socket` are read by different code paths.** Sockets become
*endpoints*, connected before the first frame because a socket in the config is
already reachable. Hosts become *dials*, run on their own tasks after the first
frame, because an ssh connection is seconds of DNS, TCP and key exchange and one
sleeping machine must not mean a client that shows nothing for twenty seconds. A
block setting both is therefore used by each in turn and produces **two** entries
in the tab bar; treat them as mutually exclusive. A block setting neither is
skipped by both and does nothing.

**Only a deliberate connection is remembered.** `alt-h` → a machine writes a
`[[remote]]` block once that machine has actually answered — remembering a host
that turned out to have no daemon would put a failure in the file and re-run it
every morning. It is idempotent by `host`, and it writes `host` alone: the picker
dials with no extra ssh arguments, so there are none to record. A machine that
announced itself from inside a pane is adopted for the session and left out of
the file entirely; otherwise a week of `ssh`-ing around turns every morning into
a start that dials nine machines and waits on the seven that are asleep.

Disconnecting a machine (`alt-h`, or the tab's row menu) removes its block. A
`socket` block is the exception and is never forgotten — it is somebody else's
forward, there is no ssh of ours to kill, so the picker's row for it says so
rather than offering a disconnect it would then refuse.

## How butai uses ssh, and how it does not

### What it runs

| Purpose | Command |
|---|---|
| Ask where the daemon is | `ssh -T -o BatchMode=yes <control opts> <args> <target> '<find>; butai ls >/dev/null 2>&1; exec butai --json whoami'` |
| Forward the socket | `ssh -N -T -o BatchMode=yes -o ExitOnForwardFailure=yes <control opts> -L <local>:<far> <args> <target>` |
| Stream one pane | `ssh -T <control opts> <args> <target> '<find>; exec butai proxy'` |

`-T` everywhere: these carry a binary protocol or one JSON document, and a pty
would echo and newline-translate it. `-N` on the forward because that connection
exists only to carry the tunnel.

**`BatchMode=yes` on the two the client runs for itself.** A key that needs a
passphrase typed, or a password, cannot be used to connect a machine from inside
the workbench — there is nowhere to type it. Use an agent. `butai proxy` does not
set it, so a script running that by hand can still be prompted.

**Connection sharing.** All three pass `ControlMaster=auto`,
`ControlPath=~/.butai/ssh-%C` and `ControlPersist=60`. Connecting one machine is
two ssh runs back to back — ask, then forward — and without a master the second
repeats the first's key exchange. `%C` rather than `%r@%h:%p` because a control
socket is still a Unix socket and still bound by the ~104-byte `sun_path` limit;
the hash form is fixed width however long the destination is. Without
`~/.butai` the option is simply ineffective and each channel opens its own
connection.

**Keepalives.** The same three also pass `ServerAliveInterval=15` and
`ServerAliveCountMax=3`, so a link that stops answering ends itself in about 45
seconds. A laptop that sleeps or changes network leaves TCP half-open — nothing
delivered, nothing refused — and without these ssh notices only when the kernel
gives up, which can be hours. The master is the reason it matters: the `-N`
forward is long-lived, so it is usually the one holding the control socket, and
a *wedged* master is worse than a dead one because every later ssh multiplexes
onto it and hangs too. That is what used to make even a deliberate reconnect
impossible without quitting butai.

**Timeouts**, all sized for a bad link rather than a LAN: 20s for the far side to
say where its daemon is (a cold start reads a session file and reopens its
workspaces first), 15s for `ssh -L` to bind, 30s for a proxy connection's
handshake, 1.5s for the handoff's terminal probe. A forward is waited on by
*connecting* to it, not by watching for the file — ssh creates the socket before
it is usable — and a dead ssh is reported by its own stderr rather than by the
timeout.

### `~/.ssh/config`

butai reads it for exactly one thing: **the rows of the host picker.** It parses
`Host`, `HostName`, `Port` and `User`, follows `Include` (relative to `~/.ssh`,
one level of `*`/`?` globbing, cycle-safe, five deep), takes the first block for
a repeated alias, and skips any pattern containing `*`, `?` or a leading `!` —
those set defaults for other blocks and are not things you can connect to.

Everything else is ssh's job, because the connection is made by running
`ssh <alias>`: `Match` blocks, `ProxyJump`, `IdentityFile`, canonicalization,
per-token expansion and every keyword not in that list are resolved by ssh
itself and are never read here. A missing or unreadable config is not an error —
most machines have none, and the picker is simply empty.

## What the client connects to, and when

1. **The local socket**, connect-or-spawned, then primed with a full read of
   `/v1/workspaces`, each workspace's detail and `/v1/system`. The event stream
   only sends what changed, so this one-time catch-up is what makes the first
   frame complete.
2. **Every `[[remote]] socket`**, in file order. One unreachable machine is a
   warning in the footer, not a failure — a forwarded socket whose tunnel is down
   is the ordinary case. Only if *nothing* answers does the client refuse to
   start.
3. **Every `[[remote]] host`**, dialled on its own task, landing whenever it
   lands. Each one asks for the far socket if `socket_path` did not say, forwards
   it, connects, primes, and pushes a new tab-bar entry.

The forward is held for the session: dropping it kills the ssh and removes the
socket, so a machine that goes away leaves nothing behind. Its local path is
`$XDG_RUNTIME_DIR/butai-forwards/<target>-<pid>.sock` (the temp dir when there is
no runtime directory), with the target reduced to `[A-Za-z0-9_-]` and cut to 24
characters — a socket path is length-limited and the limit counts the whole
path, so a long alias must not push it over.

### The auto-spawn hazard

Connect-or-spawn is how a socket that does not answer becomes one that does: the
client forks `butai daemon` with `BUTAI_SOCKET` pointing at that path, and the
daemon removes any stale socket file before binding. That is right for the local
socket, and it is what makes `butai --socket /tmp/x.sock ws ls` work anywhere.

On a *forwarded* path it would mean something else. A tunnel that is down would
get a **local, empty daemon on the forward's path** rather than an error — one
wearing the far machine's name, answering with none of its workspaces, having
deleted the socket file ssh left behind, and leaving a lock file that stops the
real one being restored.

**So a handle to a forwarded daemon never spawns.** The client builds those
differently from the local one (`Api::remote` rather than `Api::new`), and a
forwarded socket that has gone quiet is an error, which is what lets the forward
be [rebuilt](#rebuilding-the-forward) instead. The event stream and pane
connections have always used a connect that never spawns; REST was the third path
and was the one that could still do it.

**The CLI keeps auto-spawn on every socket**, including a forwarded one, because
that is the whole point of `butai --socket <path> ls`. So the hazard is still
reachable by hand. If you find a machine in the tab bar with no workspaces on it,
kill the impostor by socket:

```sh
butai --socket /tmp/host.sock kill-server
```

Never `pkill` a daemon by pattern; it matches your real one.

## The fleet

**One tab bar, flattened across machines.** Each connected daemon is one more
entry in the client's list; the tab bar is every daemon's workspaces in
connection order, with the machine as a *badge* rather than a level of hierarchy.
There is no relay: no daemon is a client of another, and the first machine never
learns the second exists.

- **The badge is empty for the local daemon** — there is nothing to qualify it
  against — and appears on every machine's tabs once more than one is connected.
- **BOOTH** (`alt-0`) is the one page that spans daemons: every agent on every
  machine in the left column, the selected agent's live pane in the middle, and
  one telemetry row per machine on the right. Telemetry stays per machine —
  averaging four boxes' CPU produces a number that is true of nothing. Every
  other page is *about a workspace* and resolves through the active one, which is
  why they can stay scoped to a single machine and this one cannot: a file tree
  merged across four hosts is a tree where two `src/main.rs` rows are different
  files.
- **Opening a workspace asks which machine first** when more than one is
  connected — machine, then folder — because "open a workspace" is otherwise a
  question with no answer.
- **Pinning an agent writes the *client's* config**, not the far machine's. A
  daemon on another machine keeps its own `config.toml` over there, and a pin
  naming an agent it does not have falls back to the first row rather than to
  nothing: the config is the client's and the agent list is the daemon's, so the
  two can legitimately disagree.

### Qualified ids

Workspace ids and pane ids are **per-daemon integers**. Two daemons both have a
workspace 1 and both have a pane 5. The TUI keeps a daemon index beside every id
and never crosses them; the browser bridge, which has to put ids in URLs, writes
every one of them `<daemon-key>:<n>` and rewrites the segment back to a bare
number when it forwards:

```
GET /api/workspaces/gpu:1/panes/gpu:5/ack  ->  gpu: POST /v1/workspaces/1/panes/5/ack
```

A string rather than an `{id, daemon}` pair for one reason: a bare integer
compared against `"gpu:1"` never matches, so code that forgot to qualify renders
*nothing* — which you see — instead of quietly acting on another machine. The
refusals that fall out of it, and the `BUTAI_SOCKET` / `BUTAI_SOCKETS` /
`BUTAI_SOCKET_NAME` roster it indexes, are in
[`web/README.md`](../web/README.md#several-daemons-one-tab-bar).

### When a machine goes away

The event stream drops, and the client's stream task reconnects on its own with
backoff from 250ms doubling to 10s, forever. Meanwhile:

- The footer flashes `daemon: <why>` once.
- **The stage keeps its last frame and says it is one.** A card over the dimmed
  screen names the machine, counts how long it has been away, and says that what
  is behind it is the last frame rather than what is happening now. See
  [the disconnected stage](workbench.md#when-the-stage-loses-its-machine).
- **Every chip for that machine takes a `·`** in the column its padding already
  reserved, and stops being painted as urgent: the `!` counts behind it were
  taken when the link died, and a red chip is a summons to somewhere you cannot
  currently go. BOOTH's compute column says `away` where the agent count goes.
- **That machine's rails keep drawing their last known contents**, unmarked. The
  tab they hang off is marked, which is the signal; dimming the rails themselves
  is not done yet.
- **If the link was one we dialled, the forward is rebuilt.** See below.

The pane on the far machine is untouched by any of this — that is why the frame
is worth keeping. A `kill-server` snapshots every workspace and restores it on
the next start, and a forward that died never reached the far machine at all. An
empty stage would say the agent went away with the link, which is the one thing
that did not happen.

#### Rebuilding the forward

Retrying the socket is the whole answer when the far *daemon* restarted. It is
no answer at all when the *forward* died — a lid closed, a network changed, a
VPN dropped — because that socket was ssh's and went with it. Nothing else
re-runs `ssh -L`, so the stream task would retry a path that could never come
back, `hosts` would go on naming the machine, and picking it again in `alt-h`
was refused as "already in the tab bar". Quitting butai was the only way out.

So a drop on a machine with an ssh of ours behind it is re-dialled:

| | |
|---|---|
| **What counts as gone** | the forward's ssh has exited, which is conclusive; or the stream has dropped twice in a row, which is what a half-open link looks like while ssh has not yet given up on it |
| **What is not re-dialled** | the local daemon, and a `[[remote]] socket` — somebody else's forward, with no ssh of ours to re-run |
| **How often** | 5s, doubling to 5 minutes, per machine. An hour of being off costs under twenty attempts rather than one per stream retry, each able to spend 20s in `whoami` |
| **With which arguments** | the ones it was reached with. `ssh_args` and an announced machine's recovered arguments are kept for as long as the machine is in the bar, because a re-dial that dropped `-J bastion` would be dialling somewhere else |
| **Where it lands** | the tab it already had. Removing and re-appending would move every tab after it |
| **What is written** | nothing. The `[[remote]]` block is already there, or was deliberately never written, and coming back must not change which |

The footer says `<host> went away — reconnecting`, then `<host> is back`.

The old forward is dropped *before* the new dial goes out, which matters twice: a
forward's path is (target, client pid), so the re-dial binds the same one and
would otherwise be deleted by the old `Forward`'s cleanup; and killing the old
ssh is what releases the ControlMaster, without which the dial multiplexes onto
a half-open master and hangs. Rationale in [design.md](design.md).

Dropping a machine deliberately (`alt-h`, choose a connected row) kills its ssh,
removes its forward's socket, and **forgets the daemon** — which is the half that
makes it visible: without it the tab bar went on showing live-looking rails, the
badge went on claiming the machine, and reconnecting was refused as "already in
the tab bar". Two guards keep it from removing the local daemon: the entry must
carry a badge (the local one has none by definition) and the socket must be one
*we* forwarded. The far daemon is untouched — it keeps running with every pane it
had, which is what makes reconnecting cost one ssh and no confirmation box.

## `remote_announce`

Typing `butai` after `ssh` puts that machine's projects in the tab bar you
already had. The mechanism is a terminal query, because `$BUTAI` does not survive
ssh and there is no other environment channel:

1. The far `butai` writes Secondary DA (`ESC[>c`) and reads the answer. It only
   does this with `$SSH_CONNECTION` set and both stdin and stdout ttys, so a
   local run never pays for it, and `BUTAI_NO_HANDOFF` skips it.
2. butai's emulator answers with `98` in the identifying field, the way tmux
   answers `84`. Every terminal answers DA2, so a plain one answers promptly with
   something else rather than costing a full timeout.
3. On a confirmed butai answer only, the far side writes a one-way APC —
   `ESC _ butai;here;<user@host>;<socket> ESC \` — prints one line and exits.

The near **daemon** is the only party that can see this: it parses every byte a
pane writes. It emits `remote_announce` on `GET /v1/events` and **does not act**,
because whose tab bar those projects appear in is a property of the *client*, and
one daemon dialling another is a relay.

| Field | What it is |
|---|---|
| `pane` | the pane the announcement came out of |
| `hint` | `user@host` as the far side derived it from `$SSH_CONNECTION`. A fallback only: behind NAT it is an address that means nothing here |
| `socket` | the far daemon's socket, for `ssh -L` |
| `ssh_target`, `ssh_args` | recovered from the pane's **own foreground process** — it is running the `ssh` that got there, so these reach the same host the same way, through the same jump hosts, with the same key. Empty when the process could not be read |

The argument recovery reads the pane's process group leader, checks it really is
`ssh`, and splits the argument list into flags and destination — dropping the
remote command and the flags that would fight with how the client re-dials
(`-t`, `-T`, `-N`, `-f`).

What a receiver does with it is its own decision. The TUI dials the machine on
its own task, subject to `[general] remote_auto_attach` (default on) and to the
machine not already being connected or on its way — a pane can announce more than
once and each one must not add a tab. It is adopted for the session, not written
to the config. The browser bridge cannot dial at all — it is usually a container
with no keys — so it turns the announcement into the two commands that *would*
connect it, said once per machine. A client that ignores the tag loses nothing
but the convenience.

## Version skew

Two numbers cross the wire and they answer different questions.

| | What it is | What a mismatch does |
|---|---|---|
| `proto_version` | one integer, currently `1`. Additive changes — a new command, a new route, a new SSE tag — do **not** bump it | The daemon refuses the connection: an `error` naming both numbers, then `detached` |
| `server_version` | the daemon's package version, on its Hello | Nothing breaks. The client puts a notice in the footer |

The notice is the point of the field:

```
daemon is 0.6.1, client is 0.7.0 — restart it: butai kill-server
```

and, for a daemon too old to send the field at all, `daemon predates this client
(0.7.0)`. That absent case is the one it exists for — a daemon left running
across an upgrade answers a client many commits ahead of it, every symptom points
at a feature, and none of them point at the version. Because `server_version`
rides the framed Hello, the notice appears when a pane is put on the stage rather
than at connect time.

Both directions skip what they do not understand. An undecodable frame is
skipped, not fatal — dropping the connection over one turns "one release behind"
into a reconnect loop, and a real session was caught doing that 25 times, looking
like the stage blanking rather than like anything version-shaped. A cap on
*consecutive* bad frames still ends a stream that is genuinely desynchronised.
Unknown SSE tags are skipped the same way. Framing errors stay fatal: a bad
length prefix means the next frame boundary is no longer known.

## Over a slow link

**What crosses per tick.** On the pane connection, a damage diff at most every
16ms and only when something changed — row-contiguous cell runs, msgpack-encoded
for the TUI — plus `full: true` on attach and resize, meaning clear then apply.
On the event stream: `system` and `workspaces` every ~2s while anything is
subscribed, `workspace_detail` on the frame clock but **only for workspaces whose
contents actually differ from the last one sent**, and `notification` / `git_op`
as they happen. An idle daemon with an idle repository sends two small records
every two seconds and nothing else. There is no separate keepalive; the 2s
`system` push is what a reader sees on a quiet machine.

**While stalled**, the client keeps drawing. The ssh dial, the socket forward and
the GIT page's reads all run on their own tasks precisely so they cannot freeze
the loop — an ssh awaited inline stopped the screen repainting and the keyboard
responding, which is what the first live run of the host picker did. A pane whose
frames stop shows its last frame; the rails show their last push.

**A reconnect resumes on its own.** The stream task re-opens `/v1/events`, and
the daemon sends `system` and `workspaces` to a new subscriber immediately rather
than making it wait for the first change. Details follow within one sampler tick:
the daemon clears its per-workspace "last sent" memory whenever it has no
subscribers, so the next push after you come back is a full one for every
workspace. Nothing needs to re-prime and nothing needs a restart.

That covers a stream that dropped over a link still standing. When the link
itself is gone the socket is too, and the client rebuilds the `ssh -L` before any
of the above can happen — [When a machine goes away](#rebuilding-the-forward).

**Changing panes does not reconnect.** `watch` re-points a live connection and
answers with a full frame, so switching what is on the stage costs one message
rather than a fresh dial — a visible stall on any link with latency. It re-points
*within one daemon*: crossing to another machine is another socket, and that is a
second connection.

## Security

**What the code enforces:**

- `~/.butai` is `chmod 0700` by the daemon when it binds. The socket file itself
  is left at the default umask, so the directory is the protection.
- An exclusive `flock` on `butai.lock` beside the socket means one daemon per
  path; a client that cannot take it shared knows one is coming up and waits
  rather than forking a second.
- `butai standalone` binds inside a `0700` directory named for its pid and
  removes it on exit.

**That is all of it.** There is no authentication, no authorization and no
multi-user model. The socket's file permissions answer exactly one question — are
you the uid that started it — and a connection grants:

- **Arbitrary process execution.** `POST /v1/workspaces/{id}/processes` runs a
  command of the caller's choosing, and spawning an agent passes that CLI's
  auto-approve flag by design.
- **Typing into anything already running**, without attaching.
- **Filesystem read and write** as that user, and git writes including
  `reset --hard`, discard, branch delete and push.

Treat the daemon's user as the blast radius, and note two things that follow for
remote access specifically:

1. **SSH keys are the authentication the socket does not have.** That is why
   there is no TCP listener to configure and no password to set: the daemon's
   reachability *is* your ssh policy.
2. **A forwarded socket is a live shell on the far machine.** The client puts its
   forwards under `$XDG_RUNTIME_DIR` when there is one, because `/tmp` is
   world-listable — but it creates that directory at the default umask and relies
   on the runtime directory's own `0700`. With no `$XDG_RUNTIME_DIR`, forwards
   land in the temp directory; check its permissions before trusting a shared
   host.

Anything that relays the socket inherits all of this and adds nothing. The
browser bridge ([`web/server/`](../web/server)) binds `0.0.0.0` with no
authentication of any kind, and with several daemons configured one
unauthenticated port reaches all of them. Its `POST /api/daemons` is bounded by
`BUTAI_SOCKET_DIRS` — a path outside
the directories the bridge was started with is refused — but that boundary
protects the bridge's roster, not the daemons already on it. Put a reverse proxy
that authenticates, an SSH tunnel, or a private overlay network in front of
anything reachable from elsewhere. See
[`web/README.md`](../web/README.md#security) and
[embedding.md](embedding.md#security).

## Where this lives

| Section | Source |
|---|---|
| Socket paths, `~/.butai`, `$BUTAI_SOCKET`, the `/tmp` fallback | `crates/butai-protocol/src/paths.rs` |
| Every name the far side is searched for | `crates/butai-protocol/src/names.rs` |
| Finding the far binary, `proxy` transport, ControlMaster options | `crates/butai-client/src/dial.rs` |
| Asking `whoami`, `ssh -L`, forward lifetime and paths | `crates/butai-client/src/ssh.rs` |
| `~/.ssh/config` parsing for the picker | `crates/butai-client/src/ssh_config.rs` |
| Connect-or-spawn, connect-existing, transports, Hello | `crates/butai-client/src/conn.rs` |
| `Api::new` vs `Api::remote`, and the event stream | `crates/butai-client/src/api.rs`, `daemon.rs` |
| Noticing a dead forward, the re-dial and its backoff | `crates/butai-client/src/workbench.rs` (`redial_lost`, `redial_due`) |
| Drawing that a machine is away: the stage card, the chip marker | `crates/butai-client/src/chrome/mod.rs` (`StageDown`, `draw_stage_down`, `TAB_AWAY_MARK`), `crates/butai-client/src/workbench.rs` (`Stage::mark_lost`) |
| `[[remote]]` fields, `save_remote` / `forget_remote` | `crates/butai-client/src/config.rs` |
| Endpoints vs dials at startup | `crates/butai-client/src/lib.rs` |
| The fleet, host picker, adoption, disconnect, skew notice | `crates/butai-client/src/workbench.rs` |
| The ssh handoff: DA2 probe and the APC | `crates/butai/src/handoff.rs` |
| `butai proxy` | `crates/butai/src/proxy.rs` |
| `butai standalone`'s private socket | `crates/butai/src/standalone.rs` |
| APC parsing, `ssh_dial_back`, argument splitting | `crates/butai-server/src/pane/terminal.rs` |
| `remote_announce` emission | `crates/butai-server/src/core.rs` |
| `RemoteAnnounceDto` | `crates/butai-protocol/src/api.rs` |
| Socket bind, `0700`, the flock | `crates/butai-server/src/daemon.rs` |
| Protocol version check, framed/HTTP routing on one socket | `crates/butai-server/src/client_conn.rs`, `core.rs` |
| Multi-daemon roster and the socket allowlist | `web/server/roster.ts` |
| Qualified ids, and every refusal that keeps one machine's id off another | `web/server/routing.ts` |
| The fleet's row model, and the page that draws it | `web/src/logic/fleet.ts`, `web/src/pages/HomePage.tsx` |
