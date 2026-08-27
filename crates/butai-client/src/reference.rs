//! The built-in reference: what the HELP page shows.
//!
//! It used to be a modal. `?` opened a two-column list of keys that outgrew the
//! screen, scrolled with `j`/`k` without saying so, and truncated the rest — so
//! a clipped list read as the whole list, and the features named only in the
//! rows below the fold read as missing. One of them was reported that way.
//!
//! Then it was the DOCS page: the topics were listed as a `butai://reference`
//! folder in the file rail and opened in the file viewer, which meant pressing
//! help rebuilt the file screen around something that was not files. It is a
//! page of its own now — see [`crate::chrome::help`], which owns every question
//! about how it is laid out and read.
//!
//! What is left here is the text. It lives beside the code it describes so the
//! two are edited together — the same reason the key table did. [`PREFIX_MARK`]
//! stands in for the prefix key, which is configurable, and is replaced when a
//! topic is drawn.

/// Stands in for the prefix key, replaced at draw time with whatever the user
/// configured. A literal `^B` would be wrong for anyone who changed it, and the
/// reference is the one place that must not be.
pub const PREFIX_MARK: &str = "{prefix}";

/// The topic `?` and `[help]` open on the first time.
///
/// The keys, because that is what the modal held and what `?` means to anyone
/// who has used tmux. Named as a constant because [`index_of`] resolves it and
/// a retitled section must not silently move where help lands.
pub const HELP_SLUG: &str = "keys";

/// One page of the reference.
pub struct Topic {
    /// The row in the contents list.
    pub name: &'static str,
    /// Its stable id — what a link, a command or [`HELP_SLUG`] names it by.
    pub slug: &'static str,
    pub body: &'static str,
}

/// Where a topic sits in [`TOPICS`], or the first page when nothing is named.
///
/// Falls back rather than answering `None`: every caller is opening the page,
/// and a reference that refuses to open because a slug was renamed is worse
/// than one that opens at the beginning.
pub fn index_of(slug: &str) -> usize {
    TOPICS.iter().position(|t| t.slug == slug).unwrap_or(0)
}

