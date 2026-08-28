# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **The FILES browser is a Finder-style trail of columns.** It was one directory
  and one cursor, and descending into a folder replaced both. That made every
  folder a one-way trip you could only reverse by remembering you had: nothing
  on screen said where you were or how you got there, and the `..` row — added
  to say *that* up existed — still could not say what was up there.

  Every directory on the path from the workspace root to where you are is now a
  column of its own, side by side, with the row you came through still marked in
  each. Where you are is the shape of the whole thing rather than a line of text
  you have to read.

  ```
  ┌ crates ───────────┬ butai-client ─────┬ src ────────[find]┐
  │  butai           ▸│  src             ▸│  chrome          ▸│
  │● butai-client    ▸│  Cargo.toml       │  hit.rs           │
  │  butai-server    ▸│                   │● workbench.rs     │
  └───────────────────┴───────────────────┴───────────────────┘
  ```

  `←`/`h` and `→`/`l` walk the trail, and the columns to the right of the cursor
  are **kept, not dropped** — so `←` then `→` is two local moves and no round
  trip, which over ssh is the difference between browsing and waiting. Moving
  the cursor is what drops them, and that is the point rather than a side
  effect: those columns are what the *old* selection contained, so leaving them
  under a new one would draw a path that does not exist.

  The browser grows a column at a time and stops at half the band, so a trail
  walked six deep still leaves the file the room; on a terminal too narrow for a
  browser and a file at once it goes to nothing rather than squeezing the thing
  you came to read. The trail scrolls left as it grows, so the column you are
  working in is the one that stays on screen.

  **`space` peeks.** It reads the file the cursor is on without handing it the
  keyboard, so the next `j` walks to the next name and shows you that one
  instead — a way to read down a directory a file at a time without committing
  to any of them. `enter` is the same read with the keyboard going to the file,
  and `←` from inside the file hands it back. Both clients bind all four, out of
  the same verb table.

- **A minimap down the right of the open file.** A file is read through a window
  a few dozen rows tall, and nothing on screen said how big the thing behind it
  was or where in it you were. The scrollbar answer is "somewhere between the
  top and the bottom"; this answers with the shape of the code — where the blank
  lines are, where a comment block sits, where the deeply indented middle of a
  function is — so a jump is aimed at something you recognise rather than at a
  fraction.

  Sixteen cells cannot hold a line of code and it does not try: each cell stands
  for a rectangle of the file, drawn as one shaded block whose density is how
  much ink is in that rectangle and whose colour is what that ink mostly *was*.
  A comment block is a muted slab, a run of strings is green, an indent is the
  blank left edge. Click anywhere on it to jump, and what you clicked lands in
  the middle of the window rather than on its top row — you aimed at a shape in
  order to read what is around it.

  It takes all sixteen cells or none. Below a floor the scale stops meaning
  anything, and a minimap you cannot read is sixteen cells of code you no longer
  have — so a narrow terminal, or a trail walked several deep, keeps the file.

  The web client draws the same picture from what it has: it prints files as
  plain text rather than highlighting them, so its texture is one colour at
  varying weight instead of a palette of token colours. Inventing a second
  highlighter there would have been a lot of code for a picture sixty pixels
  wide.

- **`BUTAI_HOME`, so a build you are still deciding about can be run against
  real work.** There was nowhere to put an unfinished butai. A build from the
  tree either replaced the installed binary or fought the running daemon for
  `~/.butai/butai.sock`, and the only isolation on offer was a fake `$HOME` —
  which is the right tool for a test and the wrong one for a build you mean to
  *use*, because it takes away the ssh config, the shell profile, the git
  identity and the repositories that make trying it worth anything.

  One variable now moves butai's state and nothing else: socket, lock, config,
  themes, logs, `session.json`, `panes/`, `scratch/`.

  ```sh
  BUTAI_HOME=~/.butai-dev target/release/butai
  ```

  Your own daemon keeps running beside it and the two never meet. It is read in
  one place, `paths::butai_dir`, so every path follows it at once — no
  combination of variables can leave a daemon holding one butai's socket and
  another's session store. Panes inherit it, so a `butai` shelled out inside a
  dev pane reaches the dev daemon.

  **It outranks `$BUTAI_SOCKET`,** which is the part that took a second attempt.
  A daemon exports `$BUTAI_SOCKET` into every pane it creates, so any command
  run inside butai already has one pointing at the daemon drawing that pane —
  and the socket variable used to be read first. `BUTAI_HOME=~/.butai-dev butai`
  typed in an ordinary butai pane therefore did the exact opposite of what it
  said: the dev daemon tried to bind the *real* socket, refused because it was
  taken, and the client attached to the real daemon with an unused state
  directory sitting beside it. The order is now `--socket`, then `BUTAI_HOME`,
  then `BUTAI_SOCKET` — `BUTAI_HOME` beats what butai exports at you and yields
  to what you typed. `--socket` also stopped being a clap `env` argument, which
  is what had been filling it in from the environment before anything else could
  be heard.

- **A `develop` branch, and dev releases separate from stable ones.** Feature
  branches land on `develop`; `develop` lands on `main` when it is worth a
  stable release, and `main` moves for nothing else — the README's install line
  fetches `scripts/install.sh` from `main` by raw URL, so what is on `main` is
  what a stranger's `curl | sh` runs today.

  One tag shape decides the track, and decides it by itself: `v1.3.0-dev.1` is
  published as a GitHub **prerelease**, `v1.3.0` as a release. That single flag
  is the whole separation, and it works because of something already true —
  `releases/latest` excludes prereleases, and `releases/latest` is the only
  endpoint `butai-update` asks and `scripts/install.sh` reads. So a dev tag is
  invisible to every stable install without either of them filtering anything,
  and reaching one is deliberate: `BUTAI_VERSION=v1.3.0-dev.1`. That is also how
  a remote machine gets a dev build, which is the case that matters — a
  workbench attached over `ssh host butai proxy` is talking to a daemon on the
  far side, and the far side is the one that has to be running the code.

  A prerelease takes its notes from `## [Unreleased]` and does not fail on a
  thin one; a stable tag with no section for its version still fails, as it did.
  CI now gates `develop` the way it gates `main`.

- **`[update] channel = "dev"`, so a dev build keeps itself current.** The dev
  track published builds nobody could follow. `releases/latest` is the endpoint
  that makes a prerelease invisible to a stable install, and it was the only one
  either half asked — so a machine on `1.3.0-dev.1` was offered `1.3.0-dev.2`
  never and `1.3.0` eventually, and moving between dev builds meant running the
  installer by hand each time.

  Underneath it, the version comparison learned what a prerelease is. It used to
  cut the suffix and compare the three integers, which made every `-dev.N` of a
  release *the same version* as every other — the reason a dev channel could not
  have worked even with the right endpoint. It is semver's ordering now:
  `1.3.0-dev.10` is ahead of `1.3.0-dev.9`, and the `1.3.0` they were leading to
  is ahead of both, so a dev install lands on stable when stable catches up.

  The key is read by **both** halves, and it has to be: the client checks for
  the binary a person runs, and the daemon checks for itself when `POST
  /v1/update` asks it to, so a daemon reading the other track would answer
  "already on the latest" to a machine whose client can see a newer one. It
  lives in the config of whichever `BUTAI_HOME` an install uses, which is what
  keeps a dev butai's track out of the stable one beside it.
  `BUTAI_CHANNEL=dev scripts/install.sh` installs the newest prerelease, and
  SETTINGS → ABOUT → **release channel** writes the key.

- **A daemon on a different build is asked about, not reported.** The handshake
  has always noticed — the daemon names its own version in it, and the client is
  the only thing holding both numbers — and what it did with that was put
  `daemon is 1.2.0, client is 1.3.0 — restart it: butai kill-server` in the
  footer. A line naming a command you have to leave butai to run, about a daemon
  the client has a socket to. On a track that cuts a build every few days, that
  is a sentence you read and step over.

  It asks now. On this machine the question is a restart and nothing is
  downloaded — a local daemon is spawned from the client's own binary, so
  stopping the old one *is* the upgrade, and the workspaces come back the way
  they do from any `kill-server`. On a tab from another machine it is the
  update question that already existed, since that daemon fetches its own build
  and this client's version says nothing about what it would get. Once per
  session, never over another box, and the footer line is still there when the
  box cannot go up.

- **`scripts/vet.sh`, which runs every check CI runs and then hands you the
  build.** `cargo fmt`, `clippy` and `test` under `-D warnings`, the
  generated-bindings diff, the four `bun` steps and `testsuite/run.sh` — each
  reported as passed, failed or skipped, and skipped cleanly when a tool is
  absent rather than failed. A named branch is checked out `--detach` into a
  throwaway worktree; no argument means this tree, uncommitted changes included,
  which is the case worth optimising for.

  `--run` is the part CI cannot do: it builds the branch and starts a daemon on
  it under `BUTAI_HOME=~/.butai-dev`, seeded once with a **copy** of your real
  `config.toml` and `themes/`. A copy and not a symlink, because the client
  writes back to `config.toml` — answering no to an update prompt lands a
  `declined_version` in it — and a build you are still vetting should not be
  able to edit the config your real butai reads.

- **`scripts/cut.sh <version>`.** The version appears four times in the root
  `Cargo.toml`: `[workspace.package] version` and the three internal `butai-*`
  pins, which carry a `version` beside their `path` so `cargo publish` has
  something to rewrite. Four strings that have to agree, edited by hand, is how
  a release ships with a crate still pinned to the last one. This rewrites all
  four, refreshes `Cargo.lock`, and stops — the commit and the tag are yours,
  being the two steps that are hard to take back.

- **BOOTH lists projects, not just agents — and you can start work in one from
  there.** The page could show you every agent on every machine and let you
  watch them; it could not let you act on the answer. Reaching a project meant
  `[open]` to it, `a` on the rail, then back — and a project you had not started
  anything in did not appear at all, because the rows were built by walking the
  agent list and emitting a header whenever the workspace changed.

  The rows come from the machine and project lists now, so a project with
  nothing running has a row and a connected machine with nothing open has one
  too. `a` starts that project's agent and `A` picks which — the AGENTS rail's
  own two verbs, bound here unchanged, acting on the project the cursor is in
  rather than on the tab you are looking at. The new agent appears in the fleet
  and the preview points at it; the page does not move.

  A project's `[+ claude]` button is the same act under the pointer, and it names
  what it will start for the reason the rail's does: a button that spawns on a
  single click with nothing in between is the only place you can see what that
  click is about to do.

- **A project's `.butai.toml` says which agent it uses.** `[agents] autostart`
  already declared what a workspace starts when it opens; it is now published on
  the workspace and read whenever a client offers to start one. So the answer to
  "which agent does this project use" is two steps — the project's own
  declaration, then the client's `default_agent` pin — and most projects need no
  new configuration at all. The preference lives with the project, travels to
  the machine it runs on, and is shared with whoever else opens it, which a
  client-side pin keyed by directory would not.

- **A project's name goes to that workspace.** An agent row still only moves the
  cursor, because a click meaning "let me look at this" must not throw the
  workbench onto somebody else's project. A project row has nothing to preview,
  so going there is the only thing pressing its name could be asking for.

- **`[x]` closes a workspace from BOOTH**, and `x` with the cursor on a project
  row does the same. It ends what the row *is*: on an agent that is the session
  and it does not ask, because an agent is a process whose transcript is on
  disk; on a project it is the workspace and everything running in it, so it
  asks — in the tab bar's own box and its own words, since that is the same act
  reached from somewhere else.

  The button is drawn on the cursor's row and nowhere else, which is the tab
  bar's rule for its own `[x]`: a button that ends a workspace has to be one you
  aimed at, not one sitting under a row you were passing.

- **`z` and `Z` fold BOOTH's fleet**, with the DIFF page's keys and its marks
  (`v` open, `>` folded) — this workbench already had a fold idiom and a second
  one for the same concept would be drift. `Z` leaves an index of every machine,
  every project, and what is running in each. A folded project draws its agents'
  sprites where their rows were, so folding costs you the titles and the buttons
  and not the states.

### Changed

- **BOOTH's compute column is one row per machine, and it names what is wrong.**
  It drew the SYSTEM rail's whole gauge stack per machine — twelve to twenty rows
  for a workstation, which is right for the rail (it describes the one machine
  you are working on) and wrong on a page whose question is which of four
  machines is in trouble. Four machines did not fit.

  A machine is a line now: what it is, how many agents it is running, and the
  *worst* of its four readings, named. Not the CPU — a box at 30% CPU with a full
  root filesystem is in trouble and its CPU number says it is fine. `z` or a
  click expands one back to the stack, drawn by the same renderer the rail uses,
  so the two cannot come to two opinions of what 41% means.

- **BOOTH's cursor walks rows rather than agents**, since a machine and a project
  are now things you can sit on. The agent under it is derived — one function for
  what `x` and the menu act on, one for what the middle column shows. On a
  project row the pane shows the agent in it that most needs you, so walking the
  fleet is a fly-over of each project's screen.

- The TUI groups the fleet's projects by *id* rather than by name, as the web
  client already did. Two machines routinely have a project of the same name
  open, and one machine may have two.

### Removed

- **The `..` row, from both clients.** It existed because descending read as a
  one-way trip and something had to say up existed. The trail says it — the
  directory you came from is the column to the left, still listed — so `..`
  became a row in every column whose only meaning was "the column immediately
  left of this one", which is the sort of thing you have to learn not to click.
  `backspace` still walks up, and so does `←`.

### Fixed

- **`scripts/install.sh` stopped the wrong daemon when `BUTAI_HOME` was set.**
  It read `BUTAI_SOCKET` and nothing else to find the daemon it was replacing,
  so installing a second butai with `BUTAI_HOME=~/.butai-dev` — the supported
  way to run one beside your own — stopped the real daemon on the way past. It
  now resolves the socket the way `paths.rs` does: `BUTAI_HOME` first, then
  `BUTAI_SOCKET`, then `~/.butai`.

- **Closing a workspace sent the DELETE to the machine the *active tab* was on.**
  From the tab bar those are the same daemon by construction, so it never bit;
  from BOOTH's fleet they are routinely not, and a `SessionId` is only unique on
  its own daemon — so closing a `gpu-box` project would have closed whatever
  held that id at home. `ConfirmKind::CloseWorkspace` carries the machine now,
  the same way `MenuTarget::Agent` already did after `x` on a fleet row had the
  identical bug.

## [1.2.0] - 2026-08-24

### Added

- **A daemon can update itself, on request: `POST /v1/update`.** `butai update`
  only ever updated the machine you typed it on. That is the wrong machine for
  half of what butai is for — a workbench attached over `ssh host butai proxy`,
  or the web client behind `web/server/`, is talking to a daemon it could not
  update, and updating the binary in front of you left the one doing the work
  exactly as it was. The only fix was to ssh in and run it by hand.

  One request now does the whole job on the far side: check the release,
  download, verify against `SHA256SUMS`, swap the binary, restart onto it. It
  answers before it goes down — `{current, latest, updating}`, `202` when it is
  restarting and `200` when there was nothing to do.

  Three ways to ask: `butai update --daemon` (new flag, aimed at
  `--socket`/`$BUTAI_SOCKET`), `:update` in the workbench on a tab from another
  machine, and the route itself. `--daemon` has no `--check` counterpart, and
  deliberately: a check the daemon answers and something else acts on is a
  version that can change in between.

  **Off unless the far machine says otherwise** — `[update] allow_remote`, new
  and defaulting to `false`. The socket's only access control is the `0700` on
  its directory, and over a forward or `butai proxy` the far end is whoever
  holds the ssh session; "can reach the daemon" is a much weaker claim than
  "may replace the program this machine runs". An unconfigured daemon answers
  `400` and names the key. `butai standalone` forces it off — its daemon is the
  same process as its client, with nothing to exec into.

  The restart is an ordinary `kill-server`, detach reason and all. That last
  part is load-bearing rather than tidy: clients match on
  `DETACH_SERVER_SHUTDOWN` to tell a daemon that is coming back from a pane
  that has gone, so a restart announcing itself more descriptively would blank
  the stage of every attached workbench at the exact moment it should hold it.
  There is now a test that fails if the string changes.

