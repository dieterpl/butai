# Design notes — the agent workbench

> **What this is.** The design document that shaped the interface butai ships
> today: the reasoning behind the fixed chrome, the keybinding layers, the
> mouse and color policies, and the calls that were made along the way. It was
> written before implementation, so the closing sections
> ([mapping onto the codebase](#mapping-onto-the-v1-codebase),
> [open questions](#open-questions-current-calls-revisitable)) read as a plan
> rather than a description — that work has since landed. Treat this as
> *rationale*, not as an API reference. For what the interface actually does
> now, see the [README](../README.md); for the wire format, see
> [`protocol.md`](protocol.md).

butai began as a faithful tmux: generic panes, freeform splits, build your own
layout. It then took a position: **butai is a workbench for running coding
agents against workspaces**, and the UI is fixed chrome designed around
that loop — *dispatch agents → watch processes → review changes → commit*.
The sections below argue for that shape.

## The frame

Three level hierarchy, always the same, never rearranged:

1. **Workspace tabs** (top) — one tab = one project directory = one daemon
   session. You think in projects; tabs match that.
2. **Rails** (left + right) — fixed sections: **Agents**, **Processes**,
   and **System** (CPU/RAM/GPU) on the left, **Changes** on the right.
   Rails are *lists and gauges with status*, not terminals.
3. **Stage** (center) — the one big surface, and it is almost always one
   of two things: **the agent you're steering or the code you're
   reading/editing** (with process logs and diffs as the visiting third
   parties). Whatever you select from a rail takes the stage. The stage
   may be split; the rails never move.

### Mockup 1 — mission control (default state)

```
  1:webapp    [ 2:butai ]   3:infra !                                  [+ new]
┌─ AGENTS ───────────┐┌─ STAGE · src/core.rs ────────────┐┌─ CHANGES (6) ─────┐
│ > claude       [!] ││    1  //! ServerCore: the single ││ Unstaged            │
│   codex        [~] ││    2  //! owner actor for all..  ││  M core.rs    +42 -3│
│   gemini       [ ] ││    3                             ││  M render.rs  +11 -1│
│ a:new  x:kill      ││    4  use std::collections::Ha.. ││  ? notes.md         │
├─ PROCESSES ────────┤│    5  use std::path::PathBuf;    ││ Staged              │
│ > dev   ok  :5173  ││    6                             ││  A proto.rs   +180  │
│   test  FAIL (2)   ││    7  const FRAME_INTERVAL: Du.. ││                     │
│   build  ...       ││    8      Duration::from_milli.. ││ main ↑2 · clean CI  │
│ p:new  r:restart   ││    9                             ││                     │
├─ SYSTEM ───────────┤│   10  pub type ClientId = u64;   ││                     │
│ cpu ▂▄▆▂ 34%  61°C ││   11                             ││                     │
│ ram ▆▆▆▂▁ 19/32 GB ││   12  /// Everything that can    ││                     │
│ gpu ▁▁▂▁   7% 4/12 ││   13  /// happen to the daemon.  ││ d:diff  c:commit    │
└────────────────────┘└──────────────────────────────────┘└─────────────────────┘
  butai ~/Projects/butai (main)   ● claude is waiting for approval    [help] [:cmd]
```

Legend for status glyphs (single-cell ASCII on purpose — no emoji width
bugs in terminals): `[!]` needs you, `[~]` working, `[ ]` idle, `[x]`
exited (stays in the rail until dismissed with `x`),
`ok`/`FAIL (n)`/`...` for processes, `!` on a tab = something inside
needs attention.

### Mockup 2 — agent focused (Enter on an agent)

```
  1:webapp    [ 2:butai ]   3:infra                                    [+ new]
┌─ AGENTS ───────────┐┌─ STAGE · claude ─── (live) ──────┐┌─ CHANGES (8) ─────┐
│ > claude       [~] ││                                  ││ Unstaged            │
│   codex        [~] ││  ● Editing src/render.rs...      ││  M core.rs    +42 -3│
│   gemini       [ ] ││                                  ││  M render.rs  +26 -9│  ← grows live
│                    ││  ✓ Updated compose_frame() to    ││  ? notes.md         │
│ a:new  x:kill      ││    accept the Chrome struct      ││ Staged              │
├─ PROCESSES ────────┤│                                  ││  A proto.rs   +180  │
│   dev   ok  :5173  ││  ? May I run `cargo test`?       ││                     │
│   test  FAIL (2)   ││    ❯ 1. Yes  2. No               ││ main ↑2             │
│   build  ok        ││                                  ││                     │
│ p:new  r:restart   ││  > _                             ││ d:diff  c:commit    │
└────────────────────┘└──────────────────────────────────┘└─────────────────────┘
  butai ~/Projects/butai (main)   stage: claude (keys pass through)   M-esc: back
```

While an agent has the stage, **all keys pass through** to its PTY —
it's a full terminal (Claude Code's own TUI renders untouched). `Alt-Esc`
returns focus to the chrome. The Changes rail keeps updating live as the
agent edits, which is the whole point of keeping it on screen.

### Mockup 3 — review mode (`d` in Changes, or Alt-g)

```
  1:webapp    [ 2:butai ]   3:infra                                    [+ new]
┌─ AGENTS ───────────┐┌─ STAGE · diff: render.rs (2/6) ──┐┌─ CHANGES (6) ─────┐
│   claude       [~] ││ @@ -63,9 +63,14 @@               ││ Unstaged            │
│   codex        [ ] ││  fn draw_status_bar(             ││  M core.rs    +42 -3│
│   gemini       [ ] ││ -    prefix_armed: bool,         ││ >M render.rs  +26 -9│
│                    ││ +    chrome: &Chrome,            ││  ? notes.md         │
│ a:new  x:kill      ││  ) {                             ││ Staged              │
├─ PROCESSES ────────┤│ +    let left = match &chrome    ││  A proto.rs   +180  │
│   dev   ok  :5173  ││ +        .status_override {      ││                     │
│   test  ok         ││ +        Some(text) => ...       ││ j/k:file  s:stage   │
│   build  ok        ││  ...                             ││ u:unstage           │
│ p:new  r:restart   ││ n/p: next/prev hunk   s: stage   ││ c:commit            │
└────────────────────┘└──────────────────────────────────┘└─────────────────────┘
  butai ~/Projects/butai (main)    reviewing claude's changes        [help] [:cmd]
```

`j/k` walks files in the rail, the stage diff follows; `s` stages, `c`
commits — the agent-review loop without ever leaving the screen.

### Mockup 4 — collapsed rails ("zen", Alt-z)

```
  1:webapp    [ 2:butai ]   3:infra !                                  [+ new]
┌──┐┌─ STAGE · src/core.rs ──────────────────────────────────────────────┐┌──┐
│A!││    1  //! ServerCore: the single-owner actor for all mutable      ││C6│
│A~││    2  //! daemon state.                                           ││  │
│A ││    3                                                              ││+4│
│──││    4  use std::collections::HashMap;                              ││-1│
│P✓││    5  use std::path::PathBuf;                                     ││  │
│P✗││    6                                                              ││↑2│
│P✓││    7  const FRAME_INTERVAL: Duration = Duration::from_millis(16); ││  │
└──┘└───────────────────────────────────────────────────────────────────┘└──┘
  butai ~/Projects/butai (main)      cpu 34%  ram 19G  gpu 7%    [help] [:cmd]
```

Rails shrink to 2-column status strips and the system gauges condense
into the footer — the stage gets ~95% of the width, but a failing test,
a waiting agent, or a pegged CPU is still one glance away.

## Why this works (the reasoning)

**1. Tabs = workspaces matches how people actually context-switch.**
Nobody interleaves three projects pane-by-pane; you're *in* one project,
occasionally jumping to another. One tab = one directory = one daemon
session means switching is `Alt-1..9`, instant and total: agents,
processes, and git state all swap together. Attention bubbles *up* the
hierarchy — an agent needing approval in a background workspace becomes a
`!` on its tab, so backgrounded work can still interrupt you exactly once,
in the corner of your eye, instead of never or constantly.

**2. Fixed chrome beats freeform splits for a workbench.** tmux's split
tree is maximally flexible and therefore maximally inconsistent — every
session ends up a different shape and your eyes have to *search*. With
fixed regions, location = meaning: left edge is "things that run," right
edge is "what changed," center is "what I'm looking at." Muscle memory
gets stable targets (Alt-a = agents, Alt-p = processes, Alt-g = changes,
Alt-e = editor). Freeform splitting survives, but demoted: it exists only
*inside the stage*, where it can't break the frame.

**3. Agents are an inbox, not a wall of terminals.** The v1 model (agent =
just a pane) fails at 3+ agents: they're long-running, mostly autonomous,
and 95% of the time you don't need to see them — you need to know **who
needs me next**. So the rail is a status list with three honest states —
working `[~]`, needs-you `[!]`, idle `[ ]` — plus exited `[x]`: a finished
agent keeps its row (gray, title suffixed `[exited]`) so its final output
stays one Enter away, and `x` dismisses it for real. The terminal itself is
something you *visit* on the stage. This is the attention-queue model
(inbox), which scales linearly where tiled terminals scale by screen area.
Crucially, butai can actually detect these states better than any wrapper
script: the daemon already parses every output byte through its VT
emulator, so "bell rung" (Claude Code rings one when it needs input),
"output flowing," and "quiet at a prompt" are all observable centrally —
no per-agent integration needed, which keeps the "any CLI agent works"
promise intact.

**4. Processes are checklights, not logs.** Dev servers, watchers, and
builds are write-only noise until they break. Rendering them as
`name · status · port/exit` rows costs 3 lines instead of 3 panes, and
`FAIL (2)` in red is *more* visible than a log pane you've scrolled past.
Enter puts the full log on the stage when you do care; `r` restarts.
Defined declaratively in `.butai.toml` at the workspace root, so `butai .`
brings the whole project up like a Procfile. Docker containers whose
compose project lives in this workspace's directory tree appear as `d name`
rows in the same section — Enter tails their logs on the stage, `r`/`x`
restart/stop them; containers from other projects collapse into a dim
`d (N other)` row.

**4b. System gauges belong with the things that spend them.** Agents fork
compilers and test suites; dev servers leak memory; local models eat the
GPU. A SYSTEM block — `cpu` trace + % + temp, `ram` used/total, `gpu`
util + VRAM, `net` throughput each way — sits directly under PROCESSES
because it's the same question at a different altitude: *what is this
machine doing right now?* When a `[~]` agent and a pegged CPU line up,
you know it's working; when the CPU is pegged and nothing is `[~]`, you
know something's wrong. Sampled by the daemon (~2 s from `/proc/stat`,
`/proc/meminfo`, `/proc/net/dev`, hwmon; GPU via `nvidia-smi` or
`/sys/class/drm` for amdgpu, gracefully absent otherwise) — so it's also
on the JSON API, and a GUI client gets machine telemetry for free. In zen
mode it condenses into the footer as plain numbers.

**4c. A gauge has to be able to say "nothing".** The other gauges measure
a *level*: a CPU at rest is still at some percentage, so a trace that
floors at one dot is the honest picture of an idle machine and a blank
row would read as a dead feed. Throughput is not a level, it is a flow,
and it has a real zero — but an interface that is up is never numerically
at zero, because keepalives, mDNS and ARP keep a few hundred bytes a
second moving on it. Autoscaling those to fill the window drew an idle
link exactly like a saturated one. The fix is a floor in *bytes* rather
than in scale units: as a fraction of the peak it would move with the
traffic and silence would never reach it. This is the general shape of
the mistake — a scale with no absolute anchor turns noise into signal the
moment noise is all there is.

**4d. Resolution is a budget, and direction costs a row.** Mirroring both
directions around a midline fits the network gauge into the same two rows
as the others, but four dot rows split two ways is one bit per direction:
everything under a quarter of the shared scale collapses onto the
baseline. A 59 kB/s download running under a 572 kB/s upload sits at 10%
of that scale — on screen, and unreadable. Giving each direction its own
row buys four levels and, more importantly, a colour and an arrow of its
own, because one glyph row holding both directions can only ever be one
colour. That is what makes the gauges different heights, and why nothing
downstream may assume they are not: hit testing walks the gauge list
instead of dividing a row by a constant, and the renderer reports the
rows it used rather than letting a caller recompute them. Both of those
had already been wrong once.

**5. Changes lives on the right because review is now the job.** When
agents write most of the diff, your primary activity shifts from typing to
reviewing. The permanent rail gives you: a live tally of what agents are
doing to your tree (watch `+42` grow while claude works — an early-warning
system for runaway edits), one-key stage/commit, and a badge count that
functions as "uncommitted risk." The rail's top row is a `[ commit... ]`
target: click it (or press `c`) and it becomes an inline message input with
the staged summary underneath — Enter commits, Esc cancels, no overlay. Right edge specifically: reading flows
left→center (agent → its effects), and the diff stage sits adjacent to
the list that drives it.

**6. One stage forces one focus.** Only one thing is ever "the work" at a
given moment — editing a file, steering an agent, reading a diff. Making
that a single swap-in surface (like a browser tab body) means
六 panes compete) means the biggest region always shows the thing you
chose, and everything else degrades gracefully into status glyphs —
unlike tmux, where six equal panes compete for attention.

