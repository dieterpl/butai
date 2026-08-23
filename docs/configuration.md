# Configuration

butai is configured by two files and a handful of environment variables.
`~/.butai/config.toml` is yours and applies everywhere; `.butai.toml` in a
project root belongs to the project and says what that workspace brings up.
Everything in both is optional — butai starts with no configuration at all, and
nothing you can set is required for it to work.

Two rules run through this whole page:

> **One file, two readers.** `config.toml` is parsed twice — once by the daemon
> and once by the client — into two structs that declare different tables.
> Neither sets `deny_unknown_fields`, so each side sees only its own half and
> silently ignores the other's.

> **Writes are surgical.** When the workbench writes a setting back, it rewrites
> one key through `toml_edit` and leaves every other key, comment and blank line
> exactly where it was. This is a file people edit by hand, and it stays one.

[keys.md](keys.md) owns the keymap and [theming.md](theming.md) owns the
palette. This page covers the mechanics: which file, which key, which default,
who reads it, and what a wrong value does.

## The files

Everything butai stores lives in one directory, `~/.butai`. With no home
directory to resolve, that becomes `/tmp/butai-<uid>` — uid-scoped, never a
shared path another user could have created first. The daemon `chmod`s the
directory `0700` when it binds the socket.

| Path | Written by | Missing | Malformed |
|---|---|---|---|
| `~/.butai/config.toml` | you, plus the SETTINGS page and a few gestures | defaults everywhere | see [Errors](#errors-and-what-they-do) |
| `~/.butai/themes/<name>.toml` | you | only the built-in palettes are selectable | warns, falls back to `blueprint-dark` |
| `~/.butai/butai.sock` | the daemon, on bind | a client starts a daemon, which creates it | stale file is removed and rebound |
| `~/.butai/butai.lock` | the daemon, at startup | created | it is a lock, not content |
| `~/.butai/logs/daemon.log.<date>` | the daemon | created | append-only text |
| `~/.butai/session.json` | the daemon, on every workspace open/close | nothing is restored; you start empty | warns in the log, ignored, next write replaces it |
| `~/.butai/panes/<key>/{agent,proc}-<i>.bin` | the daemon | panes come back blank but present | that pane comes back blank |
| `~/.butai/scratch/<key>/NNNNNN-name.ext` | the daemon, on paste-image | created on first paste | n/a |
| `~/.butai/ssh-%C` | ssh, as a ControlMaster socket | each ssh channel opens its own connection | n/a |
| `<project>/.butai.toml` | **you only** — butai never writes it | the workspace opens with just a shell | warns in the daemon log, treated as empty |
| `~/.ssh/config` | you — butai only reads it | the machines picker has no rows | unparseable lines are skipped |

`~/.ssh/config` is read for its `Host` aliases only, and `Include` lines are
followed (including `Include conf.d/*`) so an alias kept in a fragment still
reaches the picker. Nothing is written back to it.

Two more paths sit outside `~/.butai`:

- `$XDG_RUNTIME_DIR/butai-forwards/<target>-<pid>.sock` (falling back to the
  system temp directory) — one `ssh -L` forward per connected machine, removed
  when the client drops it.
- `$XDG_RUNTIME_DIR/butai-standalone-<pid>/butai.sock` — the private socket
  `butai standalone` runs on. It keeps no session store at all.

`~/.butai/config.toml` is written atomically: the new document goes to
`config.toml.tmp` beside it and is renamed over the original, so a crash
mid-save cannot truncate your config.

### The daemon's log

`~/.butai/logs/daemon.log` is rotated daily by the appender, so what you
actually find are dated files. The level filter comes from `RUST_LOG` and
defaults to `info`. This is where the config warnings the daemon produces
surface — a bad `.butai.toml`, a process that would not spawn, an agent pattern
that is not a valid regex. **None of those reach the API**, so no client can
show them to you. The one exception is a reload you asked for: `:reload-config`
sends its parse warnings back to the client that ran it.

### The restore state

`session.json` records which project directories were open, in what order, and
enough about each to bring the work back: every process (including the shell
each workspace opens with, and anything started by hand), every agent with the
conversation id it was holding, and which pane had the stage. The bulk output
lives beside it under `panes/`, one directory per workspace named
`<slug>-<hash>` — the directory's own basename, plus a hash of the full path so
that six different `.../src` directories cannot replay each other's output.

`butai kill-server` keeps both. `butai kill-server --clear` (or
`:kill-server clear`) removes both, because either half left behind is worse
than neither.

## One file, two readers

| Table | Read by | Used for |
|---|---|---|
| `[general] prefix`, `default_agent`, `remote_auto_attach`, `option_as_alt` | client | how the workbench answers |
| `[general] default_shell`, `exit_when_empty`, `scrollback`, `restore_bytes` | daemon | what a pane is made of |
| `[api]` | daemon (parsed only — see below) | |
| `[[agents]]` | daemon | what an agent pane launches |
| `[keys]` | client | the prefix table |
| `[theme]` | client | the palette |
| `[ui]` | client | rail geometry |
| `[[remote]]` | client | which daemons join this tab bar |
| `.butai.toml` (whole file) | daemon | what a workspace brings up |

The split is not tidiness. A palette is not something a daemon can have an
opinion about — two people attached to one daemon can want different ones — and
a scrollback budget is not something a client can enforce. It also means a
mistake in one half cannot break the other: a `[[agents]]` block missing its
`name` fails the *daemon's* parse and leaves your theme, keys and rails
untouched.

## Precedence

Nothing merges across sources. Each setting has exactly one place it is read
from, and where two mechanisms could name the same thing, this is which wins.

| Setting | Order, first match wins |
|---|---|
| Daemon socket | `--socket PATH` → `$BUTAI_SOCKET` → `~/.butai/butai.sock` |
| Theme directory | `$BUTAI_THEME_DIR` → `~/.butai/themes` |
| Session store | `$BUTAI_SESSION_FILE` → `~/.butai/session.json` |
| `panes/` and `scratch/` | the session store's directory → `~/.butai` |
| Workspace scope for the CLI | `--ws NAME` → `$BUTAI_WORKSPACE` (every pane carries it) |
| Shell for a pane | `[general] default_shell` → `$SHELL` → `/bin/sh` |
| Workspace name | `butai new -s NAME` / `butai ws create --name` / `POST /v1/workspaces` `name` → the directory's basename |
| Agent list | `[[agents]]` if the array exists at all → otherwise the five built-ins |
| Agent argv on a restore | `resume_args` when there is a conversation to reopen → otherwise `args` |
| A pane's palette | `[theme]` role keys → the named theme's `[colors]` → its `extends` chain → `blueprint-dark` |
| A key after the prefix | `[keys]` → the shipped prefix table |
| Rail geometry | `[ui]` (clamped) → the built-in defaults |
| A pane's environment | `[[agents]] env` → butai's own `BUTAI_*` → the daemon's inherited environment |
| What a workspace opens with | `session.json` on a restore → otherwise `.butai.toml` |

Four of those deserve spelling out.

**`[[agents]]` replaces, it does not extend.** The built-in launchers are
filled in only when the parsed config has *no* agents at all. One `[[agents]]`
block in your file means one agent in the picker.

**A restore replaces the workspace file.** The saved process list already
contains what `.butai.toml` asked for (they were spawned from it and then
recorded), plus anything you started by hand, minus anything you closed.
Replaying the file on top would duplicate the first group and lose the other
two — so on a restore the file is not consulted for processes or autostart
agents at all.

**`[[agents]] env` is applied last.** butai sets `BUTAI_SOCKET`, `BUTAI_PANE`
and `BUTAI_WORKSPACE` before your `env` table, deliberately, so a launcher can
override any of them.

**A name is not deduplicated the same way everywhere.** Opening a *directory* —
bare `butai` in it, or the folder picker — reuses the workspace already on that
path and otherwise takes the basename, appending `-2`, `-3` … until the name is
free. A name you *typed* is not adjusted: `butai new -s api` when `api` is
already open is refused with `workspace "api" already exists`, because silently
opening `api-2` is not what was asked for.

There is no per-project override of anything in `config.toml`. Rail geometry in
particular is global: `alt-l` resizes every workspace at once and saves to
`[ui]`. An old project file with a `[ui]` table still parses; nothing reads it.

## `~/.butai/config.toml`

```toml
# every table and every key is optional
[general]
[api]
[[agents]]
[keys]
[theme]
[ui]
[[remote]]
```

### `[general]`

Both halves of butai declare a `[general]`, with different keys in each. They
are listed here in the order the source declares them.

| Key | Type | Default | Read by | What it changes |
|---|---|---|---|---|
| `prefix` | string | `"C-b"` | client | The key that opens a prefix binding. Pressing it twice sends one literal through to the pane. Spelling is the key mini-language: `C-`, `M-`, `S-` prefixes over a character or a named key. |
| `default_agent` | string | unset | client | The agent `a` and `[+ agent]` spawn with no picker in between. Unset asks every time. Stored as a *name*, so it survives reordering `[[agents]]`. |
| `remote_auto_attach` | bool | `true` | client | Whether a `butai` run over ssh inside a pane may pull its machine into this tab bar on its own. Off means machines join only through the machines button or a `[[remote]]` block. |
| `option_as_alt` | bool | `true` on macOS, `false` elsewhere | client | Read macOS's Option-composed characters back as the Alt layer, so Option-o *is* `alt-o`. Only characters the workbench binds are mapped; the cost is that those characters cannot be typed into a pane. |
| `default_shell` | string | unset | daemon | The shell a terminal pane runs, and the interpreter `[[processes]]` commands go through. Unset falls back to `$SHELL`, then `/bin/sh`. |
| `exit_when_empty` | bool | `true` | daemon | Whether the daemon exits once the last workspace closes (having had at least one). `false` keeps it resident. |
| `scrollback` | integer | `5000` | daemon | Scrollback lines kept per terminal pane, in the daemon's VT emulator. Read when a pane is spawned. |
| `restore_bytes` | integer | `262144` (256 KiB) | daemon | Bytes of raw PTY output kept per pane for restart restore. `0` disables restore **and stops the capture entirely**. Counted in bytes because that is what bounds the cost: a redraw-heavy TUI spends far more per line than log text. |

`option_as_alt = false` is the setting to reach for if you write Danish or
Norwegian; use the prefix layer instead, or set your terminal to send a real
Alt, which is better than either. [keys.md](keys.md#on-a-mac) has the
terminal-by-terminal list.

### `[api]`

| Key | Type | Default | Read by | What it changes |
|---|---|---|---|---|
| `websocket_port` | integer | `0` | — | **Parsed and unimplemented.** The struct field exists and nothing in the daemon reads it; there is no listener to enable. Remote access is SSH, and the REST API and the framed protocol share the Unix socket. |

### `[[agents]]`

One block per agent type. `name` and `command` are **required** — a block
missing either fails the whole file's parse on the daemon side (see
[Errors](#errors-and-what-they-do)).

| Key | Type | Default | What it changes |
|---|---|---|---|
| `name` | string | *required* | What the picker, the rail and `default_agent` call this agent. |
| `command` | string | *required* | The executable. Resolved against `PATH` first, then `~/.local/bin`, `~/.bun/bin`, `~/bin` and the newest nvm `bin` — so an npm-installed CLI is found even though the daemon's `PATH` is not a login shell's. The USAGE page decides *installed* by this same resolution, so what it reports and what a pane can launch cannot disagree. |
| `args` | list of strings | `[]` | Argv for a fresh launch. |
| `resume_args` | list of strings | `[]` | Argv used *instead of* `args` when restart restore reopens this agent's conversation. Empty means "no resume support": the pane comes back repainted, on a fresh conversation. |
| `env` | table of strings | `{}` | Extra environment for the child, applied after butai's own variables. |
| `waiting_pattern` | string (regex) | unset | Replaces the built-in "blocked on you" markers for this agent. Matched case-insensitively against the footer band, one line at a time. |
| `busy_pattern` | string (regex) | unset | Replaces the built-in "still working" markers for this agent. |

**`{session_id}`** anywhere in `args` or `resume_args` is substituted with the
conversation id butai mints for that pane. Writing the placeholder *is* the
declaration that this launcher lets butai name its conversations — there is no
separate flag. It matters with more than one agent open: `claude --continue`
and `gemini --resume latest` both mean "the most recent conversation *in this
directory*", so two agents in one workspace would reopen the same transcript
and interleave into it. Note that the id is *set* with one flag and *reopened*
with another, because both CLIs refuse to re-declare an id that already exists.

Both patterns *replace* the built-in tables rather than adding to them, which is
the only shape that can take back a false positive as well as add a missing
match. Anchor `busy_pattern` to something a status line offers (`esc to
interrupt`) rather than a bare verb — the footer band scrolls prose too, and a
match on "interrupt" alone pins the pane to busy for as long as that sentence
is on screen, which means no spinner ever stops and no finished notification
ever fires.

**The built-ins**, used verbatim when the file declares no `[[agents]]` at all:

| `name` / `command` | `args` | `resume_args` |
|---|---|---|
| `claude` | `--dangerously-skip-permissions --session-id {session_id}` | `--dangerously-skip-permissions --resume {session_id}` |
| `codex` | `--dangerously-bypass-approvals-and-sandbox` | — |
| `gemini` | `--yolo --session-id {session_id}` | `--yolo --resume {session_id}` |
| `aider` | `--yes-always` | — |
| `agy` | `--dangerously-skip-permissions` | — |

Each launches with its CLI's auto-approve flag, because agents run unattended in
rail panes. The empty `resume_args` are deliberate, not gaps: `codex` and `agy`
assign their own conversation ids and have no way to be told one at launch, and
`aider`'s history is per directory, so there is nothing per-pane to name. A
wrong flag here makes the CLI exit on launch, so fill them in yourself only
after checking against the CLI you actually run.

### `[keys]`

A table of key string → command string. **It overrides the prefix table** —
what you press *after* the prefix. The Alt layer is fixed and not configurable;
`prefix` is what you change if Alt does not reach you.

```toml
[keys]
o = "space files"
F5 = "process build cargo build"
"C-y" = "monitor gpu"
```

Key names: optional `C-`, `M-`, `S-` prefixes, then a single character or one of
`enter`/`return`, `esc`/`escape`, `space`, `tab`, `backtab`,
`backspace`/`bspace`, `up`, `down`, `left`, `right`, `home`, `end`,
`pageup`/`pgup`, `pagedown`/`pgdn`, `delete`/`del`, `insert`, `f1`…`f12`. Named
keys are matched case-insensitively, so `F5` and `f5` are the same key. Shift is
dropped from character keys, so `X` and `S-x` are one binding rather than two —
which is what makes `X = "workspace close"` in the shipped table reachable at
all, since a terminal reports a capital as the character *plus* Shift.

**A modifier here is still a key pressed after the prefix.** `"M-y" = "monitor
gpu"` binds `{prefix} alt-y`, not `alt-y` on its own: this table is the prefix
table and nothing else reads it. That is the one thing to know before writing a
line here, because the spelling looks like the Alt layer and is not.

The command language is the same one the `:` prompt and the palette speak;
[keys.md](keys.md#changing-them) lists the vocabulary. The SETTINGS page reports
how many keys are bound and how many of them came from your config — which is
the question you have when a key does something you did not expect.

### `[theme]`

| Key | Type | Default | What it changes |
|---|---|---|---|
| `name` | string | `"blueprint-dark"` | A built-in palette, or `<name>.toml` in the themes directory. Built-ins are checked first, so a file named after one never shadows it. |
| `syntax_theme` | string | `"base16-ocean.dark"` | **Accepted and ignored.** It named a syntect theme back when the daemon ran files through syntect for its own editor pane. Kept so an existing config neither breaks nor has the key mistaken for a role override. |
| *any other key* | string | — | A role override applied over the selected palette. |

Values are `#rrggbb`, `ansi:0`–`ansi:255`, or `default` (the terminal's own
foreground/background). The pre-role names `border` and `border_focused` still
work and mean `rule` and `rule_focus`.

The eight built-ins, the full role list, and how to write a theme file that
`extends` another are in [theming.md](theming.md). `BUTAI_THEME_DIR` moves the
search directory, which is mostly useful for trying a theme out without touching
your config directory.

### `[ui]`

Chrome geometry, in terminal cells and rows. Global: this is one workbench's
layout, not one workspace's.

| Key | Type | Default | Range | What it changes |
|---|---|---|---|---|
| `left_rail` | integer | `28` | clamped to 12–60 | Width of the AGENTS / PROCESSES / SYSTEM rail. |
| `right_rail` | integer | `38` | clamped to 12–60 | Width of the CHANGES rail. |
| `procs_height` | integer | unset = automatic | floored at 3 | Rows given to PROCESSES; AGENTS takes what is left after this and the gauges. Automatic is two fifths of the list area. |
| `system_height` | integer | unset = automatic | capped at 19 | Rows for the SYSTEM gauges. Automatic is a separator plus whatever the machine's gauges need, and 0 in zen mode or below 12 rows of rail. |
| `net` | `"all"`, `"auto"`, or a list | `"all"` | — | Which interfaces get a NET gauge. |
| `disks` | `"all"`, `"auto"`, or a list | `"all"` | — | Which mounts get a DSK gauge. |
| `links` | bool | `true` | — | Whether a URL on screen is marked up as an OSC 8 hyperlink for the terminal butai is drawn on, so its pointer can follow one. Off leaves the text alone; the `f` picker works either way, because it never leaves this client. |

Widths **clamp rather than fall back**, so `left_rail = 900` gives you a
wide-but-usable rail instead of silently ignoring what you asked for. Heights
stay optional because *unset* is a real state — "size yourself to the terminal"
— and not the same as any particular number. At draw time both are fitted again
so no section is squeezed below three rows, and on a terminal too narrow to
leave 20 columns for the stage both rails collapse to nothing rather than
crushing it.

`links` is on because the sequence is one a terminal that does not implement it
discards — which is every terminal we could find, and what tmux before 3.4 does
as well, so the failure mode is a link that is merely not clickable. Turn it off
for the terminal that turns out to *print* it. It is a toggle on the SETTINGS
page's WORKBENCH group, beside the rail sizes.

`alt-l` (LAYOUT mode) and the WORKBENCH group on the SETTINGS page write the four
geometry keys; `0` on a height row clears it back to automatic, which removes the
key from the file rather than writing a number.

#### `net`

```toml
[ui]
net = "all"                      # every real link, capped at three (the default)
# net = "auto"                   # one: the default route, else the busiest
# net = ["enp1s0", "vpn-tunnel"] # exactly these, in this order
```

The daemon publishes **every** interface it can see and says what each one is;
this key is the client's side of that. `all` and `auto` skip loopback, bridges
and veths, because their bytes are counted again on whatever they egress from —
on a box where the agents talk to a local daemon, `lo` alone would dwarf the real
link. The cap exists for docker hosts, where three real links can hide behind
twenty veths and eight bridges.

`all` also leaves out a link that has carried nothing for the whole history
window — a tunnel that has been silent for two and a half minutes is three rows
saying nothing. The default route is always drawn, busy or not, since it is the
way out either way. Judging it over the window rather than the last sample is
what stops a row blinking out between packets.

A **list is honoured literally**: in the order given, uncapped, and without the
filter, because naming `docker0` is a decision rather than a mistake to correct.
A name that matches nothing is skipped, and `net = []` draws no NET gauge at all
— which is not the same as leaving the key out. A word that is neither `all` nor
`auto` is a config error rather than a silent fallback, since it is a typo.

Each gauge is three rows: a head, then a trace for each direction. See
[SYSTEM](workbench.md#system).

#### `disks`

```toml
[ui]
disks = "all"                  # every real disk, capped at three (the default)
# disks = "auto"               # one: the filesystem holding /
# disks = ["/", "/media/fast"] # exactly these, in this order
```

The same bargain `net` states, and deliberately the same shape: the daemon
publishes every mount and says what each one *is*, and this key is the client's
side of it. `all` and `auto` keep only the real disks — a tmpfs is RAM the `RAM`
gauge already counts, a container layer or an installed snap is a read-only image
that is 100% full by construction, and a network mount's capacity is a fact about
a machine that has a rail of its own. The cap exists for docker hosts, where the
mount table is dozens of image layers deep.

`all` takes them **largest first**, which is also the order it cuts from: fullest
first would spend the cap on squashfs before naming a real disk. `auto` is the
filesystem holding `/`, falling back to the largest real disk — which is what a
container gets, where `/` is an overlay.

A **list is honoured literally**: in the order given, uncapped, and without the
filter, so `disks = ["/dev/shm"]` draws the tmpfs. Match is on the mount point as
the daemon spells it. A mount that matches nothing is skipped, and `disks = []`
draws no DSK gauge at all — worth reaching for if you would rather spend those
rows on agents. A word that is neither `all` nor `auto` is a config error rather
than a silent fallback.

Each gauge is one row: a disk is a level with no history, so there is no trace to
draw. See [DSK](workbench.md#dsk).

### `[update]`

Whether butai looks for a newer release of itself, which one you already turned
down, and whether the daemon may be told to update itself.

The one table both halves read — a key at a time. `check` and
`declined_version` are the **client**'s; `allow_remote` is the **daemon**'s.
Neither struct declares the other's keys and serde ignores what it does not
know, so they share the table without either seeing the other's part.

```toml
[update]
check = true                 # default
declined_version = "1.1.0"   # written for you; see below
allow_remote = false         # default
```

| key | type | default | read by | effect |
|---|---|---|---|---|
| `check` | bool | `true` | client | Ask GitHub for the latest release at start, and every six hours after. This is the only outbound request a butai client makes; everything else in it talks to a Unix socket. |
| `declined_version` | string | unset | client | A release you answered **no** to. Written by the prompt, not by you. |
| `allow_remote` | bool | `false` | daemon | Let a client attached to this daemon make it update *itself* — `POST /v1/update`, and `butai update --daemon` on top of it. |

**`allow_remote` is off by default, and the default is the interesting half.**
The socket's only access control is the `0700` on its directory, and over an
`ssh -L` forward or `butai proxy` the far end is whoever holds the ssh session.
"Can reach the daemon" is a much weaker claim than "may replace the program this
machine runs"; turning this on is the machine's owner saying those are the same
set here. An unconfigured daemon answers `400` and names the key.

It is not a promise that the update is quiet. When it fires, clients are
detached and every workspace is killed and restored — exactly what `kill-server`
already does, and the same snapshot comes back. It is also not a second opinion
about *whether* to update: the daemon does its own check, and answers "already
on the latest" when there is nothing to do. See
[cli.md](cli.md#--daemon-updating-a-butai-you-are-not-on).

**The check knows which build it is.** A release publishes seven tarballs, one
per target, and the artifact this machine wants is decided at *compile* time:
`crates/butai-update/build.rs` bakes the target triple in, so a musl build asks
for the musl tarball because it is the musl build. That is stricter than
`scripts/install.sh`, which has to read `uname` and `ldd --version` because it
runs before any butai exists. If a release publishes nothing for this triple —
a build from source on a platform the matrix does not cover — you are told, and
nothing is downloaded.

**What the prompt's two answers mean.** `yes` downloads the tarball, checks it
against the release's `SHA256SUMS`, stops the daemon, replaces the binary and
restarts into it. Stopping the daemon is not destructive: it snapshots the open
workspaces and every pane's output first, exactly as `butai kill-server` does,
and the new build restores them. `no` writes `declined_version` and that release
stops asking — the *next* one still asks once of its own. `esc` answers nothing,
so you are asked again next launch.

`:update` ignores `declined_version` and asks now, which is the way back to a
prompt you dismissed or turned down. So does `butai update` — see
[cli.md](cli.md#butai-update).

**Nothing is offered that cannot be carried out.** Before the question is asked,
butai checks that the directory holding the binary is writable and that the
binary is not a `cargo` build in a `target/` directory. An update offer you
cannot accept is worse than no offer, and finding out after the download and
the daemon stop is the worst moment of all.

`BUTAI_NO_UPDATE_CHECK` switches the whole thing off from the environment, for a
butai installed by a package manager whose updates arrive some other way.

### `[[remote]]`

Other daemons whose workspaces join this tab bar. Connected at start, so they
are there without a gesture every morning.

| Key | Type | Default | What it changes |
|---|---|---|---|
| `name` | string | the destination, or the socket's file name | The badge this machine's tabs carry. |
| `host` | string | unset | An ssh destination: an alias from `~/.ssh/config`, or `user@host`. |
| `ssh_args` | list of strings | `[]` | Extra ssh flags, placed before the destination — `["-p", "2222"]`, `["-J", "bastion"]`. |
| `socket` | string | unset | A socket already reachable from here (your own `ssh -N -L` forward), used instead of dialling ssh ourselves. |
| `socket_path` | string | unset | Where the *far* daemon listens. Normally left unset so the far `butai` resolves its own default and finds the daemon already running there, rather than starting a second one on a path nothing else uses. |

`host` and `socket` are the two ways in and should be treated as mutually
exclusive — they are read by different code paths, and a block setting both is
used by each in turn.

They also differ in *when*: a `socket` block is an endpoint, connected before the
first frame, because a socket in the config is already reachable. A `host` block
is dialled on its own task after the first frame is up — an ssh connection is
seconds of DNS, TCP and key exchange, and one sleeping machine must not mean a
client that shows nothing for twenty seconds. With no `socket_path`, dialling
asks the far machine where its daemon listens (`butai ls` to make one exist, then
`butai --json whoami`) and forwards that path.

A block with neither `host` nor `socket` is skipped by both paths and does
nothing.

### Keys that are no longer read

An old config keeps loading — unknown keys are ignored, never rejected — but
these have no effect any more:

| Key | Status |
|---|---|
| `[layouts]`, `[general] default_layout` | Removed with layout presets. The workbench has fixed rails and no free panes, so a preset had nothing to describe. |
| `[general] remain_on_exit` | Removed; unread for as long as the presets were. |
| `[ui] all_agents` | Removed with the ALL AGENTS panel; the fleet lives on the BOOTH space now. |
| `[theme] syntax_theme` | Accepted and ignored (above). |
| `[api] websocket_port` | Parsed, unimplemented (above). |

## `.butai.toml`

One file in a project root, entirely the daemon's, and **never written by
butai**. Nothing else is put into a project directory either.

```toml
name = "api"                 # parsed; not applied today — see below

[[processes]]
name = "dev"
cmd = "npm run dev"
ready = "Local:"             # substring that flips the row to ok

[agents]
autostart = ["claude"]
```

| Key | Type | Default | What it changes |
|---|---|---|---|
| `name` | string | unset | **Parsed and currently ignored.** The field is deserialized and never read: a workspace is named by `butai new -s`, by the API's `name`, or by the directory's basename. |
| `[[processes]] name` | string | *required* | The row's label in the PROCESSES rail. |
| `[[processes]] cmd` | string | *required* | Run through the workspace shell's `-c`, in the workspace directory. That is `[general] default_shell`, then `$SHELL`, then `/bin/sh` — the same resolution a shell pane uses. |
| `[[processes]] ready` | string | unset | A **case-sensitive substring** of the process's output that flips the row's status to `ok`. Matched against the raw output stream, across burst boundaries. |
| `[agents] autostart` | list of strings | `[]` | `[[agents]]` names spawned into the AGENTS rail when the workspace opens, in order. |

`name` and `cmd` are required in the same sense `[[agents]]`' are: a block
missing either fails the *whole file's* parse, so one typo costs you every
process in it, not one row. The workspace still opens with its shell.

Without a `ready`, a row reads `run` (or `...` while output is arriving) for as
long as the command lives, then `done` or `FAIL(n)` — it never reaches `ok`.
That is the right shape for a server; give a `ready` to anything whose startup
you want to be able to see finish.

`$SHELL -c` is a *non-interactive* shell, which sources none of the files that
put `~/.local/bin` or nvm on `PATH`. The daemon puts back the directories a
login shell would have added — in front, and only the ones that exist and are
not already there — so `npm run dev` in a workspace file finds `npm` even though
the daemon was started from a session manager.

### Lifecycle

- **Read once**, when the workspace is created: opening a project directory that
  is not already open, whether from `butai` in that directory, `alt-n`,
  `POST /v1/workspaces`, or `butai ws create --cwd`.
- **Not watched and not re-read.** Editing it changes nothing about a running
  workspace, and `:reload-config` does not touch it. Close the workspace and
  open it again.
- **Not consulted on a restore.** A daemon coming back rebuilds from
  `session.json` instead, so a process you removed from the file does not come
  back and one you started by hand is not lost.
- **Never rewritten.** Adding a process from the UI (`t`, `:process`) starts one
  in that workspace and records it in the daemon's session state; it does not
  edit your project's file.
- Its warnings go to the daemon log only. A malformed file yields zero
  configured processes, and the workspace still opens with its shell.

## What the workbench writes back

Every one of these rewrites a single key or block and leaves the rest of the
file — comments, ordering, the daemon's keys the client cannot even see —
exactly as it was.

| Gesture | Key written |
|---|---|
| SETTINGS → APPEARANCE → theme, Enter | `[theme] name` |
| SETTINGS → AGENTS → default agent, or `d` in the agent picker, or `:agent-default NAME` | `[general] default_agent` (removed outright when you unpin, so the file looks like one that never set it) |
| SETTINGS → MACHINES → auto-attach, `space` | `[general] remote_auto_attach` |
| SETTINGS → ABOUT → check for updates, `space` | `[update] check` |
| Answering **no** to the update prompt | `[update] declined_version` (that release only; `esc` writes nothing and asks again next launch) |
| SETTINGS → WORKBENCH size rows (`-`/`+`/`0`), or leaving `alt-l` LAYOUT mode | all four `[ui]` keys; a height cleared to automatic is removed rather than written |
| the machines button (`alt-h`), once the machine answers | a new `[[remote]]` block with `host`, plus `name`/`ssh_args` when they differ from the destination |
| Disconnecting a machine (`alt-h`, or the tab's row menu) | removes that `[[remote]]` block |

Four properties of those writes are load-bearing:

- **There is no Save button.** A change applies and is written when you make it,
  which is what the client already does everywhere else.
- **A pin is validated first.** `default_agent` is checked against
  `GET /v1/agents` before it is saved — a pin left behind by a renamed agent
  should cost a keystroke, not fail every spawn.
- **Only a deliberate connection is remembered.** A machine that announced
  itself from inside a pane is adopted for the session and left out of the file;
  otherwise a week of `ssh`-ing around turns every morning into a start that
  dials nine machines and waits on the seven that are asleep. The machines button is the
  act that says "this one is mine". Remembering the same host twice writes one
  block.
- **A `[[remote]] socket` block is never forgotten.** It is somebody else's
  forward — the client has no ssh under it to kill — so a disconnect leaves it
  alone however its badge reads. Forgetting a machine that was never remembered
  is a silent no-op that does not even create a config file.

The one thing the SETTINGS page does *not* write is the role overrides sitting
beside `name` in `[theme]`: a page that also rewrote `accent = "#ff8800"` would
be silently discarding something the file's owner typed on purpose.

## Reloading

| What | When it takes effect |
|---|---|
| `[general] default_shell`, `exit_when_empty`, `scrollback`, `restore_bytes`, `[[agents]]` | `:reload-config` (or `butai` restart). `scrollback` and `restore_bytes` are read when a pane spawns, so existing panes keep the budget they were born with. |
| `[general] prefix`, `[keys]`, `[theme]`, `[ui]`, `[[remote]]` | the next client start — with the exceptions below |
| `[theme] name` | live from the SETTINGS page, which applies each palette as the cursor passes it and puts the old one back if you leave without choosing |
| `[general] default_agent`, `remote_auto_attach`, `[ui]`, `[update] check` | live when *you* change them in the client; a hand edit needs a restart |

`:reload-config` is a command to the daemon and reloads the daemon's half only —
re-reading the file and replacing the config the daemon holds, with any parse
warning reported back to the client that asked rather than buried in the log.
There is no client-side reload: a palette, a keymap and a rail width belong to
whatever is drawing, and the client holds them from startup.

It changes nothing that is already running. Panes keep the shell, scrollback and
restore budget they were born with, and an `[[agents]]` block you edited applies
to the *next* agent you spawn.

`:theme NAME` no longer switches at runtime. The command remains in the
vocabulary and answers with where to set it instead, so an old binding gets a
sentence rather than silence.

## Errors and what they do

Nothing in butai's configuration is fatal. The worst case is "this half of the
file fell back to defaults, and something said so".

| Situation | What happens | Where it surfaces |
|---|---|---|
| `config.toml` absent | defaults everywhere, silently | — |
| `config.toml` unparseable | **that reader** falls back to *all* defaults; the other reader is unaffected if its own tables are intact | client: a flash on the first frame. Daemon: `config: …` in the log at startup, and back to the client that asked when the reload was `:reload-config`. `butai standalone` prints it to stderr |
| A required `[[agents]]` field missing (`name`, `command`) | the daemon's whole parse fails → built-in agents, default shell, default scrollback. Client tables still load normally | daemon log |
| A required `[[processes]]` field missing in `.butai.toml` (`name`, `cmd`) | that whole file fails to parse → no configured processes and no autostarts | daemon log |
| An unknown key or table | ignored | nowhere — this is what keeps old and new configs loading |
| `prefix` unparseable | falls back to `C-b` | flash: `bad prefix (…); falling back to C-b` |
| A `[keys]` entry whose key or command does not parse | that entry is dropped; every other binding still loads | flash on start, and the SETTINGS bindings count |
| A key pressed after the prefix that nothing binds | nothing happens, and it says so | flash: `<key> is not bound` |
| `[theme] name` names nothing | falls back to `blueprint-dark` | flash, listing the built-ins |
| A malformed color, anywhere | that role is skipped, the rest of the theme loads | flash: `invalid color "…"; expected #rrggbb, ansi:N, or default` |
| An unknown role in a theme file | skipped, the rest loads | flash naming the file and the role |
| A theme file's `extends` naming nothing, or a cycle, or a chain over 16 deep | the base becomes `blueprint-dark`; the file's own colors still apply | flash |
| A `[ui]` value out of range | clamped, never ignored | nowhere |
| `waiting_pattern` / `busy_pattern` not a valid regex | dropped, that agent falls back to the built-in markers — refusing to start would cost you the agent | daemon log |
| `.butai.toml` unparseable | zero configured processes, zero autostarts; the workspace still opens with its shell | daemon log **only** — nothing surfaces over the API |
| A `[[processes]]` command that will not spawn | that row is skipped; the others still come up | daemon log |
| `agents.autostart` naming an agent that is not configured | skipped: `no agent named "x" in config` | daemon log |
| `session.json` unreadable | ignored; you start empty rather than half-restored | daemon log |
| A restored workspace whose directory is not readable right now | **kept, not deleted** — it is held aside and written back out, so an external disk that is not mounted yet restores next time | daemon log |

## Environment

| Variable | Read by | Effect |
|---|---|---|
| `BUTAI_SOCKET` | everything | The daemon socket. `--socket` beats it; it is exported into every pane, and passed to a daemon the client auto-spawns. |
| `BUTAI_WORKSPACE` | the CLI | Default for `--ws`. Exported into every pane, so a command run inside butai acts on its own workspace. |
| `BUTAI_PANE` | the CLI | Which pane a command is running in. Its *absence* is the test for "not inside butai" — there is no separate marker variable. |
| `BUTAI` | the client | Set in every pane to the daemon's socket; the nesting guard compares it against the socket being attached to, so attaching a *different* daemon from inside a pane is still allowed. |
| `BUTAI_THEME_DIR` | client | Overrides `~/.butai/themes`. |
| `BUTAI_SESSION_FILE` | daemon | Overrides `~/.butai/session.json`, and takes `panes/` and `scratch/` with it. Deliberately **not** keyed off `BUTAI_SOCKET`: a second daemon on a custom socket shares the real session store unless you set this. |
| `BUTAI_NO_UPDATE_CHECK` | client | Non-empty and not `0` stops the update check entirely, whatever `[update] check` says. For a butai a package manager owns. |
| `BUTAI_NO_HANDOFF` | the CLI | Non-empty and not `0` stops bare `butai` over ssh from handing its machine to the workbench you are already looking at. |
| `SSH_CONNECTION` | the CLI | Its presence (plus a tty) is what makes that handoff probe run at all, so a local `butai` never pays for it. |
| `SHELL` | daemon | Fallback shell when `default_shell` is unset. |
| `RUST_LOG` | daemon | `tracing` filter for `~/.butai/logs/`; defaults to `info`. |
| `HOME` | both | Resolves `~/.butai`, and the login `bin` directories added to a pane's `PATH`. |
| `XDG_RUNTIME_DIR` | client, CLI | Where ssh forward sockets and the `butai standalone` socket directory go; falls back to the system temp directory. |
| `TERM`, `COLORTERM` | — | *Set* by the daemon for every pane's child: `xterm-256color` and `truecolor`. |
| `BUTAI_VERSION`, `BUTAI_INSTALL_DIR` | `scripts/install.sh` | Install a specific tag, or install somewhere other than `/usr/local/bin` → `~/.local/bin`. |

## Examples

### Minimal

Most people need one or two lines. This is a complete, valid config:

```toml
[general]
default_agent = "claude"

[theme]
name = "gruvbox-dark"
```

### Full

Every key below is one butai reads today.

```toml
# ~/.butai/config.toml

[general]
prefix = "C-a"                 # C-b by default
default_agent = "claude"       # `a` spawns it; `A` still opens the picker
default_shell = "/usr/bin/fish"
scrollback = 20000             # lines per terminal pane
restore_bytes = 524288         # 512 KiB of raw output per pane; 0 turns restore off
exit_when_empty = false        # keep the daemon resident after the last workspace
remote_auto_attach = true      # let `butai` over ssh pull its machine into the bar
option_as_alt = true           # macOS: read Option-composed characters as Alt

# ── agents ─────────────────────────────────────────────────────────────────
# Declaring any block replaces the five built-ins entirely.

[[agents]]
name = "claude"
command = "claude"
args = ["--dangerously-skip-permissions", "--session-id", "{session_id}"]
resume_args = ["--dangerously-skip-permissions", "--resume", "{session_id}"]

[[agents]]
name = "gemini"
command = "gemini"
args = ["--yolo", "--session-id", "{session_id}"]
resume_args = ["--yolo", "--resume", "{session_id}"]

[[agents]]
name = "aider"
command = "aider"
args = ["--yes-always", "--watch-files"]
# no resume_args: aider's history is per directory, so there is nothing
# per-pane to name. It comes back painted, on a fresh conversation.

[[agents]]
name = "review"                # the same CLI under a second name, with its
command = "claude"             # own environment
args = ["--dangerously-skip-permissions"]
env = { ANTHROPIC_MODEL = "claude-opus-5", CLAUDE_PROJECT_ROLE = "reviewer" }

[[agents]]
name = "mycli"                 # status detection is generic and can misread a
command = "mycli"              # CLI whose footer is worded unusually. These
waiting_pattern = "shall i|proceed\\?"   # *replace* the built-in markers for
busy_pattern = "esc to halt"             # this agent, so they can also take
                                         # back a false positive.

# ── keys ───────────────────────────────────────────────────────────────────
# The prefix table: what `C-a <key>` does. The Alt layer is fixed.

[keys]
o = "space files"
r = "space git"
F5 = "process build cargo build --workspace"
"C-y" = "monitor gpu"       # {prefix} then C-y — not a second Alt layer

# ── appearance ─────────────────────────────────────────────────────────────

[theme]
name = "blueprint-dark"        # eight built in, or ~/.butai/themes/<name>.toml
accent = "#ff8800"             # override one role without writing a theme file
faint = "ansi:8"               # let the terminal decide this one
ground = "default"             # ...and this one

[ui]
left_rail = 30                 # cells, clamped to 12..60
right_rail = 44
procs_height = 12              # rows; omit and PROCESSES sizes itself
system_height = 10             # rows for the gauges; capped at 19
net = ["enp1s0", "vpn-tunnel"] # or "all" (the default) / "auto"
disks = ["/", "/media/fast"]   # or "all" (the default) / "auto"

# ── other machines ─────────────────────────────────────────────────────────

[[remote]]
host = "gpu-box"               # dialled after the first frame
name = "gpu"                   # the badge its tabs carry
ssh_args = ["-p", "2222", "-J", "bastion"]

[[remote]]
socket = "/tmp/fwd.sock"       # your own `ssh -N -L` forward: already reachable,
name = "prod"                  # so it is connected before the first frame
```

### A workspace file for a web project

```toml
# ~/Projects/shop/.butai.toml

[[processes]]
name = "web"
cmd = "npm run dev"
ready = "Local:"               # Vite prints this when the server is up

[[processes]]
name = "api"
cmd = "cargo watch -x 'run --bin api'"
ready = "listening on"

[[processes]]
name = "db"
cmd = "docker compose up postgres"
ready = "database system is ready to accept connections"

[[processes]]
name = "types"
cmd = "npx tsc --watch --noEmit"   # no ready marker: it never finishes starting

[agents]
autostart = ["claude"]
```

Opening the workspace brings those four up like a Procfile — each in its own
rail row, each in the project directory — and spawns the autostart agents into
the AGENTS rail. The shell keeps the stage regardless.

## Where this lives

| Section | Source |
|---|---|
| The files, `~/.butai`, socket, logs, session, panes | `crates/butai-protocol/src/paths.rs` |
| Directory permissions, lock file, log rotation, `RUST_LOG` | `crates/butai-server/src/daemon.rs` |
| Client half of `config.toml`: `[general]`, `[keys]`, `[theme]`, `[ui]`, `[[remote]]`, and every write-back | `crates/butai-client/src/config.rs` |
| Daemon half: `[general]`, `[api]`, `[[agents]]`, built-in launchers, `.butai.toml` | `crates/butai-server/src/config.rs` |
| `{session_id}` substitution, agent spawn, restore, `session.json`, `panes/`, `scratch/` | `crates/butai-server/src/core.rs` |
| `.butai.toml` read at workspace creation; `ready` matching across output bursts | `crates/butai-server/src/core.rs`, `crates/butai-server/src/workbench.rs` |
| Pane environment, `PATH` repair, `waiting_pattern` / `busy_pattern` compilation | `crates/butai-server/src/pane/terminal.rs` |
| Key strings, the command language, the shipped prefix table | `crates/butai-client/src/keymap.rs` |
| Palettes, theme files, `extends`, `BUTAI_THEME_DIR` | `crates/butai-client/src/theme.rs` |
| `[ui]` clamping and the automatic heights | `crates/butai-client/src/chrome/model.rs`, `crates/butai-client/src/chrome/mod.rs` |
| The SETTINGS page and what each row writes | `crates/butai-client/src/chrome/settings.rs` |
| Where config warnings are flashed, remotes dialled, settings edits applied | `crates/butai-client/src/workbench.rs` |
| `--socket`, `--ws`, `whoami`, daemon auto-spawn | `crates/butai/src/cli/mod.rs`, `crates/butai-client/src/conn.rs` |
| `[[remote]]` dialling, ssh forwards, `~/.ssh/config` | `crates/butai-client/src/ssh.rs`, `dial.rs`, `ssh_config.rs` |
| Install-time environment | `scripts/install.sh` |