- **`butai-update`, a fifth crate.** The updater moved out of `butai-client`
  whole, because the daemon needs it and should not carry ratatui, crossterm,
  arboard and png to get it. It knows nothing about sockets: *stopping* the
  daemon before the swap stays with the caller, since a client does it by
  asking and a daemon does it by leaving its own event loop. `build.rs` and the
  compiled-in target triple went with it.

### Fixed

- **A daemon restart was untested from the one angle that matters.** Every
  restore test dropped its client first and restarted on a *fresh* socket, so
  nothing covered a client that was up throughout, or rebinding a socket path a
  dead daemon left behind — both of which a self-updating daemon does every
  time. `a_restart_detaches_viewers_with_the_reason_that_means_it_is_coming_back`
  covers them.

- **`butai update` from inside a butai pane destroyed the daemon and did not
  come back.** It staged the new binary, sent `kill-server`, and the daemon
  tore down every workspace — including the pane the command itself was running
  in, which killed the command between staging the binary and putting it in
  place. The rename never happened and neither did the restart: daemon gone,
  binary unchanged, and nothing left running to start one. Any other client then
  sat on "reconnecting" forever, because a client connects to a daemon and never
  spawns one.

  It now refuses, ahead of both the question and the download, and points at the
  two ways that work: `butai update --daemon`, where the daemon updates *itself*
  and execs into the new build with the pane restored around it, or the same
  command from outside butai. `--check` is unaffected — reporting what is
  available costs nobody anything, wherever it is run.

  `scripts/install.sh` has refused this since it was written, and told you to run
  `butai update` instead. That was the one path with no guard.

- **Reconnecting got slower the longer a client stayed open.** The event
  stream's backoff doubled on every drop and nothing ever lowered it, so a
  workbench open all day sat permanently at the ten-second ceiling: every daemon
  restart under it — `butai update`, `kill-server`, a daemon updating itself —
  spent the full ten seconds on "reconnecting", however healthy the daemon
  already was. It now starts over after a stream that stayed up for five
  seconds, which a flapping daemon never does, so a crash loop still backs off.
  Measured on the same restart: 1.6s to 0.5s.

## [1.1.1] - 2026-08-23

### Fixed

- **`butai update` brings the daemon back.** It stopped the daemon, swapped the
  binary in, printed `updated X -> Y` and exited — leaving the machine with no
  daemon at all until some later command happened to spawn one. The workbench
  path never had this: it execs into the new build, which re-attaches. A bare
  `butai update` has nothing to exec into, so it now starts the daemon itself
  and reports whether that worked (`daemon_restarted` in `--json`).

  The restart goes by install path, never `current_exe()`. By that point the
  rename has turned this process's own path into the deleted inode of the build
  that was just replaced, so starting it would put the *old* daemon back — the
  version skew the feature exists to end.

- **A flaky end-to-end test.** `a_shell_process_is_named_by_its_full_command`
  typed into the pane without waiting for the shell's prompt, so a prompt drawn
  between two keystrokes split the echo — CI caught `sle$ ep 41` — and the wait
  for `"sleep 41"` then timed out. Every other test here types `echo MARKER`,
  which the command prints again on a line of its own; `sleep` prints nothing,
  leaving the echo as the only occurrence.

- **Two CI checks that could not fail for the right reason.** The generated
  TypeScript check diffed `web/app/src/protocol/generated/`, gone since the
  client moved up to `web/src/`, so it reported green over any drift. The web
  relay test spawned `/var/tmp/butai-probe/butai` when `BUTAI_BIN` was unset —
  absent on a clean runner, and worse on a developer box, where it proved the
  relay against whatever binary was last left there. It now skips unless
  `BUTAI_BIN` names one, and a CI job builds the daemon and points it at that.

## [1.1.0] - 2026-08-23

### Added

- **butai updates itself.** A workbench asks once at launch when a newer
  release exists — *"butai 1.1.0 is available — you have 1.0.0"* — and **yes**
  downloads it, checks it against the release's `SHA256SUMS`, replaces the
  binary and restarts into the new build with the session intact. Until now the
  only way to move to a new release was to re-run `scripts/install.sh` by hand,
  and the usual result of doing that was a client on the new build talking to a
  daemon on the old one, because installing a binary restarts nothing. That is
  the skew `skew_notice` has been reporting in the footer all along; this is the
  thing that ends it.

  **`no` is an answer, not a dismissal.** It means *this version*, and it is
  written to `[update] declined_version`, so 1.1.0 stops asking and 1.2.0 still
  asks once of its own. `esc` answers nothing and you are asked again next
  launch. `:update` and `butai update` ignore the recorded answer — typing
  either one is what changing your mind looks like.

  **Which of the seven published tarballs this machine wants is decided at
  compile time.** `crates/butai-client/build.rs` bakes the target triple in, so
  a musl build asks for the musl tarball because it *is* the musl build.
  `scripts/install.sh` has to guess with `uname` and `ldd --version | grep musl`
  — it runs before any butai exists and has no better option — and a wrong guess
  there produces a binary that does not exec. A release with no artifact for
  this triple is reported and never approximated.

  **Nothing is offered that cannot be carried out.** Before the question is
  asked, butai checks that the directory holding the binary is writable and that
  it is not a `cargo` build in a `target/` directory. Finding out that
  `/usr/local/bin` belongs to root *after* the download and the daemon stop is
  the worst possible moment for it.

  **The daemon is stopped before the binary is replaced, not after**, and the
  order is load-bearing rather than tidy: a daemon is located through
  `std::env::current_exe()`, and on Linux that reads `".../butai (deleted)"` once
  the file underneath it has been renamed over. Stopping first, waiting for the
  socket to actually stop answering, swapping, then exec'ing an explicitly
  resolved path is the sequence with no window in it. The stop is the same
  snapshot `butai kill-server` takes — every open workspace, its agents, and
  each pane's output, written before anything is torn down — so a failure at any
  step costs a restart and no work.