**7. It stays a terminal, and the API gets better.** All of this is still
server-side chrome rendered into cell grids — detach/reattach keeps
working, SSH keeps working. And because rails are structured state
(agent list, process list, change list), the public API grows matching
structured queries — which is exactly what a future Electron/Swift client
wants to render natively instead of screen-scraping cells.

### Two git surfaces, and why that is not a duplicate

The CHANGES rail and the GIT space both list the working tree's files and
both stage them with the same letters. That looks like the same thing
twice, and the GIT page originally carried a rule against it: nothing on
it staged anything, and its `working tree` row only sent you to the rail.

The rule bought one thing — no second implementation of staging — and
cost the case it was drawn for. Reviewing a change means reading a diff
and deciding, hunk by hunk, what goes in; being sent to a rail on another
page to act on what you just read is the loop broken in the middle. So
the page keeps the files and the rule is paid for a different way: the
rows *are* the rail's `ChangeRow`s, and the keys resolve to the same
`VerbId`s and the same `GitAction`s. There is one implementation of
"stage this file" and two places to reach it, which is not the same
shape as two implementations that agree today.

What separates them is what each is *for*. The rail sits beside the
agents doing the changing — it is the peripheral-vision copy, always
present whatever page you are on, and it owns the commit box and the
sync buttons. The space is where you go to read: the history, the refs,
and a diff with room to be a diff. Neither is a smaller version of the
other, and the split is the one PROCESSES and the Docker page already
make.

