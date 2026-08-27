# Keys

Every action in the workbench has a keyboard shortcut. This page is the whole
list; the in-app reference (`?`, or `[help]` in the footer) is the same material
split by subject, and the footer under each list is the short version that fits.

## The rule

> **Nothing is reachable by pointer alone, and nothing is bound that cannot be
> found.**

Two halves, and both are enforced rather than intended:

- **Every click target has a key.** `every_click_target_has_a_key` in
  `crates/butai-client/src/workbench.rs` is a `match` over `hit::Target` with no
  catch-all, so a new clickable thing does not compile until someone has said
  which key reaches it. The assertions read the real tables, not a list of
  letters, so a key that moves fails the test rather than leaving a stale
  comment behind.
- **Every key is in a table.** `crates/butai-client/src/verbs.rs` holds one
  table per surface, and it drives four things at once: the footer text, the
  click hit-test, the key dispatch and the `?` reference. Binding a key without
  listing it is not possible — dispatch reads `VerbId`.

A verb that does not fit the footer is marked `quiet`: still bound, still in the
reference, just not competing for a column. That is the difference between *not
shown here* and *undiscoverable*, and it is the answer to "why isn't `m` written
under the rail?" — the PROCESSES footer is `t new · r restart · x kill`, 26
columns in 26.

Two gestures are the pointer's alone, deliberately: **dragging to select text**
and **the wheel**. Neither stands for a verb, so neither has a key.

## The two layers

The same set on the same letters, reaching the workbench from anywhere.

- **Alt** works from inside a running program — that is what it is for. An Alt
  key the workbench does not bind falls through, so `alt-b` and `alt-f` still
  move by words in readline.
- **The prefix** (`C-b` by default, `prefix` under `[general]`) is for terminals
  that eat Alt. Press it twice to send a literal one through.

### Spaces and workspaces

| | Alt | prefix | |
|---|---|---|---|
| files | `alt-o`, `alt-e` | `C-b o`, `C-b e` | `alt-e` does not toggle back |
| docs | `alt-m` | `C-b m` | a project's own markdown |
| docker | `alt-c` | `C-b c` | `alt-d` is detach, so containers take `c` |
| git | `alt-r` | `C-b r` | the repository: its refs, its history, its working tree |
| usage | `alt-u` | `C-b u` | which agent account stops you first. On a Mac, Option-u is the diaeresis dead key, so use `C-b u` there |
| work | the space key again | `C-b w` | each space key toggles back |
| walk the spaces | `alt-,` `alt-.` | `C-b ,` `C-b .` | |
| the spaces menu | `alt-space` | `C-b space` | every space with its badge — what the tab bar's own control opens |
| BOOTH | `alt-0` | — | a peer of the workspaces, not a space |
| settings | `alt-s` | — (`C-b S` in the browser) | entered and left, not cycled |
| workspace by number | `alt-1`..`alt-9` | `C-b 1`..`C-b 9` | |
| walk the tab bar | `alt-<` `alt->` | `C-b [` `C-b ]` | spans every machine |
| open a workspace | `alt-n` | `C-b n` | bare `n` too, off the stage |
| close this one | `alt-x` | `C-b X` | asks first |
| machines | `alt-h` | `C-b H` | connect one, or disconnect one |

### The rails

| | Alt | prefix | bare |
|---|---|---|---|
| AGENTS | `alt-a` | `C-b A` | |
| PROCESSES | `alt-p` | `C-b P` | |
| CHANGES | `alt-g` | `C-b G` | |
| the fleet (BOOTH) | `alt-w` | `C-b W` | `x` and `m` too — see below |
| the stage | — | `C-b s` | `enter` |
| off the stage | `alt-esc` | | |
| cycle | | | `tab` |
| move, open | | | `j` `k`, `enter` |

`alt-g` is the CHANGES rail, **not** the git space — `alt-r` is. They are the
two most easily confused things here, so they do not share a letter. Both show
the working tree's files and both stage them with the same letters; the rail is
the one that sits beside the agents doing the changing and owns the commit box,
and the space is the one with the history and the diff beside it.

### What fills them