- **`butai update`**, the deliberate form of that question, for a shell prompt
  rather than a workbench. `--check` reports and changes nothing, `--yes`
  installs without asking, and `--json` answers with `current`, `latest`,
  `target`, `asset`, `install_path` and `update_available`. It stops after the
  swap instead of opening a workbench: the daemon is down and the next `butai`
  starts it on the new build. See [docs/cli.md](docs/cli.md#butai-update).

- **`scripts/install.sh` finishes an upgrade.** Installing a binary restarts
  nothing, so running it over an existing butai used to leave the old daemon
  serving the old build — the skew above, arrived at by the one route that was
  *meant* to be the upgrade path. It now stops the daemon after replacing the
  binary, which keeps the workspaces and pane output like every other stop, and
  from inside a butai pane it says so and leaves it alone rather than closing
  the workbench you are reading it in. `BUTAI_NO_RESTART=1` opts out.

- **`[update]` in `config.toml`**, read by the client. `check = false` turns the
  whole thing off, as does `BUTAI_NO_UPDATE_CHECK` for a butai a package manager
  owns. The SETTINGS ▸ ABOUT group grows a `check for updates` toggle beside the
  version row, which now names the release waiting when there is one. See
  [docs/configuration.md](docs/configuration.md#update).

- **`:update`** in the command vocabulary — the way back to a prompt you
  dismissed or turned down, without leaving the workbench.

### Changed

- A confirm box's **`no` now runs through the same answering path as its `yes`**
  rather than just dropping the overlay. Every existing question still does
  nothing when answered no — they are all about destroying something, where no
  means "leave it alone" and there is nothing left to say. The update prompt is
  the first one whose no carries a decision, and it could not have been written
  without this.


## [1.0.0] - 2026-08-22

First stable release. Everything below landed since `0.12.1`; the move to
`1.0.0` puts the command set, config format, and wire protocol under
semantic versioning, so breaking changes to them now wait for `2.0.0`.

### Added

- **Links are clickable.** A URL drawn anywhere on the screen — a shell's
  output, an agent's answer, a diff, a file, a git remote — is marked up as an
  OSC 8 hyperlink as the cells are painted, so the terminal butai is drawn on
  gives it a hover and its own cmd- or ctrl-click. Until now an address on
  screen was characters and nothing else: the daemon turns a program's bytes
  into cells, and a cell is a character with a colour, so the only thing that
  could see a link was a person.

  **And a picker, because the mark-up cannot be the whole answer.** `f` — or
  `C-b f` from a focused pane — lists every URL on screen, in reading order and
  once each; `enter` opens it here, `y` copies it. Two reasons it is not a
  fallback: tmux before 3.4 drops OSC 8 entirely, and a TUI's home is an ssh
  session where there is no browser to open anything on. There, `enter` copies
  too and the picker's title says so — the copy is OSC 52, so it reaches the
  clipboard of the machine you are sitting at, which is the one with the
  browser.

  **The scan is the drawing client's, over the composed buffer**, which is why
  it covers every surface rather than only PTY panes, and why the terminal's
  mark-up and the picker cannot disagree about what is on screen — they read
  one map, built once per frame. Inside the stage its rows are joined first, so
  a URL that wraps at the pane's edge opens whole instead of truncated;
  everywhere else the client laid the text out itself and truncates rather than
  wrapping, so the rows are left alone. See
  [`docs/design.md`](docs/design.md#a-link-is-the-drawing-clients-question-not-the-daemons).

  **The browser client hit-tests the same map.** A canvas has no terminal
  underneath to hand an address to, so `<Screen>` does it: hover underlines the
  link and shows it, a click opens a tab, and a program that asked for the mouse
  keeps its clicks unless ctrl or cmd says otherwise — the rule kitty and iTerm2
  use, so the gesture is one people already have. `web/src/logic/links.ts` is a
  port of the Rust scanner and its tests are the Rust tests transcribed, because
  two clients that disagreed about what a URL is would be a bug report about
  whichever one you happen to be using.

  `http`, `https`, `file`, `ftp`, `ftps`, `ssh`, `git`, `ws`, `wss`, `mailto`
  and a bare `www.` host count; `javascript:` and `data:` deliberately do not.
  Trailing sentence punctuation and unbalanced brackets are trimmed, so `(see
  https://example.com/a).` links the address rather than the prose around it.
  [`[ui] links = false`](docs/configuration.md#ui) — a toggle on the SETTINGS
  page — stops the mark-up for a terminal that shows the sequence instead of
  acting on it; the picker still works.


## [0.12.1] - 2026-08-20

### Fixed

- **The DSK gauge was blank on macOS, and the rail had no way to say why.** The
  mount reader was written against `/proc/self/mounts`, so `read_disks` was
  Linux-only and every other platform took a stub that returned no disks at all.
  The daemon published an empty list, both clients drew exactly that, and a Mac
  looked like a machine with no filesystems rather than one nobody had taught to
  count them.

  macOS enumerates through `getfsstat(MNT_NOWAIT)` now. The *waiting* form is the
  one to avoid: it asks every filesystem to refresh its own statistics and blocks
  the sampler task while a dead SMB or NFS server takes its time, which is the
  failure the capacity sweep already runs on a deadline to prevent.

  **A Mac mounts nine filesystems to boot and has one disk.** All but `/` are
  `MNT_DONTBROWSE` — the flag Finder reads to decide what a person has — so they
  are dropped the same way, and what survives is deduplicated by APFS *container*
  rather than by volume: a volume has no size of its own, so `/` and
  `/System/Volumes/Data` are one 460 GiB disk counted twice. `/` is the row that
  wins its container, because `…/Update` standing in for the boot disk is the
  right number under a name nobody recognises. Two partitions of one USB stick
  are still two rows — they share a `/dev/diskN` prefix and no capacity.

  **Expect the gauge to disagree with `df -h /`, and to be right.** It reads 73%
  where that command says 9%: `/` is the sealed system volume, a dozen gigabytes
  by design, and the space is spent on the data volume beside it in the same
  container. `df -h /System/Volumes/Data` is the one that agrees.

  Only the enumerator is per-platform now. The cooldown for hung mounts, the
  order the sweep asks in, the `total > 0` filter and the cap are one
  implementation that both platforms reach, rather than a second copy to drift
  from — and the six helpers that were dead code on macOS are live there.

## [0.12.0] - 2026-08-19

### Added

- **The disks are on the rail.** `DSK` joins `CPU`, `RAM`, `GPU` and `NET` in
  SYSTEM, and in the web client's SYSTEM and COMPUTE columns: the mount, its
  fullness, and `used/total` in the binary units `df -h` prints. The daemon has
  published `SysDto.disks` since it learned to read the mount table and no
  client drew a single one of them, which is the whole failure mode the field
  exists for — builds, container layers, transcripts and logs all land on a disk
  nobody is watching, and the workspace stops working with no visible cause.

  **One row, and no trace.** Every other gauge is a series; a disk is a level
  with no history, so a second row would be a flat line drawn once per disk. The
  mount is the identity and is cut from the *left* when the rail is narrow —
  `/media/fast` and `/media/archive` agree on everything but their last segment,
  so `…/archive` identifies it and `/media/` would not.

  Which mounts appear is [`[ui] disks`](docs/configuration.md#ui): `all` by
  default — every real disk, largest first, capped at three — `auto` for the
  filesystem holding `/`, or a list honoured literally. tmpfs, container layers
  and network mounts are out of the automatic modes: a tmpfs is RAM the `RAM`
  gauge already counts, and a snap is 100% full by construction.

  A mount that missed the daemon's deadline keeps its last reading and is drawn
  faint rather than red. 99% full and a minute out of date is news about the
  clock, not an alarm about the disk.

### Fixed

- **The generated TypeScript had been written to a directory nothing reads.**
  `TS_RS_EXPORT_DIR` still named `web/app/src/protocol/generated` after the
  TypeScript cutover moved the client to `web/src/...`, so `cargo test -p
  butai-protocol --features ts` wrote a complete `protocol.ts` into a path git
  does not track. CI's freshness check diffs the *real* directory, found it
  unchanged every time, and went green — a file nobody writes never differs. The
  cost was `SysDto.disks` reaching the daemon and never reaching the browser,
  which is exactly the class of drift that check exists to catch.

## [0.11.0] - 2026-08-17

### Added

- **Delete a file from the FILES page.** `x` on the tree in the terminal, a
  `delete` button on the web client, and `DELETE /v1/workspaces/{id}/file?path=`
  underneath both. Until now the only thing that could remove a file was
  `changes/discard`, and only for an untracked one — a file that was committed,
  or one you had just made and staged, could be created and edited from the
  workbench but never deleted from it.

  Both clients ask first, with the path in the question and the box opening on
  "no", and both say it more firmly than the discard box does: discard is bounded
  by what git already holds, and this is bounded by nothing. There is no trash.

  The route is deliberately the narrow one. A directory is a `400` rather than a
  recursive removal, so one confirmed keystroke cannot take out `src`; a symlink
  is removed as the link rather than followed; a path that climbs out of the
  workspace with `..` is refused before anything is touched; and a file that is
  already gone answers `404` rather than reporting a success for a deletion
  something else did.

- **The REST API and the event stream compress, when asked.**
  `Accept-Encoding: gzip` gets any JSON reply over 1 KiB, and `GET /v1/events`,
  back as `content-encoding: gzip`. Send nothing and the bytes are identical to
  0.10.0, so no shipped client changes and `tests/e2e_http.rs` — which never sent
  the header — did not move.

  It is worth asking for on anything that is not a local socket. `/v1/system` is
  the largest payload served and the one a live client reads most often, and it
  compresses better than 6:1; the event stream is ~98% `system` and measured **9×
  smaller** over a 20-second window (155 KB → 17 KB). Over `ssh host butai proxy`
  that is most of what staying current costs.

  The stream is one long gzip stream flushed after every record, so nothing
  arrives later than it did before — a decoder has to be a streaming one, which
  browsers, `URLSession`, `curl --compressed` and Bun's `fetch` all are. Only
  `application/json` is compressed: `/download` serves arbitrary bytes, and
  gzipping a PNG only makes it bigger. `gzip;q=0` is honoured as the refusal it
  is, and replies carry `vary: accept-encoding` whether or not this particular
  one was big enough to compress.

- **A `redraw` button in the corner of the web client's stage.** A pane holds one
  size and the last viewer to attach, resize or type wins it, so opening the same
  pane in a second window leaves the first one drawing the program's screen in a
  corner of its own frame. The daemon sends no message when that happens — the
  protocol has none — so until now the only way back was to type into the pane,
  which is a poor answer when the pane belongs to an agent mid-turn.

  The button sends a `resize` at the size the stage already is, which drops the
  daemon's diff baseline for this client (so the next frame is a full one) and
  points the PTY back at this viewer. Both halves are existing protocol; nothing
  on the daemon changed. Clicked while the socket is down, it skips the
  reconnect backoff and re-dials instead.

### Changed

- **The web bridge fetches a daemon's workspace details all at once.** Building
  `/api/state` walked the workspaces one at a time, on the reasoning that firing
  them together "only queues them at the far end". It does — and that is where
  the queue belongs when the wire, not the daemon, is the bottleneck. Measured
  against eight workspaces the snapshot went from 13.6 ms to 8.7 ms on a local
  socket; over a forwarded socket it is N round trips collapsed into one, so a
  30 ms link costs 30 ms instead of 30 ms per workspace. Telemetry is started
  alongside them rather than after. Each request keeps its own error handling, so
  one workspace that will not answer still becomes one `detail_error` — now
  beside the others instead of in front of them.

- **`GET .../tree` takes `?filter=`, and computes the `●` markers behind it.**
  `all` is the default and is byte-identical to what this route has always
  answered, so an embedder that never sends the parameter sees no change;
  `docs` is markdown, READMEs and every directory but `target` and
  `node_modules`. Anything else is a `400` rather than a silently unfiltered
  listing. `is_doc` moved to `butai-protocol` and is one definition again — it
  had been written twice, once per client.

- **A directory listing no longer rebuilds the change set to draw its markers.**
  It used to copy every changed path into a fresh `HashSet` on the core event
  loop, then answer each entry by scanning that whole set for a prefix. With
  5,000 changed files, listing a 200-directory root measured **17.5 ms against
  0.8 ms clean**, per directory — and the web client's nested tree fetches one
  directory per expansion, so opening that tree cost ~3.5 s and 200 trips
  through the loop that drives every pane in every session.

  The set is now closed over the ancestors of every changed path and built once
  per git rescan, which turns each entry — file and directory alike — into one
  hash lookup, and leaves an `Arc` clone on the core loop.

- **The view rail is gone; the spaces are one button on the tab bar.** The rail
  down the left edge listed the six spaces and cost 14 columns of screen — bought
  from the AGENTS page's stage, which is the one page that could least afford
  them, since every agent CLI reflows against 80. It also had a second spelling:
  below 154 columns it disappeared and six buttons took its place on the tab bar,
  so the whole layout rearranged itself as a terminal was resized past a width
  nobody knew about.

  There is one control now, at every width: `[agents v]`, which names the space
  you are in and opens a menu of all of them. `alt-space` is its key, `C-b space`
  on the prefix layer, and `alt-,` / `alt-.` still walk the spaces without
  opening anything. Its ink is as wide as the space it names, but the columns are
  reserved at the widest and the ink right-aligned inside them, so the chip strip
  does not reflow as you switch space and no button is padded to make that true.

  **The counts move into the menu.** A badge on the rail was the only thing that
  outlived the page it was about, which is what paid for a rail on every screen —
  but a waiting agent already says so on its rail row, its workspace chip, the
  booth chip, the footer, BOOTH's NEEDS YOU tray and the bell, so on the signal
  that matters most the badge was the sixth copy. What is genuinely given up is
  narrower: a branch that has fallen behind, and an account limit under pressure,
  are no longer visible on the pages that draw no CHANGES rail. See
  [workbench.md](docs/workbench.md#the-spaces-button).

  Every page gains the rail's 14 columns: at 160 columns the work stage is 94
  rather than 80. Zen no longer moves two things at once — the tab bar is
  untouched, so the way out is where it always was.

- **The machine count and `[+ host]` are one button.** They sat beside each other
  and both opened the MACHINES picker, which is already where you add a machine
  and where you let one go — two widgets for one action, and the count had to be
  positioned against whichever furniture happened to be on its right, which is
  how it once landed straight through `docs`. Now the label follows the state:
  `[+ host]` on a single-machine client, where it is an offer and `1 host` would
  label a fact that needs no label, and `[N hosts]` past one, where it is the roll
  call.

- **A rule between the booth chip and the workspace chips.** BOOTH is a peer of
  the workspaces rather than one of them — every project on every machine, beside
  chips that are one project each — and on a row of look-alike chips that
  distinction was carried by nothing but a space. Dropped below 52 columns, where
  two columns of rule are two columns of project name.

### Fixed

- **`DELETE …/git/remote` removed nothing, and had never removed anything.** The
  daemon built `git remote remove -- <name>`. `git remote add` accepts that `--`;
  `git remote remove` refuses it and answers its usage text with exit 129, so the
  route reported `200` with `ok: false` and left the remote configured. The status
  code said nothing was wrong, because from the daemon's side nothing was: the
  operation genuinely ran and genuinely failed.

  The separator is gone from that one arm, and the asymmetry with `add` now has a
  comment saying it is git's rather than an oversight. Nothing is given up:
  `valid_remote` already refuses a name starting with `-`, which is the whole
  threat `--` was covering there, and an option-shaped or URL-shaped name is
  still a 400 before argv is built. Naming a remote that does not exist still
  answers `200` with `ok: false` and git's own `No such remote` — that case was
  never the bug.

  The argv assertion that locked the defect in is now the assertion that keeps it
  out, and asserts the *absence* of the token as well as the exact arguments.
  Verified against a real daemon over `/v1/*`: the route answers `ok: true` and
  the remote is gone from both `git/remotes` and the repository's config on disk.
  `testsuite`'s own case for this — which has been failing all along — asserts
  exactly that, and runs in the container.

- **A deleted file marked the directory it left.** The tree lists what is on
  disk, so a deleted file can never be a row in it — but it was still in the set
  the markers are built from, so its parents lit up and following that `●` down
  arrived at a directory with nothing marked in it. The change is real and the
  CHANGES rail shows it correctly; the tree is simply not where it lives.

  Only the worktree side counts now. A worktree deletion means gone whatever the
  index says — which also covers staged content whose file was then deleted,
  where the staged row would otherwise have marked a file that is not there — and
  a staged deletion means gone only when nothing is left on disk, so `git rm
  --cached` keeps its marker.

- **A filename that is not valid UTF-8 was dropped in silence.** The status scan
  read each path with git2's `path()`, which answers `None` for such a name, and
  skipped the entry with no log line and no row. The file was missing from the
  CHANGES rail *and* from every marker in the tree, so `git status` and the
  workbench disagreed about how many changes there were and nothing said why —
  butai's own tree had one, and reported 24 changes against git's 25.

  The path is read as bytes now and kept as one. The name still reaches a client
  lossily, because every path on the wire is a JSON string: it draws with
  replacement characters and staging it by path will not match. That is a smaller
  problem than a change the workbench cannot see at all, and carrying such a name
  faithfully needs an encoding change to the protocol.

- **The DOCS rail's amber `●` led to an empty directory.** A directory's marker
  means "something under here changed", and the daemon decided it across the
  whole git change set — then each client filtered the *rows* afterwards. On
  DOCS, which keeps only markdown and READMEs, a folder holding nothing but
  changed code therefore kept a marker earned by a file the page had just
  dropped. Following one down was four keystrokes to an empty box, every time,
  and in every client at once: the terminal and the web page shared the bug
  rather than either causing it. Seven directories in butai's own tree did this.

  The filter is the daemon's now — `?filter=docs` on `GET .../tree` — so the rows
  and their markers are one decision. A folder of changed code is still listed
  on DOCS, because the writing is inside those folders, but it carries no marker
  it cannot honour.

  The rule this trades away is real, and was deliberate: one route, and each
  client decided what its pages showed. It does not survive contact with a
  marker computed on the other side of the wire.

- **USAGE drew a stale `session 0%` on an account that was busy.** `claude`
  caches its limits in `~/.claude.json` and refreshes them only when it runs and
  decides to fetch — twenty hours between refreshes was observed, with the CLI
  running the whole time. The page trusted that cache unconditionally and spent
  its freshness on prose, so a snapshot taken before a five-hour window rolled
  over was drawn as a confident `0%`, with a bar, `metered` state and the
  `published` badge, while the transcripts held millions of tokens in that same
  window. The one guard there was checked `resets_ms`, which is `null` on an idle
  window and so could not fire on the case that mattered — and where it did fire
  it substituted a `0` of its own.

  A published window is now used only while the snapshot is younger than the
  window it names — five hours for `session`, seven days for the weekly rows — so
  one cache can stay authoritative for `week · all models` and be discarded for
  `session` at the same instant. Whatever it can no longer speak for is counted
  from this machine's transcripts instead, which means one CLI's `windows` may
  mix `percent` and `tokens`; the `note` says so. A window whose reset has passed
  is dropped rather than zeroed. See
  [protocol.md](docs/protocol.md#account-standing).


## [0.10.0] - 2026-08-12

### Added

- **A NET gauge per interface, and the hardware's name beside every reading.**
  The daemon has always published every interface with its kind, carrier and
  default-route flag; the rail threw all but one of them away. `[ui] net` now
  chooses — `"all"` by default (every link that is up and not double-counted,
  capped at three), `"auto"` for the old single pick, or an explicit list, which
  is honoured literally and in order because naming `docker0` is a decision and
  not a mistake to correct.

  Each gauge's head row grew a middle slot for what the thing *is*: the CPU model
  and thread count, the GPU model, the interface name and its link speed, and
  swap once any is in use. It elides whole words and is the first thing dropped
  on a narrow rail, since the label says which gauge this is and the value is the
  reading. The GPU's model, temperature and board power were already being
  sampled every two seconds and thrown away before the wire — `GpuDto` simply had
  nowhere to put them — so those cost no new collection. `cpu_model`,
  `cpu_cores`, `cpu_threads`, `swap_*`, and each interface's `speed_mbps` and
  `driver` are new, additive and `#[serde(default)]`.

  The DDR type and speed that "which RAM is it" really asks for are deliberately
  absent: they live in DMI, which is `0400 root:root`, and the daemon should not
  run as root to label a gauge.

### Changed

- **The CHANGES rail says which branch you are on.** Its title read
  `CHANGES (6) ↑2↓1` — how far you had diverged, but never from what. The branch
  was on screen in the footer only, sharing one line with the workspace name,
  the path, the waiting agent and four buttons: the first thing cut on a narrow
  terminal, and gone outright in layout mode. It now reads
  `CHANGES (6) · main ↑2↓1`, and `CHANGES (6) · main · REBASING` mid-sequence,
  which is where the browser client has always carried it.

  The branch is the one part of that title with no bound, so on a narrow rail it
  is the part that gives way: cut to what the counts and the arrows leave, and
  dropped rather than shown as a stub. The arrows are the half that says whether
  you can push, so they are never the half that is cut.

### Fixed

- **The NET gauge drew a line when nothing was happening, and hid incoming
  traffic when something was.** Its level function ended in `.clamp(1, 2)`, so
  each direction always lit a dot row: 230 B/s of ssh keepalives and mDNS painted
  the same solid two-row axis a saturated gigabit link did, and so did an
  unplugged cable. Traffic under 4 KiB/s now draws as an empty row — throughput
  has a real zero, unlike a CPU, and a floor in bytes is the only kind silence
  can actually reach.

  Incoming traffic was never dropped, as it appeared to be; it was pinned to that
  same baseline. Both directions shared one autoscale peak and the level function
  had two steps, so anything under about a quarter of the peak rounded to zero
  and was clamped back to one dot. A 59 kB/s download running under a 572 kB/s
  upload sits at 10.2% of the shared peak and was therefore drawn as silence.
  Each direction now gets its own trace row: four levels instead of two, its own
  colour, and its own arrow so the pair still reads on a monochrome terminal.
  That makes the gauge a row taller than the others, so hit testing walks the
  gauge list rather than dividing the row by a constant, and the renderer reports
  the rows it used rather than letting BOOTH recompute them.

- **`accent` and `info` were the same colour in both blueprint palettes.** It
  cost nothing until the NET gauge started using the pair to mean *direction*, at
  which point `↓` and `↑` came out identical. The dark palette takes tokyonight's
  cyan and the light one a darker cyan that holds up on a near-white ground; a
  test now keeps the two apart in every built-in palette.

## [0.9.0] - 2026-08-12

### Added

- **The browser client's TypeScript is generated from the DTOs, not written
  beside them.** `butai-protocol` grows an optional `ts` feature;
  `cargo test -p butai-protocol --features ts` emits all 77 wire types — every
  REST DTO and the whole framed message set — into one
  `web/app/src/protocol/generated/protocol.ts`, with the Rust doc comments
  carried through as JSDoc.

  Until now a client's types were hand-written and could only be checked by
  asserting on them after the fact, which is what a good deal of `web/check.py`
  was doing. A field added to a DTO now fails CI at the line rather than
  surfacing as a client that quietly ignores it.

  Two details it gets right that hand-writing tends not to. `u64` fields are
  `number`, not ts-rs's default `bigint`: they arrive through `JSON.parse`, so a
  `bigint` binding would describe a value that never exists, and none of them is
  near 2^53. And a field is optional in TypeScript exactly when serde may omit
  it — `#[serde(default)]` alone still always serializes, so only the cell-run
  fields carrying `skip_serializing_if` become `?`.

  The feature is **off by default**, because a crate on crates.io should not
  make every downstream build compile a code generator. Nothing here changes a
  byte on the wire: ts-rs reads the same `serde` attributes serde does.

### Added

- **Help is a screen of its own.** `?` and `[help]` used to open the DOCS space,
  point its rail at a `butai://reference` folder and put a topic in the file
  viewer — so a press on help rearranged the *file* screen around a listing that
  was not files, with a breadcrumb, a `..` row, a find button and an editor that
  had to refuse to save. It is now a page on SETTINGS's terms: a contents column
  down the left, the topic beside it, and `esc` (or the button again) back to
  whatever you were doing, which is left exactly as you left it.

  `j`/`k` scroll a line, `space` and the page keys a screen, `home`/`end` the
  ends, `tab` walks the topics, and the page says `more below` while there is —
  the thing the modal it descends from never did. The reference is compiled in,
  so the page opens with no daemon in the loop and reads the same over ssh.

  DOCS goes back to being a project's own markdown and nothing else: the
  reference folder is gone from its rail, and with it the built-in-file
  machinery the file widget carried to support it. The browser client still
  shows the reference inside DOCS — see the note in [`docs/keys.md`](docs/keys.md).

- **An agent row now knows whether you have read it.** `AgentState` said what an
  agent *was*; nothing said whether you already knew. `finished` holds until the
  agent works again, so a turn that landed while you were away and one you read
  an hour ago were the same word in the same colour — and the longer a workbench
  stayed open the less the rail meant.

  `AgentDto.unread` is the missing bit. The daemon sets it on the edges into
  `finished` and `exited`, and clears it wherever a client looks at the pane —
  staging, watching, streaming, sending input, or `POST .../panes/{pane}/ack`.
  Those four call sites already existed to clear the bell and are now one method
  (`look_at_pane`), because "the user looked" is one event and clearing half of
  it in three of four places is how they drift.

  It is deliberately not set for `waiting`: an unanswered question is urgent
  however many times you have read it, and a read flag beside it would only
  invite a client to quieten the one state that must not be quietened. It is
  also not set by the baseline pass that seeds a fresh daemon, for the same
  reason the notification feed stays silent there — attaching to a workbench of
  long-finished agents must not light every one of them up as new.

  In the rails an unread turn keeps its colour and takes a `•`; a read one drops
  to dim. `WorkspaceSummary.unread` counts them, so a tab badge can say how much
  news a project is holding without fetching its detail.

- **BOOTH's tray is now "what needs me", not "what is waiting".** It already
  collected blocked agents above the fleet list; it now also collects unread
  finished turns and unread exits, ranked — blocked first, then a crash you have
  not seen, then turns in fleet order. The tray draws four rows and does not
  scroll, so that ranking is what decides whether a blocked agent is visible at
  all when three turns land at once.

  The fleet list below is untouched, and deliberately: sorting *it* by urgency
  was measured and rejected (~174 row moves per ten sampler ticks at 24 agents),
  which is why the tray copies rows upward instead of moving them.

- **BOOTH can now end a session, and its tray answers the pointer.** The page
  that exists to tell you what every agent on every machine is doing was the one
  page where you could not act on the answer: reaching an agent meant `[open]` to
  its project on its machine, `x` on the rail there, then back. `x` on the fleet
  now ends the row the cursor is on, and `m` or the right button opens that row's
  menu — the AGENTS rail's own three rows (`Close agent`, `Close others`, `Close
  all agents`), so there is one menu and two ways to reach it rather than a
  second one that would drift.

  They act on the row's **own** machine and project. That is the whole difficulty
  of this page: every other cursor in the workbench sits inside the workspace the
  tab bar names, so a pane id was address enough — the fleet's rows cross
  daemons, and a pane id is only unique within one. `AllAgentRow` now carries its
  workspace's id and `MenuTarget::Agent` carries the machine, the workspace and
  the pane, so nothing on this path can resolve through "the tab you happen to be
  looking at" and end an agent at home that shares an id with the one on
  `gpu-box`.

  Neither asks first, for the same reason the rail's `x` does not: an agent is a
  process whose transcript is on disk.

  The NEEDS YOU tray's rows are clickable too. They were the only list on the
  page the pointer could not reach — a press fell through to nothing while the
  identical row six lines down worked — and since a tray row *is* a copy of a
  fleet row, clicking one puts the cursor on the agent it stands for, which is
  what clicking the original does. The browser client has done this since the
  page shipped.

- **The GIT page shows the working tree, and stages it.** Its `working tree · N
  changed` row used to be a signpost — `enter` on it sent you to the CHANGES
  rail. The changed files are now listed under it, under the rail's own
  `Unstaged` / `Staged` / `Conflicts` headings, answering the rail's own
  letters: `s` stage, `u` unstage, `x` discard, `o`/`t`/`a` resolve.

  `enter` on a file opens its diff in the body *beside* the lists rather than
  taking over the DIFF space, so you keep the refs and the history you opened it
  from — and `space`, `v` and `x` work there, which is the half a whole-file `s`
  cannot do: a hunk, or a run of picked lines.

  This reverses the page's original rule that nothing on it staged anything.
  That rule bought no-duplicate-of-the-rail, and the duplication is avoided a
  better way: the rows *are* the rail's `ChangeRow`s and the keys resolve to the
  same `VerbId`s and `GitAction`s, so there is one implementation of "stage this
  file" and two places to reach it. The rail is untouched, keeps every verb it
  had, and still owns the commit box and the sync buttons — which is what the
  summary row's `C changes` goes to.

- **A diff reads like a review.** The widget every diff goes through — the DIFF
  space, the GIT page's body, a commit, a stash — drew the patch as text. It now
  draws one card per file (its path, whether it was added, deleted or renamed,
  and its `+n -m`) over the lines, each numbered on the side it exists in: a
  removed line has no number on the new side because it is not in the new file,
  and an added one has none on the old. Git's four header lines per file are
  gone; on a twenty-file working tree that was eighty rows of restated path and
  blob hashes.

  `z` folds the file under the cursor, `Z` folds them all. Stepping into a
  folded file opens it, so the cursor can never sit on a hunk you cannot see
  with `space` about to stage it.

  The numbers are the first thing a narrow body gives up, on the terms the
  commit graph gives up its lanes — the GIT page's body is the narrow case.
  `crate::selection` asks the view how wide the gutter came out rather than
  subtracting a constant, so dragging a selection out of a diff still yields a
  patch and not code with a column of numbers welded to the front.

- **A USAGE space (`alt-u`), answering which agent account stops you first.**
  Every configured CLI on one screen: whether it is installed, the account and
  plan it is signed in on, and how much of each limit it has burned. Limits, not
  spend — the question is whether the account you are about to start a long job
  on has room. Served by the daemon at **`GET /v1/usage`**, so every client gets
  the same surface.

  For `claude` the numbers are **the provider's own**. It renders its `/usage`
  screen from `cachedUsageUtilization` in `~/.claude.json` — a percentage per
  window and the instant each one resets, refreshed whenever it runs — so the
  page shows the real session and weekly limits rather than a total with no
  denominator: `session  ▇▇▇▇▁▁▁▁  42%   resets in 2h 15m`. A window whose reset
  has already passed reads as empty, because that window emptied and the cached
  percentage describes one that no longer exists.

  What it still refuses to do is **invent a ceiling**. No CLI reports its limits
  through a subcommand, and asking a provider directly would mean authenticating
  as the user — so a CLI that publishes nothing keeps a total and no
  denominator, and `source` on the wire always says which of the two a window
  is. Declare a `[[budgets]]` number and those windows gain a bar as well,
  measured against yours and labelled as such.

  Every configured CLI was surveyed rather than assumed, and the three that are
  installable land in three states for three different reasons. **`gemini` is
  now counted**: it publishes no ceiling, but each assistant turn in
  `~/.gemini/tmp/*/chats/*.json` carries its token counts, and its account comes
  from `google_accounts.json` — so it moves from `unknown` to real five-hour and
  weekly totals, with replayed context subtracted the way claude's cache reads
  are. **`agy` stays `unknown` and now says why**: it *has* a quota and never
  writes it down, pulling it into an in-memory cache on each run, so there is
  neither a limit to read nor a per-turn cost to total. `aider` remains
  `no_account`. That gemini and agy differ is the reason the page keeps
  `unknown` and `no_account` apart — "a quota exists and is unreadable" and
  "there is no quota" look identical on a screen that only draws numbers.

  Counting handles both transcript layouts: claude appends JSONL and is read
  from a byte offset, gemini rewrites a session whole and is re-opened only when
  its mtime moves, deduplicated by message id.

  Five states, because "no numbers" has five meanings: `metered` (a ceiling
  exists — `source` says whose), `counted` (real totals, nothing published),
  `unknown` (installed, and butai cannot read its usage), `no_account` (your own
  API key — nothing to meter), `absent` (no binary where a pane would find one).
  Collapsing `unknown` into `no_account` would tell someone their subscription
  CLI has no limits, which is the one wrong answer this page can give.

  **`absent` is decided by the pane spawner's own resolution**, not by `PATH`.
  The daemon's inherited environment is rarely a login shell's — a daemon whose
  `PATH` is `~/.local/bin:/usr/bin:/bin` while `claude` and `gemini` live under
  `~/.nvm/versions/node/*/bin` is the ordinary case — and every pane launches
  them anyway, because spawning has always fallen back to the directories a
  login shell adds. A page that answered `PATH` alone reported two working
  installs as missing. The `--version` probe runs with a pane's repaired `PATH`
  for the same reason: an npm-installed CLI is a `#!/usr/bin/env node` launcher,
  and the inherited `node` is routinely too old to parse it.

  The account and plan come from `~/.claude.json`, which the CLI already wrote
  in plain text. **No credential store is opened** — authenticating to a
  provider as the user is a decision they have not made.

  The view rail's badge shows the tightest declared window, so the number
  follows you onto the pages that are about the workspace. It appears only when
  a budget exists: without a ceiling there is no threshold to have crossed. On a
  Mac use `{prefix} u` — Option-u is the diaeresis dead key.

- **Everything in the workbench has a keyboard shortcut**, and it is a test
  rather than a promise. `every_click_target_has_a_key` is a `match` over the
  hit-test's own target type with no catch-all, so a new clickable thing does
  not compile until someone has said which key reaches it. Three things were
  reachable by pointer alone and now are not:

  - **The context menu.** `m` opens it on the row the cursor is on, and on the
    workspace itself when the cursor is not on a rail. It was the right button's
    alone, and it is the only place "close others" and "close all agents" live —
    a mouseless client could not reach either. (It also carries a remote tab's
    "disconnect host", which `alt-h` now reaches as well.)
  - **The SYSTEM gauges.** `{prefix} S` stages `htop` and `{prefix} Y` a GPU
    monitor, which is what clicking a gauge has always done. The gauges are not
    a list the cursor can walk, so they had no key at all.
  - **The GIT space.** `alt-r` reached it, but the prefix layer had nothing and
    `space git` was not a phrase the command language knew, so it could be
    neither rebound nor reached from `:`. Now `{prefix} r` and `:space git`.

  `monitor [gpu]` joins the mini-language that `[keys]`, the `:` prompt and the
  palette share, so the new keys are rebindable like every other one.

  [`docs/keys.md`](docs/keys.md) is the whole list in one place, with the rule
  it follows; the in-app reference (`?`) carries the same material split by
  subject.

- **`alt-h` says which machines you are connected to, and disconnects them.**
  The box it opens listed only machines you could *add*: the ones already in the
  tab bar were filtered out of it, so nothing anywhere in the client answered
  "which machines am I holding open?" — and the one way to drop a link was a
  right-click on a tab that machine happened to own, a menu you can only find if
  you already know the link is there. Connected machines are now the first rows,
  marked `*`, and Enter on one drops it. A machine whose ssh is still coming up
  says `connecting…` rather than looking like one nobody asked for, and one
  reached through a `[[remote]] socket` forward you set up yourself says so
  instead of offering a disconnect it cannot perform.

  Dropping a link now really removes the machine. It killed the ssh before, but
  the client went on holding that daemon's last known state: its workspaces
  stayed in the tab bar looking live, reconnecting was refused as "already in
  the tab bar", and its event-stream task retried a socket that no longer
  existed until the client quit. Nothing on the far side is touched — the daemon
  there keeps running with every pane it had — so reconnecting is one keystroke
  and the box does not ask first.

- **The folder picker makes folders.** `alt-n` browses for somewhere to open a
  workspace, and a project that does not exist yet was the one case it could
  not answer: you left for a shell, ran `mkdir`, and came back. `[new folder]`
  now sits beside `[open this folder]` — it asks for a name, creates the folder
  on whichever machine the picker is pointed at, and steps into it with
  `[open this folder]` already under the cursor, so making a project and opening
  it is one gesture.

  No new surface: `POST /v1/fs/mkdir` has been in the daemon and in the web
  client's picker all along, and the TUI is the client that never called it.

- **Four more built-in themes: `catppuccin-mocha`, `gruvbox-dark`, `nord` and
  `solarized-light`.** Three of them shipped as files under `examples/themes/`,
  which meant trying one cost a `cp` and a config edit — so almost nobody did.
  Eight palettes are now selectable by name, and the SETTINGS page walks them
  and applies each one as the cursor passes it, which only works for names that
  already resolve. `examples/themes/` keeps a file per built-in, each written
  out in full, as the thing to copy when you want a built-in with two values
  changed.

- **Antigravity is a built-in agent.** Google's agent CLI — the announced
  successor to Gemini CLI — ships as `agy`, so it is `agy` in the picker too,
  launched with `--dangerously-skip-permissions` like every other built-in's
  auto-approve flag.

  It gets no `resume_args`, for the reason codex gets none: `agy --conversation
  <id>` reopens a conversation, but nothing names one at *launch*, so butai has
  no id to hand back at restore. The flag that looks like the answer is
  `--continue`, and it is the bug — it means "the most recent conversation in
  this directory", which is one transcript for every agy pane in the workspace.
  A test now holds that line for the whole built-in table.

  Its first screen is a folder-trust dialog, so a fresh agy row is parked on a
  question before it has done anything — which is what turned up the band bug
  under Fixed below. One rule reaches that dialog once the band can see it: the
  highlighted `> Yes, I trust this folder`. Its hint line reads `enter Confirm`,
  near enough to Claude Code's `Enter to confirm` to look covered by the prompt
  markers and not near enough to match one.

### Changed

- **HOME is now BOOTH.** The page that spans every machine was named for a
  position, and for the wrong one: `agents` is the page a client starts on,
  nothing falls back to this one, and the only ways in are `alt-0`, its chip and
  `alt-w`. A word meaning "where you land" sat on the one page you never land
  on. It is now named for what you do there, the way `agents`, `files`, `docker`
  and `docs` are — a control booth is the room at the back of the house a show
  is run from: you watch the whole stage from it, the standby board says which
  department is holding, and you can key into any one channel without being in
  the scene. That is this page's three columns exactly — FLEET, the NEEDS YOU
  tray, and a live pane in the middle you can type into.

  The keys do not move: `alt-0`, the chip, and `alt-w` for the fleet list.
  `:space booth` is the phrase, and **`:space home` still parses** — it is in
  people's keymaps, and a config that stops loading is a worse greeting than a
  retired word.

- **The project is now called `butai`.** `bmux` is published on crates.io by an
  unrelated Rust terminal multiplexer, which blocked `cargo publish` and
  `cargo install` outright — the name had to move before there could be a
  release at all. `butai` is Japanese 舞台, "stage", which is the word
  `docs/design.md` already uses for the centre pane the whole workbench is built
  around.

  **This is a hard break, with no fallback to the old spellings.** Nothing reads
  `~/.bmux/`, `.bmux.toml` or `BMUX_*` any more:

  | was | is |
  | --- | --- |
  | `bmux` binary | `butai` |
  | `~/.bmux/`, `~/.bmux/bmux.sock` | `~/.butai/`, `~/.butai/butai.sock` |
  | `.bmux.toml` in a project root | `.butai.toml` |
  | `BMUX_PANE`, `BMUX_WORKSPACE`, `BMUX_SOCKET`, … | `BUTAI_*` |
  | crates `bmux`, `bmux-protocol`, `bmux-server`, `bmux-client` | `butai`, `butai-protocol`, … |
  | `<bmux-stage>`, `<bmux-screen>`, … | `<butai-stage>`, `<butai-screen>`, … |

  Kill a running daemon by its **old** socket before starting the new binary —
  it holds `~/.bmux/` open and the new one looks in `~/.butai/`:
  `bmux --socket ~/.bmux/bmux.sock kill-server`. Copy `~/.bmux/config.toml` and
  `~/.bmux/themes/` across by hand; the session in `~/.bmux/session.json` is not
  migrated, so reopen the workspaces once.

  The wire protocol is unchanged in shape, but two identifiers in it moved: the
  ssh handoff announce is now `ESC _ butai ; …`, and the daemon's own reference
  scheme is `butai://`. Entries below this one describe releases that really were
  called `bmux` and are left as they were.

### Fixed

- **A dropped connection no longer looks like every agent dying at once.** When
  the daemon went down — or a forwarded socket did — the stage cleared to a black
  rectangle, the tab bar went on showing live-looking chips, and the only word
  about any of it was a footer flash that scrolled past. The screen said "there
  is nothing here", which is the one thing that is never true: the pane is on the
  far machine, still running, and `kill-server` restores every workspace on the
  next start.

  The last frame now stays, dimmed to one faint colour, under a card naming the
  machine, counting how long it has been away, and saying that what is behind it
  is the last frame rather than what is happening now. Every chip for that
  machine takes a `·` in a column its padding already reserved — so nothing on
  the busiest row moves when a laptop closes — and stops being painted as urgent,
  because the `!` counts behind it were taken when the link died. BOOTH's compute
  column says `away` where the agent count goes; its gauges were animating from
  the last telemetry the machine sent, which is a strong claim to be alive.

  Two bugs fell out of the same place. `ServerMsg::Detached` carries a *reason*
  and the client was matching `Detached { .. }`, so "the daemon is shutting down"
  and "this pane closed" — which call for opposite screens — were one line;
  `DETACH_SERVER_SHUTDOWN` is now a named constant on both sides. And the stage's
  reconnect rode the repaint, which is every 120ms while anything animates, with
  each failure writing its own `stage: …` into the footer: a machine that was
  simply off turned the one line that could have explained itself into a strobe.
  It is a one-second clock now, and a failure is silent because the card already
  says more than the flash did.

- **Scrolling a pane back more than one screen no longer kills the daemon.**
  vt100 0.15 composed a scrolled-back view as *`offset` scrollback rows +
  `rows - offset` live rows* and clamped `offset` only against the depth of the
  scrollback, never against the height of the pane. Both are `usize`, so an
  offset past one screen underflowed that subtraction and panicked the thread
  holding the PTY — a few wheel notches in a quiet pane was enough. Nothing
  clamped it on the way in either, and vt100 *increments* the offset itself for
  every line that scrolls off while the view is parked, so a pane left scrolled
  back walked into the panic on its own with no one touching it.

  It was hidden for as long as it was because new output used to snap the view
  back to live on every frame, which kept the offset near zero and made the
  scrollback almost unusable in the same motion — the bug and the thing masking
  it were one behaviour. Keeping a parked view (above) is what walks straight
  into it.

  Fixed upstream in vt100 0.16, which bounds the take and saturates the
  subtraction. Taking it meant moving off `Screen::title` and
  `Screen::audible_bell_count`, which 0.16 replaces with a `Callbacks` object —
  butai now holds the last OSC 0/2 title and a monotonic ring count itself —
  and moving `set_size`/`set_scrollback` from `Parser` to `Screen`.

  The version chain that had pinned the tree to 0.15 came out with it: vt100
  0.16 wants `unicode-width >= 0.2.1`, ratatui 0.29 pins it to exactly `0.2.0`,
  and `tui-textarea` 0.7 — the last release under that name — pins ratatui
  0.29. So **ratatui is now 0.30** and **the editor widget is
  `tui-textarea-2`**, the maintained continuation of the same crate, taken under
  the old name so nothing that imports it changed. ratatui is now taken with
  `default-features = false`: butai uses it for the cell grid and its geometry
  and drives the terminal through crossterm directly, so its backends were only
  ever pulling a second crossterm in beside ours.

- **The tab bar scrolls its chips instead of pushing everything off the row.**
  Past about eight open projects the chips ran out of bar, and the row gave way
  in the worst possible order: `[+ new]`, `[+ host]` and the machine count were
  dropped one at a time as the chips reached them, so the client with the most
  projects was the one with no button left to open another — and the chips it
  spent those columns on ran past the right edge, where no pointer can reach
  them.

  The reservation now runs the other way. The machine count, the space buttons
  where the bar carries them, and both `[+ host]` and `[+ new]` get their columns
  first; the chips scroll inside what is left. The strip follows the workspace
  you are in and moves only as far as it must to show it whole — the same rule
  the rails scroll by — and `[<]` / `[>]` at its right end reach the workspaces
  it is not showing, each one selecting the nearest workspace off that edge.
  They are the pointer's spelling of `alt-<` / `alt->`, and each is drawn only
  when there is something that way.

  On a bar too narrow to keep both — roughly under 52 columns — the buttons are
  dropped and the chips take the whole row as they did before, because there the
  tabs are the only thing left worth drawing.

- **A workspace chip on BOOTH is clickable where it is painted.** The bar sizes
  the active chip wider than the rest, for the brackets and the `[x]` it carries
  — but on BOOTH no chip is the active one, and the strip went on reserving the
  wider label anyway. Every chip right of it was hit-tested four columns from
  where it was drawn, so clicking a project from BOOTH could open its neighbour.
  The width, the label and the colour now come from one answer to "which chip is
  the one you are on", which on BOOTH is *none of them*.

- **The rails scroll.** AGENTS, PROCESSES and CHANGES each drew as many rows as
  the section was tall and stopped there, so on a busy workspace the agents past
  the fold were not on screen and there was no way to bring them there. The
  cursor went to them regardless — `j` walks the whole list — which is what made
  it look broken rather than full: the highlight vanished, the rows did not
  move, and the keys appeared to stop working. Every one of them now scrolls to
  keep the cursor in view, by the same "only as far as it has to" rule the files
  and git lists have always used, so the rows around the cursor stay still
  instead of recentring on every keypress. The wheel and `j`/`k` both take you
  there, being the same walk.

  Clicking a scrolled rail selects the row under the pointer. The scroll is
  derived where it is needed from the cursor and the list's length rather than
  stored, so the drawing and the hit test cannot come to disagree about where a
  list starts — which is how a click comes to select a different agent than the
  one it landed on.

- **The cursor is back on the stage.** A terminal pane had no caret in it: no
  block to type against in a shell, no way to see that an agent was parked on a
  prompt waiting for an answer, and nothing at all to say where the next
  character would land. The position was never missing from the wire — the
  daemon sends it with every frame, because it holds the PTY and the escape
  sequences that move a cursor are consumed there and never reach your terminal
  — and it went on arriving the whole time. The client that took over the
  drawing kept the cells and dropped the position, so a field that existed and
  was correct was read into a variable nobody used.

  It is placed where the pane was blitted, so it tracks the program cell for
  cell: type ten characters and it moves ten columns, backspace and it comes
  back, and a bare cursor move — a left arrow at a shell prompt, which rewrites
  no cell at all — moves it too. It is your terminal's own cursor, in the shape
  and blink you configured, rather than a drawn-on block: the daemon cannot
  report a shape (the emulator does not track `DECSCUSR`), and forcing a block
  in the name of a value nothing measured would be worse than leaving your own
  alone.

  With the keyboard on a rail it stays visible as a **steady underline** — the
  terminal's nearest thing to the hollow cursor the web client has always drawn
  for this, and for the same reason: the pane is live and where its cursor sits
  is still worth knowing, but a solid blinking caret that ignores you is the
  worst of both. A modal takes it off the pane entirely, since the modal draws
  its own and two carets on one screen is one too many.

- **A single container on the DOCKER page shows its status dot.** The page does
  not list a one-container stack's container underneath it — the header *is* the
  container, and listing it twice would make every standalone container two
  identical rows. But only container rows drew the `●`/`○`, so on a machine of
  standalone containers the page was a column of bare labels whose state you
  could only read off the `up` at the right-hand edge, and the one row that
  *was* a container looked like the one kind of row that is not. It now wears
  the same dot a container row does, which is what the web client has drawn
  there all along.

  A compose project with rows under it gets `▾` in the same two cells, so both
  kinds of header start their label in the same column and a container listed
  beneath its project still sits one further in.

- **A disconnected machine stays disconnected.** Connecting one from `[+ host]`
  writes a `[[remote]]` block so it comes back tomorrow, and nothing ever took
  one out again: disconnecting dropped the link for as long as the client was
  running, and the next attach dialled the machine straight back into the tab
  bar. It read as the disconnect having quietly undone itself, which — a detach
  and an attach later — is exactly what had happened.

  Disconnecting now forgets the block as well as the link, and says so
  (`gpu-box disconnected — forgotten`). The block is matched by the badge the
  tabs carry, which is the only name a disconnect has: a block renamed with
  `name = "gpu"` is found by `gpu`, not only by its ssh destination. A machine
  that announced itself from inside a pane was never written down, so there is
  nothing to forget and the file is not touched at all — and a `[[remote]]
  socket` block is somebody else's forward, which the client already refuses to
  disconnect and now never forgets.

- **A question on a screen that has not filled up yet is seen.** Agent state is
  read from a band at the bottom of the pane's visible grid, which assumes the
  agent's chrome is at the bottom — true of every screen that has scrolled at
  least once, and false of the first one. A CLI that opens with a dialog paints
  it at the top with blank rows underneath, so the band read eight empty rows
  and the rail said `idle` while the agent sat waiting for an answer. `agy` does
  this on every first run (its folder-trust question), and it is the shape of
  any first-frame prompt: a login, a "resume or start fresh?", a model picker.

  The *question* scan now measures its band up from the last written row rather
  than the bottom of the grid; on a screen that has filled up the two are the
  same rows. The busy scan deliberately keeps the grid, because the two mistakes
  are not the same size: a spinner phrase that appears in an agent's own prose
  would pin the pane to `working` forever and swallow the finished notification
  with it, while a question noticed early rings once and you look at the pane.
  `butai pane read <id> --source footer` follows the same rule, so it still
  answers "what did the detector see?" rather than showing rows it never read.

- **A CHANGES row scrolls its path and keeps its status code.** The rail drew the
  row as one string — `M some/very/long/path.rs` — and handed the whole thing to
  the marquee, so a path too long for the rail took the `M` with it and a
  scrolling row stopped saying whether the file was modified, added or untracked.
  The code is now pinned between the cursor marker and the name, and only the path
  travels; it is the one part that is too long to fit. Rows without such a token —
  agents, processes — are unchanged, and their sprites and status tokens were
  never in the marquee to begin with.

- **`d diff` on the CHANGES rail does something.** It has been in the verb table
  since the table existed — drawn in the footer on four kinds of row, printed by
  `?`, and clickable — and bound to nothing: the click path resolves the word to
  a key and hands it to the rail's key handler, which had no arm for it. `enter`
  opened the diff and only `enter` did. That is exactly the failure the table
  was built to prevent, so `d` is bound rather than deleted, and a test now
  walks every row kind and every verb its footer would draw. Found while
  re-shooting the diff screenshot, which pressed `d` and photographed a shell.

- **HOME's rows scroll their text like every other row does.** The rails have
  marqueed long names since they existed; HOME cut them with an ellipsis
  instead, so a title longer than the FLEET column was one you could never
  finish reading. At 160 columns that column is 21 cells wide, minus the sprite
  and the `[open]` button — narrow enough that ordinary agent titles hit it. The
  fleet list, the NEEDS YOU tray, the machine and workspace headers and the
  COMPUTE column's machine names now all use the same `marquee` the rails do, on
  the same clock, so the whole workbench scrolls its text one way. Box titles
  still ellipsize: they are not rows and a moving frame is not a fix.

- **A SETTINGS row no longer stretches across a wide terminal.** Values are set
  hard right, so now that the body takes every column the group list does not, a
  row on a 170-column terminal read `auto-attach  [general] remote_auto_attach`
  and then ninety-odd columns of nothing before `on`. Rows are drawn to the
  width their three columns need and the rest of the body is margin.

- **The footer says which machine its path is on.** Its left zone read
  `proj /home/me/proj (main)`, which was unambiguous while a client talked to one
  daemon and stopped being so the moment a second machine could appear in the tab
  bar — `/home/me/proj` exists on all of them. The tab chip already qualifies
  itself as `host:name` when more than one daemon is connected; the path now does
  the same as `host:/path`, scp's spelling of "that path, on that machine". A
  single-daemon client is unchanged. This matters most on FILES, DOCKER and DOCS,
  where the footer is the only chrome still naming the workspace.

## [0.8.0] - 2026-08-09

### Added

- **A HOME page across every connected machine.** The one surface that spans
  daemons: every workspace and every agent on every machine you are attached to,
  in one list. The rest of the pages are *about a workspace* and resolve through
  one daemon, which is why they stay scoped to a machine and this cannot — a file
  tree merged across four hosts is a tree where two `src/main.rs` rows are
  different files. It sits beside the workspace chips in the tab bar rather than
  in the view rail: every entry in that rail is a way of looking at one
  workspace, and HOME is not one of those. An agent listed there can be opened
  where it lives — the tab bar moves to that workspace, on that machine, and
  stages it.

- **The views became a rail down the left edge.** The tab bar was answering two
  questions at once — which project, and which view — in one undivided row. They
  are now split by axis: projects across the top, views down the side. The rail's
  width is derived rather than chosen and it is the *first* thing given up when
  the stage would go under `MIN_STAGE_W`, so on a narrow terminal it collapses
  and the space buttons on the tab bar carry the same job.

- **Files, docker and docs take the whole width.** A file body had 60 of 150
  columns — less than the tree and the two rails flanking it — while the AGENTS
  rail answered a question nobody reading a file had asked. These three pages now
  own the band between the tab bar and the footer, less the view rail, which is
  how you leave. WORK keeps its rails, because the agents and the changes *are*
  what that page is about.

- **Remembered machines are dialled at start.** `[+ host]` connections are kept
  as `[[remote]]` blocks and dialled when the client starts. They are
  deliberately not endpoints: a socket named in config is already reachable,
  while these need an `ssh -L` forward first, so they are dialled on their own
  tasks after the first frame rather than before it — otherwise one sleeping
  machine costs twenty seconds of blank screen. A machine with no bmux on it now
  says so instead of failing as `: command not found`.

- **The daemon names its own build in the handshake**, as `server_version` on the
  server's hello, so a client can tell a stale daemon from a broken one.
  `proto_version` cannot do this — it deliberately stays put across additive
  changes, so two builds many releases apart both report `1` and the handshake
  sees nothing wrong. The field is optional and omitted when unset, so the wire
  is byte-identical for anything that does not set it, and **its absence is
  itself the signal**: a daemon that does not send it predates the field. The TUI
  now says "daemon is 0.7.0, client is 0.8.0 — restart it" in the footer instead
  of leaving the user to hunt for the several unrelated-looking bugs that one
  stale process produces.

### Fixed

- **The workbench opens on the stage again.** `Focus` had taken its derived
  default, so the keyboard landed on the AGENTS rail and every keystroke at
  startup was a workbench command rather than terminal input — typing `echo` into
  a fresh client opened the agent picker on the `a`.

- **The spaces are back, and so is the Docs page.** 0.7.0 moved the workbench
  into the client without the `[work] files docker docs` buttons or the Docs
  page, leaving Files and Docker reachable only by keys that nothing advertised.

- **Overlays answer the mouse.** Any click dismissed an open overlay instead of
  activating the row under it, so a picker or a context menu could be opened with
  the mouse and then not used with it. The active tab's `[x]` and `[find]` on the
  tree came back with it.

- **The Alt layer belongs to the chrome.** Eight bindings had been dropped and
  `alt-a` repurposed; `alt-,` / `alt-.` had silently changed from cycling spaces
  to cycling workspace tabs, leaving no gesture for switching view at all.

- **The page follows the tab.** Switching workspace while a tree page was open
  moved the chip, the footer and the CHANGES rail, and left the tree listing the
  *previous* project's files — so the click read as having done nothing.

- **A diff is what is on the stage, not a place you go.** It had been given a
  space button between `files` and `docker`; staging anything else then changed
  what the stage held while leaving the full-screen diff in front of it.

- **A tree can be climbed as well as descended.** `Backspace` walked up and
  nothing on screen said so, so opening a folder read as a one-way trip. There is
  a `..` row now.

- **The verbs under the rails are keys that exist.** Both left-rail sections drew
  hint lines naming `x` and `r`, neither of which was bound anywhere, and each
  line was a single hit box — so clicking the word `x:kill` spawned an agent.

- **An unknown message no longer ends the connection.** The versioning rule is
  that additive changes do not bump `proto_version` — but a side that met a
  message invented after its own build could not decode it and hung up. `watch`,
  added in 0.6, hit exactly this: a current client attaching to a daemon left
  running from before it would be dropped, re-dial, send another `watch` at the
  next stage change, and be dropped again. A one-release gap therefore presented
  as the stage blanking over and over, with nothing anywhere naming a version;
  one real session logged 25 of them. Undecodable frames are now skipped in both
  directions, which is what makes the additive rule true rather than merely
  stated. Sixteen in a row still ends the connection, because at that point the
  stream has stopped making sense rather than merely being newer; a malformed
  length prefix remains fatal, since the next frame boundary is then unknown.

- **A pane's `PATH` gets the directories a login shell would have added.** The
  daemon inherits its environment from whatever started it — a desktop session, a
  systemd unit, an ssh command — which routinely has neither `~/.local/bin` nor
  nvm's `bin` on it. Resolving an agent's launcher out of those directories was
  only half the job: an npm-installed CLI is a `#!/usr/bin/env node` script, so
  it looked `node` up on the short `PATH` and found the distribution's, which is
  often years too old — the agent then died on a syntax error, reading as "the
  agent is broken" rather than "the daemon's `PATH` is short". Managed processes
  had the same gap for the same reason: `[[processes]]` runs through `$SHELL
  -c`, a non-interactive shell reads no rc file, so `npm run dev` in a
  `.bmux.toml` could not find `npm` even though the identical line works typed
  into a pane.

  Only directories that exist and are not already on `PATH` are added, in front,
  where a login shell puts them. A daemon started from one gets its `PATH` back
  byte for byte. nvm is all-or-nothing: a `PATH` naming a version keeps it
  untouched, and otherwise only the newest is added, so this can never shadow the
  node the user chose with an older one.

## [0.7.0] - 2026-08-09

### Added

- **The rails are pushed, not polled.** `GET /v1/events` gains a
  `workspace_detail` event carrying one workspace's full AGENTS / PROCESSES /
  CHANGES contents — the same body as `GET /v1/workspaces/{id}`. The stream
  previously pushed only counts, so every client that draws those rails polled
  the detail route on a 1–2 second timer; the macOS and iOS clients both do, and
  their own protocol guide tells you to.

  Two properties make it usable by a client that renders rails beside a live
  pane. It is emitted on the **frame clock** rather than the ~2s sampler tick,
  so a rail changes when the pane next to it does. And it is **diffed against
  the last one sent**: pane output marks the workbench dirty on nearly every
  frame while leaving the rails identical, so an unfiltered push would be a full
  snapshot per workspace per frame — affordable on a Unix socket, ruinous over
  ssh. Two identical details are never sent in a row, and with no subscribers
  nothing is built at all.

  Additive: a new SSE tag, which clients are already required to ignore when
  unknown, so `PROTOCOL_VERSION` is unchanged and the polling path still works.

- **Another machine's projects, in your tab bar.** Connect a host with `Alt-h`
  (or `[+ host]`, which lists the `Host` entries in `~/.ssh/config`) and its
  workspaces appear as tabs beside your local ones, marked `⇄ host`. `Alt-1..9`
  switches to one like any other tab; the git rail, the agents, the editor and
  the SYSTEM gauges on it are all that machine's, live. `Alt-x` twice
  disconnects. Hosts declared as `[[remote]]` blocks in `~/.bmux/config.toml`
  are connected when the daemon starts, so they are simply there each morning.

  The daemon does this by being a *client* of the far daemon, over the
  `ssh host bmux proxy` path that has always been the documented way in — so
  there is no new network surface, SSH keys remain the authentication, and the
  far daemon cannot tell the relay from a TUI. One control connection per host
  polls its workspace list; a workspace opens a second connection only once you
  actually look at it.

  It needed one protocol addition, a `relay` attach target: `attach`, but with
  the tab-bar row left blank. The row is still *reserved*, which is the whole
  trick — the near daemon paints its own merged tab bar over it, so nothing is
  lost, and every other coordinate agrees on both sides, so input including
  mouse coordinates forwards verbatim. `relay` is a new enum variant, so
  `PROTOCOL_VERSION` is unchanged and existing clients are untouched. Clients
  needed no work at all: the relaying daemon applies the far frames to a buffer,
  blits it, and re-diffs, so a TUI, the web stage and the phone app all see one
  ordinary frame stream.

- **`ssh` somewhere, type `bmux`, and it is already there.** Run `bmux` on the
  far end of an ssh session started from a pane and it hands that machine to the
  bmux you are already looking at — printing one line and exiting — instead of
  drawing a second workbench inside the first.

  `$BMUX` does not survive ssh, so the detection is a terminal query: bmux now
  answers Secondary DA with `98` (`b`) in its identifying field, the way tmux
  answers `84` (`T`). Every terminal answers DA2, so the far side gets a prompt
  yes *or* no rather than waiting out a timeout, and a plain terminal sees one
  invisible query and nothing else. Only after a confirmed answer does the far
  side announce itself, in an APC the near daemon picks out of the pane's output
  — the same scanner that already answers the queries a pane would otherwise
  block on.

  The near daemon then dials back by **reusing the pane's own `ssh` arguments**,
  read from its foreground process. That is what makes it need no
  configuration: `ssh -p 2222 -J bastion prod` reconnects through the same jump
  host with the same port, because it is the same command. Turn it off with
  `[general] remote_auto_attach = false`.

- **A pane no longer briefly labels itself with the daemon's own name.** The
  shell rail names a pane after its foreground command, read from the terminal's
  foreground process group. Two windows gave the wrong answer: between opening
  the pty and the child claiming it, `tcgetpgrp` still names *our* process
  group; and between `fork` and `exec` the child has no argv yet while its
  accounting name is still the forking thread's, inherited across the fork. The
  probe now skips its own process group, and treats an empty `cmdline` as
  nameless unless the process runs as another user — which is the case the
  accounting-name fallback was there for. Mostly invisible in normal use; under
  a loaded test run it named panes after test threads.

- **`kill-server` remembers, and `kill-server clear` forgets.** Stopping the
  daemon has always kept the persisted session — a restart is not a decision to
  throw work away — but there was no way to ask for the other thing. There is
  now: `bmux kill-server --clear`, or `:kill-server clear` in the prompt, removes
  both halves of the restore state (the workspace list *and* the per-pane output
  dumps) so the next start comes up empty. On the wire it is a separate
  `kill_server_clear` command rather than a field on `kill_server`, because unit
  variants travel as a bare string and adding a payload would have made every
  existing client's message unparseable.

### Changed

- **The daemon renders a terminal's screen, and nothing else.** It used to
  compose the whole workbench — tab bar, both rails, footer, overlays, the
  editor, the file tree, the diff — into one cell grid per attached client and
  ship damage diffs. Every other client already worked the other way, drawing
  its own chrome from `/v1/*` and streaming only the centre pane; the bundled
  TUI was the exception, and being the exception is what kept the boundary
  muddy.

  It is not the exception any more. The rule is now a property of the data
  rather than a preference: a pane holds a PTY, so what is on its screen is the
  accumulated effect of every byte a program wrote, and reconstructing that
  needs a VT emulator. Everything else about a workspace is *state*, and state
  crosses the wire as JSON. So frames arrive only on a `{"pane": …}` connection
  and cover only that pane; the three session targets (`attach`, `new`,
  `default`) still scope a connection to a workspace but send nothing to draw.

  Nothing about the workbench looks different. The same layout, the same rails,
  the same keys — drawn on the other side of the socket.

- **A theme is the client's.** `:theme NAME` no longer switches at runtime: set
  `[theme] name` in `~/.bmux/config.toml`. The old command worked because one
  process composed every frame and could repaint all attached clients at once,
  which is exactly what is no longer true — and one terminal being dark while
  another is light on the same daemon is now the point rather than a problem.
  The command remains in the vocabulary and answers with where to set it, so an
  existing binding gets a sentence rather than silence.

- **The pinned agent is the client's**, for the same reason. `set_default_agent`
  asked the daemon to write `[general] default_agent` into a config file it
  never reads that key from. `:agent-default NAME` still pins — the client
  validates against `GET /v1/agents` and writes the file itself — and the
  command is refused on the wire with a message saying so.

- `[general] remote_auto_attach` is read by the client now, because the client
  is what dials. The daemon detects a machine announcing itself over ssh (only
  it can — it reads every byte a pane writes) and reports it as a
  `remote_announce` event; whose tab bar that machine lands in was never the
  daemon's decision to make.

- **Four crates, not six.** `bmux-core` is gone: `config.toml` splits into a
  daemon half (the shell, scrollback and restore budgets, `[api]`,
  `[[agents]]`, and `.bmux.toml`) and a client half (the prefix, `[keys]`,
  `[theme]`, `[ui]`, `[[remote]]`). Both parse the same file and ignore each
  other's tables. `bmux-connect` folded into `bmux-client`; it existed so one
  daemon could be a client of another, which is not something a daemon does any
  more.

### Removed

- **Layout presets.** `[layouts]`, `[general] default_layout`, and
  `bmux new --layout` / `bmux standalone --layout` described a tree of pane
  splits to apply to a new workspace. The workbench has fixed rails and no free
  panes — which is why `apply_layout` has been answered with an error for some
  time — so a preset has had nothing to describe, and nothing had read one
  since the v2 chrome landed. `[general] remain_on_exit` goes with them, unread
  for just as long.

  `AttachTarget::New` and `new_session` keep their `layout` field on the wire,
  accepted and ignored, because shipped clients send it.

## [0.6.0] - 2026-08-05

### Added

- **Staging is no longer whole-file.** The diff pane stages by hunk (`]`/`[`
  to walk them, `Space` to take one) and by line (`v`, then `Space` to pick),
  discards a hunk with `x`, and unstages with the same key on a staged diff.
  That is `git add -p` without the questionnaire, and it was the one thing
  every real git UI had that bmux did not — a file with one finished change
  and one debug line had to be committed whole or edited first.

  The arithmetic lives in a pure module with no I/O, because it is where the
  risk is: an unselected `+` line is dropped, but an unselected `-` line
  becomes *context*, since the file being applied to still contains it, and
  every `@@` count is recomputed rather than copied. Exposed as
  `POST /v1/workspaces/{id}/git/apply`, which covers stage, unstage and
  discard with one route — see [`docs/protocol.md`](docs/protocol.md).

- **Worktrees, and a worktree is a workspace.** `g w` lists every checkout of
  the repository and `Enter` opens one as a bmux workspace — its own agents,
  processes, branch and changes rail, with no stashing and no switching. A
  worktree already open switches to its workspace rather than opening a second
  one on the same tree. `g w n` creates one on a new branch, placing it beside
  the repository so there is one prompt rather than two.

  There was no worktree support anywhere before this — not in the TUI, not
  over REST. `GET .../git/worktrees` reports which workspace is on each path,
  plus `POST`/`DELETE .../git/worktree` and `.../git/worktree/prune`.

- **Remote management**, the one place a URL reaches git:
  `POST`/`DELETE .../git/remote`. A URL is an arbitrary-code-execution vector
  (`ext::sh -c …` makes git run a shell), so the URL is checked against an
  **allowlist** of transports — https, http, ssh, git, file, git+ssh, an
  absolute path, and scp-style `user@host:path` — and every `<helper>::<rest>`
  form is refused. Every spawned git additionally carries
  `-c protocol.ext.allow=never`, so a miss in the allowlist is inert.

- **Six operations that had REST routes and no way to reach them** now have
  one: stash list and drop, tag create and delete, branch delete, remote
  remove. Each opens the picker with a title that says what `Enter` will do.

### Changed

- **The changes rail's keys follow the row you have selected**, and the footer
  always names them. An unstaged file offers `s`/`x`/`d`, a staged one `u`/`d`,
  a commit `d` — and a **conflicted file offers `o` ours, `t` theirs, `a`
  resolved**, which it could not do at all before: `resolve` existed as a REST
  route and the only client that could reach it was the web app.

  One table now drives the footer text, the click hit-testing, the key
  dispatch and `?`, so the rail cannot answer to a key it did not draw or draw
  one it will ignore. `p` (push) and `Enter` were both bound and never shown;
  `p` is now drawn exactly when there is something to push. Verbs that lose
  the competition for 38 columns (`r`, `C`) stay bound and stay in `?`.

  Two footer rows instead of three at the default width, so the list gets a
  line back. `?` generates its CHANGES entry from the same tables — the
  hand-written one had already drifted, advertising `C stages everything then
  commits` and telling users to resolve conflicts "from the web client".

- **The confirmation modal says which operation it is confirming.** It read
  "force push" for *every* git operation, which was true while a force push
  was the only one that asked and became wrong as soon as `reset --hard` and
  `worktree remove` joined it.

### Fixed

- **A commit made while the status scan was running did not reach the rail.**
  The scan walks the worktree off-thread, and a per-pane guard stops the ~2s
  sampler stacking scans on a slow repository — but that guard *dropped* any
  request arriving mid-scan. The running scan had started before the commit,
  so it reported the tree from before it, and nothing was left to correct
  that: the rail went on showing files as staged that were already committed
  until some unrelated event triggered another scan. Requests are now
  deferred rather than dropped.

  Found while chasing three separate "flaky" tests that all looked like
  ordinary polling races and were in fact this.

- **Diffs lost `\ No newline at end of file`.** The pane trimmed each line for
  display, which is invisible in a viewer and fatal in a patch — the marker is
  an annotation on the line above it, and a patch that drops it is rejected.
  The diff is now printed once, faithfully, and everything else is derived
  from it.

### Added

- **Git is a complete tool, not a status list.** The CHANGES rail grows a `g`
  menu (also `C-b g` / `:git-menu`) covering branches, remote sync, stash,
  merge/rebase and fixups, with a filtering branch picker — which finally gives
  `checkout` a TUI surface at all; it had been reachable only over REST and from
  the web client. Everything is exposed as `POST /v1/workspaces/{id}/git/*`, so
  an embedder gets the same surface: fetch, pull, push (`--set-upstream`,
  `--force-with-lease`), stash, amend, reset, revert, cherry-pick, merge,
  rebase, tags, branch create/delete/rename, conflict resolution, and paged
  history. See [`docs/protocol.md`](docs/protocol.md).

  Long operations answer `200` when they finish inside a short grace window and
  `202` when they do not, reporting progress over the new `git_op` SSE event and
  `GET .../git/op`. **A rejected push is a `200` with `ok:false`** — the call
  succeeded, the operation did not — because once an operation outlives the
  request there is no status code left to carry that.

- `ChangesDto` gains `conflicted`, `upstream`, `ahead`, `behind`, `state` and
  `detached`; `WorkspaceSummary` gains `conflicts` and `repo_state`, so a list
  view can badge "this tab is mid-rebase" without fetching detail.

### Changed

- **Conflicted files leave `unstaged` and get their own list.** A client that
  staged one without knowing it was conflicted would commit the `<<<<<<<`
  markers, so the two are now separate everywhere — rail, DTO and web client.
  `commit-all` refuses outright while anything is unmerged.
- `p` (push) now runs through the operation runner: it can be cancelled, it
  reports progress, and it can no longer hang. It previously ran on a blocking
  thread with no timeout and no `GIT_TERMINAL_PROMPT=0`, so a single credential
  prompt parked that thread for the life of the daemon.

### Fixed

- Diffs and changed-file markers in a workspace opened **below** the repository
  root. Status paths are relative to the worktree root, but both were anchored
  on the workspace cwd, so they named a path that does not exist — the diff came
  back empty and the Files tree marked nothing.
- `GET /v1/workspaces/{id}/branches` opened the repository on the core actor
  thread, which is the freeze that off-thread scanning exists to prevent.
- `C-b g` printed an error: it was still bound to a pane-splitting command the
  workbench removed. It opens the git menu now.

### Added
- **Releases now cover seven targets instead of four, from one workflow.**
  Tagging `v*` runs `.github/workflows/release.yml`, which builds `bmux` for
  `x86_64`/`aarch64` Linux against both glibc and musl, `armv7` Linux, and both
  macOS arches, then publishes a tarball per target plus `SHA256SUMS`. The two
  musl targets are new and are statically linked, so bmux now runs on Alpine,
  on distroless and scratch images, and on a glibc older than the `gnu` builds
  require — verified by executing the `aarch64-unknown-linux-musl` binary on a
  bare Alpine image. `armv7-unknown-linux-gnueabihf` is also new, which covers
  32-bit Raspberry Pi.

  A nightly `targets` job in CI type-checks the same seven, so a dependency that
  picks up a C library or a `cfg` gate that assumes glibc surfaces within a day
  rather than at release time.

### Changed
- **Every platform ships the same artifact.** macOS used to publish bare,
  `rcodesign`-signed binaries cross-built from Linux with `cargo-zigbuild`,
  because the native GUI clients bundled a daemon out of the release. That
  packaging is gone along with `scripts/build-macos.sh`: macOS is now built on
  macOS runners, where the linker ad-hoc signs the binary as a side effect, and
  is packaged as a `.tar.gz` exactly like every other target. `scripts/install.sh`
  drops its bare-binary branch as a result, and picks the musl build wherever
  glibc is not the system libc.

  **This renames the macOS release assets.** `bmux-darwin-arm64` and
  `bmux-darwin-x86_64` become `bmux-<version>-aarch64-apple-darwin.tar.gz` and
  `bmux-<version>-x86_64-apple-darwin.tar.gz`. Anything fetching the old asset
  names from a release needs updating.

- **The Windows note says what a port would actually take.** It is a transport
  and tty port — named pipes or loopback TCP instead of Unix domain sockets, and
  a Console API backend instead of `termios` and POSIX signals — not a build
  target that anyone forgot to enable. Under WSL2 bmux runs as an ordinary Linux
  binary today.

- **The minimum supported Rust version is now stated as 1.88.** It was declared
  as 1.80, but the tree already used `Option::is_none_or` (stabilised in 1.82)
  and the dependency graph floors at 1.88 with dev-dependencies excluded — so
  1.80 never actually built and anyone following the README hit a compile
  error. This corrects the claim; it does not raise a requirement that was
  being met.

### Fixed
- **A workspace whose directory is not readable at daemon start is no longer
  deleted from the saved session.** Restore treated an unresolvable `cwd` as a
  folder the user had removed, skipped it, and then rewrote `session.json` from
  the workspaces that *did* come up — so the entry, its agents and their
  conversation ids were destroyed, with nothing left to restore from. But absent
  is not the same as gone: a network share or an external disk that has not
  finished mounting reads exactly like a deleted folder, and daemon start is
  precisely when a mount is least likely to be up. On an SMB-mounted tree every
  workspace on the share was lost at once, which is what "restore does nothing"
  looked like from the outside.

  Unreadable entries are now carried and written back out on every persist, so
  the next start rebuilds them once the directory is there again; their pane
  dumps are kept for the same reason, rather than swept as orphans. A workspace
  reopened by hand retires the held copy, so closing it still means closed.

- **The ALL AGENTS panel remembers whether it is open.** `[ui] all_agents` was
  read at startup but never written back, so toggling the panel lasted only as
  long as the daemon — the rail widths beside it persisted, which made it read
  as a setting that had failed rather than one that was never saved. It is now
  written on the toggle, like the rail geometry and the theme.

- **Restart restore no longer tries to reopen conversations that were never
  written.** Both CLIs create the transcript on the *first user message*, not at
  launch, so the id bmux minted for an agent you opened and never typed into
  names nothing. Asking for it back is not a no-op — `claude --resume` prints
  "No conversation found with session ID" and exits 1 — so every idle agent died
  on restart and came back through the missing-conversation fallback, which
  exists for the rare case rather than the common one. The visible cost was a
  dead pane, a relaunch, and a warning in the scrollback of an agent that had
  done nothing wrong.

  An agent pane now records whether it has been sent input, and only a pane that
  has is asked to reopen anything; the rest start fresh, which is what they were
  going to get anyway, minus the failed launch. Tracked as "was this pane typed
  into" rather than by looking for a vendor's transcript file, so it stays true
  for a CLI whose storage bmux has never heard of. Scrolling and selecting do
  not count — neither writes a transcript.

- **The saved agent roster keeps up with the panes.** `session.json` was written
  only when a workspace opened or closed and on a graceful shutdown, while the
  pane dumps beside it were rewritten every sampler tick. A daemon that was
  killed rather than asked to stop therefore restored one against the other:
  agents started since the last workspace opened were absent from the roster and
  simply did not come back, and because dumps are keyed by position and pruned
  to the live set, closing an agent shifted the rest — so a pane could return
  holding its own conversation under a neighbour's screen. The roster is now
  written on the same tick as the dumps it has to agree with, and immediately
  whenever a pane is spawned or closed.

## [0.5.0] - 2026-08-04

### Added
- **An agent in a pane can drive the workbench around it.** New CLI verb groups
  `bmux pane`, `bmux agent` and `bmux process`, plus `bmux whoami` — thin
  wrappers over the REST routes, so this is simultaneously the agent-facing
  surface and a shell-out plugin surface.

  The capability was mostly there already: the API could spawn an agent, list
  siblings and inject input without attaching. What was missing was a way for a
  program *in* a pane to reach any of it. Three things closed that:

  - **Every pane now carries `$BMUX_PANE`, `$BMUX_WORKSPACE` and
    `$BMUX_SOCKET`** — agent, process and shell alike. `--ws` and `--socket`
    already defaulted to the latter two, so a command run inside a pane now acts
    on its own workspace, on its own daemon, without arguments. There is no
    separate "inside bmux" flag: `BMUX_PANE` is always set by the spawner and
    says more than a boolean, so its absence is the test.
  - **`GET /v1/workspaces/{id}/panes/{pane}/output`** returns a pane's rendered
    output as *text*. The daemon already runs the VT emulator, so it resolves
    wide graphemes and trailing blanks once, server-side, rather than every
    client reimplementing the cell-grid rules the framed protocol needs. It is a
    query: unlike a framed `pane` attach it neither resizes the pane nor clears
    its bell. `?source=footer` returns the exact band the state detector scans,
    which makes "why does bmux think this agent is working?" answerable from
    outside the daemon.
  - **`bmux agent wait`** blocks until an agent reaches `finished`, `exited`, or
    whatever `--until` names, and reports which through its exit code. `wait` is
    what turns poking at a sibling into coordinating with it.

  `bmux agent send … --wait` is the form to prefer: a bare `wait` issued right
  after a prompt can return on the *previous* turn's `finished`, since agent
  state is recomputed on a ~2s tick. `send --wait` reads the notification feed's
  position before it types and only accepts a state reached after it.

  Exit codes are now a documented interface, since the point of all this is to
  be shelled out to: `0` success, `2` no such target, `3` timed out, `4` exited,
  `64` usage. Under `--quiet` the code is the whole answer, so
  `bmux agent wait 7 -q && ./deploy.sh` does the right thing. `exited` is `4`
  even when you waited for it — it is in the default set so the wait terminates,
  not because a dead agent is a success.

  `POST /v1/workspaces/{id}/agents` takes `{"background":true}` to spawn without
  taking the stage, so a helper does not yank the view from whoever is watching.
  Default is unchanged.

  `skills/bmux/SKILL.md` is the agent-facing
  documentation and [`docs/agents.md`](docs/agents.md) the human one. The skill
  triggers on `$BMUX_PANE` being set rather than on the user saying "bmux".

  Known limit, reported honestly rather than papered over: a pane read reaches
  the visible screen plus about one screen of history, however deep
  `[general] scrollback` is. vt100 0.15 composes its view as *`offset`
  scrollback rows + `rows - offset` live rows*, so the offset cannot pass one
  screen; `"more": true` says when there is more it could not reach. (The same
  arithmetic makes a deep `scroll_page` unsound; vt100 0.16 fixes it but removes
  the `title` and `audible_bell_count` accessors this crate reads, so that
  upgrade is its own change.)

- **Paste an image into an agent.** New `put_file` command: a client sends the
  bytes off its clipboard, and the daemon writes them to
  `~/.bmux/scratch/<workspace>/` and pastes the resulting absolute path into the
  pane — the form agent CLIs accept an image in. It answers `file_put` with the
  path so a client can say where the file went.

  The scratch directory is deliberately outside the project.
  `POST /v1/workspaces/{id}/upload` writes into the workspace and repaints the
  changes rail, which is what you want for a file you meant to add and not for a
  screenshot; pasted images would otherwise show up as untracked files and ride
  along in someone's commit. Each workspace keeps its most recent 32.

  It is a command rather than a second REST route because of where it has to
  work: a TUI attached to a remote daemon has exactly one `ssh host bmux proxy`
  stdio channel open, so routing this over HTTP would cost it another ssh
  channel per paste. The framed connection is already there. `data` is base64 so
  that JSON and MessagePack clients send the identical structure, capped at
  8 MiB decoded — frames go to 32 MiB, so the cap is there to refuse a
  40-megapixel photo with a readable error rather than a rejected frame.

  Because the daemon does the work, every client inherits the gesture rather
  than implementing it: the web client now takes an image off Ctrl/Cmd-V *and*
  accepts a file dragged onto the stage, in ~40 lines. Client authors get the
  short version in [`docs/building-a-client.md`](docs/building-a-client.md).

- **`Alt-v` (or `C-b v`, or `:paste-image`) pastes the clipboard image in the
  TUI**, including over `ssh host bmux proxy` — the clipboard read happens on
  your machine, which is the whole point.

  A clipboard belongs to the client's machine, so `paste_image` is a *request*:
  the daemon replies `read_clipboard_image` and the client answers with
  `put_file`. It is a command rather than a client-side keybinding so the
  keymap, the `:` prompt and the help overlay all describe it from one place,
  and a client with no clipboard simply ignores the request. `arboard` does the
  reading; it hands back raw RGBA, so the client re-encodes to PNG, because an
  agent CLI needs a file it can actually decode.

  New `notice` message in the other direction — the client's only way to put a
  sentence in front of the user, since the daemon draws every frame and a client
  has no footer of its own. "no image on the clipboard" has to come from the
  side that looked.

### Fixed
- **The web bridge unmasked WebSocket payloads a byte at a time in Python**,
  which was invisible for keystrokes and cost ~210 ms per 6 MiB once `put_file`
  started sending images through it. One big-integer XOR instead: ~15 ms.
- **Restart restore now brings back the work, not just the workspaces.**
  `session.json` has restored open project directories since it was added, but
  each one reopened on a blank shell: the agents you had running were gone, and
  so was everything on screen. Three things changed.

  Every terminal pane keeps a bounded ring of its raw output — `[general]
  restore_bytes`, 256 KiB by default, `0` to disable — written to
  `~/.bmux/panes/` on the sampler tick and once more on the way down, and
  replayed into the pane that replaces it. Replaying the untouched byte stream
  is what makes the restored pane look like the one that was lost: wrapping,
  scroll regions and colors land the way they originally did, because the
  parser is fed exactly what it was fed the first time. Each dump carries the
  geometry it was recorded at, since a recording is full of absolute cursor
  moves and replaying it a few columns narrower shreds the text.

  The session file now records each workspace's agents and processes — the ones
  started by hand as well as the ones from `.bmux.toml` — in order, and which
  pane held the stage. A restored workspace rebuilds from that list rather than
  re-running the workspace file's autostart block, so a process removed from
  `.bmux.toml` since does not come back and one started by hand is not lost.

  Agents get an `[[agents]] resume_args`, used *instead of* `args` when a
  restore respawns them, so the CLI reopens its previous conversation instead of
  starting an empty one under a transcript it has never seen. It is a full
  replacement rather than a suffix because resume is not uniformly a flag —
  `claude` takes `--continue` alongside its other args, and `codex` resumes
  through a subcommand that has to come first.

  **Each pane reopens its own conversation, by name.** Every CLI's own resume
  flag — `claude --continue`, `gemini --resume latest` — means "the most recent
  conversation *in this directory*". That is ambiguous precisely where bmux is
  most opinionated: a workspace running two agents. Both would reopen the same
  transcript, and Claude Code's own documentation spells out what happens next —
  *"if you resume the same session in two terminals without forking, messages
  from both interleave into one transcript."* So bmux names the conversation
  instead. A launcher writes `{session_id}` where its id belongs; bmux mints one
  before the process starts — closing any window in which two agents could
  observe each other — records it in `session.json`, and substitutes the same id
  into `resume_args` on the way back. A launcher that never mentions
  `{session_id}` is passed through untouched, so an existing `[[agents]]` block
  behaves exactly as it did.

  Claude Code and Gemini CLI ship configured, both verified against the
  installed binaries. Gemini's `--resume <uuid>` is undocumented — its `--help`
  advertises only `latest` and an index — but its own source resolves a full
  UUID first. Note the id is *set* with one flag and *reopened* with another,
  because both CLIs refuse to re-declare an id that already exists. Codex has no
  way to be told an id at launch, so it is left alone; aider has no session
  concept at all, its history being per-directory.

  **A conversation that has gone missing no longer kills the pane.** It can age
  out of Claude Code's 30-day retention, be cleared by hand, or never have been
  written because the agent was launched and never used — and the CLIs do not
  degrade, they exit (verified: `claude` exits 1, `gemini` 42). A pane that dies
  on every restart is worse than one that comes back empty, so an agent that
  exits within ten seconds of a restore is given one clean start on a fresh
  conversation, with the reason written into its scrollback. Deliberately
  generic rather than a per-CLI check against each vendor's storage layout:
  bmux does not need to know *why* the launch failed to know the fallback is
  the same either way.

  Removed the matching **Non-goals** entry from the README, which now described
  the opposite of what the daemon does.

- **`bmux workspace`, and a CLI that speaks the REST API.** The daemon has
  served a documented JSON API since 0.2, but the CLI could not reach a single
  route of it — nine subcommands, four flags, and hand-formatted `println!`.
  `bmux workspace ls|show|create|rm` is the first group to go through the HTTP
  face on the daemon socket, the way `docker` is a client of dockerd, so every
  primitive it gains is one the iOS, macOS, and web clients gain too.

  New global flags: `--json`, which re-emits the daemon's own response body
  **verbatim** rather than re-serializing a parsed struct — so the CLI's JSON is
  the REST API's JSON by construction and cannot drift as DTOs gain fields;
  `--quiet`, which prints nothing and leaves the exit code as the answer;
  `--socket` to pick a daemon; and `--ws` for workspace scope, defaulting to
  `$BMUX_WORKSPACE`.

  `crates/bmux/src/main.rs` is now an entry point and nothing else: the command
  tree lives in `cli/`, the HTTP client in `api.rs`, and every write to stdout
  in `out.rs`. Adding a subcommand no longer means editing `main`. The existing
  commands are untouched, down to the byte — `ls`, `kill-session` and
  `kill-server` stay on the framed control path, because their output is a
  contract the test suite drives.

### Fixed
- **btop renders.** It was drawing every pane it owns as overlapping soup: the
  right numbers and graphs, piled onto whatever line the cursor happened to sit
  on. btop positions the cursor with HVP (`CSI row;col f`) and never with CUP
  (`CSI row;col H`) — roughly 700 seeks a frame — and vt100 implements only the
  latter, dropping each `f` with nothing louder than a debug log. The emulator
  now normalizes HVP to CUP as PTY output arrives; ECMA-48 defines the two as
  equivalent, the swap is one byte, and it is length-preserving, so the
  terminal-query scanner's offsets are untouched. The scan itself uses `memchr`
  to skip between escapes, which keeps it off the profile for ordinary panes.
  Every other TUI in the matrix positions with CUP, which is why btop alone
  looked broken.
- **A multiple-choice question now reads as "needs you"** instead of a turn that
  never ends. Claude Code's `AskUserQuestion` dialog gives an agent's status away
  in neither of the two ways bmux knew about: its options carry a description
  line each, so the highlighted `❯ 1. …` sits ten to twenty rows up — outside the
  8-row footer band — and it asks "Which database…?" rather than "Do you want
  to…?". Worse, its hint line offers `Esc to cancel`, which is how Gemini spells
  its *interrupt* hint, so the pane read as **working** for as long as the
  question went unanswered: no notification, a spinner that never stopped, and no
  "finished" when the turn really did end. The hint line's `Enter to select` half
  is now prompt chrome, and a chrome line no longer counts as a working marker.

### Added
- **Per-agent status detection overrides.** An `[[agents]]` block now takes a
  `waiting_pattern` and a `busy_pattern` regex, matched case-insensitively
  against the footer band. Status detection is deliberately generic — the same
  marker tables have to work for a CLI nobody has seen — so it misreads an agent
  whose footer is worded unusually, and until now fixing that meant editing the
  tables in Rust and waiting for a release. Each pattern **replaces** the
  built-in table for that one signal instead of extending it, which is the only
  shape that fixes the expensive half of the problem: a false positive pins a
  pane to "busy", and a pane pinned to busy never fires its finished
  notification. An additive pattern could add a missing match but never take one
  back. A pattern that fails to compile is dropped with a warning and the
  built-in tables stand, so a typo costs accuracy rather than the agent.
- **Docker test suite** (`testsuite/`, run with `./testsuite/run.sh`). The crate
  tests run the daemon in-process; this runs the binary in a container and
  drives it as a client does. It covers every HTTP route and protocol variant
  (and fails if one is never exercised), real terminal applications — btop,
  htop, vim, less, a nested tmux — read back through the wire protocol, agent
  status detection against scripted doubles of each agent CLI, and a stress
  profile reporting latency percentiles and RSS/thread/descriptor drift.
  Standard library Python only; three profiles (`smoke` / `standard` / `soak`)
  plus separate passes under `--pids-limit`, `--memory` and `--cpus`. An opt-in
  `--real-agents` lane runs the same assertions against the actual CLIs, which
  is what catches an upstream status-line reword. See
  [testsuite/README.md](testsuite/README.md).

## [0.4.0] - 2026-08-02

### Added
- **Themes.** The chrome now draws from a named palette of 18 semantic roles
  (`accent`, `danger`, `faint`, `selection`, …) instead of hardcoded ANSI colors,
  selected with `[theme] name = "..."` in `config.toml`. Four ship built in:
  `blueprint-dark` (the new default), `blueprint-light`, `tokyonight` (the colors
  bmux used before), and `terminal` (defers every role to your terminal's own
  palette). User themes are `~/.bmux/themes/<name>.toml`, may `extends` a
  built-in and override only the roles they care about, and take `#rrggbb`,
  `ansi:N` or `default` per role. Unknown roles warn instead of failing the load.
  Any extra key in `[theme]` overrides one role without needing a file. Because
  the daemon composes the frame, the browser client's screen view picks the theme
  up too. See [docs/theming.md](docs/theming.md).
- **`:theme`** at the command prompt (`C-b :`) switches palette without a
  restart: `:theme gruvbox-dark` applies immediately to every attached client and
  writes the name back to `[theme] name` in `config.toml`, preserving the rest of
  the file — including role overrides in the same table. Bare `:theme` lists the
  built-ins and your own themes, marking the current one; an unknown name is
  rejected instead of silently falling back to the default. Bindable like any
  other command (`[keys] "t" = "theme terminal"`), and available over the control
  socket. `examples/themes/` gains gruvbox-dark, catppuccin-mocha and
  solarized-light to copy, plus a three-role `mine.toml` showing the usual shape.
- Changes rail: `C` stages every change and opens the commit box in one step —
  the common "commit all my work" case without pressing `s` on each file first.
  Also exposed as `POST /v1/workspaces/{id}/changes/commit-all` (`{message}`) and
  wired into the web client as a **Commit all** button.
- macOS client: REST requests now pipeline over one long-lived SSH `exec`
  channel (a single `bmux proxy` per server), instead of opening a fresh channel
  and remote process per request — a whole poll completes in one round-trip with
  no per-request spawns, cutting idle CPU and latency.
- macOS client: the terminal is now selectable — drag to select text (flow /
  reading-order), **Cmd-C** to copy, **Cmd-A** to select all, and a right-click
  **Copy / Paste / Select All** menu. An I-beam cursor marks the region; typing
  or scrolling clears the selection.
- `bmux reset`: puts a terminal that a `SIGKILL`ed (or older) bmux left in
  mouse-tracking and raw mode back to normal. Talks only to the tty — no daemon,
  no nesting guard — so it works from the wedged shell itself.

### Changed
- **Everything bmux stores now lives in one directory, `~/.bmux/`** — `config.toml`,
  `themes/`, `logs/`, `session.json`, and the `bmux.sock`/`bmux.lock` runtime
  pair. It replaces a three-way XDG split (`~/.config/bmux`,
  `~/.local/state/bmux`, `$XDG_RUNTIME_DIR/bmux` falling back to
  `/tmp/bmux-<uid>`). Moving the socket in with the rest also fixes a class of
  "second, empty daemon" bug: `$XDG_RUNTIME_DIR` is set for a login shell but
  routinely absent from a non-interactive `ssh host bmux ...`, so the socket
  path moved between the two and a remote client would miss the daemon already
  running. `BMUX_SOCKET` and `BMUX_THEME_DIR` still override.

  **This is a hard cutover — no migration and no fallback.** Files left at the
  old paths are ignored. To keep your config, themes and open workspaces:

  ```sh
  mkdir -p ~/.bmux
  mv ~/.config/bmux/config.toml ~/.bmux/config.toml
  mv ~/.config/bmux/themes ~/.bmux/themes
  mv ~/.local/state/bmux/session.json ~/.bmux/session.json
  ```

  Kill any daemon still running on the old socket first (`bmux kill-server`, or
  it will linger with no client attached).
- PROCESSES rail: a shell row is now named by the **whole command line** running
  in it — `sudo apt-get update -y`, not a bare `sudo`. Previously the row showed
  only the foreground process's name, read from `/proc/<pid>/comm`, which also
  meant the feature did not exist at all on macOS (no `/proc`, so every shell row
  just read `shell`). The lookup now reads full argv from `/proc/<pid>/cmdline` on
  Linux and `KERN_PROCARGS2` on macOS, cached for 500ms so the animating rail does
  not syscall per frame. Rows for processes named in the workspace file keep their
  configured name. Two caveats: a command running with privilege will not show its
  argv to an unprivileged caller — `sudo …`, and on macOS anything setuid, which
  includes `/usr/bin/top` — so those rows are named for the program alone (`top`),
  read from `/proc/<pid>/comm` on Linux and `proc_pidpath` on macOS; and a pane
  whose child has forked but not yet `exec`'d is suppressed rather than briefly
  labelled `bmux`.
- Rail rows now marquee-scroll whenever their text overflows, instead of only the
  staged row and the row under the cursor. Long agent titles and command lines are
  readable without selecting them first, and rows are staggered so the rail does
  not slide as one block. The cost is that the frame keeps repainting on the 450ms
  animation tick for as long as any row overflows; the wire cost stays negligible
  because only changed cells are shipped.
- A pane spawned from a command string keeps its full command as its label rather
  than the first whitespace token, so the STAGE box titles `docker logs -f web`
  instead of `docker`. Titles longer than the box are ellipsized so the border
  stays visible.
- The chrome now ships 24-bit color by default. Previously most of it was emitted
  as ANSI indices 0–15 and resolved against whatever colorscheme your terminal
  had; it now resolves to the selected theme server-side. Set
  `[theme] name = "terminal"` to get the old inherit-from-my-terminal behaviour
  back, or `"tokyonight"` for the exact colors bmux used before.
- `[theme]`'s `border` and `border_focused` keys are now aliases for the `rule`
  and `rule_focus` roles. Existing configs keep working unchanged.

### Fixed
- The list of open workspaces was saved under `/tmp`, so a reboot silently lost
  it. `session_state_path()` put the session file beside the socket whenever
  `BMUX_SOCKET` was set — intended as a test escape hatch, but a client
  auto-spawning the daemon always passes the socket through the environment, so
  the branch fired for *every* normal run and the file landed at
  `/tmp/bmux-<uid>/bmux.session.json`. It now resolves to `~/.bmux/session.json`
  regardless of the socket; set `BMUX_SESSION_FILE` (matching the
  `BMUX_THEME_DIR` convention) to give an experimental daemon its own store.
- The terminal was left in mouse-tracking mode when the client died on a signal:
  every mouse move then spewed escape sequences (`<35;80;12M`) into the shell,
  which was also left raw and on the alternate screen. Teardown ran only from
  `Drop` and the panic hook, so `kill`, a closed window or dropped ssh
  connection (`SIGHUP`), and crashes all skipped it. The client now installs
  handlers for `SIGHUP`/`INT`/`QUIT`/`TERM` and `SIGSEGV`/`BUS`/`ILL`/`FPE`/
  `ABRT` that restore the terminal with two async-signal-safe syscalls, then
  chain to the previous disposition so crash diagnostics (including Rust's
  stack-overflow message) still print. `SIGKILL` remains uncatchable — that is
  what `bmux reset` is for.
- The client no longer enables `?1003h` (any-event mouse tracking). crossterm's
  `EnableMouseCapture` sets it, but bmux drops motion events at every layer, and
  it is the mode that turns a missed restore into a flood of garbage on idle
  mouse movement rather than the odd stray click.
- A panic on the input thread or a tokio worker restored the terminal while the
  main loop kept painting over the shell. The hook now exits the process when
  the panicking thread isn't the one driving the UI.
- macOS client: the pipelined transport couldn't parse any daemon reply
  (`HTTP 200 reply has no Content-Length`) because HTTP headers were split on
  `"\r"`/`"\n"` compared as `Character`s — but Swift treats a CRLF pair as one
  grapheme, so the split never fired and the header collapsed to a single line.
  Now split on `Character.isNewline`, which recognises the CRLF grapheme.

## [0.3.0] - 2026-07-24

### Added
- SYSTEM rail: native macOS CPU and RAM telemetry. The sampler was Linux-only
  (`/proc`, `/sys`), so a Mac-hosted daemon showed a dead `0% / 0/0G` rail; it
  now reads CPU load and memory via mach (`host_processor_info`,
  `host_statistics64`), matching Activity Monitor. GPU and temperature stay
  Linux-only for now.

## [0.2.0] - 2026-07-24

### Added
- Changes rail: `x` discards a file's worktree changes — restoring it from the
  index, or deleting it when untracked. Because that can't be undone it opens a
  confirmation modal (Cancel preselected; click a row, or `y`/`n`/Esc), and the
  verb is a clickable hint button like the others. Staged work is never touched:
  unstage it first. Also exposed as `POST /v1/workspaces/{id}/changes/discard`.
- Web client: hybrid API-driven chrome with a streamed terminal stage.
- Native macOS (SwiftUI) client.
- Daemon: `POST /v1/workspaces/{id}/panes/{pane}/input` injects a single key /
  paste into a pane's PTY without a streaming attach (and without the resize a
  transient attach would force), so a list UI can approve or interrupt an agent.
- Daemon: `POST /v1/fs/mkdir` creates one folder on the host and replies with
  that folder's listing, so a client can create a directory and navigate into it
  in a single round-trip. The name must be a single path component.
- iOS/macOS/web: **New Folder** in the create-workspace folder picker — start a
  project that doesn't exist yet without dropping to a shell first.
- Daemon: `WorkspaceSummary` now carries `waiting` / `working` agent counts so a
  list view can badge "needs you" state without fetching each workspace's detail.
- iOS/macOS: one-tap **Accept** (Enter) and **Stop** (Esc) on a waiting or
  working agent, straight from the agent row — no typing into the terminal.
- iOS: reworked workspace screen — agents and terminals as a scannable list with
  inline Accept/Stop and Restart/Kill, plus Files / Docker / Git cards; tap a row
  to open it full-screen. "Needs you" state bubbles up to the Workspaces and
  Servers screens as glanceable badges.
- `scripts/release.sh`: cross-platform release builds for Linux amd64 and
  arm64 (glibc), packaged as tarballs with checksums under `dist/`.
- Repository scaffolding: `LICENSE`, CI workflow, `CONTRIBUTING.md`,
  `.editorconfig`, and this changelog.

### Changed
- Layout is global. `Alt-l` now resizes the rails for **every** workspace at
  once — open ones reflow immediately and new ones inherit the width — and the
  result is saved to `[ui]` in `~/.config/bmux/config.toml` instead of the
  project's `.bmux.toml`. A `[ui]` table left over in a `.bmux.toml` still
  parses without warning, but its rail widths are ignored.

### Fixed
- Daemon: opening an idle agent no longer flips it to "working" and then fires a
  phantom "agent finished" notification. Attaching resizes the pane, and the
  agent's one-shot repaint used to register as a whole turn; "working" now needs
  a visible busy marker or output that keeps streaming, and a marker-less run
  shorter than a few seconds settles back to idle without notifying.
- Daemon: agent state detection reworked. Working markers are now anchored to
  the key you press (`esc to interrupt`, …) instead of bare verbs, so an answer
  mentioning "to stop" or a backgrounded server no longer pins an agent to
  "working" forever. Decision prompts are matched by shape — a cursor on a menu
  entry (`❯ 2. No`), inside a dialog box — so any highlighted option counts, not
  just the first, while a numbered list in prose no longer reads as a question.
  A question also loses to a visible working marker (mid-turn prose is not a
  dialog). The live rail and the state clients see are now read from the same
  signals, so an agent stays "working" through a thinking pause.
- Daemon: a repository created after a workspace opened (`git init`, a clone
  landing in the cwd) is now detected on the next ~2s tick and gets its CHANGES
  rail, instead of staying invisible until the workspace was reopened.
- Daemon: opening a workspace no longer freezes the whole TUI while `git status`
  runs. The CHANGES rail's status scan (a full worktree walk — slow on big repos
  and network filesystems) now runs off the core event loop and streams in when
  ready, so the client paints immediately and the ~2s refresh tick never stalls
  rendering. On a slow (e.g. SMB-mounted) repo this had shown as a black, frozen
  TUI at startup.

## [0.1.0]

Initial workspace: per-user daemon with server-side VT emulation, editor,
file tree, and git panes; a terminal (TUI) client; and a public framed +
REST protocol. See [`docs/protocol.md`](docs/protocol.md).

[Unreleased]: https://github.com/dieterpl/butai/compare/v1.1.1...HEAD
[1.1.1]: https://github.com/dieterpl/butai/compare/v1.1.0...v1.1.1
[1.1.0]: https://github.com/dieterpl/butai/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/dieterpl/butai/compare/v0.12.1...v1.0.0
[0.12.1]: https://github.com/dieterpl/butai/compare/v0.12.0...v0.12.1
[0.12.0]: https://github.com/dieterpl/butai/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/dieterpl/butai/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/dieterpl/butai/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/dieterpl/butai/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/dieterpl/butai/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/dieterpl/butai/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/dieterpl/butai/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/dieterpl/butai/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/dieterpl/butai/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/dieterpl/butai/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/dieterpl/butai/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/dieterpl/butai/releases/tag/v0.1.0