/// The reference, in reading order.
///
/// Ordered as someone meets the workbench rather than alphabetically: where you
/// are, then the things in the rails, then the work, then the machinery.
pub const TOPICS: &[Topic] = &[
    Topic {
        name: "Getting around",
        slug: "getting-around",
        body: "\
# Getting around

The workbench has a fixed frame: a tab bar along the top, rails down the
left, and one stage in the middle. Nothing splits, and nothing moves. What
changes is *which space* the middle is showing.

## Spaces

    alt-o       files
    alt-m       docs — this page
    alt-c       docker (containers; alt-d is detach)
    alt-r       git — the repository over time
    alt-u       usage — which account limit stops you first
    alt-, / .   walk the spaces
    alt-space   the menu of them, with what each one is asking for
    alt-e       files, without the toggle back

Each space key toggles: the key that took you there brings you back to
work. The tab bar names the one you are on; the menu adds what each of the
others is asking for — `2!` for two agents waiting, `↓3` for a branch that
has fallen behind.

`alt-g` is *not* the git space — it puts the cursor on the CHANGES rail,
which is the other git surface. The two are the most easily confused things
here, so they do not share a letter.

## Workspaces

A workspace is one project directory. The tab bar holds all of them,
across every machine you are connected to.

    alt-1..9    a workspace by number
    alt-< / >   walk the whole bar
    alt-n       open another project, starting where you are
    alt-x       close this one, agents and all — it asks first
    alt-0       BOOTH

A project that does not exist yet starts in the same picker: `[new folder]`
asks for a name, makes the folder on that machine and steps into it, leaving
`[open this folder]` under the cursor. Nothing here needs a shell.

BOOTH is not a view of a workspace, it is the surface that spans them: every
project on every machine, with the selected one's screen. That is why it sits
beside the workspace chips rather than in the space list. The name is the
control booth at the back of a theatre — the one seat that watches every
stage at once and can key into any single one of them.

The middle column is that agent's live pane, not a picture of one. The keyboard
starts on the FLEET list, so `j`/`k` walk rows; `tab` or a click hands it to
the pane and everything you type from then on is the agent's. `alt-w` takes it
back. `[open]` is the other move — it goes to that agent's project, which
changes the tab you are on; typing at the preview does not. A project's name
does the same, because a project row has nothing to preview.

The fleet lists the projects with nothing running in them too, so `a` can start
one: it spawns that project's own agent — `[agents] autostart` in its
`.butai.toml`, then your `default_agent` — and `A` picks the type. `z` folds the
machine or project the cursor is on and `Z` folds every project, which leaves an
index of the whole fleet.

## Other machines

    alt-h       the machines: connect one, or disconnect one

Its projects join this tab bar, badged with the host name. The client dials
it directly — there is no daemon in the middle relaying another daemon's
screen.

The box lists the machines you are already connected to first, marked `*`,
then the `~/.ssh/config` aliases you could add, then a row to type a
destination `ssh` would take. Enter on a connected one drops the link: its
tabs leave the bar and the ssh goes with them. Nothing on the far side is
touched — the daemon there keeps running, with every pane it had — so
reconnecting is the same one keystroke and it does not ask first.

## Settings

    alt-s       this client's own configuration

Or `[settings]` in the footer, beside the other things that are about this
client rather than about a project. Not a space, so it is entered and left
instead of cycled through: `esc`, the button again, or anything you click
on the two bars puts you back on the page you came from.

## Room to read

    alt-z       zen: collapse both rails to markers
    alt-l       layout: resize them, arrows to adjust, esc saves
",
    },
    Topic {
        name: "Agents",
        slug: "agents",
        body: "\
# Agents

An agent is a coding CLI — claude, codex, gemini, aider, agy — running on a PTY
the daemon owns. It appears as a row on the AGENTS rail, and staging it puts
its screen in the middle.

    a           spawn the pinned agent
    A           choose which, from the list
    alt-enter   the same picker, from a focused pane
    j / k       move the cursor
    enter       put the selected one on the stage
    x           kill it
    m           the row's menu: close others, close all agents

`m` is the right-click menu, opened from the keyboard. It is where the two
verbs that act on the *rest* of the rail live, and it is the only place
they live.

## The pin

In the picker, `d` pins the highlighted agent. The rail's `[+ agent]` button
then reads `[+ NAME]` and spawns it on one click, and `a` stops asking. `A`
still picks, and `:agent-default` with no name clears it.

## What the states mean

    working     busy — the CLI says so in its own footer
    waiting     blocked on you: a confirmation, a question, a dialog
    finished    it stopped, and said something
    idle        up, with nothing to do
    exited      the process is gone

The daemon reads these from the last few rows of the agent's own screen, on
a tick of about two seconds — the bottom of the grid for `working`, and the
bottom of what has actually been drawn for `waiting`, so a dialog on a
screen that has not filled up yet still counts. An agent whose CLI words its
prompts unusually can be taught the pattern with `waiting_pattern` and
`busy_pattern` under `[[agents]]`.

## Driving them from a script

Everything on this page has a command-line form, which is how one agent
supervises others:

    P=$(butai agent spawn claude --background)
    butai agent send $P \"summarise ./logs\" --wait
    butai pane read $P --lines 40
    butai agent kill $P

`butai agent wait` blocks until an agent finishes, and its exit code is the
answer: 0 finished, 3 timed out, 4 exited. See `docs/agents.md`.
",
    },
    Topic {
        name: "Processes",
        slug: "processes",
        body: "\
# Processes

The PROCESSES rail holds long-running commands: dev servers, watchers,
whatever a project needs up. They are panes like any other, so staging one
shows its output.

    alt-t       a new shell
    [+ term]    the same thing, by mouse
    r           restart the selected process
    x           kill it
    m           the row's menu — the same one the right button opens

A process that exits non-zero leaves a row marked `FAIL(code)` rather than
disappearing, because a build that died is the thing you most need to see.

## From a script

    butai process start web \"npm run dev\"
    butai process ls
    butai process status -q || echo \"something died\"

`status` exits non-zero when any process has failed, so it reads well in a
condition.

## Declared in the project

A workspace's `.butai.toml` can list the processes it always wants running.
They start with the workspace, which is why a project that has one needs no
setup step.

## The machine, under the rail

The gauges below PROCESSES are this machine's cpu, ram and — when it has
one — gpu. They are the only part of the left rail the cursor does not
walk, so they have keys of their own rather than a row to press enter on:

    {prefix} S  the system monitor (htop)
    {prefix} Y  the gpu monitor (nvtop, nvidia-smi, rocm-smi, radeontop)

Clicking a gauge does the same thing. Both go on the stage as ordinary
process panes, so they detach and come back like everything else.
",
    },
    Topic {
        name: "The stage",
        slug: "stage",
        body: "\
# The stage

The middle of the screen shows one pane: whatever the cursor last staged.
While it has the keyboard, every key goes to the program inside it —
that is what makes it a terminal and not a preview.

    enter       put the cursor on the stage
    alt-esc     take it back off
    tab         cycle the rails
    {prefix} PgUp / PgDn   scroll its scrollback

The Alt layer is the exception: it always belongs to the workbench, so
`alt-o`, `alt-a` and the rest work from inside a running program. What the
workbench does *not* bind stays the program's, which is how `alt-b` and
`alt-f` still move by words in readline.

The prefix key works from inside a pane too. Press it twice to send a
literal one through.

## Why the daemon draws this one

A pane is a program's bytes on a PTY, and turning those into a screen needs
a terminal emulator. That is the one thing the daemon renders. Everything
else on screen — the rails, the tabs, this page — is JSON the client draws
itself, which is why the Mac, iOS and web clients exist without a terminal
emulator between them.
",
    },
    Topic {
        name: "Changes and diffs",
        slug: "changes",
        body: "\
# Changes and diffs

The CHANGES rail is the workspace's git status, live.

    s           stage the file the cursor names
    u           unstage it
    x           discard it
    c           commit
    C           commit everything
    p           push
    enter       open the diff of the file, the section, or the commit

## Conflicts

A conflicted file is listed first, in its own group, never mixed in with
ordinary work.

    o           take ours
    t           take theirs
    a           mark it resolved

## The diff

    ] / [       next / previous hunk
    space       stage this hunk
    v           pick lines within it
    x           discard it
    q           close

## The git menu

    g           branch, remote, stash, merge/rebase, amend, reset
    b           the branch picker on its own

Anything destructive asks first, with the thing it would destroy named in
the question.
",
    },
    Topic {
        name: "The repository",
        slug: "repository",
        body: "\
# The repository

    alt-r       the git space

Three columns: REFS above, the commit graph below them, and whatever the
cursor names drawn as a diff on the right.

**Not a second CHANGES rail.** That rail is about *now* — what is staged,
what is modified, what you are about to commit. This space is about the
repository over time: branches, worktrees, stashes, tags and the history
they point into.

    tab         walk the three columns
    enter       read what the row names
    home / end  the ends of the list

`enter` never changes the repository. It scopes the history to a ref, opens
a commit, or shows a stash — reading, in every case. The verbs written under
each list are what act, and each row offers only the ones that work on it.

    c           checkout
    m           merge
    d           delete
    x           drop a stash, remove a worktree
    y           copy the commit's full sha
    v           revert
    p           cherry-pick
    esc         widen back to every ref

A branch checked out in another worktree says so, and offers no `c` — the
checkout that would fail is not advertised. Neither is `c` on a remote
branch, which needs a local one tracking it first.

## The graph

The lanes on the left are the real parent edges, computed over the whole
page rather than the visible slice, so a lane a merge opened above the fold
still passes through the rows below it.
",
    },
    Topic {
        name: "Files",
        slug: "files",
        body: "\
# Files

    alt-o       the files space
    j / k       move
    enter       open a file, or descend into a directory
    ..          the row back up
    /           find, across the workspace
    e           edit the open file
    C-s         save
    esc         leave edit mode
    x           delete the file the cursor is on

The tree is on the left and the file on the right. A yellow marker means
git sees a change in that file, or somewhere under that directory.

`x` asks first, and the box opens on `no`. It is the only key here that
git cannot undo — the changes rail's `x` puts a file back to what the
index holds, and this one leaves nothing to put back. Directories are
refused; on one the key does nothing.

Searching is the daemon's: it walks the workspace and returns hits, so it
is as fast over ssh as it is locally, and a hit opens the file at its line.
",
    },
    Topic {
        name: "Docker",
        slug: "docker",
        body: "\
# Docker

    alt-c       the containers space

This project's compose stacks and their containers, with the logs of
whichever one the cursor is on. Selecting a container follows its logs;
leaving the page stops following, so a `docker logs -f` never outlives the
view that asked for it.

The logs arrive the same way everything else does: as a process pane the
daemon supervises. There is no docker client in the client — which is why
this page works unchanged against a daemon on another machine.
",
    },
    Topic {
        name: "Mouse and clipboard",
        slug: "mouse",
        body: "\
# Mouse and clipboard

Everything on screen is clickable: a rail row, a tab, the spaces button, a
footer button. The wheel scrolls whatever is under the pointer.

    click           select; click again to stage
    right-click     the menu for an agent, a process or a tab — `m` too
    drag            select text and copy it
    alt-drag        select inside a running program
    shift-drag      the terminal's own selection, for when you want that

**The mouse is never the only way.** Everything clickable has a key, and
the Keys page says which — including the menu the right button opens, which
for a long time it did not.

## Links

A URL anywhere on screen is a link. It is marked up for *your* terminal as
it is drawn, so hovering underlines it and your terminal's own gesture
opens it — cmd-click, or ctrl-click, or ctrl-shift-click, depending on
which one you use. butai keeps mouse tracking on, so the modifier is what
tells a click on a link from a click in butai.

    f           list the links on screen
    {prefix} f  the same, from a focused pane
    enter       open the highlighted one here
    y           copy it instead

Stock tmux drops the mark-up, and an ssh session has no browser to open
anything on. The picker covers both: `y` copies to the clipboard of the
machine you are sitting at, and where there is nothing to open with,
`enter` copies too and the title says so.

`[ui] links = false` turns the mark-up off for a terminal that shows the
sequence instead of acting on it. The picker still works.

## Images

    alt-v       paste the image on your clipboard

It is written into the workspace's scratch directory and its path pasted
where typing would have gone — which is what an agent CLI can actually
open. This works over ssh, because the clipboard is read on the machine
you are sitting at.
",
    },
    Topic {
        name: "Keys",
        slug: "keys",
        body: "\
# Keys

Two layers reach the workbench, and they are the same set on the same
letters. The Alt layer works from anywhere, a focused pane included. The
prefix layer is for terminals that eat Alt, or fingers that prefer it.

## The Alt layer

    alt-o m c       files · docs · docker
    alt-r u         git · usage
    alt-, .         walk the spaces
    alt-space       the menu of spaces
    alt-e           files
    alt-0           BOOTH
    alt-1..9        workspace by number
    alt-< >         walk the tab bar
    alt-n           open a workspace
    alt-x           close this workspace
    alt-h           add a host
    alt-a p g w     agents · processes · changes · all agents
    alt-esc         off the stage
    alt-t           a new shell
    alt-l           layout mode
    alt-z           zen
    alt-/           find
    alt-v           paste an image
    alt-enter       the agent picker
    alt-d           detach

## The prefix layer

The prefix is `{prefix}`, and `{prefix}` twice sends a literal one to the
pane.

    {prefix} o m c w    files · docs · docker · work
    {prefix} r          git — the repository
    {prefix} , .        walk the spaces
    {prefix} space      the menu of spaces
    {prefix} 1..9       workspace by number
    {prefix} [ ]        walk the tab bar
    {prefix} n          open a workspace
    {prefix} X          close this workspace
    {prefix} H          add a host
    {prefix} A P G W    the rails
    {prefix} s          the stage
    {prefix} a          spawn an agent
    {prefix} t          a new shell
    {prefix} x          kill what is staged
    {prefix} g b        git menu · branches
    {prefix} S Y        system monitor · gpu monitor
    {prefix} l          layout
    {prefix} z          zen
    {prefix} /          find
    {prefix} f          the links on screen
    {prefix} v          paste an image
    {prefix} ?          this reference
    {prefix} d          detach
    {prefix} :          the command prompt
    {prefix} PgUp/PgDn  scroll the stage

## On a Mac

Option is a compose key, not a modifier: pressing Option-o types `ø`, and
no terminal reports Alt at all. So the Alt layer arrives as punctuation and
nothing happens.

butai reads those characters back. Option-o *is* alt-o, and you need change
nothing. Only the keys the Alt layer binds are read this way, so `∫`
(Option-b) and the rest are still characters you can type.

Two cannot be recovered — Option-e and Option-n are dead keys, and emit
nothing until the next keystroke. Use `{prefix} n` to open a workspace;
alt-e is only another way to reach files, which alt-o already does.

To type `ø` and the others instead, turn the reading off:

    [general]
    option_as_alt = false

Then use the prefix layer, which reaches everything the Alt layer does — or
set your terminal to send a real Alt, which is better than either:

    Terminal.app    Settings › Profiles › Keyboard › Use Option as Meta Key
    iTerm2          Profiles › Keys › Left Option Key › Esc+
    Ghostty         macos-option-as-alt = true
    kitty           macos_option_as_alt = yes

Inside tmux, keep `xterm-keys` on so it passes Alt through rather than
eating it.

## Bare keys, when a rail has the cursor

    a A         spawn the pinned agent · choose one
    b           branches
    g           the git menu
    x           end the row — the agent or process the cursor is on
    m           the menu for the row — the right button's, from the keyboard
    n           open a workspace
    X           close this one
    /           find
    f           the links on screen
    ?           this reference
    q           detach
    tab         cycle the rails
    j k         move
    enter       stage the row, or open what it names

BOOTH's fleet takes `j`, `k`, `enter`, `tab`, `x`, `m`, `a`, `A`, `z` and `Z` —
and each of them acts on the row's own machine and project, not on the tab you
are looking at. `a`/`A` are the AGENTS rail's verbs and `z`/`Z` are the diff's
folds; nothing here is a key this workbench did not already have.

## Everything has one

**Nothing in this workbench is reachable by pointer alone.** Every button,
every row, every gauge and every menu entry has a key, and where a surface
is too narrow to say so, the key still works and is written down here.

That is why the verbs under a list are drawn from the same table the keys
are dispatched from: a key that is in no table does not exist, and a verb
that loses the competition for 38 columns is still bound. `m` is the clearest
case — the row menu holds actions that live nowhere else, and until it had
a key a mouseless client could not reach them at all.

Two things are the pointer's alone, and deliberately: dragging to select
text, and the wheel. Both are gestures rather than actions — there is no
verb they stand for.

## Changing them

    [keys]
    o = \"space files\"
    m = \"space docs\"
    F5 = \"process build cargo build\"

`prefix` under `[general]` moves the prefix key itself. The words are the
same ones the `:` prompt takes — see Commands — so anything the prompt can
say can be put on a key, including the ones this table does not ship.
",
    },
    Topic {
        name: "Commands",
        slug: "commands",
        body: "\
# Commands

    {prefix} :      the command prompt

One vocabulary, shared by the prompt, the `[keys]` table and the command
palette. What it takes:

    space work|files|git|docker|docs|booth|next|prev
    workspace 1..9|next|prev|new|close
    focus agents|processes|changes|fleet|stage
    agent [NAME]            spawn one, or pick from the list
    agent-default [NAME]    pin what `a` spawns; bare unpins
    process NAME COMMAND    start a process
    terminal                a new shell
    monitor [gpu]           the machine's own monitor, on the stage
    host                    the machines: connect or disconnect
    branch                  the branch picker
    update                  check for a newer butai, and offer it
    find                    search the workspace
    links                   the URLs on screen: enter opens, y copies
    layout                  resize the rails
    zoom                    zen
    git-menu
    close-pane              kill what is staged
    paste-image
    help                    this reference
    detach
    reload-config
    kill-server [clear]

`kill-server` remembers the open workspaces and comes back to them; only
`kill-server clear` forgets them.

## Themes

    theme = \"blueprint-dark\"

Under `[theme]` in `~/.butai/config.toml`. The palette is the client's, not
the daemon's — which is what lets one terminal watch a workspace in dark
while another watches it in light.
",
    },
    Topic {
        // The contents column is 24 wide; the title inside the page is the
        // sentence, and the row is the word for it.
        name: "Architecture",
        slug: "architecture",
        body: "\
# How butai is put together

butai is a daemon with clients. The daemon owns the workspaces, the panes,
the git state and the processes; a client draws them. This TUI is one
client, and it has no privileges the others lack.

## One API

    ~/.butai/butai.sock     the socket, one daemon per user
    /v1/*                   REST: workspaces, panes, agents, git, files
    framed protocol         attach, and stream one pane

Everything this workbench does, `butai` on the command line and any other
client can do. There is no side channel.

## The one thing the daemon renders

A pane's screen — because a pane is a program's bytes on a PTY and turning
those into cells needs a terminal emulator. Everything else crosses as
JSON. That is why the macOS, iOS and web clients draw a full workbench
without one, and why a design tool can embed the daemon and get a working
agent workbench without building one.

## Over ssh

    ssh host butai proxy

bridges stdio to a remote daemon's socket; the client spawns it as a child
and speaks the protocol down it. The daemon never listens on TCP. Add a
host with `alt-h` and its projects join your tab bar.

## Where things live

    ~/.butai/config.toml    yours: theme, keys, agents, rails
    ~/.butai/session.json   the open workspaces, restored on restart
    .butai.toml             the project's: processes it wants running
",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slug_round_trips_to_its_own_topic() {
        for (i, t) in TOPICS.iter().enumerate() {
            assert_eq!(index_of(t.slug), i, "{} resolved to another page", t.slug);
        }
    }

    /// A slug nothing answers to opens the reference rather than refusing to.
    #[test]
    fn an_unknown_slug_opens_the_first_page() {
        assert_eq!(index_of("no-such-topic"), 0);
    }

    #[test]
    fn every_topic_is_reachable_and_distinct() {
        let mut slugs: Vec<&str> = TOPICS.iter().map(|t| t.slug).collect();
        slugs.sort_unstable();
        let before = slugs.len();
        slugs.dedup();
        assert_eq!(slugs.len(), before, "two topics share a slug");
        for t in TOPICS {
            assert!(!t.name.is_empty(), "{} has no rail row", t.slug);
            assert!(t.body.starts_with("# "), "{} does not open with its title", t.slug);
        }
    }

    /// `?` has to land somewhere, and the key reference is where it landed
    /// before this was a page.
    #[test]
    fn the_key_reference_is_the_page_help_opens_on() {
        assert_eq!(TOPICS[index_of(HELP_SLUG)].slug, "keys", "`?` opens this one");
    }

    /// The reference names every way out of a pinned default agent.
    ///
    /// Carried over from the modal this replaced, where it was written after
    /// the pin was reported as "multi agent support seems gone, it now only
    /// says claude" — a fair reading of an interface that never mentioned the
    /// picker. With `default_agent` set, `a` spawns that agent and the rail
    /// button reads `[+ claude]`, so `A` is the only route to the others.
    #[test]
    fn the_reference_says_how_to_choose_a_different_agent() {
        let agents = TOPICS.iter().find(|t| t.slug == "agents").expect("an agents topic");
        for needle in ["A ", "alt-enter", "agent-default", "pins"] {
            assert!(
                agents.body.contains(needle),
                "the agents page never mentions `{needle}`:\n{}",
                agents.body
            );
        }
    }

    /// Everything that still works is still named.
    ///
    /// The key-by-key list the modal briefly became stopped mentioning the
    /// agent picker, the clipboard, the git menu and the theme command — all of
    /// which worked. A feature nothing on screen names is a feature reported as
    /// missing, so the subjects are pinned here rather than left to whoever
    /// edits a page next.
    #[test]
    fn the_reference_still_covers_what_a_key_list_stopped_naming() {
        let all: String = TOPICS.iter().map(|t| t.body).collect::<Vec<_>>().concat();
        for subject in
            ["alt-v", "alt-drag", "right-click", "git menu", "theme", "agent-default", "alt-h"]
        {
            assert!(all.contains(subject), "nothing in the reference mentions `{subject}`");
        }
    }

    /// The keys that were the pointer's alone are written down.
    ///
    /// Both were reachable by clicking and by nothing else — the row menu, and
    /// the monitor behind the SYSTEM gauges — so the reference not naming them
    /// would leave them exactly as undiscoverable as they were. The claim the
    /// Keys page now makes ("nothing is reachable by pointer alone") is only
    /// worth making if the page also says how, which is what this pins.
    #[test]
    fn the_reference_names_the_keys_that_used_to_be_pointer_only() {
        let all: String = TOPICS.iter().map(|t| t.body).collect::<Vec<_>>().concat();
        for subject in [
            "the row's menu",
            "the menu for the row",
            "monitor [gpu]",
            "{prefix} S",
            "Everything has one",
        ] {
            assert!(all.contains(subject), "nothing in the reference mentions `{subject}`");
        }
    }

    /// The prefix is configurable, so the reference must never print the
    /// placeholder — a page saying `{prefix} d` is worse than one naming the
    /// wrong key, and this is the one place that must be right.
    #[test]
    fn every_placeholder_is_substituted() {
        for t in TOPICS {
            let shown = t.body.replace(PREFIX_MARK, "^B");
            assert!(!shown.contains(PREFIX_MARK), "{} still shows the placeholder", t.slug);
        }
        let keys = TOPICS.iter().find(|t| t.slug == "keys").expect("a keys topic");
        assert!(keys.body.contains(PREFIX_MARK), "the keys page should name the prefix at all");
    }
}