### A diff is a document, not coloured text

The patch git prints is a transport format that happens to be legible.
Drawn verbatim it spends four rows per file restating the path and two
blob hashes, and it hides the number every reader actually wants — which
line of the file am I looking at — inside a `@@` header they have to
count from. On a twenty-file working tree that is eighty rows of nothing
and a lot of counting.

So the widget parses first and draws a row model: a card per file, and
every line numbered on the side it exists in. The parse was already
there — partial staging is *made* of it, since taking half a hunk means
building a new valid patch — so this is the drawing catching up with the
model underneath it rather than new machinery. Folding follows from the
same place: once files are objects rather than runs of text, shutting one
is a filter over the rows.

## Keybinding philosophy

Two layers, no modes to remember:

- **`Alt` is chrome, always.** `Alt-1..9` tabs, `Alt-a/p/g/e` jump to
  section, `Alt-Enter` new agent, `Alt-z` zen, `Alt-Esc` focus out of the
  stage, `Alt-;` command palette. Alt-chords almost never collide with
  terminal apps, and they work while an agent owns the stage.
- **Everything else goes to the focused thing.** In rails: `j/k/Enter`
  plus the section verbs printed at the bottom of each rail (`s`tage,
  `r`estart, `c`ommit...). On the stage: raw passthrough to the PTY or
  editor. `C-b` prefix survives for tmux muscle memory but is no longer
  the primary interface.