| | Alt | prefix | bare |
|---|---|---|---|
| spawn the pinned agent | | | `a` |
| choose an agent | `alt-enter` | `C-b a` | `A` |
| a new shell | `alt-t` | `C-b t` | `t` (PROCESSES) |
| restart | | | `r` (PROCESSES) |
| kill the row | | | `x` |
| kill what is staged | | `C-b x` | |
| **the row's menu** | | | **`m`** |
| the system monitor | | `C-b S` | click a gauge |
| the gpu monitor | | `C-b Y` | click a gauge |
| scroll the stage | | `C-b PgUp/PgDn` | the wheel |

`m` is the right-click menu opened from the keyboard: on a rail it is that
row's, and off one it is the workspace's own. It is the only route to **close
others** and **close all agents** — two actions that live nowhere else in the
interface. It also carries **disconnect host** on a remote tab, which is the one
row of it that has a second way in: `alt-h` lists the machines you are connected
to, and choosing one drops it.

`x`, `m`, `a` and `A` also answer on **BOOTH's fleet**, and there they act on the
row's own machine and project rather than on the tab you are looking at — the one
list in the workbench where those are routinely not the same thing.

The SYSTEM gauges are the one part of the left rail the cursor cannot walk, so
their monitor is keyed rather than entered: `C-b S` is `htop`, `C-b Y` is
whichever GPU monitor the machine has (`nvtop`, `nvidia-smi`, `rocm-smi`,
`radeontop`).

### The rest of the workbench

| | Alt | prefix | bare |
|---|---|---|---|
| zen — collapse the rails | `alt-z` | `C-b z` | |
| layout — resize them | `alt-l` | `C-b l` | |
| find | `alt-/` | `C-b /` | `/` |
| links — the URLs on screen | | `C-b f` | `f` |
| the git menu | | `C-b g` | `g` |
| branches | | `C-b b` | `b` |
| paste an image | `alt-v` | `C-b v` | |
| this reference | | `C-b ?` | `?` |
| the command prompt | | `C-b :` | |
| detach | `alt-d` | `C-b d` | `q` |

Bare keys need the cursor off the stage. On it, every key is the program's —
which is what makes it a terminal and not a preview.

## Keys that belong to one surface

Each of these comes from that surface's verb table, and the footer under it
names the ones that fit.

**CHANGES** — the verbs follow the selected row:

| row | keys |
|---|---|
| unstaged file | `s` stage · `x` discard · `d` diff |
| staged file | `u` unstage · `d` diff |
| conflicted file | `o` ours · `t` theirs · `a` resolved · `d` diff |
| a commit | `d` show |
| always | `c` commit · `C` stage all and commit · `p` push · `g` git menu · `r` refresh · `?` keys |

**The diff** — `]` `[` hunks · `space` stage the hunk · `v` line-select
(`space` picks, `enter` applies) · `x` discard · `z` fold the file · `Z` fold
them all · `r` refresh · `q` close. On a staged diff the same keys unstage; a
commit's diff is read-only and offers none of them.

The same widget draws every diff — the DIFF space, the GIT page's body, a
commit, a stash — so these keys mean the same thing wherever you meet one. It
draws one card per file (its path, whether it was added, deleted or renamed, and
its `+n -m`) over the lines, each numbered on the side it exists in: a removed
line has no number on the new side, an added one none on the old. On a body too
narrow for the numbers they are dropped and the `@@` headers come back, which is
the same bargain the commit graph makes with its lanes.

**GIT** — `tab` walks the three columns. What acts depends on the row.

| row | keys |
|---|---|
| working tree | `enter` diff the whole worktree · `C` go to the CHANGES rail |
| unstaged file | `enter` diff · `s` stage · `x` discard |
| staged file | `enter` diff · `u` unstage |
| conflicted file | `o` ours · `t` theirs · `a` resolved · `enter` diff |
| a branch | `enter` scope · `c` checkout · `m` merge · `d` delete |
| a tag | `enter` scope · `x` delete |
| a stash | `enter` show · `p` pop · `x` drop |
| a remote | `f` fetch |
| a worktree | `enter` open · `x` remove |
| a commit | `enter` diff · `y` copy the sha · `v` revert · `p` cherry-pick |
| always | `esc` widen the scope back · `g` the menu · `r` refresh · `?` keys |

The file rows are the CHANGES rail's rows, under the rail's headings, answering
the rail's letters — one gesture for "stage this file" wherever you meet it.
`enter` opens the file's diff in the body *beside* the lists rather than taking
over the DIFF space, so you keep the refs and the history you opened it from,
and the diff keys above work there. The rail is unchanged and still owns the
commit box and the sync buttons, which is what `C` goes to.