- **The footer is a toolbar.** Right side holds clickable buttons —
  `[tree] [edit] [find] [commit] [help] [:cmd]` — for the mouse-first
  moments. `[help]` (or `?` outside the stage, or `C-b ?` from anywhere)
  opens the HELP page at the key reference. It was a centered modal until
  the list outgrew a screen: a box that clips is a box that quietly stops
  documenting half the keys, and it covered the thing you opened it to ask
  about. It was then the DOCS space, with the topics listed as a folder in
  the file rail — which answered a press on help by rearranging the file
  screen. It is a page of its own now, entered and left the way SETTINGS is:
  a contents column, the topic beside it, and `esc` back to what you were
  doing.

## Mouse — full support, promoted from a v1 cut

Everything visible is a target; the daemon owns the layout, so it can
hit-test every click server-side:

- **Click** a tab to switch workspace; click `[+ new]` to create one.
- **Click** a rail row to select it; **double-click** (or Enter) to put it
  on the stage. The printed verb hints (`c:commit`, `r:restart`,
  `d:diff`…) are real buttons — click them.
- **Wheel** scrolls whatever is under the pointer: rail lists, editor,
  diff, or terminal scrollback — no focus change needed.
- **Drag** the rail/stage boundaries to resize them; drag splitters
  between stage splits.
- **Inside a stage terminal**, clicks and wheel are forwarded to the
  application when it has enabled mouse reporting (vim, htop, lazygit,
  Claude Code's TUI all work — the VT emulator already tracks the
  requested mouse protocol per pane). In the editor, click places the
  cursor and drag selects.
- **Shift bypasses butai entirely** (the standard tmux escape hatch), so
  your terminal emulator's native selection/copy always works.

Mechanically: the client forwards SGR mouse events with position and
modifiers (protocol `InputEvent` grows `MouseUp`/`Drag` + button/mods);
the server routes them by region — chrome, rail, stage — and re-encodes
for the inner PTY when the app wants them.

## Color — truecolor chrome, untouched pane content

- **The chrome is themed, pane content is sacred.** Agents' and TUIs' own
  colors pass through the VT pipeline untouched (24-bit already works
  end-to-end in v1); only butai's own chrome — tabs, rails, borders,
  status — draws from the theme.
- **Semantic, not decorative:** green = ok/staged, red = failed/needs-you/
  deletions, yellow = working/modified, blue = info/navigation, one accent
  color = focus & selection. States are never color-only — every state
  keeps its glyph (`[!]`, `FAIL`) so the UI survives colorblindness and
  monochrome terminals.
- **Truecolor-first with honest downgrade:** the client reports its color
  capability in `Hello` (from `COLORTERM`/terminfo); the daemon renders
  the chrome in 24-bit and quantizes to 256/16 colors per client, so an
  old SSH session and a kitty window can watch the same session and both
  look right.
- **Theme presets** ship in config — `tokyonight` (default), and the
  usual suspects (catppuccin, gruvbox, solarized) — each pairing the
  chrome palette with a matching syntect theme for the editor and diffs.

## Workspace config (`.butai.toml` in the repo)

```toml
name = "butai"

[[processes]]
name = "dev"
cmd = "npm run dev"
ready = ":5173"          # substring that flips status to ok

[[processes]]
name = "test"
cmd = "cargo watch -x test"

[agents]
autostart = ["claude"]   # spawned into the Agents rail on workspace open
```

Rail geometry is *not* a per-project setting: layout belongs to the
workbench, so `Alt-l` resizes every workspace at once and saves the result
to `~/.butai/config.toml`. `←/→` set the rail widths, `↑/↓` the height of
whichever section has focus — AGENTS and PROCESSES trade rows with each
other, and PROCESSES trades with SYSTEM. The right rail holds one list, so
CHANGES is simply as tall as the rail:

```toml
[ui]
left_rail = 28           # rail widths in cells (defaults 28 / 38)
right_rail = 38
procs_height = 12        # section heights in rows; omit one and that
system_height = 6        # section sizes itself to the terminal instead
```

Every height is optional, and unset is the default: PROCESSES takes a share
of the rail proportional to its height, and SYSTEM takes 6 rows (none at all
below 12). A height written here is honoured literally, fitted only so the
sections it shares a rail with keep a row and their verb hint.

(An older `[ui]` table in a project's `.butai.toml` still parses, but its
geometry is ignored.)

`butai .` = open (or reattach to) the workspace for this directory: tabs
persist in the daemon, processes come up, autostart agents spawn.

## Mapping onto the v1 codebase

Nothing structural is thrown away — this is chrome over existing bones:

| v2 concept | v1 machinery |
|---|---|
| Workspace tab | `Session` (+ tab bar replaces window list in the status line) |
| Agent / process rows | `TerminalPane` + a `role` tag and status derived from bell/output-quiet/exit — emulator hooks already exist |
| Stage | the active window's pane tree (splits allowed here only) |
| Changes rail | `GitPane` restyled; diff-on-stage is `DiffPane` |
| Rails/chrome | drawn in `render::compose_frame` exactly like the status bar today |
| Attention badges | new `ServerMsg` event + per-pane state; also exposed on the JSON API |
| System gauges | new daemon sampler task (`/proc`, hwmon, nvidia-smi) feeding a `SYSTEM` rail widget + API endpoint |
| Mouse | client already ships crossterm mouse events; extend `InputEvent`, add server-side region hit-testing, per-pane mouse-protocol passthrough |
| Color | client capability flag in `Hello` + per-client quantization in the frame diff; theme presets extend the existing `[theme]` table |

Biggest new pieces: attention detection heuristics, the chrome layout
(fixed three-column with collapse), `.butai.toml` loading, the Alt keymap
layer, the system sampler, and mouse hit-testing. All server-side except
mouse-event capture; the TUI client barely changes.

> **This table is a record of how the workbench was built, not of where it
> lives.** The last line of it — "all server-side" — is no longer true, and
> deliberately so. The daemon renders a pane's screen only when a program's
> bytes are on the other end of a PTY; everything else crosses the wire as JSON
> on `/v1/*` and the client draws it. So the rails, the chrome layout, the
> keymap, hit-testing and the palette are `butai-client`'s, `render::compose_frame`
> and `DiffPane` no longer exist, and `GitPane` survives as the thing that
> produces `ChangesDto` rather than as something drawn. The rule and its reasons
> are in [protocol.md](protocol.md#frames--how-pane-content-reaches-you); what
> the daemon still owns is unchanged — panes, PTYs, git, processes, telemetry.

## One vocabulary in the browser client, and no build step to get it

The browser client's problem was never colour. Its palette was tokenised, it was
coherent, and `check.py` already asserted it matched `settings.js` to the digit.
The problem was that nine components each carried a `<style>` inside their own
shadow root, and a shadow root is a wall you cannot see over: nothing in that
file could observe what the other eight had chosen. So there was no drift
*decision* anywhere — there was just drift. Four section-header styles. Three
ways to draw one selection cursor. Six button shapes. Paddings of 2, 4, 6, 7 and
8 pixels on a single 12px gutter, none of them wrong on their own page.

The fix is not a stylesheet, it is a **vocabulary**: one `SectionTitle`, one
`Row`, one `Button` with five variants, in one file every page composes from and
adds no geometry to. Where a page needs a shape the kit lacks, the kit grows it,
which is what makes the next page's version of that shape the same shape. That
is a social property as much as a technical one, and it is why the components
have opinions — `Row`'s selection is a background band and a ring rather than a
border because a border changes the box, so a moving cursor would reflow the
list under itself; `Meter` always draws a track because a bar that stops at its
value with nothing behind it does not say what full would have been.

Three positions are worth writing down because each looks like a mistake from
one angle:

**shadcn, hand-ported.** shadcn is not a library you can link to — it is MIT
source you copy in, which turns out to be Radix behaviour plus Tailwind class
strings. Both halves survive being vendored by hand: Radix arrives as an ES
module, the class strings are kept as written. What is lost is JSX, so the trees
are `htm` tagged templates. Adding Babel-standalone to get JSX back would
compile on every page load, which is slower than the thing being replaced.

**A CDN and an import map, rather than npm.** The repo stays free of
`node_modules/`, a bundler and a lockfile, and the client stays something you
can read in a browser's sources tab and edit in place. The cost is real and
specific: the dependency set is pinned in an import map rather than resolved,
and the shipped image has to vendor those URLs so it needs no network at
runtime. That is a smaller cost than a build step in a repo whose whole client
story is "no build step".

What that cost is *made of* is worth being exact about, because it is the part
that looks like a one-line `curl` and is not. An esm.sh entry point is a
three-line shim that re-exports the real module by URL, and that module imports
more: the seven pinned URLs reach around a hundred files. Downloading the seven
produces seven files that all still fetch from esm.sh on first load — vendored
by appearance and not at all in fact. So `web/tools/vendor.py` walks the graph,
rewriting every specifier that names a URL and following what it named.

The other half of the cost is that an import map has no resolver behind it, and
so nothing checks it. A package manager would have told us that `htm/react`
resolves its React peer to whatever esm.sh's unpinned `/react` currently is;
instead the page loaded React 18 *and* React 19, and React 19 stamps elements
`Symbol.for("react.transitional.element")` where 18 uses `react.element`, so
react-dom@18 rejected every element htm made — "objects are not valid as a React
child", from a file nobody wrote. `?external=react` on each dependency is what
makes packages import a bare `react` the map resolves once, and the same
argument applied to `react-dom` halved the vendored graph, because a Radix
Dialog portals through react-dom and had been pulling a floating canary of its
own. Two builds a week apart had been vendoring different files.

The general shape of that: **a bare specifier resolves through the import map or
not at all**, with or without a network, and nothing in the repository was
positioned to say so. The vendorer now is — it collects every bare specifier the
served modules import and fails the build if the map resolves nothing for one,
which is how ten Radix modules importing `react/jsx-runtime` were found before a
browser found them. Vendoring did not cause any of this. It is only what made a
resolution problem visible early, in a build, instead of late, in a console.

**The light DOM, and Tailwind for geometry only.** Tailwind's Play CDN injects
one `<style>` into `document.head`, and shadow roots cannot see it — so the
rewritten views cannot use shadow DOM, and the ports are rewrites rather than
restyles. That is the genuine price of the approach. What is bought is that the
config maps `bg-background` to `var(--bg)` **by reference**: Tailwind supplies
the geometry and never a colour, so a theme is still CSS variables written onto
`<html>` and nothing else, and there is no second palette to keep in step.

The corollary is a failure mode with no symptom: Tailwind emits no rule at all
for a colour its config does not define, so a class that falls through the
bridge is *invisible* rather than wrong — no console line, no red, just an
element with no background. Two independently written halves of this drifted
exactly that way before landing. It is checked now, which is the same argument
as the palette check that came before it: a design decision that is not asserted
somewhere is a design decision with a shelf life.

## USAGE — reporting a number you do not own

The page answers *which agent account stops me first*. Three calls are worth
recording, because each of them was the tempting thing not to do.

**A percentage has to be somebody's, or it is nobody's.** No agent CLI *asks*
for its limits on the command line, and querying a provider directly would mean
authenticating as the user — so the first build of this page drew totals only:
`4.5M tokens`, `of: null`, and a bar only where the user wrote a `[[budgets]]`
number. The alternative on the table then — a plausible ceiling from a table of
published plan tiers — would have looked better in a screenshot and been wrong
on the day a plan changed.

What that reasoning missed is that a CLI can publish a limit *to itself*.
`claude` renders its own `/usage` screen from `cachedUsageUtilization` in
`~/.claude.json` — a percentage per window and a reset instant, refreshed
whenever it runs, in the same plain config the account and plan already came
from. Reading it costs no trust and invents no denominator, which is the whole
objection answered: the number is the provider's, and the page says so
(`source: "published"`). Windows for a CLI that publishes nothing still draw as
totals. **The rule was never "draw totals" — it was "never invent a ceiling",
and a published one is not invented.**

One consequence fell out of that immediately: the page reads the `limits` array
rather than its sibling per-window keys (`five_hour`, `seven_day_opus`, and a
rotating cast of internal codenames), because it is the normalised shape and
does not grow a key every time a plan gains a tier.

**A cached limit is a snapshot, not a feed — and that was got wrong first.**
The original build trusted `cachedUsageUtilization` unconditionally: if the key
parsed, its numbers won. The freshness was read and spent entirely on prose, in
a note saying how long ago the file was written. There was one guard, and it
guarded the wrong thing — a window whose `resets_at` had passed was reported as
empty, on the reasoning that the window had rolled over and a stale 87% was
describing something that no longer existed.

Both halves of that were wrong, and a user found it. `resets_at` is `null` on an
idle window, so the guard could not fire on the case that mattered, and an
arbitrarily old `0` sailed through as a confident `session 0%` — bar, `Metered`
state, `Published` badge, the most authoritative rendering this page has — on a
machine that had spent millions of tokens in that very window. The transcript
count that would have shown it was never computed, because the fallback fired
only when the key was missing, never when it was merely old. And where the guard
*did* fire it substituted a `0` of its own: a fabricated number wearing the
provider's authority, which is the failure this page exists to avoid.

The rule now is per window, and it is the window's own span: a snapshot is
evidence about *now* only while it is younger than the window it names. Once the
cache has outlived a five-hour window, every second of that window happened
after the reading. The same file can be authoritative for the seven-day row at
that same instant — a week is barely dented by six hours — which is why the
check cannot be one freshness cutoff per file. Whatever the snapshot can no
longer speak for is dropped and counted from the transcripts instead, and the
note owns up to the mix. **The span is not a tunable**, deliberately: any
constant would encode a guess about how often `claude` refreshes, and it makes
no such promise. Observed on the machine this was found on: twenty hours between
refreshes, with the CLI running the whole time.

**Five states, because "no data" has five different meanings.** `counted` (real
numbers, no ceiling), `unknown` (installed, and we cannot see its usage),
`no_account` (nothing to meter — your own API key), `absent` (not installed),
`metered` (a ceiling exists). Collapsing `unknown` into `no_account` would
tell somebody their subscription CLI has no limits, which is the one wrong
answer this page can give. `metered` deliberately does *not* split by whose
ceiling it is — that distinction matters to a reader, not to a renderer, so it
rides on `source` and every client keeps one code path for drawing a bar.

**Each CLI was surveyed, not assumed.** The three installed here land in three
different states for three different reasons, and the reasons are the design:

- `claude` caches the provider's limits, so it is `metered` — above.
- `gemini` publishes no ceiling anywhere, but every assistant turn in its
  session files carries `input`/`output`/`cached`/`thoughts`/`tool`. So it is
  `counted`, and `cached` comes off the total for the same reason claude's
  cache reads do: replayed context is not work done.
- `agy` is the instructive one. It **has** a quota — its `quota_manager` pulls
  a user quota summary on each run — and it writes none of it down, keeping it
  in an in-memory cache for the life of the process. So `unknown` is not a gap
  in this code, it is the honest report, and the note says so rather than
  implying butai has not got round to it yet.

That third case is why the page keeps `unknown` and `no_account` apart. "A
quota exists and is unreadable" and "there is no quota" look identical on a
screen that only knows how to draw a number.

**Two transcript layouts, one accumulator.** Claude appends JSONL, so the
counter resumes from a byte offset. Gemini rewrites a session file whole every
time it grows, so there is no offset to resume from — that reader re-opens a
file only when its mtime moves and deduplicates by message id, because a
rewritten file presents all of its earlier turns again on every read. Both fill
the same entry list, so the windowing and pruning are written once.

**"Installed" is the spawner's answer, not `PATH`'s.** The page's first version
asked `PATH` whether a CLI existed, and on the machine it was written on that
made `claude` and `gemini` both `absent` — they live under
`~/.nvm/versions/node/*/bin`, and the daemon's inherited `PATH` was
`~/.local/bin:/usr/bin:/bin`. Every pane launched them regardless, because pane
spawning has always fallen back to the directories a login shell would have
added. Two answers to "is this installed?" is one too many, so the sampler calls
the spawner's own resolution rather than keeping a copy: a USAGE page that
contradicts the AGENTS rail is describing a machine nobody is running. The
`--version` probe inherits the same repaired `PATH` a pane's child gets, which
is what keeps a `#!/usr/bin/env node` launcher from being handed the
distribution's decade-old node and reporting no version at all.

**It reads config, never credentials.** The account and plan come out of
`~/.claude.json`, which the CLI wrote in plain text. `.credentials.json` sits
beside it and is not opened: authenticating to a provider *as the user* to ask
for live limits is a decision they have not made, and the page is worth less
than that decision.

The page is one of the spaces even though an account limit is not a property of
a workspace — by the test that put BOOTH on the tab bar, it is BOOTH-shaped. It
is a space because the question is asked *while you work*.

**Half of the original bargain is gone, and it should be said plainly.** The
argument was that its badge followed you: the one number that mattered rode the
view rail onto every page, so the page did not have to be visited to do its job.
The rail became a tab-bar menu and the badge rode the button for a while, and
then the button gave the badge up too — a number nobody reads is a column spent
on nothing, and the count that mattered most was already announced five other
ways. So the badge now lives on the USAGE row of the spaces menu, one keystroke
away rather than in the corner of your eye.

That weakens the case for USAGE being a space rather than a fourth column on
BOOTH beside COMPUTE. It is not settled by this change; it is made closer. What
would decide it is whether anyone opens the menu to check.

## Rebuilding a link, rather than only retrying a socket

A forwarded daemon is reached at a local path, and for a long time that path was
treated as the whole connection: if the stream dropped, retry the path. That is
right when the far daemon restarted and wrong when the *forward* died, because
the path was ssh's and went with it. The client had no route from "the forward
died" back to "the forward is alive", so it retried a socket that could never
answer, forever, while `hosts` went on naming the machine and the picker refused
to add it again. On a desktop that is rare. On a laptop it is every closed lid,
and it read as butai being broken.

**Three decisions inside the fix are worth keeping written down.**

**It lands in the tab the machine already had.** The obvious implementation is
the one the deliberate disconnect already uses — drop the machine, dial, append
— and it is wrong here. `Vec::remove` shifts every index after it, so other
machines' tabs move, `view.tab` and `view.browse_daemon` point somewhere else,
and all of that happens for a machine the user never asked to remove. The
replacement is in place: the entries at that index are overwritten and nothing
is added or removed. The rule it comes from is the one the whole client/daemon
split runs on — *if a screen looks different at the end, that is a bug*.

**The old forward is dropped before the new dial, not after.** A forward's local
path is derived from the target and the client's pid, so a re-dial from the same
client binds the *same* path, and `forward()` unlinks it before binding. Cleaning
up the stale `Forward` afterwards would therefore delete the socket the new ssh
had just created — a bug that only appears on the second connection to a machine
and looks like ssh failing. Dropping first also kills the old ssh, which is what
releases the ControlMaster it was holding: on a slept laptop that master is
half-open, and a dial that multiplexes onto it hangs instead of connecting. The
alternative was a fresh path per dial, which trades one ordering rule for a
directory of sockets to reap.

**Two signals, because neither alone is enough.** An ssh child that has exited is
conclusive — act at once. A child that is still running proves almost nothing on
a laptop that slept, where the link is half-open and ssh has not yet given up, so
that path waits for the stream to drop twice, which is what separates a machine
that went away from a far daemon that is merely restarting. The keepalives are
the third leg: without `ServerAliveInterval` a half-open link is neither alive
nor dead for hours, and it takes the shared control socket with it.

The backoff is per machine and grows to five minutes because a dial is not free —
one can spend twenty seconds asking a sleeping host where its daemon is. The
cost of being wrong in the cheap direction is an ssh; in the expensive direction
it is a client that quietly stops trying.

## A stage that goes black is making a claim

Rebuilding the link fixed what the client *did*. What it showed was still wrong,
and wrong in a specific way: the stage cleared. A blank rectangle where a program
was is not a neutral absence — it is a statement, and the statement is "there is
nothing here." That is the one thing that is never true when a link drops. The
pane is on the other machine, still running, and `kill-server` snapshots every
workspace and restores it on the next start. What ended was the telling, not the
program.

So the last frame stays. It is dimmed to one faint colour, and a card over it
names the machine, counts the seconds, and says in as many words that what is
behind it is the last frame rather than what is happening now. Three properties
were worth paying for:

**Dimming does more work than the card.** A screen flattened to one colour reads
as inert from across the room, before a word is read. The card is for the person
who then leans in; the colour is for the person who glances. Neither alone is
enough — colour is the first thing a terminal, a theme or a screenshot loses,
which is also why an away chip takes a `·` rather than only going faint.

**The age is the number that changes what you do.** "Down 4s" is a daemon
restarting and you wait. "Down 2h10m" is a laptop that is not coming back and you
go and open its lid. A notice that flattened both to "reconnecting" would answer
neither question, so the seconds count from the drop — not from the last retry,
which would leave it reading "down 0s" forever.

**A closed pane must not borrow this screen.** A program that exited genuinely
leaves nothing to show, and dressing that as a lost connection is the same lie
pointing the other way. The daemon already knew the difference and was throwing
it away: `detached` carries a reason, and the client was matching `Detached { .. }`.
One of those reasons — the daemon shutting down — means the opposite of all the
others, so it is a named constant on both sides now rather than a string literal
on each. End-of-stream with no reason at all is folded in with it: a daemon that
was killed, or a forward that died, never got to say anything.

The retry that goes with it is on a clock rather than on the repaint. It used to
ride the paint loop, which is every 120ms while anything animates, and each
failure wrote its own line into the footer — so a machine that was off turned the
one place that could have explained the situation into a strobe, above a stage
that had already gone black.

## A link is the drawing client's question, not the daemon's

An agent prints a URL and the natural next thing to do is click it. Nothing did,
because nothing in butai had an opinion about what a URL is: the daemon holds the
PTY and turns a program's bytes into cells, and a cell is a character with a
colour. The address was there and only a person could see it.

The daemon is the obvious place to fix that — it has the pane's whole text, and it
knows the one thing a client cannot know, which is where the program *wrapped* a
line. It is still the wrong place. Three reasons, in the order they mattered:

**It would only cover panes.** URLs are also in a diff, in a file, in a commit
message, in the remote a git rail names — all of which the client already has,
none of which the daemon renders any more. Half a feature at the daemon plus the
other half at the client is two implementations of "what is a URL".

**It would be a rendering concern on the wire.** Which runs are clickable is a
property of the composed *screen*, and the screen is the client's. Shipping spans
would mean shipping coordinates that are only meaningful against a layout the
daemon does not decide — the boundary refactor exists to stop exactly that.

**The client already has the cells.** It is painting them; scanning the buffer it
is about to write costs one pass over the same memory. So the scanner lives in
`butai-client::links` and reads the composed buffer once per frame, which also
means the terminal's mark-up and the `f` picker cannot disagree about what is on
screen — they are the same map.

What is genuinely lost is the wrap flag, and the cost is visible: a URL that ends
exactly at the right edge of a pane is joined to the row below it on the evidence
of the row being full, which is what a wrap leaves behind but not proof of one.
The alternative — never joining — offers a *truncated* address as a link, and a
link that quietly goes somewhere else is worse than a link that is missing.

The evidence needed a second clause, and a shell found it within an hour: `$ echo
https://…` fills a narrow pane's row exactly, the shell echoes the URL on the
next row, and the join produced one address that was the URL written twice — and
offered it. So a row that *begins a link of its own* is never a continuation: a
wrapped URL resumes in the middle of a path, and a path does not start with
`https://`. That is cheap and it covers the case that actually occurs; a program
that ends a line exactly at the edge and then prints unrelated prose can still be
got wrong. If that ever bites, the fix is a per-row wrapped bit on the frame,
which is additive and small — but it is a protocol change for an ambiguity the
two rules above have not yet been seen to lose.

**Two ways to follow one, because there are two pointers.** OSC 8 hands the URL
to the terminal butai is drawn on, which is what makes hover and cmd-click work
without butai implementing either — the terminal already has that gesture, and a
client that drew its own underline would be competing with it. But OSC 8 is
useless in stock tmux, which drops it, and useless to a keyboard. So the picker
is the other half: `f` lists what is on screen, `enter` opens it *here*, and `y`
copies it *there* — over ssh the browser is on the machine the terminal is on,
and OSC 52 is how a string reaches it. The fallback is not a consolation prize;
on the machines butai is mostly used from, it is the primary path.

## Open questions (current calls, revisitable)

- **Windows-within-workspace**: dropped. Tabs are workspaces; the stage
  swap replaces most window use. (`C-b c` power users can still split the
  stage.)
- **Editor in rails?** No — files open on the stage via a picker
  (`Alt-o`, fuzzy) instead of a permanent file tree. The tree earned its
  keep poorly; changes + fuzzy-open cover 90% of navigation. The tree
  remains available *on the stage* for exploration.
- **Agent status fidelity**: heuristics (bell, output-quiet-after-prompt)
  will misfire occasionally; the escape hatch is a per-agent
  `waiting_pattern` / `busy_pattern` regex pair on an `[[agents]]` block.
  Built: each pattern *replaces* the built-in marker table for that one
  signal rather than adding to it, because the expensive misfire is the
  false positive — a pane pinned to "busy" never fires its finished
  notification, and no additive pattern can take that back.