**BOOTH** — `j` `k` walk the rows · `enter` go there: an agent's workspace on its
machine, or the project the cursor names · `a` start that project's agent · `A`
pick which · `z` fold this machine or project · `Z` fold every project · `tab` the
preview · `x` end the session · `m` the row's menu.

Ten keys, and every one of them is a key this workbench already had: `a` and `A`
are the AGENTS rail's, and `z`/`Z` are the DIFF page's, marks (`v` open, `>`
folded) and all. It used to be six, and the reason given was that the rest of the
rails' table is about a project and this page is not in one — which stopped being
true when its rows became projects.

The NEEDS YOU tray answers the pointer as the fleet does — a click puts the
cursor on the row the copy stands for. An agent folded away inside its project
has no row to move to, so its copy names nothing rather than selecting whatever
sits at that index; the tray is still the shortest way to it, through unfolding.

**FILES / DOCS** — `j` `k` move · `enter` open or descend · `backspace` up ·
`/` find · `e` edit · `C-s` save · `esc` stop editing · `x` delete · `q` close.

`x` deletes the file the cursor is on, and it asks first — the box names the
path and opens on "no", as discarding does. It is the one key here that git
cannot undo: an unstaged file is not in the index and a never-committed one is
not anywhere, so there is nothing to restore from. Directories are refused; the
key does nothing on one.

**DOCKER** — `enter` follow the logs · `r` restart · `x` stop · `s` a shell in
the container · `q` close.

**USAGE** — `j` `k` walk the CLIs · `r` re-read. Three keys and no more: the
page reports a state of the world it does not own — an account's standing lives
with the provider — so there is nothing on it to change from here.

**SETTINGS** — `j` `k` rows · `tab` groups · `enter` open a list or choose ·
`space` toggle · `-` `+` resize a rail · `0` back to automatic · `esc` leave.

**Overlays** — `j` `k` move · `enter` choose · `esc` or `q` dismiss · `y` / `n`
answer a confirmation. In the agent picker, `d` pins the highlighted agent as
what `a` spawns. In the link picker, `y` copies the highlighted URL instead of
opening it — which is the one that works from an ssh session with no browser on
it, and what `enter` falls back to there.

**Links have no Alt key**, unlike everything else in the table above. `alt-f` is
readline's forward-word and butai leaves it — with `alt-b` and `alt-y` — to the
pane, so a shell inside butai edits its line the way it does everywhere else.
`f` bare and `C-b f` reach the picker instead, which is the pairing `g` (the git
menu) and `b` (branches) already have.

## Changing them

Every key is a name in one mini-language, shared by `[keys]`, the `:` prompt and
the command palette — so anything the prompt can say can go on a key, including
the things the shipped table does not bind.

**`[keys]` is the prefix layer.** An entry names the key you press *after* the
prefix, and it overrides that one row of the table above — the Alt layer is
built in and not reconfigurable. So `o = "space files"` binds `C-b o`, and
`M-y` binds `C-b alt-y`, which is a real binding but rarely the one you meant.

```toml
[general]
prefix = "C-a"

[keys]
o = "space files"                     # C-a o
F5 = "process build cargo build"      # C-a F5
```

The vocabulary:

```
space work|files|docker|docs|git|usage|booth|next|prev|menu
workspace 1..9|next|prev|new|close
focus agents|processes|changes|fleet|stage
agent [NAME]            spawn one, or pick from the list
agent-default [NAME]    pin what `a` spawns; bare unpins
process NAME COMMAND    start a process
terminal                a new shell
monitor [gpu]           the machine's own monitor
host                    add a machine
branch                  the branch picker
update                  check for a newer butai, and offer it
find                    search the workspace
layout                  resize the rails
zoom                    zen
git-menu
close-pane              kill what is staged
paste-image
help
detach
reload-config
kill-server [clear]
```

An entry that does not parse is a warning on the SETTINGS page, not a refusal to
start — and that page also reports how many keys are bound and how many of them
came from your config, which is the question you have when a key does something
you did not expect.

## In the browser

The web reference client (`web/`) carries the same rule and the same tables,
ported to `web/verbs.js`. Two things are different, and both come from the
browser rather than from the design:

- **The prefix earns its place twice.** Here the thing that eats Alt is the
  browser: Chrome and Firefox take `alt-1`..`alt-9` for their own tabs on Linux
  and Windows, and Firefox's menu bar takes `alt-t` and `alt-v`. Those keys are
  bound anyway and each carries a note the `?` reference prints beside it; the
  `C-b` spelling is the one that always arrives.
- **Option on a Mac is recoverable there.** A terminal cannot get Option-e and
  Option-n back, which is why this page says to use `C-b n`. A browser reports
  `e.code` alongside the composed character, so the web client reads the key
  rather than the `ø` it typed.

What the web client does *not* have, it does not bind: no row menu, no
monitors, no find, no layout. `web/README.md` has the list and the reasoning.
The GIT space is there (`alt-r`, `C-b r`) and so is its `g` menu, with two rows
the terminal's does not carry — `Branch > rename…` and `Remote > add a remote…`,
both of them operations the daemon already had and no client called.

Two things the terminal's GIT page has and the browser's does not yet: the
working tree's **files listed under its summary row**, with `s`/`u`/`x` on them,
and the **file-card diff** — one card per file with per-side line numbers and
`z` to fold it. Both are client-side drawing over routes the daemon already
serves (`changes`, `diff`, `git/apply`), so this is a porting gap in
`web/butai-git.js` and `web/butai-files.js`'s `renderPatch`, not a missing
capability. Until it closes, the browser's GIT page still sends you to its
CHANGES rail to stage.

**SETTINGS and DOCS are there too.** `alt-m` opens DOCS, which is this client's
files page filtered to a project's writing, with a `reference` folder at the top
of the rail that `?` lands in — generated from the client's own verb tables
rather than written twice. **This is where the two clients differ today:** in
the terminal the reference is a page of its own (`?` opens HELP, and DOCS is a
project's markdown and nothing else), because opening it inside the file screen
rearranged that screen around a listing that was not files. The browser still
does it the old way. `alt-s` opens SETTINGS, and it takes a
prefix spelling the terminal has no need of: **`C-b S`**, not `C-b s`, which is
the stage. The terminal spends `C-b S` on a system monitor the web client does
not draw, so the letter is free — and every verb that reaches a *page* is
required to carry a prefix spelling, because a browser that claims the Alt key
would otherwise leave the page unreachable from the keyboard altogether.

The settings themselves are per client and live in that browser's
`localStorage`, which is the same rule the terminal follows for a different
store: the daemon renders no chrome, so it has no palette and no keymap to hold.
One difference the browser earns: the prefix is editable there, and `alt-=` /
`alt--` (the terminal's font) and `alt-z` (the rails) now survive a reload,
because SETTINGS is where they are kept.

**HOME is there too** (`alt-0`, and the `home` chip at the far left of the tab
bar), with the fleet on `alt-w`. Both take a prefix spelling the terminal has no
need of — `C-b 0` and `C-b W` — because a browser may claim the Alt key. The
fleet's bare keys here are the four it navigates with — `j`, `k`, `enter`, `tab`
— and no more. The terminal's fleet has two beyond them, `x` and `m`; this client
has neither yet, because its keys work by pressing the button a verb names and
its rows carry no kill button and no row menu to open. That is a gap in this
client rather than a rule, and the footer says so by listing only what it has.
One difference the browser forces: `tab` reaches the preview and nothing brings
it back, because from inside a pane `tab` is the agent's — `alt-w` and
`alt-esc` are the way out, exactly as they are in the terminal.

## On a Mac

Option is a compose key by default: pressing Option-o types `ø`, and no terminal
reports Alt at all. butai reads those characters back, so Option-o *is* `alt-o`
and nothing needs configuring. Only the keys the Alt layer binds are read this
way — `∫` (Option-b) is still a character you can type.

Option-e and Option-n are *dead* keys and emit nothing until the next keystroke,
so they cannot be recovered: use `C-b n` to open a workspace, and `alt-o` for
files. To type the punctuation instead, set `option_as_alt = false` under
`[general]` and use the prefix layer — or set the terminal to send a real Alt,
which is better than either:

```
Terminal.app    Settings › Profiles › Keyboard › Use Option as Meta Key
iTerm2          Profiles › Keys › Left Option Key › Esc+
Ghostty         macos-option-as-alt = true
kitty           macos_option_as_alt = yes
```

Inside tmux, keep `xterm-keys` on so it passes Alt through rather than eating it.
