//! The workbench, drawn here.
//!
//! Tab bar, rails and footer, painted from the DTOs on `/v1/*` rather than
//! arriving as cells the daemon composed. The one thing that still crosses the
//! wire as pixels is a pane's grid, because a PTY's screen is the daemon's to
//! know — everything on this page is drawn from JSON.
//!
//! **Why the client owns layout.** The daemon cannot compute this geometry: it
//! does not know how wide we chose to make the rails, or whether we collapsed
//! them. So the stage rectangle is derived *here* and told to the daemon as the
//! pane connection's `cols`/`rows`, and the frames it sends are always exactly
//! that size.
//!
//! **Measurements and ink are separate files.** [`model`] holds the rectangles
//! and the row vocabulary — where the left rail ends, what a working agent's
//! row says — and this one turns them into cells. They were one module in
//! `butai-core` while the daemon drew a workbench too, so that two renderers
//! could not drift apart on either question. There is one renderer now, so the
//! split is about size rather than agreement, and the whole thing lives here.
//! Colour arrives as a [`Role`] and is resolved against the palette held here.

mod model;
pub use model::*;

pub mod help;
pub use help::Help;

pub mod settings;
pub mod usage;
pub use settings::Settings;

// The rectangles go by `Geom` here, where `Chrome` already means the drawing.
use self::model::Chrome as Geom;
use crate::config::{DiskMode, DiskSelect, NetMode, NetSelect, RailGeom};
use crate::layout::Rect as LRect;
use crate::theme::{Palette, ThemeColor};
use crate::verbs::Verb;
use butai_protocol::api::{
    AgentDto, AgentState, ApplyTarget, BranchDto, BranchesDto, ChangesDto, DiskDto, DiskKind,
    FileChange, LogEntryDto, NetDto, NetKind, ProcessDto, RemoteDto, StackDto, StashDto, SysDto,
    WorkspaceDetail, WorkspaceSummary, WorktreeDto,
};
use butai_protocol::hunk::{FilePatch, Hunk, Origin, Patch, Selection};
use butai_protocol::{PaneId, SessionId};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;
use std::collections::BTreeSet;
use tui_textarea::TextArea;
use unicode_width::UnicodeWidthStr;

use crate::syntax::{Highlighter, Lang, Token};

/// One entry in the tab bar.
///
/// Carries the daemon it came from, because the tab bar spans several: a client
/// dials each machine itself rather than asking one daemon to relay another, so
/// "which host is this" is a property of the tab rather than of the connection.
#[derive(Debug, Clone, Copy)]
pub struct Tab<'a> {
    pub summary: &'a WorkspaceSummary,
    /// Badge shown when more than one daemon is connected; `None` for the local
    /// one, which needs no qualifying.
    pub host: Option<&'a str>,
    /// False while this tab's daemon is not answering.
    ///
    /// The counts on the chip — the `!` that says something is waiting for you —
    /// are then as old as the link, and a chip that goes on demanding attention
    /// on behalf of a machine that is not there is the most expensive kind of
    /// stale: it sends you to a workspace to find out why.
    pub live: bool,
}

/// One row of the ALL AGENTS panel: an agent, and where it lives.
///
/// The panel is cross-workspace by definition — it is the answer to "is anything
/// waiting for me anywhere" — so it cannot be drawn from the active
/// `WorkspaceDetail` alone.
#[derive(Debug, Clone, Copy)]
pub struct AllAgentRow<'a> {
    pub workspace: &'a str,
    /// The workspace's id on its own daemon — what a route acting on this agent
    /// needs.
    ///
    /// The name is what the row *says*; this is what it *is*. Two machines
    /// routinely have a project of the same name open, and one machine may have
    /// two — so anything that goes back to the daemon (going there, ending it)
    /// resolves through the id, and the name is left to the drawing.
    pub workspace_id: SessionId,
    pub agent: &'a AgentDto,
    /// The daemon it is on, when more than one is connected.
    pub host: Option<&'a str>,
    /// Index into `daemons`, always set — unlike [`host`](Self::host), which is
    /// `None` on a single-daemon client because there is nothing to qualify.
    ///
    /// BOOTH needs the index rather than the label: it groups by machine, points
    /// the stage at the selected agent's *own* daemon, and looks that machine's
    /// telemetry up by position. A label cannot do any of the three, and two
    /// daemons are allowed to have none.
    pub daemon: usize,
}

/// One connected daemon, for the BOOTH page's compute column.
///
/// The telemetry is borrowed from that daemon's own state rather than merged:
/// `SysDto` describes a machine, and averaging four machines' CPU produces a
/// number that is true of nothing.
#[derive(Debug, Clone, Copy)]
pub struct MachineRow<'a> {
    /// What to call it. `local` for the daemon with no host badge.
    pub label: &'a str,
    pub sys: &'a SysDto,
    /// Agents this machine is running, across all of its workspaces.
    pub agents: usize,
    /// False while this machine's event stream is down. Its gauges and its
    /// agent count are then the last ones it sent, so they are drawn faint —
    /// a machine that went away must not keep reporting 4% CPU as though it
    /// were still measuring it.
    pub live: bool,
}

/// A stage whose pane connection has dropped, and what to say about it.
///
/// **The cells behind the notice are kept, not cleared.** Dropping them was the
/// old behaviour and it drew a black rectangle, which says "there is nothing
/// here" — the one thing that is not true. The program is still running on the
/// far machine; it is this client that stopped hearing about it. So the last
/// frame stays, dimmed to say it is a photograph, and the notice over it says
/// whose photograph and how old.
#[derive(Debug, Clone, Copy)]
pub struct StageDown<'a> {
    /// What to call the machine that went away. `None` for the local daemon,
    /// which is named "the daemon" rather than by a host that does not exist.
    pub host: Option<&'a str>,
    /// Whole seconds since the connection dropped. The age is the difference
    /// between "a daemon is restarting" and "that machine is not coming back
    /// without you doing something", and only the client can measure it.
    pub secs: u64,
    /// Whether there are any cells under the notice. False when the stage was
    /// opened while the machine was already down, where there is no photograph
    /// to explain and the line about one would be a lie.
    pub has_frame: bool,
}

/// A row of the BOOTH page's fleet column.
///
/// Headers are in the same list as agents so the painter and the hit-test walk
/// one sequence and cannot disagree about which y is which row — the bug that
/// every "draw it twice" list eventually has.
#[derive(Debug, Clone, Copy)]
pub enum BoothRow<'a> {
    Machine {
        label: &'a str,
        agents: usize,
        daemon: usize,
    },
    Space {
        name: &'a str,
    },
    /// `sel` is the row's index among agents *only*, which is what
    /// `all_agents_sel` counts and what `j`/`k` walk.
    Agent {
        row: AllAgentRow<'a>,
        sel: usize,
    },
}

/// Everything one paint reads.
///
/// A struct rather than eight positional arguments: the list had already grown
/// past the point where a call site said what it was passing, and the panel
/// below adds another.
pub struct Scene<'a> {
    pub tabs: &'a [Tab<'a>],
    /// How many daemons this client is connected to, counting the local one.
    ///
    /// Not derivable from `tabs`: a machine that is connected but has no
    /// workspaces open contributes no chip, and that is exactly the machine you
    /// most want to know is there before you open something on it.
    pub daemons: usize,
    /// The workspace behind the active tab, if its detail has arrived.
    pub workspace: Option<&'a WorkspaceDetail>,
    pub system: &'a SysDto,
    /// Every agent on every connected daemon, for the ALL AGENTS panel and for
    /// the BOOTH page's fleet column — the same list at two sizes.
    pub all_agents: &'a [AllAgentRow<'a>],
    /// Every connected daemon and its telemetry, for the BOOTH page's compute
    /// column. Empty on every other page, which asks only `system` and only
    /// about the active tab's machine.
    pub machines: &'a [MachineRow<'a>],
    /// The Files page's contents, when it is the page showing.
    pub files: Option<&'a Files>,
    /// The Docs page's contents — the same shape, filtered to markdown.
    pub docs: Option<&'a Files>,
    /// The diff on the stage, when it is the page showing.
    pub diff: Option<&'a DiffView>,
    /// The Docker page's cursor, when it is the page showing.
    pub docker: Option<&'a Docker>,
    /// The GIT page's refs, history and open commit.
    pub git: Option<&'a Git>,
    /// The SETTINGS page's cursor and the lists it loaded.
    pub settings: Option<&'a Settings>,
    /// The HELP page's topic and reading position. Its text is compiled in, so
    /// unlike every other page here there is nothing to load.
    pub help: Option<&'a Help>,
    /// The USAGE page's roster, and the badge on that page's row of the spaces
    /// menu. Nothing else reads it: the tab bar carried a badge from here for a
    /// while, and does not any more.
    pub usage: Option<&'a usage::Usage>,
    /// Set while the staged pane's connection is down. `None` covers both a
    /// live stage and an empty one: nothing staged is not a failure and must
    /// not be dressed as one.
    pub stage_down: Option<StageDown<'a>>,
}

impl<'a> Scene<'a> {
    /// The workbench with no page contents. Each page fills in its own field
    /// with struct-update syntax, so adding a page does not touch every caller.
    pub fn new(tabs: &'a [Tab<'a>], system: &'a SysDto) -> Self {
        Self {
            tabs,
            daemons: 1,
            workspace: None,
            system,
            all_agents: &[],
            machines: &[],
            files: None,
            docs: None,
            diff: None,
            docker: None,
            git: None,
            settings: None,
            help: None,
            usage: None,
            stage_down: None,
        }
    }
}

/// Which full-screen view of a workspace is showing.
///
/// The stage and the Files page share the middle of the screen; the rails and
/// the tab bar do not move between them, which is the fixed-chrome promise the
/// whole interface rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Page {
    /// Every agent on every connected daemon, the selected one's screen, and
    /// what each machine is doing to itself.
    ///
    /// The only page that spans daemons. The rest are *about a workspace* and
    /// resolve through `active_daemon`, which is why they can stay scoped to one
    /// machine and this one cannot: a file tree merged across four hosts is a
    /// tree where two `src/main.rs` rows are different files.
    ///
    /// **Named for the control booth**, the room at the back of the house the
    /// show is run from: you watch the whole stage from it, the standby board
    /// tells you which department is holding for a go, and you can key into any
    /// one channel without being in the scene. That is this page's three
    /// columns — [`booth_rows`]'s fleet, [`booth_tray`]'s waiting agents, and a
    /// live pane in the middle you can type into.
    ///
    /// It was `home`, which named a position rather than a thing and named the
    /// wrong one: [`Page::Agents`] is `Default`, nothing falls back here, and
    /// the only ways in are `alt-0`, the chip and `alt-w`. A word meaning "where
    /// you land" on the one page you never land on is worse than a vague name.
    /// The same objection that retired `work` — see [`Page::Agents`].
    Booth,
    /// This workspace's agents and processes, its changes, and the pane on the
    /// stage — the page the rails belong to.
    ///
    /// Named for the rail that dominates it, the way `files`, `docker` and
    /// `docs` are named for what is on them. It was `work`, which named nothing:
    /// every page is work. Not to be confused with [`Focus::Agents`], which is
    /// the cursor being *in* that rail — this is the page the rail is on, and
    /// you can be on this page with the cursor anywhere.
    #[default]
    Agents,
    Files,
    /// A unified diff of what the CHANGES cursor named.
    Diff,
    /// The repository itself: its branches, worktrees, stashes and tags beside
    /// a commit graph, with whatever the cursor names shown as a diff.
    ///
    /// **Not a second CHANGES rail, and the difference is the whole design.**
    /// CHANGES is the working tree *right now* — what I changed, stage it,
    /// commit it — and it stays on the AGENTS page beside the agents doing the
    /// changing. This page is the repository *over time and across branches*,
    /// which is a different question and one nothing else on screen answers.
    /// The same split PROCESSES and [`Page::Docker`] already make.
    ///
    /// It follows from that split that nothing here mutates on `Enter`.
    /// `Enter` reads — it scopes the graph, or loads a diff. Checkout, merge
    /// and delete are lettered verbs, exactly as `enter follow` sits beside
    /// `r restart` on the Docker page.
    Git,
    Docker,
    /// The Files page filtered to markdown: a project's own writing, without
    /// the code it is about.
    Docs,
    /// This client's own configuration: its palette, its rails, the agent `a`
    /// spawns, the machines it dials.
    ///
    /// **A page rather than a modal**, for the reason a modal is right for one
    /// question whose whole answer fits on screen and wrong for seven groups of
    /// them. Choosing a palette in particular cannot be done in a box: the only
    /// way to judge one is to see what it does to a screen, and the room to
    /// show that beside the list is exactly what a page has and a modal does
    /// not.
    ///
    /// **Deliberately absent from [`Page::ORDER`]**, for the same reason
    /// [`Page::Booth`] is: that list is the views *of one workspace*, which is
    /// what makes it a list you can walk with one pair of keys. This is about
    /// the client, not the project, so `alt-,` / `alt-.` walk past it and it is
    /// reached from `[settings]` in the footer or `alt-s` — a peer of the
    /// workbench's own controls rather than an entry in a menu of views.
    ///
    /// Settings are per client, and that is correct: the daemon renders no
    /// chrome, so it has no palette and no keymap to hold. The Mac app and this
    /// one do not share a theme any more than they share a font.
    Settings,
    /// butai's own reference: how to get around, what the rails hold, every key.
    ///
    /// **A page for the reason SETTINGS is one, arrived at the same way.** It
    /// was a modal that scrolled without saying so, and then it was the DOCS
    /// page — `?` pointed the file rail at a `butai://reference` folder and
    /// opened a topic in the file viewer, so a press on help rearranged the
    /// file screen into something that was not files. Both failures are the
    /// same failure: the reference was borrowing a surface that belonged to
    /// something else, and inherited that surface's questions (which directory
    /// is this, what is `..`, can I save it).
    ///
    /// Absent from [`Page::ORDER`] on SETTINGS's terms — it is about the
    /// program rather than about a workspace — so it is entered and left rather
    /// than cycled, and it remembers where it was entered from.
    Help,
    /// Which agent account stops you first, and when it comes back.
    ///
    /// **The one page in [`Page::ORDER`] that is not about the workspace**, and
    /// the tension is worth stating rather than hiding: an account limit spans
    /// every workspace and every machine, which by the same test that put BOOTH
    /// on the tab bar makes it BOOTH-shaped.
    ///
    /// It is in the rail anyway, because the question is asked *while you work*
    /// — you check what is left before starting something long, in the project
    /// you are starting it in — and a page you reach with `alt-,` is a page you
    /// actually check. [`page_badge`] is the other half of the bargain: the one
    /// number that matters follows you onto the pages that are about the
    /// workspace, so the page does not have to be visited to do its job.
    ///
    /// Reads `GET /v1/usage` on arrival and on `r`. No timer — the daemon
    /// samples on its own clock, and nothing here moves between keystrokes.
    Usage,
}

impl Page {
    /// Left-to-right order of the space buttons, and the order `alt-,` /
    /// `alt-.` cycle in.
    ///
    /// Work (terminals and agents) → Files (code) → Docker (running services) →
    /// Docs (reference).
    ///
    /// **`Booth` is deliberately absent**, and this is the list's whole shape.
    /// Every page here is a way of looking at *one workspace*, which is what
    /// makes them a list you can walk with one pair of keys. BOOTH is not: it is
    /// the surface that spans daemons, so it is a peer of the workspace chips
    /// rather than an entry in a list of views, and it lives on the tab bar
    /// beside them ([`tabbar_booth_span`]). Putting it in this list put the
    /// widest question in the product inside a menu of the narrowest ones.
    ///
    /// It is also deliberately not [`Default`]: landing somewhere new is a
    /// change to every existing session's first screen, and that is the user's
    /// call, not this list's.
    ///
    /// **`Diff` is deliberately absent.** A diff is not a place, it is what is
    /// on the stage: `f73e59d`'s `open_diff` set `ws.stage` to the diff pane and
    /// said so — *"Diff takes the stage; focus stays on the changes rail"* — and
    /// staging anything else replaced it. Giving it a button made it a space
    /// between `files` and `docker`, which reads as somewhere you navigate to
    /// rather than something you are looking at, and it is why clicking a shell
    /// while a diff was open appeared to do nothing.
    ///
    /// [`Page::Diff`] still exists, because the client does draw the diff in the
    /// stage's place. It is reached from the CHANGES rail and left by staging
    /// anything, which is the behaviour it always had.
    pub const ORDER: [Page; 6] =
        [Page::Agents, Page::Files, Page::Git, Page::Docker, Page::Docs, Page::Usage];

    /// Short lowercase label on the space button.
    pub fn label(self) -> &'static str {
        match self {
            Page::Booth => "booth",
            Page::Agents => "agents",
            Page::Files => "files",
            Page::Diff => "diff",
            Page::Git => "git",
            Page::Docker => "docker",
            Page::Docs => "docs",
            Page::Settings => "settings",
            Page::Help => "help",
            Page::Usage => "usage",
        }
    }

    fn order_index(self) -> usize {
        Page::ORDER.iter().position(|p| *p == self).unwrap_or(0)
    }

    pub fn next(self) -> Page {
        Page::ORDER[(self.order_index() + 1) % Page::ORDER.len()]
    }

    pub fn prev(self) -> Page {
        Page::ORDER[(self.order_index() + Page::ORDER.len() - 1) % Page::ORDER.len()]
    }

    /// Whether this page owns the middle of the screen with a list and a body
    /// — the two that share [`draw_files_page`].
    pub fn is_tree(self) -> bool {
        matches!(self, Page::Files | Page::Docs)
    }

    /// Whether this page is one of the *spaces* — a view of the workspace you
    /// are in, and so a row in the tab bar's spaces menu.
    ///
    /// Everywhere but BOOTH, SETTINGS and HELP. None of the three is a page
    /// *about* a workspace — one spans daemons, one is about this client, one is
    /// the reference — so all three are peers of the workspace tabs rather than
    /// entries in a list of views. You leave them the way you arrived: the tab
    /// bar, which never moves.
    ///
    /// The menu's trigger says `views` rather than a page name while one of them
    /// is up, so the control never claims you are in a space you are not.
    pub fn is_space(self) -> bool {
        Page::ORDER.contains(&self)
    }

    /// Whether this page takes the whole band between the tab bar and the
    /// footer, rather than sitting on the stage between the rails.
    ///
    /// The test is what the page is *about*. WORK is about the agents and the
    /// changes, so the rails describing them are the page. A file, a container's
    /// logs and a rendered document are about none of that, and the rails beside
    /// them were answering a question nobody asked while the body they crowded
    /// got less width than the two of them put together.
    ///
    /// DIFF stays on the stage deliberately: it is what is *on* the stage rather
    /// than somewhere you navigate to, and the CHANGES rail beside it is how you
    /// walk to the next file.
    pub fn owns_full_width(self) -> bool {
        matches!(
            self,
            Page::Booth
                | Page::Files
                | Page::Docs
                | Page::Docker
                | Page::Git
                | Page::Settings
                | Page::Help
                | Page::Usage
        )
    }

    /// Whether this page draws the pane a keystroke may be typed into.
    ///
    /// WORK and BOOTH. Every other page puts something of its own where the
    /// stage was — a tree, a graph, a diff, a form — and none of those is a
    /// terminal. [`Focus::Stage`] still occurs on them (the GIT page spends it
    /// on the COMMIT body), so a key the page itself does not consume has to
    /// stop rather than reach a pane: forwarding it typed into a shell that was
    /// not on screen, and on the GIT page that meant `enter`, `r` and `g` ran
    /// as blind shell commands instead of as the page's own verbs.
    ///
    /// **BOOTH is the exception to [`owns_full_width`](Self::owns_full_width),
    /// and it is a real one rather than a hedge.** The other full-width pages
    /// replaced the stage; BOOTH re-carves one in the middle of the band it took
    /// — [`booth_columns`] hands it a rectangle, [`stage_rect`] is already
    /// telling the daemon to size the pane to it, and a click in it already
    /// lands on [`Focus::Stage`] and forwards the mouse. Only the keyboard
    /// stopped, which made the middle column a screen you could point at and
    /// scroll and click and not type into — and the keys did not stop harmlessly
    /// either, they fell through to the global table, so `q` on a preview
    /// detached the client.
    pub fn draws_stage(self) -> bool {
        matches!(self, Page::Agents | Page::Booth)
    }
}

/// Whether a tree entry belongs on the Docs page.
///
/// Markdown and READMEs, plus every directory except the two that are only ever
/// full of build output.
///
/// **This used to be the definition, and the filter used to run here.** It is
/// the daemon's now — `?filter=docs` on the tree route — because the `●`
/// markers are decided there, over the whole change set, and a page that
/// filtered the rows afterwards kept directories marked for files it had just
/// dropped. Re-exported rather than deleted so there is still one name for the
/// rule in this crate, and exactly one body behind it.
pub use butai_protocol::api::is_doc;

/// The Docker page's own state: where the cursor is, and which container's logs
/// are being followed.
///
/// The logs are a *process pane* — the client asks the daemon to run
/// `docker logs -f` and then streams that pane like any other. There is no
/// docker-logs message on the wire and there does not need to be one: following
/// a log is running a program and watching its output, which the protocol has
/// always been able to express. It is how the Mac client does it too.
#[derive(Debug, Clone, Default)]
pub struct Docker {
    pub sel: usize,
    /// The pane running `docker logs -f`, once one has been started.
    pub logs: Option<PaneId>,
    /// What that pane is following, for the box title.
    pub following: Option<String>,
}

impl Docker {
    pub fn move_sel(&mut self, delta: isize, len: usize) {
        if len == 0 {
            self.sel = 0;
            return;
        }
        let next = self.sel as isize + delta;
        self.sel = next.clamp(0, len as isize - 1) as usize;
    }
}

/// What the GIT page's history is a history *of*.
///
/// Held rather than recomputed because it is what the next page of log is
/// fetched with, and because the box title says it: a graph scoped to one
/// branch and one scoped to everything look alike until you read the header.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum GitScope {
    /// Every branch, tag and remote — `?all=1`. The default, because the page
    /// exists to show the branches you are *not* on.
    #[default]
    Everything,
    /// One ref, by name — `?rev=`.
    Ref(String),
}

impl GitScope {
    /// The query string this scope walks with, `?`-less.
    ///
    /// One function so the fetch and the title cannot disagree about what is
    /// being shown, the same reason [`staged_pane`] exists.
    pub fn query(&self) -> String {
        match self {
            GitScope::Everything => "all=1".into(),
            GitScope::Ref(name) => format!("rev={}", crate::workbench::urlencode(name)),
        }
    }

    pub fn label(&self) -> &str {
        match self {
            GitScope::Everything => "all refs",
            GitScope::Ref(name) => name,
        }
    }
}

/// Everything the GIT page draws, and where its two cursors are.
///
/// The DTOs are held whole rather than flattened into rows: `ref_rows` builds
/// the flat list on demand, so there is exactly one place that decides what the
/// rail contains and the cursor can never index a list that was built
/// differently — the discipline [`change_rows`] set.
#[derive(Debug, Clone, Default)]
pub struct Git {
    pub branches: Option<BranchesDto>,
    pub tags: Vec<String>,
    pub stashes: Vec<StashDto>,
    pub remotes: Vec<RemoteDto>,
    pub worktrees: Vec<WorktreeDto>,
    /// One page of history, newest first, in topological order.
    pub log: Vec<LogEntryDto>,
    /// Another page exists after this one.
    pub more: bool,
    pub scope: GitScope,
    pub refs_sel: usize,
    pub hist_sel: usize,
    /// The commit on the right, once one has been opened.
    pub body: Option<DiffView>,
    /// Set while a fetch is in flight, so the page says "loading" instead of
    /// "this repository has no branches".
    pub loaded: bool,
}

/// One row of the REFS list, headings included.
///
/// Flat with headings in it, and *the same list* is what the cursor walks and
/// what `Enter` reads — see [`change_rows`], which is where this pattern and
/// the reason for it were written down.
#[derive(Debug, Clone, PartialEq)]
pub enum RefRow<'a> {
    Header(&'a str),
    /// The uncommitted work: a summary row, and the head of the section the
    /// files below it belong to.
    WorkingTree {
        dirty: usize,
    },
    /// One changed file, on whichever side of the index it sits.
    ///
    /// The same [`ChangeRow`] the CHANGES rail draws, deliberately: the rail
    /// keeps every verb it had, and this page shows the same rows under the same
    /// headings so that "stage this file" is one gesture wherever you meet it. A
    /// second row model for the same eight fields is two things that drift.
    Change(ChangeRow<'a>),
    Branch {
        entry: &'a BranchDto,
        current: bool,
        /// The worktree this branch is checked out in, when it is not this one
        /// — cross-referenced from [`Git::worktrees`] rather than asked for,
        /// because the daemon already answers it once.
        elsewhere: Option<&'a str>,
    },
    Remote {
        name: &'a str,
        url: &'a str,
    },
    Tag(&'a str),
    Stash(&'a StashDto),
    Worktree {
        dto: &'a WorktreeDto,
        /// This is the checkout the page is looking at.
        here: bool,
    },
}

/// Lay the REFS list out as rows, in the order they are drawn.
///
/// `here` is the workspace this page belongs to, so the checkout it is open on
/// can be marked rather than offered as somewhere to go. An id and not a path
/// on purpose: `git worktree list` reports libgit2's canonical spelling
/// (`/private/var/…`) while a workspace's cwd is whatever the client passed in
/// (`/var/…`), so comparing the two as strings answers "not here" for the very
/// checkout you are standing in — which then offers `x remove` on it, the row
/// that only ever errors. `WorktreeDto.workspace` is the daemon answering the
/// same question exactly, which is what that field is for.
pub fn ref_rows<'a>(
    git: &'a Git,
    changes: Option<&'a ChangesDto>,
    here: Option<SessionId>,
) -> Vec<RefRow<'a>> {
    let mut rows = Vec::new();

    // The working tree leads, and it is the only part of this list that is about
    // *now*. It used to be a single row whose `Enter` sent you to the CHANGES
    // rail; the files are here as well now, with the rail's own verbs on them,
    // so staging is something you can do on the page where you are reading the
    // diff rather than somewhere you have to be sent.
    //
    // The rail is untouched by this — it is still where staging lives while an
    // agent is working, and it keeps every verb it had. These are the same
    // `ChangeRow`s under the same headings, which is what stops the two being
    // two products.
    if let Some(c) = changes {
        let dirty = c.staged.len() + c.unstaged.len() + c.conflicted.len();
        rows.push(RefRow::WorkingTree { dirty });
        // A heading is held back until something turns up under it. The rail's
        // `Commits` section is dropped here — the whole log is in the box
        // directly below, so those would be the same commits twice, ten rows
        // apart — and emitting its heading anyway left a `Commits` label over
        // nothing, which is a row the cursor can land on that says nothing.
        let mut pending: Option<&str> = None;
        for row in change_rows(c) {
            match row {
                ChangeRow::Header(name) => pending = Some(name),
                ChangeRow::Commit { .. } => {}
                file => {
                    if let Some(name) = pending.take() {
                        rows.push(RefRow::Header(name));
                    }
                    rows.push(RefRow::Change(file));
                }
            }
        }
    }

    let current = git.branches.as_ref().and_then(|b| b.current.as_deref());
    let entries = git.branches.as_ref().map(|b| b.entries.as_slice()).unwrap_or(&[]);

    let locals: Vec<&BranchDto> = entries.iter().filter(|e| !e.remote).collect();
    if !locals.is_empty() {
        rows.push(RefRow::Header("Branches"));
        for entry in locals {
            // A branch checked out in another worktree cannot be checked out
            // here, so the row says where it went instead of offering a verb
            // that would fail.
            let elsewhere = git
                .worktrees
                .iter()
                .find(|w| {
                    w.branch.as_deref() == Some(entry.name.as_str())
                        && !(here.is_some() && w.workspace == here)
                })
                .map(|w| w.path.as_str());
            rows.push(RefRow::Branch {
                entry,
                current: Some(entry.name.as_str()) == current,
                elsewhere,
            });
        }
    }

    let remotes: Vec<&BranchDto> = entries.iter().filter(|e| e.remote).collect();
    if !remotes.is_empty() {
        rows.push(RefRow::Header("Remote branches"));
        rows.extend(remotes.into_iter().map(|entry| RefRow::Branch {
            entry,
            current: false,
            elsewhere: None,
        }));
    }
    if !git.remotes.is_empty() {
        rows.push(RefRow::Header("Remotes"));
        rows.extend(git.remotes.iter().map(|r| RefRow::Remote { name: &r.name, url: &r.url }));
    }
    if !git.tags.is_empty() {
        rows.push(RefRow::Header("Tags"));
        rows.extend(git.tags.iter().map(|t| RefRow::Tag(t)));
    }
    if !git.stashes.is_empty() {
        rows.push(RefRow::Header("Stashes"));
        rows.extend(git.stashes.iter().map(RefRow::Stash));
    }
    // One worktree is just "this repository"; the section earns its rows only
    // once there is somewhere else to go.
    if git.worktrees.len() > 1 {
        rows.push(RefRow::Header("Worktrees"));
        rows.extend(
            git.worktrees
                .iter()
                .map(|dto| RefRow::Worktree { dto, here: here.is_some() && dto.workspace == here }),
        );
    }
    rows
}

/// Which kind of row a REFS index names, in the verb table's vocabulary.
///
/// The table keys off *what is selected* — the whole point of it. A branch
/// already checked out somewhere else must not be offered `c checkout`, because
/// git refuses to check one out twice and the row would be advertising a
/// failure.
pub fn ref_row_kind(rows: &[RefRow<'_>], sel: usize) -> crate::verbs::GitRow {
    use crate::verbs::GitRow;
    match rows.get(sel) {
        Some(RefRow::WorkingTree { .. }) => GitRow::WorkingTree,
        Some(RefRow::Change(ChangeRow::Conflicted { .. })) => GitRow::ChangeConflicted,
        Some(RefRow::Change(ChangeRow::File { staged: true, .. })) => GitRow::ChangeStaged,
        Some(RefRow::Change(ChangeRow::File { staged: false, .. })) => GitRow::ChangeUnstaged,
        // A heading or a commit cannot reach here — `ref_rows` unwraps the one
        // and drops the other — but the match has to be total, and a row with no
        // verbs is the honest answer for anything that ever does.
        Some(RefRow::Change(_)) => GitRow::None,
        Some(RefRow::Branch { entry, current, elsewhere }) => {
            if *current {
                GitRow::CurrentBranch
            } else if elsewhere.is_some() {
                GitRow::BranchElsewhere
            } else if entry.remote {
                GitRow::RemoteBranch
            } else {
                GitRow::Branch
            }
        }
        Some(RefRow::Remote { .. }) => GitRow::Remote,
        Some(RefRow::Tag(_)) => GitRow::Tag,
        Some(RefRow::Stash(_)) => GitRow::Stash,
        Some(RefRow::Worktree { here: true, .. }) => GitRow::ThisWorktree,
        Some(RefRow::Worktree { .. }) => GitRow::Worktree,
        Some(RefRow::Header(_)) | None => GitRow::None,
    }
}

/// How a GIT list box splits between its rows and its verb footer.
///
/// Both the drawing and the hit test go through this, so a click on a verb
/// cannot land on the row above it — the rule [`changes_split`] already sets.
pub fn git_split(rows: LRect, verbs: &[Verb]) -> (u16, u16) {
    let footer = crate::verbs::rows_needed(verbs, rows.width as usize, 2) as u16;
    let footer = footer.min(rows.height.saturating_sub(1));
    (rows.height - footer, footer)
}

impl Git {
    /// Move a cursor within a list, clamped. Shared by both lists because
    /// "walk a list" is one behaviour, not two.
    pub fn move_in(sel: &mut usize, delta: isize, len: usize) {
        if len == 0 {
            *sel = 0;
            return;
        }
        *sel = (*sel as isize + delta).clamp(0, len as isize - 1) as usize;
    }

    /// The commit the HISTORY cursor is on.
    pub fn commit(&self) -> Option<&LogEntryDto> {
        self.log.get(self.hist_sel)
    }
}

/// A docker stack as the page shows it: the DTO, plus whether it belongs to
/// this workspace.
#[derive(Debug, Clone, Copy)]
pub struct Stack<'a> {
    pub dto: &'a StackDto,
    /// The compose project's working directory is at, under or over the
    /// workspace's cwd.
    pub mine: bool,
}

impl Stack<'_> {
    /// Whether this stack's header row has container rows under it.
    ///
    /// False for a one-container stack, whose header *is* the container — which
    /// is why that row wears a container's status dot rather than a compose
    /// project's marker. The row model and the marker both turn on this, so it
    /// is one predicate instead of `containers.len() > 1` spelled twice.
    pub fn expands(&self) -> bool {
        self.dto.containers.len() > 1
    }
}

/// The stacks this workspace's Docker page shows.
///
/// `SysDto.stacks` arrives grouped but unsorted and unfiltered — its doc says
/// so, and says the client is the one holding the workspace cwd to judge
/// "mine" against. So this is the client's half of a split the API already
/// documented, not a rule invented here.
///
/// Stopped stacks are dropped and, when anything belongs to this project,
/// everything else is: a page listing every container on the machine is a
/// machine inspector, and this is a workbench. When *nothing* matches it falls
/// back to showing them all, so the page is never mysteriously empty.
pub fn project_stacks<'a>(sys: &'a SysDto, cwd: &str) -> Vec<Stack<'a>> {
    let cwd = std::path::Path::new(cwd);
    let mut stacks: Vec<Stack<'a>> = sys
        .stacks
        .iter()
        .filter(|s| s.running > 0)
        .map(|dto| {
            let mine = !dto.workdir.is_empty() && {
                let wd = std::path::Path::new(&dto.workdir);
                cwd.starts_with(wd) || wd.starts_with(cwd)
            };
            Stack { dto, mine }
        })
        .collect();
    stacks.sort_by(|a, b| b.mine.cmp(&a.mine).then_with(|| a.dto.label.cmp(&b.dto.label)));
    if stacks.iter().any(|s| s.mine) {
        stacks.retain(|s| s.mine);
    }
    stacks
}

/// A selectable row on the Docker page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DockerRow<'a> {
    /// A stack header. The value indexes the stacks slice it came from.
    Stack(usize),
    Container {
        stack: usize,
        name: &'a str,
        running: bool,
    },
}

/// Flatten stacks into their rows: each header, then its containers.
///
/// A one-container stack is not expanded — its header *is* the container, and
/// listing it twice would make every standalone container two identical rows.
pub fn docker_rows<'a>(stacks: &[Stack<'a>]) -> Vec<DockerRow<'a>> {
    let mut rows = Vec::new();
    for (i, s) in stacks.iter().enumerate() {
        rows.push(DockerRow::Stack(i));
        if s.expands() {
            for c in &s.dto.containers {
                rows.push(DockerRow::Container {
                    stack: i,
                    name: &c.name,
                    running: c.state == "running",
                });
            }
        }
    }
    rows
}

/// The Files page's contents, fetched from `/v1/*` and drawn here.
///
/// A directory listing and, when one is open, an editor. The daemon used to own
/// a `FileTreePane` and an `EditorPane` with their own expansion, cursor,
/// scroll and text buffer, and render both into cells; none of that is state
/// the daemon can act on, so all of it lives here now.
#[derive(Debug, Default)]
pub struct Files {
    /// Directory being listed, relative to the workspace root (`""` = root).
    pub dir: String,
    pub entries: Vec<FileEntry>,
    pub sel: usize,
    /// The open file, if there is one.
    pub open: Option<Editor>,
}

/// One row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    /// Git-changed, or containing something changed — the yellow marker.
    pub changed: bool,
}

/// Whether keys go to the buffer or to the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditMode {
    #[default]
    View,
    Edit,
}

/// One open file: its text, a cursor into it, and whether it has been changed.
///
/// **The buffer lives here now, and that is a deliberate trade.** The daemon's
/// `EditorPane` kept it server-side so unsaved edits survived a detach; a
/// client that owns its buffer loses that, and gains being an ordinary client —
/// the Mac and web clients never had server-side buffers either. What replaces
/// the guarantee is a refusal: closing a changed file needs the key twice, and
/// the second press is the one that discards.
#[derive(Debug)]
pub struct Editor {
    pub path: String,
    /// The text, with cursor, selection and undo. Editing is a solved widget
    /// problem and this is the same one the daemon used.
    pub area: TextArea<'static>,
    lang: Lang,
    /// The buffer as tokens, recomputed whenever the text changes. Held rather
    /// than computed per paint because a block comment's state depends on every
    /// line above it, so highlighting row 900 means highlighting rows 1..900.
    highlighted: Vec<Vec<(Token, String)>>,
    pub mode: EditMode,
    /// Changed since the last load or save.
    pub dirty: bool,
    /// A close was refused because the buffer was dirty; the next one discards.
    pub discard_armed: bool,
    /// View-mode scroll. Edit mode scrolls with the cursor, inside the widget.
    pub scroll: usize,
    /// The daemon stopped reading at its cap; there is more on disk.
    pub truncated: bool,
    pub notice: Option<String>,
}

impl Editor {
    pub fn new(path: String, text: &str, truncated: bool) -> Self {
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        let lang = Lang::of(&path);
        let mut area = TextArea::new(lines.clone());
        // The widget's default underlines the line the cursor is on, which
        // reads as a selection in a terminal. The cursor itself is a reversed
        // cell, which is the only thing marking it — the real terminal cursor
        // is parked on the streamed pane.
        area.set_cursor_line_style(Style::default());
        area.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
        Self {
            path,
            area,
            lang,
            highlighted: Highlighter::lines(lang, &lines),
            mode: EditMode::View,
            dirty: false,
            discard_armed: false,
            scroll: 0,
            truncated,
            notice: None,
        }
    }

    pub fn lines(&self) -> &[String] {
        self.area.lines()
    }

    /// Whether this buffer may be edited at all.
    ///
    /// A truncated read is a *prefix* of the file. Saving one would silently
    /// throw away everything past the daemon's cap, so the file opens read-only
    /// and says why.
    pub fn editable(&self) -> bool {
        !self.truncated
    }

    /// Enter edit mode, or explain why not.
    pub fn edit(&mut self) {
        if !self.editable() {
            self.notice = Some("read-only: only part of this file was read".into());
            return;
        }
        self.mode = EditMode::Edit;
        self.notice = None;
    }

    /// Leave edit mode, bringing the view to where the cursor was left.
    pub fn stop_editing(&mut self) {
        self.mode = EditMode::View;
        self.scroll = self.area.cursor().0;
        self.rehighlight();
    }

    /// Record that the buffer changed.
    pub fn touch(&mut self) {
        self.dirty = true;
        self.discard_armed = false;
        self.notice = None;
        self.rehighlight();
    }

    fn rehighlight(&mut self) {
        self.highlighted = Highlighter::lines(self.lang, self.area.lines());
    }

    /// The bytes to write, as the file should end up on disk.
    pub fn contents(&self) -> String {
        let mut text = self.area.lines().join("\n");
        text.push('\n');
        text
    }

    /// Whether it is safe to close. A dirty buffer refuses once and arms
    /// itself, so the keystroke that discards work is never the first one.
    pub fn may_close(&mut self) -> bool {
        if !self.dirty || self.discard_armed {
            return true;
        }
        self.discard_armed = true;
        self.notice = Some("unsaved changes — press again to discard, C-s to save".into());
        false
    }

    /// Record a successful write.
    pub fn saved(&mut self) {
        self.dirty = false;
        self.discard_armed = false;
        self.notice = Some("saved".into());
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.lines().len().saturating_sub(1) as isize;
        self.scroll = (self.scroll as isize + delta).clamp(0, max.max(0)) as usize;
    }
}

impl Files {
    pub fn move_sel(&mut self, delta: isize) {
        if self.entries.is_empty() {
            self.sel = 0;
            return;
        }
        let next = self.sel as isize + delta;
        self.sel = next.clamp(0, self.entries.len() as isize - 1) as usize;
    }

    pub fn selected(&self) -> Option<&FileEntry> {
        self.entries.get(self.sel)
    }

    /// The parent of the directory being listed, or `None` at the root.
    pub fn parent(&self) -> Option<String> {
        parent_of(&self.dir)
    }
}

/// The directory above `dir`, or `None` when `dir` is the workspace root.
///
/// Shared by `Backspace` and by the `..` row the listing puts at the top, so the
/// key and the row cannot disagree about where up is. The root is `""` and has
/// no parent on purpose: walking up from it would leave the workspace, which is
/// the one place a project tree must not go.
pub fn parent_of(dir: &str) -> Option<String> {
    if dir.is_empty() {
        return None;
    }
    Some(dir.rsplit_once('/').map(|(head, _)| head.to_string()).unwrap_or_default())
}

/// What a diff is *of*, which is what decides where staging sends it.
///
/// The three cases are one operation with two booleans — `Index` or `Worktree`,
/// forwards or backwards — which is why they share a path here rather than
/// being written out three times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffKind {
    /// Worktree vs index. `Space` stages, `x` discards.
    Unstaged { path: Option<String> },
    /// Index vs HEAD. The same key unstages.
    Staged { path: Option<String> },
    /// A commit. History: nothing here can be staged.
    Commit { id: String, summary: String },
}

impl DiffKind {
    /// Whether this diff is something that can be staged at all. A commit's
    /// diff is history — there is no index side to move it to.
    pub fn mutable(&self) -> bool {
        !matches!(self, DiffKind::Commit { .. })
    }

    pub fn staged(&self) -> bool {
        matches!(self, DiffKind::Staged { .. })
    }

    /// The title the diff box wears, matching what the daemon's pane called
    /// itself so the screen does not change under the reader.
    pub fn title(&self) -> String {
        match self {
            DiffKind::Unstaged { path: Some(p) } => format!("diff {p}"),
            DiffKind::Unstaged { path: None } => "diff (unstaged)".into(),
            DiffKind::Staged { path: Some(p) } => format!("diff --staged {p}"),
            DiffKind::Staged { path: None } => "diff --staged".into(),
            DiffKind::Commit { id, summary } => {
                let short: String = id.chars().take(7).collect();
                format!("commit {short} {summary}")
            }
        }
    }
}

/// Whether `Space` takes a whole hunk or individual lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffMode {
    #[default]
    Read,
    Lines,
}

/// Where a display row sits in the patch: `(file, hunk, line-within-hunk)`.
type Anchor = (usize, usize, usize);

/// One drawn row of a diff.
///
/// A row rather than a string because this view draws four things on one line —
/// the two line numbers, the marker and the text — each in its own colour, and a
/// renderer handed `"+    let x = 1;"` cannot put the numbers back. One
/// `DiffRow` is exactly one row of the screen, which is what lets [`DiffView`]'s
/// `scroll`, `anchors` and `cursor_row` keep indexing the same list they always
/// did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffRow {
    /// A file's header: what it is called and how much of it moved.
    File {
        path: String,
        added: usize,
        removed: usize,
        /// `new file`, `deleted`, `renamed` — whatever the header said happened
        /// to the file itself, as opposed to its contents.
        note: Option<String>,
        folded: bool,
    },
    /// The rule under a file header. Its own row so that one `DiffRow` is one
    /// screen row everywhere; the alternative is a header that is sometimes two
    /// rows tall, which every scroll calculation would then have to know about.
    Rule,
    /// A `@@` separator, with the enclosing section git named.
    Hunk { old: usize, new: usize, section: String },
    /// A line of the file, numbered on whichever side it exists in.
    Line { old: Option<usize>, new: Option<usize>, origin: Origin, text: String },
    /// Something with no place in either file: `\ No newline at end of file`, or
    /// the placeholder an empty diff stands in for itself.
    Note(String),
}

/// Where the cursor is, as a position in the patch rather than a screen row —
/// so it survives a refresh that reflows the text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffCursor {
    pub file: usize,
    pub hunk: usize,
}

/// A unified diff, and a cursor into it.
///
/// A *view over* [`Patch`] rather than a list of strings: the rows on screen,
/// the hunk under the cursor and the subset that gets staged are all the same
/// parse, so the hunk you are looking at is the hunk that moves. That was true
/// of the daemon's `DiffPane` and it is the reason this is a port rather than a
/// rewrite — only the two ends changed. The patch text arrives from
/// `GET .../diff`, and the piece the cursor names goes back to
/// `POST .../git/apply`. Nothing in between is the daemon's business.
#[derive(Debug, Clone, Default)]
pub struct DiffView {
    pub kind: Option<DiffKind>,
    pub patch: Patch,
    /// Display rows, exactly as `patch` renders them.
    pub rows: Vec<DiffRow>,
    /// Maps a display row back to the patch. `None` for file headers, rules and
    /// hunk separators, which belong to no line.
    anchors: Vec<Option<Anchor>>,
    /// Files whose hunks are hidden, by index into `patch.files`. A many-file
    /// diff is unreadable without this — the working tree's is the case that
    /// made it necessary — and it is the one piece of view state that survives
    /// a re-read, because staging a hunk should not reopen every file you shut.
    folded: BTreeSet<usize>,
    /// Digits the widest line number in this patch needs, so the gutter is as
    /// narrow as this particular diff allows rather than as wide as any diff
    /// might be.
    digits: u16,
    pub scroll: usize,
    pub cursor: DiffCursor,
    /// Selected line indices within the cursor's hunk. Empty outside
    /// [`DiffMode::Lines`].
    picked: Vec<usize>,
    /// Which line-select cursor is on, as an index into the hunk's changed
    /// lines.
    line_cursor: usize,
    pub mode: DiffMode,
    pub notice: Option<String>,
    /// Rows the diff body has on screen, so paging and "scroll the cursor into
    /// view" match what is actually visible.
    ///
    /// Told to the view rather than measured during the paint: the paint takes
    /// `&self`, and threading a measurement back out of it would make the
    /// scroll depend on having painted once already.
    view_rows: u16,
}

impl DiffView {
    /// A diff of `kind`, from the patch text the daemon printed.
    pub fn new(kind: DiffKind, patch_text: &str) -> Self {
        let mut d = Self { kind: Some(kind), view_rows: 24, ..Self::default() };
        d.set_patch(patch_text);
        d
    }

    /// Re-read the patch, keeping the cursor where it can still stand.
    ///
    /// Called after every apply: staging a hunk removes it from the diff, so
    /// without the clamp the cursor walks off the end of the file it was just
    /// working on.
    pub fn set_patch(&mut self, text: &str) {
        self.patch = Patch::parse(text);
        // A file that is no longer in the diff must not keep a fold: the set is
        // indexed by position, so leaving a stale index in it folds whichever
        // file slid into that slot when the one above was staged away.
        self.folded.retain(|f| *f < self.patch.files.len());
        self.digits = line_number_digits(&self.patch);
        self.relayout();
        self.clamp_cursor();
    }

    /// Rebuild the display rows from the patch and the folds. Split out because
    /// folding changes the rows without changing the patch, and re-parsing to
    /// close a file would throw away the cursor for no reason.
    fn relayout(&mut self) {
        let (rows, anchors) = render_rows(&self.patch, &self.folded);
        if rows.is_empty() {
            self.patch = Patch::default();
            self.anchors = vec![None];
            self.rows = vec![DiffRow::Note("(no differences)".to_string())];
        } else {
            self.rows = rows;
            self.anchors = anchors;
        }
    }

    /// Open or shut the file the cursor is in.
    ///
    /// Nothing else about the cursor moves: the hunk it names is still the hunk
    /// `Space` stages, so folding a file you are working in and unfolding it
    /// again puts you back exactly where you were.
    pub fn toggle_fold(&mut self) {
        if self.patch.files.is_empty() {
            return;
        }
        let f = self.cursor.file;
        if !self.folded.insert(f) {
            self.folded.remove(&f);
        }
        self.relayout();
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(1));
        self.scroll_to_cursor();
    }

    /// Shut every file, or open every file — whichever leaves more of the diff
    /// visible than it is now. On a twenty-file working tree this is the only
    /// affordable way to get an overview.
    pub fn toggle_fold_all(&mut self) {
        let all = 0..self.patch.files.len();
        if self.folded.len() == self.patch.files.len() {
            self.folded.clear();
        } else {
            self.folded = all.collect();
        }
        self.relayout();
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(1));
        self.scroll_to_cursor();
    }

    /// Cells of chrome before a line's text: the cursor marker, and the two
    /// line-number columns when the body is wide enough to spare them.
    ///
    /// Public because a drag-selection has to start after it — line numbers are
    /// chrome drawn inside the text, and a copy that took them pastes code that
    /// does not compile. `crate::selection` asks this rather than reading a
    /// constant, because the answer depends on this patch and this box.
    pub fn gutter_w(&self, inner_w: u16) -> u16 {
        DIFF_GUTTER_W + self.numbers_w(inner_w)
    }

    /// Width of the `old new │` block, or zero when the body cannot spare it.
    ///
    /// The numbers are the first thing given up on a narrow body, for the reason
    /// the commit graph gives up its lanes: they are an orientation aid over the
    /// text, and a column of them that leaves the text twenty cells wide has
    /// taken more than it gave.
    fn numbers_w(&self, inner_w: u16) -> u16 {
        let w = 2 * self.digits + 3;
        if inner_w >= DIFF_GUTTER_W + w + DIFF_TEXT_MIN_W {
            w
        } else {
            0
        }
    }

    fn clamp_cursor(&mut self) {
        if self.patch.files.is_empty() {
            self.cursor = DiffCursor::default();
            self.picked.clear();
            self.mode = DiffMode::Read;
            return;
        }
        self.cursor.file = self.cursor.file.min(self.patch.files.len() - 1);
        let hunks = self.patch.files[self.cursor.file].hunks.len();
        if hunks == 0 {
            self.cursor.hunk = 0;
        } else if self.cursor.hunk >= hunks {
            self.cursor.hunk = hunks - 1;
        }
        let changed = self.changed_indices().len();
        if changed == 0 {
            self.mode = DiffMode::Read;
        }
        self.line_cursor = self.line_cursor.min(changed.saturating_sub(1));
        let hunk_lines = self.current_hunk().map_or(0, |h| h.lines.len());
        self.picked.retain(|i| *i < hunk_lines);
        self.scroll = self.scroll.min(self.rows.len().saturating_sub(1));
    }

    fn current_hunk(&self) -> Option<&Hunk> {
        self.patch.files.get(self.cursor.file)?.hunks.get(self.cursor.hunk)
    }

    /// The selectable (added/removed) line indices of the hunk under the cursor.
    fn changed_indices(&self) -> Vec<usize> {
        self.current_hunk().map(|h| h.changed_line_indices()).unwrap_or_default()
    }

    fn mutable(&self) -> bool {
        self.kind.as_ref().is_some_and(DiffKind::mutable)
    }

    /// Move the cursor `delta` hunks, across file boundaries, and bring it into
    /// view.
    pub fn step_hunk(&mut self, delta: isize) {
        let flat: Vec<DiffCursor> = self
            .patch
            .files
            .iter()
            .enumerate()
            .flat_map(|(f, file)| {
                (0..file.hunks.len()).map(move |h| DiffCursor { file: f, hunk: h })
            })
            .collect();
        if flat.is_empty() {
            return;
        }
        let at = flat.iter().position(|c| *c == self.cursor).unwrap_or(0);
        let next = (at as isize + delta).clamp(0, flat.len() as isize - 1) as usize;
        self.cursor = flat[next];
        self.mode = DiffMode::Read;
        self.picked.clear();
        self.line_cursor = 0;
        // Stepping into a file you had shut opens it. The alternative is a
        // cursor sitting on a hunk that is not on screen, with `Space` about to
        // stage something you cannot see.
        if self.folded.remove(&self.cursor.file) {
            self.relayout();
        }
        self.scroll_to_cursor();
    }

    /// Drop into line-select, so `Space` takes individual lines.
    pub fn line_select(&mut self) {
        if self.changed_indices().is_empty() {
            self.notice = Some("nothing to pick in this hunk".into());
            return;
        }
        self.mode = DiffMode::Lines;
        self.line_cursor = 0;
        self.picked.clear();
        self.scroll_to_cursor();
    }

    pub fn cancel_line_select(&mut self) {
        self.mode = DiffMode::Read;
        self.picked.clear();
    }

    /// Move the line-select cursor within the hunk.
    pub fn step_line(&mut self, delta: isize) {
        let n = self.changed_indices().len();
        if n == 0 {
            return;
        }
        let next = self.line_cursor as isize + delta;
        self.line_cursor = next.clamp(0, n as isize - 1) as usize;
        self.scroll_to_cursor();
    }

    /// Take or drop the line under the line-select cursor, then advance — so a
    /// run of lines is Space-Space-Space rather than Space-j-Space-j.
    pub fn pick_line(&mut self) {
        if let Some(&i) = self.changed_indices().get(self.line_cursor) {
            match self.picked.iter().position(|p| *p == i) {
                Some(at) => {
                    self.picked.remove(at);
                }
                None => self.picked.push(i),
            }
        }
        self.step_line(1);
    }

    /// Scroll the body, in rows.
    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.rows.len().saturating_sub(1) as isize;
        self.scroll = (self.scroll as isize + delta).clamp(0, max.max(0)) as usize;
    }

    /// Tell the view how tall its body is. [`diff_body_rows`] computes it from
    /// the same geometry the paint uses.
    pub fn set_view_rows(&mut self, rows: u16) {
        self.view_rows = rows;
    }

    /// One screenful, one row short so a page keeps a line of context.
    pub fn page(&self) -> usize {
        self.view_rows.saturating_sub(1).max(1) as usize
    }

    pub fn scroll_to_end(&mut self) {
        self.scroll = self.rows.len().saturating_sub(1);
    }

    /// The display row the cursor is on: the hunk header in read mode, the
    /// selected line in line-select.
    fn cursor_row(&self) -> Option<usize> {
        let want_line = match self.mode {
            DiffMode::Lines => self.changed_indices().get(self.line_cursor).copied(),
            DiffMode::Read => None,
        };
        let row = match want_line {
            Some(li) => self
                .anchors
                .iter()
                .position(|a| *a == Some((self.cursor.file, self.cursor.hunk, li))),
            // The hunk header is the row just before the hunk's first line.
            None => self
                .anchors
                .iter()
                .position(|a| matches!(a, Some((f, h, _)) if *f == self.cursor.file && *h == self.cursor.hunk))
                .map(|i| i.saturating_sub(1)),
        };
        // A folded file has no anchored rows at all, so the cursor shows on the
        // one row it still has: its header. Without this the highlight vanishes
        // the moment you shut a file, and `scroll_to_cursor` stops working —
        // which reads as the page having lost the cursor entirely.
        row.or_else(|| {
            self.rows
                .iter()
                .enumerate()
                .filter(|(_, r)| matches!(r, DiffRow::File { .. }))
                .nth(self.cursor.file)
                .map(|(i, _)| i)
        })
    }

    fn scroll_to_cursor(&mut self) {
        let Some(row) = self.cursor_row() else { return };
        let h = self.page();
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll + h {
            self.scroll = row + 1 - h;
        }
    }

    /// The patch to send to `git/apply`, and how to apply it: the whole hunk in
    /// read mode, the picked lines in line-select.
    ///
    /// `discard` reverse-applies to the worktree; otherwise it goes to the
    /// index, forwards for an unstaged diff and backwards for a staged one.
    ///
    /// `None` (with a notice set) when there is nothing to send — which is a
    /// refusal, not a failure, so the caller does not need to distinguish them.
    pub fn selection(&mut self, discard: bool) -> Option<(String, ApplyTarget, bool)> {
        self.notice = None;
        let kind = self.kind.clone()?;
        if !kind.mutable() {
            return None;
        }
        // Discard throws away a worktree change, and a staged diff is not
        // showing one — the daemon's rail does not offer the key here, so
        // neither does this. Reverse-applying it to the worktree would undo an
        // edit the reader is looking at from the other side.
        if discard && kind.staged() {
            return None;
        }
        let lines = match self.mode {
            DiffMode::Read => None,
            DiffMode::Lines if self.picked.is_empty() => {
                self.notice = Some("no lines picked".into());
                return None;
            }
            DiffMode::Lines => {
                let mut p = self.picked.clone();
                p.sort_unstable();
                Some(p)
            }
        };
        let sel = Selection { file: self.cursor.file, hunk: self.cursor.hunk, lines };
        let Some(patch) = self.patch.subset(&sel) else {
            self.notice = Some("nothing to apply".into());
            return None;
        };
        Some(if discard {
            (patch, ApplyTarget::Worktree, true)
        } else {
            (patch, ApplyTarget::Index, kind.staged())
        })
    }

    /// The key hints the footer row shows, which change with the mode. The
    /// daemon drew these from its `verbs` table; the labels are the same words
    /// so the muscle memory survives.
    fn hints(&self) -> String {
        // `z` earns a slot only where there is a second file to shut; on a
        // one-file diff it works and says nothing, which is the right trade for
        // a row this crowded.
        let fold = if self.patch.files.len() > 1 { "   z fold" } else { "" };
        if !self.mutable() {
            return format!("] next hunk   [ prev hunk{fold}   r refresh   esc close");
        }
        match self.mode {
            DiffMode::Read => {
                let stage = if self.kind.as_ref().is_some_and(DiffKind::staged) {
                    "space unstage hunk"
                } else {
                    "space stage hunk"
                };
                let discard = if self.kind.as_ref().is_some_and(DiffKind::staged) {
                    ""
                } else {
                    "   x discard hunk"
                };
                format!(
                    "] next hunk   [ prev hunk   {stage}   v pick lines{discard}{fold}   esc close"
                )
            }
            DiffMode::Lines => {
                "j/k line   space pick   enter apply picked   v cancel   esc close".into()
            }
        }
    }
}

/// Lay the patch out as display rows, with each row's place in the model beside
/// it.
///
/// Built from the parsed patch rather than from the diff text, so every row on
/// screen can be pointed back at a `(file, hunk, line)` — which is what lets
/// `Space` mean "this line" instead of "row 27 of some string list".
fn render_rows(patch: &Patch, folded: &BTreeSet<usize>) -> (Vec<DiffRow>, Vec<Option<Anchor>>) {
    let mut rows = Vec::new();
    let mut anchors = Vec::new();
    for (f, file) in patch.files.iter().enumerate() {
        // The four lines git puts above every file — `diff --git`, `index`,
        // `---`, `+++` — are three restatements of the path and a pair of
        // blob hashes. They used to be drawn verbatim, which cost four rows per
        // file to say what one row says better; what is *not* in the path lives
        // on as `note`, so a rename or a new file still announces itself.
        let fold = folded.contains(&f);
        rows.push(DiffRow::File {
            path: file_label(file),
            added: count(file, Origin::Added),
            removed: count(file, Origin::Removed),
            note: file_note(file),
            folded: fold,
        });
        anchors.push(None);
        rows.push(DiffRow::Rule);
        anchors.push(None);
        if fold {
            continue;
        }
        for (h, hunk) in file.hunks.iter().enumerate() {
            rows.push(DiffRow::Hunk {
                old: hunk.old_start,
                new: hunk.new_start,
                section: hunk.section.clone(),
            });
            anchors.push(None);
            // The two sides count independently, which is the whole point of
            // showing both: a `-` line has no number on the new side because it
            // is not in the new file, and an added one has none on the old.
            let (mut old, mut new) = (hunk.old_start, hunk.new_start);
            for (l, line) in hunk.lines.iter().enumerate() {
                let (o, n) = match line.origin {
                    Origin::Context => {
                        let at = (Some(old), Some(new));
                        old += 1;
                        new += 1;
                        at
                    }
                    Origin::Removed => {
                        let at = (Some(old), None);
                        old += 1;
                        at
                    }
                    Origin::Added => {
                        let at = (None, Some(new));
                        new += 1;
                        at
                    }
                    // Not a line of either file — an annotation on the one above
                    // — so it advances neither counter and wears neither number.
                    Origin::NoNewline => (None, None),
                };
                rows.push(DiffRow::Line {
                    old: o,
                    new: n,
                    origin: line.origin,
                    text: line.text.clone(),
                });
                // Anchored even though it cannot be picked: `picked` holds
                // indices into `hunk.lines`, and a row model that skipped one
                // would shift every index after it.
                anchors.push(Some((f, h, l)));
            }
        }
    }
    (rows, anchors)
}

/// How many lines of one origin a file's hunks hold — the `+12 -3` on its
/// header, which is the number a reader actually wants and the one git's own
/// header does not carry.
fn count(file: &FilePatch, origin: Origin) -> usize {
    file.hunks.iter().flat_map(|h| h.lines.iter()).filter(|l| l.origin == origin).count()
}

/// What to call a file: its new name, its old one when it was deleted, and both
/// when it moved.
fn file_label(file: &FilePatch) -> String {
    const NULL: &str = "/dev/null";
    match (file.old_path.as_str(), file.new_path.as_str()) {
        (NULL | "", new) => new.to_string(),
        (old, NULL | "") => old.to_string(),
        (old, new) if old != new => format!("{old} -> {new}"),
        (_, new) => new.to_string(),
    }
}

/// What happened to the file itself, as opposed to its contents. Read off the
/// header git wrote rather than inferred from the paths, because a mode change
/// says so there and nowhere else.
fn file_note(file: &FilePatch) -> Option<String> {
    for line in &file.header {
        if line.starts_with("new file mode") {
            return Some("new file".into());
        }
        if line.starts_with("deleted file mode") {
            return Some("deleted".into());
        }
        if line.starts_with("rename from") {
            return Some("renamed".into());
        }
        if line.starts_with("old mode") {
            return Some("mode changed".into());
        }
    }
    None
}

/// Digits the widest line number in a patch needs.
///
/// Measured from the last line each hunk reaches rather than from its start, so
/// a hunk that begins at 998 and runs twenty lines gets the four columns it
/// ends up needing. Floored at three so a short diff does not draw a gutter
/// that jitters as hunks are staged away, and capped so a generated file with a
/// million lines cannot eat the body.
fn line_number_digits(patch: &Patch) -> u16 {
    let mut max = 0usize;
    for file in &patch.files {
        for hunk in &file.hunks {
            let old =
                hunk.old_start + hunk.lines.iter().filter(|l| l.origin != Origin::Added).count();
            let new =
                hunk.new_start + hunk.lines.iter().filter(|l| l.origin != Origin::Removed).count();
            max = max.max(old).max(new);
        }
    }
    (max.to_string().len() as u16).clamp(3, 6)
}

/// A modal drawn over the workbench.
///
/// One at a time by construction: an overlay is a question the interface is
/// asking, and two questions at once has no sensible answer. Client-owned like
/// the rest of [`View`] — the daemon used to hold one of these per connected
/// client, which is most of what made its per-client state a whole TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    /// A chooser: agents to spawn, workspaces to open, branches to check out.
    /// One shape for all of them, because they differ only in what fills the
    /// list and what happens on Enter.
    List(ListOverlay),
    /// A line of text being typed — a commit message, a branch name.
    Prompt(PromptOverlay),
    /// A yes/no about something that is about to happen.
    Confirm(ConfirmOverlay),
    /// Typing and choosing at once: a query at the top, its hits below.
    Search(SearchOverlay),
}

/// Find a file, or a line in one.
///
/// A prompt and a list in one box, because the two are one action: every
/// keystroke narrows the list, and separating them would mean typing a query,
/// pressing Enter, and only then finding out it matched nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchOverlay {
    pub query: String,
    pub cursor: usize,
    /// What the last answered query found. Rows are `path` or `path:line`.
    pub hits: Vec<SearchHit>,
    pub sel: usize,
    /// A query has gone out and its answer has not come back. Shown, because a
    /// grep over a large tree is not instant and a list that has not caught up
    /// yet otherwise looks like a list with no matches.
    pub searching: bool,
}

/// One row of the search results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub path: String,
    pub line: Option<u32>,
    pub preview: String,
}

impl SearchOverlay {
    pub fn move_sel(&mut self, delta: isize) {
        if self.hits.is_empty() {
            self.sel = 0;
            return;
        }
        let next = self.sel as isize + delta;
        self.sel = next.clamp(0, self.hits.len() as isize - 1) as usize;
    }

    pub fn chosen(&self) -> Option<&SearchHit> {
        self.hits.get(self.sel)
    }

    /// How a hit reads: a path, and where in it when that is known.
    pub fn label(hit: &SearchHit) -> String {
        match (hit.line, hit.preview.is_empty()) {
            (Some(n), false) => format!("{}:{n}  {}", hit.path, hit.preview),
            (Some(n), true) => format!("{}:{n}", hit.path),
            (None, _) => hit.path.clone(),
        }
    }

    pub fn insert(&mut self, c: char) {
        let at = self.byte_at(self.cursor);
        self.query.insert(at, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let from = self.byte_at(self.cursor - 1);
        let to = self.byte_at(self.cursor);
        self.query.replace_range(from..to, "");
        self.cursor -= 1;
    }

    fn byte_at(&self, i: usize) -> usize {
        self.query.char_indices().nth(i).map(|(b, _)| b).unwrap_or(self.query.len())
    }
}

/// A single-line text editor in a box.
///
/// One line, not a buffer: every prompt the workbench asks is a name or a
/// message, and a full editor here would be the third text-editing
/// implementation in the client after the pane and [`Editor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptOverlay {
    pub title: String,
    pub text: String,
    /// Caret position, as a char index in `0..=text.chars().count()`. Chars
    /// rather than bytes because a commit message is prose and prose has
    /// multi-byte characters in it.
    pub cursor: usize,
    pub kind: PromptKind,
    /// Shown under the input — what is about to be committed, usually.
    pub subtitle: Option<String>,
}

/// What pressing Enter in a prompt does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    Commit {
        all: bool,
    },
    /// Create a branch here and switch to it.
    NewBranch,
    /// Tag the commit you are on.
    NewTag,
    /// A branch name for a new worktree; its directory is derived from it.
    NewWorktree,
    /// The `:` prompt. Its text is a line of the same mini-language `[keys]`
    /// binds, so typing a command and binding one are the same vocabulary.
    Command,
    /// An ssh destination to connect, typed rather than picked — the machines
    /// that are not `Host` entries in `~/.ssh/config`.
    SshDestination,
    /// A name for a folder to create in the picker, and the folder to create it
    /// in.
    ///
    /// It carries `dir` because there is one overlay: asking for the name
    /// *replaces* the browse list, so by the time the name has been typed, the
    /// list that knew where "here" was is gone. Reading the picker's directory
    /// off the view instead would be a second place that has to be kept true.
    NewFolder {
        dir: String,
    },
}

impl PromptOverlay {
    pub fn insert(&mut self, c: char) {
        let at = self.byte_at(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let from = self.byte_at(self.cursor - 1);
        let to = self.byte_at(self.cursor);
        self.text.replace_range(from..to, "");
        self.cursor -= 1;
    }

    pub fn delete(&mut self) {
        let len = self.text.chars().count();
        if self.cursor >= len {
            return;
        }
        let from = self.byte_at(self.cursor);
        let to = self.byte_at(self.cursor + 1);
        self.text.replace_range(from..to, "");
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let len = self.text.chars().count() as isize;
        self.cursor = (self.cursor as isize + delta).clamp(0, len) as usize;
    }

    pub fn to_start(&mut self) {
        self.cursor = 0;
    }

    pub fn to_end(&mut self) {
        self.cursor = self.text.chars().count();
    }

    /// The byte offset of char index `i`. The caret is counted in chars and the
    /// string is indexed in bytes, and mixing the two panics on the first
    /// accented character someone types.
    fn byte_at(&self, i: usize) -> usize {
        self.text.char_indices().nth(i).map(|(b, _)| b).unwrap_or(self.text.len())
    }
}

/// A yes/no, with the thing it is about spelled out above it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmOverlay {
    pub title: String,
    /// What is about to happen, e.g. `M src/main.rs  +12 -3`.
    pub header: String,
    /// `true` while the destructive answer is selected. Starts `false`, so the
    /// keystroke that throws work away is never the one that opened the box.
    pub yes: bool,
    pub kind: ConfirmKind,
}

/// What confirming does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmKind {
    /// Throw away a file's worktree changes.
    Discard { path: String },
    /// Delete a file off disk, from the Files page. A different question from
    /// [`ConfirmKind::Discard`], which only ever takes a file back to what git
    /// already has: this one leaves nothing to restore from.
    DeleteFile { path: String },
    /// Close a workspace, killing everything running in it.
    CloseWorkspace { id: SessionId, name: String },
    /// A git-menu row the table marked destructive. Which one is held on
    /// [`View::pending_menu_action`] rather than in here, so this enum does
    /// not have to know the menu's vocabulary.
    MenuAction,
    /// A chosen row that destroys something, now that there is a name to put
    /// in the question.
    Pick { target: PickTarget, value: String, label: String },
}

/// A titled list with a cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListOverlay {
    pub title: String,
    pub items: Vec<String>,
    /// What each row *means*, when that differs from what it says: a stash's
    /// index, a worktree's path, a branch name without the `*` that marks the
    /// checked-out one. Same length as `items` when present.
    ///
    /// One field rather than a per-kind rule for turning a label back into a
    /// value — parsing `stash@{3}` out of a display string to get `3` works
    /// right up until a stash message contains one.
    pub values: Option<Vec<String>>,
    pub sel: usize,
    /// What the caller does with the chosen row.
    pub kind: ListKind,
}

/// Why a list is open, so the loop knows what Enter means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListKind {
    /// Which space to look at. Rows are [`Page::ORDER`] with their badges, and
    /// the choice is read by index rather than by label — the label carries a
    /// cursor mark and a badge, and parsing a page name back out of `"> agents
    /// 2!"` is the kind of thing that works until a badge changes shape.
    Space,
    SpawnAgent,
    /// Check out the chosen branch. The current one is marked in the row and
    /// choosing it is a no-op the daemon shrugs off.
    Branch,
    /// A right-click context menu. Everything on it is reachable another way —
    /// `x` on a rail row, `X` on a workspace — so this is a shortcut, not a
    /// capability, and it carries what it was opened on rather than reading the
    /// cursor: by the time it is answered the cursor may have moved.
    Menu(MenuTarget),
    /// Bring another machine into the tab bar. Rows are `~/.ssh/config`
    /// aliases, read here rather than fetched: the file is the *client's*, and
    /// which machines this person uses is not something the daemon they happen
    /// to be attached to has any business knowing.
    Host,
    /// Which connected machine to open a workspace on.
    ///
    /// Distinct from [`Host`](Self::Host): that one brings a *new* machine into
    /// the bar and its rows come from `~/.ssh/config`; this one chooses among
    /// the machines already here, and its rows are daemon indices.
    Machine,
    /// Choose the thing an already-decided operation acts on.
    Pick(PickTarget),
    /// The git menu's top level: choose a group.
    GitGroups,
    /// One group of the git menu. `..` goes back up.
    GitGroup(crate::git_menu::MenuGroup),
    /// Looking through the filesystem for somewhere to open a workspace.
    ///
    /// Carries the directory being listed, because a row means either "descend
    /// from here" or "open here" and neither can be read without knowing where
    /// "here" is.
    Browse {
        dir: String,
    },
    Theme,
    /// A URL on screen. Enter opens it on the machine this client is running
    /// on, `y` copies it — see [`crate::links`].
    Links,
}

/// What a context menu was opened on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuTarget {
    /// An agent, named the way a route names one: which machine, which
    /// workspace on it, which pane.
    ///
    /// **All three, rather than the pane alone**, because BOOTH's fleet spans
    /// daemons and the menu it opens has to act on the row's own workspace —
    /// not on the tab you happen to be looking at. A pane id is only unique
    /// within one daemon, so the same number is a different agent on the next
    /// machine along, and "close agent" on a `gpu-box` row would have closed
    /// whatever happened to hold that id here.
    Agent {
        /// Index into the client's `daemons`, as [`AllAgentRow::daemon`] is.
        daemon: usize,
        workspace: SessionId,
        pane: PaneId,
    },
    Process(butai_protocol::PaneId),
    /// A workspace tab, by its index in the flattened tab list.
    Tab(usize),
    /// A tab belonging to a machine reached over ssh. Its one action is about
    /// the *host*: closing someone else's project from a context menu is not
    /// ours to do, but dropping the link is.
    RemoteTab(usize),
}

impl MenuTarget {
    /// The rows it offers, as `(label, destructive)`.
    ///
    /// One table, so the drawing and the dispatch below cannot disagree about
    /// how many rows there are or what order they are in.
    pub fn rows(self) -> Vec<(&'static str, bool)> {
        match self {
            // "others" and "all" mean *that agent's workspace*, wherever it is,
            // which is the same sentence on a rail — there the workspace is the
            // one you are in, and on BOOTH it is the one the row names.
            MenuTarget::Agent { .. } => {
                vec![("Close agent", false), ("Close others", false), ("Close all agents", true)]
            }
            MenuTarget::Process(_) => vec![("Close", false), ("Restart", false)],
            MenuTarget::Tab(_) => vec![("Close workspace", true)],
            MenuTarget::RemoteTab(_) => vec![("Disconnect host", true)],
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            MenuTarget::Agent { .. } => "AGENT",
            MenuTarget::Process(_) => "PROCESS",
            MenuTarget::Tab(_) | MenuTarget::RemoteTab(_) => "WORKSPACE",
        }
    }
}

/// What a [`ListKind::Pick`] is choosing *for*.
///
/// The git-menu rows that need a second answer. Each is one list and one call;
/// the enum exists so the list knows what Enter means, and so adding a row is a
/// compile error until something handles it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickTarget {
    /// Switch to a branch by name. Named here rather than reached through the
    /// branch picker, because the GIT page's cursor has already chosen one —
    /// the picker exists for the case where nothing has.
    Checkout,
    /// Undo a commit with a new one, and copy one onto this branch. Both take
    /// a revision rather than a branch, which is why they live beside the
    /// others rather than in the `g` menu's pickers.
    Revert,
    CherryPick,
    DeleteBranch,
    Merge,
    Rebase,
    StashPop,
    StashDrop,
    TagDelete,
    RemoteRemove,
    /// A worktree is a second checkout on another branch, and butai's model is
    /// already one workspace per directory — so this opens it *as a workspace*.
    OpenWorktree,
    RemoveWorktree,
}

impl PickTarget {
    /// The title its chooser wears.
    pub fn title(self) -> &'static str {
        match self {
            PickTarget::Checkout => "CHECKOUT",
            PickTarget::Revert => "REVERT",
            PickTarget::CherryPick => "CHERRY-PICK",
            PickTarget::DeleteBranch => "DELETE BRANCH",
            PickTarget::Merge => "MERGE",
            PickTarget::Rebase => "REBASE ONTO",
            PickTarget::StashPop => "POP STASH",
            PickTarget::StashDrop => "DROP STASH",
            PickTarget::TagDelete => "DELETE TAG",
            PickTarget::RemoteRemove => "REMOVE REMOTE",
            PickTarget::OpenWorktree => "OPEN WORKTREE",
            PickTarget::RemoveWorktree => "REMOVE WORKTREE",
        }
    }

    /// Whether choosing here destroys something, so it asks once more with the
    /// chosen row named — which is the only point at which the question can say
    /// *what* is about to go. The menu's own destructive rows ask before their
    /// picker for the opposite reason: there, nothing is chosen yet.
    pub fn destroys(self) -> bool {
        matches!(
            self,
            PickTarget::DeleteBranch
                | PickTarget::StashDrop
                | PickTarget::TagDelete
                | PickTarget::RemoteRemove
                | PickTarget::RemoveWorktree
        )
    }
}

/// The virtual rows a browse list starts with — the ones that are not
/// directories. Constants because the drawing and the dispatch both have to
/// agree on their wording, and a row that stops matching its own literal is a
/// row that silently becomes a folder called `[open this folder]`.
pub const BROWSE_OPEN: &str = "[open this folder]";
/// Make a directory here and step into it. A project that does not exist yet is
/// the ordinary case for "open a workspace" — it was the one case that sent you
/// out to a shell for `mkdir` and back.
pub const BROWSE_NEW: &str = "[new folder]";
pub const BROWSE_UP: &str = "..";

impl ListOverlay {
    pub fn move_sel(&mut self, delta: isize) {
        if self.items.is_empty() {
            self.sel = 0;
            return;
        }
        let next = self.sel as isize + delta;
        self.sel = next.clamp(0, self.items.len() as isize - 1) as usize;
    }

    /// What the chosen row *means* — its value when it has one, otherwise what
    /// it says.
    pub fn chosen(&self) -> Option<&str> {
        if let Some(values) = &self.values {
            return values.get(self.sel).map(String::as_str);
        }
        self.chosen_label()
    }

    /// What the chosen row says, for a question that has to name it.
    pub fn chosen_label(&self) -> Option<&str> {
        self.items.get(self.sel).map(String::as_str)
    }
}

/// Which rail section the keyboard is in. Client-owned: each client has its own
/// cursor, exactly as the Mac and web clients do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    Agents,
    Processes,
    Changes,
    /// BOOTH's FLEET list — every agent on every connected machine. Reachable
    /// only on that page, which is the only place the list is drawn.
    AllAgents,
    /// The GIT page's REFS list. Reachable only while that page is up, the way
    /// `AllAgents` is reachable only on BOOTH.
    Refs,
    /// The GIT page's commit graph.
    History,
    Stage,
}

/// Everything about the view that belongs to *this* client — where the cursor
/// is, what it is looking at, how wide it made the rails.
///
/// None of it reaches the daemon. That is the point: an arrow key moves this and
/// repaints locally, with no round trip, and two clients on one workspace no
/// longer have to agree about a selection neither of them can see.
#[derive(Debug, Clone)]
pub struct View {
    pub focus: Focus,
    pub agent_sel: usize,
    pub proc_sel: usize,
    pub changes_sel: usize,
    /// Cursor in BOOTH's FLEET list.
    pub all_agents_sel: usize,
    pub zen: bool,
    pub geom: RailGeom,
    /// Which interfaces the SYSTEM rail draws. Read from `[ui] net` and held
    /// beside `geom` because it is the same kind of thing: a configured shape
    /// for the chrome that every page has to agree about.
    pub net: NetSelect,
    /// Which mounts the SYSTEM rail draws, from `[ui] disks`. Beside `net` for
    /// the same reason.
    pub disks: DiskSelect,
    /// Whether the painter marks a URL up as a hyperlink for the terminal it is
    /// drawing on, from `[ui] links`. Here with the other two because it is the
    /// same kind of thing: a configured fact about the chrome that the drawing
    /// has to read on every frame.
    pub links: bool,
    /// Which workspace's tab is active.
    pub tab: usize,
    /// Animation phases: the slow one drives marquees, the fast one sprites.
    pub tick: u64,
    pub fast_tick: u64,
    /// A transient message for the footer.
    pub flash: Option<String>,
    /// The modal on top, if any.
    pub overlay: Option<Overlay>,
    /// Which full-screen view is showing.
    pub page: Page,
    /// First machine drawn in the BOOTH page's compute column.
    ///
    /// Its own offset rather than a cursor, because the column has nothing to
    /// select: it is read, not walked, so the wheel moves it and `j`/`k` stay
    /// with the fleet list where the selection lives.
    pub booth_compute_scroll: usize,
    /// A destructive git-menu row waiting on its confirm box.
    pub pending_menu_action: Option<crate::git_menu::GitAction>,
    /// The one the box was answered "yes" to, so the second pass through
    /// carries it out rather than asking again.
    pub confirmed_menu_action: Option<crate::git_menu::GitAction>,
    /// In LAYOUT mode, holding the geometry it was entered with — so leaving
    /// knows whether anything actually moved and only then writes the file.
    pub layout: Option<RailGeom>,
    /// The agent the AGENTS `+` spawns without asking, when one is pinned.
    ///
    /// On the view because the button's *label* names it: a pinned button
    /// spawns on a single click, and the label is the only place the user can
    /// see what that click is about to do.
    pub pinned_agent: Option<String>,
    /// Which pane this client is looking at, when it has chosen one.
    ///
    /// `None` follows the workspace's own default, which is what a fresh attach
    /// wants. Choosing a row overrides it *for this client only* — the stage is
    /// a viewport, not a property of the workspace, and two people looking at
    /// one project should be able to look at different panes in it. The daemon
    /// still keeps a `stage` per workspace; phase 4a is where that goes.
    pub staged: Option<butai_protocol::PaneId>,
    /// The prefix key as it reads on screen — `^B`. Held rather than derived
    /// because it is configurable, and both the footer and the help list would
    /// otherwise print a marker that is a lie on a changed one.
    pub prefix: String,
    /// The prefix has been pressed and the next key is a binding.
    ///
    /// Client state now, and it always should have been: the daemon kept one of
    /// these per connected client, which is a keyboard mode — about as far from
    /// "state a server holds" as interface state gets.
    pub prefix_armed: bool,
    /// Which machine a browse-and-open goes to, once the picker has answered.
    ///
    /// `None` means "the tab's own", which is the answer on every
    /// single-machine client — there the question is never asked at all.
    pub browse_daemon: Option<usize>,
    /// What the SYSTEM section is holding, in drawing order — see
    /// [`system_gauges`].
    ///
    /// The list rather than a count, because hit testing has to name what was
    /// clicked and "the last one is the network, unless it is a GPU" is not
    /// recoverable from a number. Kept on the view because the section's height
    /// depends on it and both the drawing and the hit testing take their
    /// geometry from here: a rail drawn for four gauges and clicked as if it
    /// had three is how a pointer lands two rows off.
    pub gauges: Vec<Gauge>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            // The stage, not a rail. A workbench that opens with the keyboard
            // pointed at the AGENTS list turns the first thing you type into
            // commands: `echo $PATH` starts by opening the agent picker on the
            // `a` and spawning something on the `e`. The daemon opened on the
            // stage for exactly this reason, and Alt-Esc is how you leave it.
            focus: Focus::Stage,
            agent_sel: 0,
            proc_sel: 0,
            changes_sel: 0,
            all_agents_sel: 0,
            zen: false,

            net: NetSelect::default(),
            disks: DiskSelect::default(),
            links: true,
            geom: crate::chrome::default_geom(),
            tab: 0,
            tick: 0,
            fast_tick: 0,
            flash: None,
            overlay: None,
            page: Page::default(),
            booth_compute_scroll: 0,
            pending_menu_action: None,
            confirmed_menu_action: None,
            layout: None,
            pinned_agent: None,
            staged: None,
            prefix: "^B".into(),
            prefix_armed: false,
            browse_daemon: None,
            // CPU and RAM: what every machine has before the daemon has said
            // anything about GPUs or interfaces. The real list arrives with the
            // first telemetry and the rail grows into it.
            gauges: vec![Gauge::Cpu, Gauge::Ram],
        }
    }
}

/// The palette, resolved once into ratatui colours.
#[derive(Debug, Clone)]
pub struct Theme {
    pub ground: Color,
    pub surface: Color,
    pub selection: Color,
    pub ink: Color,
    pub muted: Color,
    pub faint: Color,
    pub rule: Color,
    pub rule_focus: Color,
    pub accent: Color,
    pub info: Color,
    pub ok: Color,
    pub attention: Color,
    pub danger: Color,
    pub status_bg: Color,
    pub status_fg: Color,
}

fn to_ratatui(c: ThemeColor) -> Color {
    match c {
        ThemeColor::Default => Color::Reset,
        ThemeColor::Ansi(n) => Color::Indexed(n),
        ThemeColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

impl Theme {
    pub fn from_palette(p: &Palette) -> Self {
        Self {
            ground: to_ratatui(p.ground),
            surface: to_ratatui(p.surface),
            selection: to_ratatui(p.selection),
            ink: to_ratatui(p.ink),
            muted: to_ratatui(p.muted),
            faint: to_ratatui(p.faint),
            rule: to_ratatui(p.rule),
            rule_focus: to_ratatui(p.rule_focus),
            accent: to_ratatui(p.accent),
            info: to_ratatui(p.info),
            ok: to_ratatui(p.ok),
            attention: to_ratatui(p.attention),
            danger: to_ratatui(p.danger),
            status_bg: to_ratatui(p.status_bg),
            status_fg: to_ratatui(p.status_fg),
        }
    }

    /// Resolve a semantic role from the shared row model.
    pub fn role(&self, role: Role) -> Color {
        match role {
            Role::Ink => self.ink,
            Role::Faint => self.faint,
            Role::Ok => self.ok,
            Role::Info => self.info,
            Role::Attention => self.attention,
            Role::Danger => self.danger,
            Role::Accent => self.accent,
        }
    }

    fn border(&self, focused: bool) -> Color {
        if focused {
            self.rule_focus
        } else {
            self.rule
        }
    }

    fn row_bg(&self, cursor: bool) -> Color {
        if cursor {
            self.selection
        } else {
            self.ground
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::from_palette(&crate::theme::BLUEPRINT_DARK)
    }
}

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

fn to_rect(r: LRect) -> Rect {
    Rect::new(r.x, r.y, r.width, r.height)
}

/// Write `s` at `(x, y)`, stopping at `bound`.
///
/// Advances one column per `char`, which is what the frame encoding expects —
/// see the note in `butai_core::chrome` about single-cell glyphs.
pub(crate) fn put_str(buf: &mut Buffer, x: u16, y: u16, s: &str, bound: u16, style: Pen) {
    for (cx, ch) in (x..).zip(s.chars()) {
        if cx >= bound {
            break;
        }
        if let Some(cell) = buf.cell_mut((cx, y)) {
            cell.set_char(ch);
            cell.set_fg(style.fg);
            cell.set_bg(style.bg);
            if style.bold {
                cell.set_style(Style::default().add_modifier(Modifier::BOLD));
            }
        }
    }
}

/// Foreground, background and weight for one run of text.
///
/// A struct rather than three positional arguments because every call site was
/// passing the same trio and reading `put_str(.., a, b, false)` told you nothing
/// about which colour was which.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pen {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
}

impl Pen {
    fn new(fg: Color, bg: Color) -> Self {
        Self { fg, bg, bold: false }
    }
}

/// A single-line box with a label let into its top border.
fn draw_box(buf: &mut Buffer, r: LRect, label: &str, color: Color, bg: Color) {
    if r.width < 2 || r.height < 2 {
        return;
    }
    let (x0, y0) = (r.x, r.y);
    let (x1, y1) = (r.x + r.width - 1, r.y + r.height - 1);
    for x in x0..=x1 {
        put_str(buf, x, y0, "─", x1 + 1, Pen::new(color, bg));
        put_str(buf, x, y1, "─", x1 + 1, Pen::new(color, bg));
    }
    for y in y0..=y1 {
        put_str(buf, x0, y, "│", x0 + 1, Pen::new(color, bg));
        put_str(buf, x1, y, "│", x1 + 1, Pen::new(color, bg));
    }
    put_str(buf, x0, y0, "┌", x0 + 1, Pen::new(color, bg));
    put_str(buf, x1, y0, "┐", x1 + 1, Pen::new(color, bg));
    put_str(buf, x0, y1, "└", x0 + 1, Pen::new(color, bg));
    put_str(buf, x1, y1, "┘", x1 + 1, Pen::new(color, bg));
    if !label.is_empty() && r.width > 4 {
        let text = ellipsize(label, r.width.saturating_sub(4) as usize);
        put_str(buf, x0 + 1, y0, &text, x1, Pen::new(color, bg));
    }
}

/// A labelled separator row inside a box.
fn draw_section_sep(buf: &mut Buffer, r: LRect, y: u16, label: &str, color: Color, bg: Color) {
    if r.width < 2 {
        return;
    }
    let x1 = r.x + r.width - 1;
    for x in r.x..=x1 {
        put_str(buf, x, y, "─", x1 + 1, Pen::new(color, bg));
    }
    put_str(buf, r.x, y, "├", r.x + 1, Pen::new(color, bg));
    put_str(buf, x1, y, "┤", x1 + 1, Pen::new(color, bg));
    if !label.is_empty() && r.width > 4 {
        let text = ellipsize(label, r.width.saturating_sub(4) as usize);
        put_str(buf, r.x + 1, y, &text, x1, Pen::new(color, bg));
    }
}

/// Copy `src` into `dst` at `at`. How a streamed pane lands on the stage.
pub fn blit(dst: &mut Buffer, src: &Buffer, at: Rect) {
    for y in 0..src.area.height.min(at.height) {
        for x in 0..src.area.width.min(at.width) {
            let Some(from) = src.cell((src.area.x + x, src.area.y + y)).cloned() else { continue };
            if let Some(to) = dst.cell_mut((at.x + x, at.y + y)) {
                *to = from;
            }
        }
    }
}

/// Human-readable age for a connection that has been down `secs` seconds.
///
/// Seconds up to a minute, then minutes, then hours. The number is the whole
/// reason the notice is worth reading — "4s" and "40m" call for completely
/// different reactions — so it stays legible rather than becoming "a while".
fn down_for(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m{:02}s", s / 60, s % 60),
        s => format!("{}h{:02}m", s / 3600, s % 3600 / 60),
    }
}

/// The lines of the stage's disconnected notice, widest first in the caller's
/// hands rather than this one's.
///
/// Split out from the drawing so the wording is testable without a `Buffer`:
/// what this says is the whole feature, and asserting on it is how "reconnecting
/// to a machine" cannot silently become "the pane exited".
fn stage_down_lines(down: &StageDown, tick: u64) -> Vec<(String, bool)> {
    let spin = model::SPINNER[(tick as usize) % model::SPINNER.len()];
    let who = match down.host {
        Some(host) => format!("{host} went away"),
        // No host badge means the local daemon, and there is no machine name to
        // print — "localhost went away" would be a stranger sentence than the
        // situation deserves.
        None => "the daemon went away".to_string(),
    };
    let mut lines = vec![(format!("{spin}  {who}"), true)];
    lines.push((format!("reconnecting — down {}", down_for(down.secs)), false));
    if down.has_frame {
        lines.push(("what is behind this is its last frame".to_string(), false));
    }
    lines
}

/// Draw the "this machine went away" notice over the stage.
///
/// **Called after the pane is blitted, not with the rest of the chrome.** The
/// cells it dims and covers are the pane's, so drawing it earlier would put the
/// photograph on top of the notice explaining that it is one — the same
/// ordering trap `compose` documents for the overlay layer.
///
/// The dimming is the larger half of the message and the cheaper one: a screen
/// flattened to one faint colour is legibly inert at a glance, from across the
/// room, without reading a word of the card.
pub fn draw_stage_down(buf: &mut Buffer, area: Rect, down: &StageDown, theme: &Theme, tick: u64) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_fg(theme.faint);
                cell.set_bg(theme.ground);
                cell.set_style(Style::default());
            }
        }
    }

    let lines = stage_down_lines(down, tick);
    let text_w = lines.iter().map(|(s, _)| s.chars().count()).max().unwrap_or(0) as u16;
    // Two columns of padding each side, plus the border.
    let card_w = (text_w + 6).min(area.width);
    let card_h = (lines.len() as u16 + 2).min(area.height);
    // A stage too small for the card still gets the first line, centred and
    // unboxed. The rails can be dragged down to a few rows and the notice
    // disappearing there would take the only explanation with it.
    if card_w < 8 || card_h < 3 {
        let (text, _) = &lines[0];
        let x = area.x + area.width.saturating_sub(text.chars().count() as u16) / 2;
        let y = area.y + area.height / 2;
        put_str(buf, x, y, text, area.x + area.width, Pen::new(theme.attention, theme.ground));
        return;
    }
    let x0 = area.x + (area.width - card_w) / 2;
    let y0 = area.y + (area.height - card_h) / 2;
    let card = LRect::new(x0, y0, card_w, card_h);
    // The interior is repainted so the card is opaque: the point of a card over
    // a dimmed screen is that the words on it are not competing with whatever
    // the program had drawn underneath.
    for y in y0..y0 + card_h {
        for x in x0..x0 + card_w {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(' ');
                cell.set_bg(theme.surface);
                cell.set_style(Style::default());
            }
        }
    }
    draw_box(buf, card, "", theme.attention, theme.surface);
    let bound = x0 + card_w - 1;
    for (i, (text, strong)) in lines.iter().enumerate() {
        let y = y0 + 1 + i as u16;
        if y >= y0 + card_h - 1 {
            break;
        }
        let fg = if *strong { theme.attention } else { theme.muted };
        let mut pen = Pen::new(fg, theme.surface);
        pen.bold = *strong;
        // Centred within the card, so a long host name and a short one both
        // sit under the same middle.
        let x = x0 + 1 + (card_w - 2).saturating_sub(text.chars().count() as u16) / 2;
        put_str(buf, x, y, text, bound, pen);
    }
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

/// One rail row: an optional marker, an optional pinned token, a
/// marquee-scrolled name, a right-aligned status token. Returns whether the name
/// is scrolling, so the caller knows to keep repainting.
///
/// `pin` is a short token that sits between the marker and the name and stays
/// put while the name travels — a git status code, and nothing longer. It is the
/// row's *kind*, so scrolling it out of view says nothing and loses the one
/// thing that was readable at a glance; only the name is too long to fit, so
/// only the name is what moves. Pass `""` for a row that has no such token.
#[allow(clippy::too_many_arguments)]
fn draw_row(
    buf: &mut Buffer,
    area: LRect,
    y: u16,
    cursor: bool,
    active: bool,
    pin: &str,
    left: &str,
    left_color: Color,
    right: &str,
    right_color: Color,
    tick: u64,
    theme: &Theme,
) -> bool {
    let marker = if active { "> " } else { "  " };
    let bg = theme.row_bg(cursor);
    let right_w = right.width() as u16;
    // The pin's columns, its separating space included — zero when there is no
    // pin, so a row without one starts its name exactly where it always did.
    let pin_w = if pin.is_empty() { 0 } else { pin.width() as u16 + 1 };
    let name_max = area.width.saturating_sub(2 + pin_w + right_w + 1);
    let (text, scrolling) = marquee(left, name_max as usize, tick);
    let right_edge = area.x + area.width;
    for x in area.x..right_edge {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(" ");
            cell.set_bg(bg);
        }
    }
    let marker_color = if active { theme.accent } else { theme.ink };
    put_str(buf, area.x, y, marker, right_edge, Pen::new(marker_color, bg));
    // Stop the name short of the status token: `marquee` budgets in chars while
    // `put_str` advances a column per char, so a name carrying a wide glyph
    // would otherwise paint over the status.
    let name_bound = right_edge.saturating_sub(right_w);
    put_str(buf, area.x + 2, y, pin, name_bound, Pen::new(left_color, bg));
    put_str(buf, area.x + 2 + pin_w, y, &text, name_bound, Pen::new(left_color, bg));
    if right_w < area.width {
        let rx = right_edge.saturating_sub(right_w);
        put_str(buf, rx, y, right, right_edge, Pen::new(right_color, bg));
    }
    scrolling
}

/// Now, as unix epoch millis — the clock the timers run on.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Seconds since an epoch-millis instant, saturating.
fn secs_since(ms: u64) -> u64 {
    now_ms().saturating_sub(ms) / 1000
}

/// The PROCESSES status token.
fn proc_status(p: &ProcessDto, theme: &Theme) -> (String, Color) {
    match p.exited {
        Some(0) => ("done".into(), theme.ink),
        Some(code) => (format!("FAIL({code})"), theme.danger),
        None => match p.status.as_str() {
            "ok" => ("ok".into(), theme.ok),
            "..." => ("...".into(), theme.attention),
            other => (other.to_string(), theme.ink),
        },
    }
}

/// The CHANGES box label: `" CHANGES (3) · main ↑2↓1 "`, or
/// `" CHANGES (3) · main · REBASING "` while a sequence is running.
///
/// The rail is the narrowest thing on screen and the only always-visible git
/// surface, so its title carries the three facts that change what you would do
/// next: which branch you are on, how far you have diverged, and whether git is
/// halfway through something. The sequence displaces the divergence when both
/// apply — you cannot push mid-rebase.
///
/// The footer names the branch too, but it names the *workspace's* — one line
/// for the whole screen, cut down first on a narrow terminal and gone entirely
/// in layout mode. Beside the file list is where "which branch am I about to
/// commit this to" is actually asked, and it is where the web client has always
/// put it.
///
/// Width-aware, because the branch is the one part of the title with no bound.
/// [`draw_box`] ellipsizes from the right, so a long branch name would take the
/// arrows with it — and the arrows are the half that says what to do next. The
/// branch is cut to whatever the rest leaves instead, and dropped outright
/// rather than shown as a stub on a rail too narrow to name it.
fn changes_label(c: &ChangesDto, width: u16) -> String {
    /// Fewer visible characters than this and the name is not a name any more.
    const MIN_BRANCH: usize = 6;
    let n = c.staged.len() + c.unstaged.len() + c.conflicted.len();
    let head = format!(" CHANGES ({n})");
    let tail = if c.state.in_progress() {
        format!(" · {} ", c.state.label())
    } else {
        let mut arrows = String::new();
        if c.ahead > 0 {
            arrows.push_str(&format!("↑{}", c.ahead));
        }
        if c.behind > 0 {
            arrows.push_str(&format!("↓{}", c.behind));
        }
        if arrows.is_empty() {
            " ".to_string()
        } else {
            format!(" {arrows} ")
        }
    };
    // The same budget `draw_box` will hold the label to, minus the parts of it
    // that are never the ones dropped. Three for the ` · ` in front of it.
    let room = (width.saturating_sub(4) as usize)
        .saturating_sub(head.chars().count() + tail.chars().count() + 3);
    match c.branch.chars().count() {
        _ if room < MIN_BRANCH => format!("{head}{tail}"),
        len if len <= room => format!("{head} · {}{tail}", c.branch),
        _ => format!("{head} · {}{tail}", ellipsize(&c.branch, room)),
    }
}

// ---------------------------------------------------------------------------
// The workbench
// ---------------------------------------------------------------------------

/// What one paint produced, beyond the cells themselves.
#[derive(Debug, Default, Clone, Copy)]
pub struct Painted {
    /// A row is marquee-scrolling, so the slow clock should keep repainting.
    pub wants_anim: bool,
    /// A sprite is moving, so the fast clock should too. Gated, so an idle
    /// panel costs nothing.
    pub wants_fast_anim: bool,
}

/// Draw the whole workbench except the stage, which is blitted separately.
pub fn draw(
    buf: &mut Buffer,
    cols: u16,
    rows: u16,
    scene: &Scene<'_>,
    view: &View,
    theme: &Theme,
) -> Painted {
    let mut out = Painted::default();
    for cell in buf.content.iter_mut() {
        cell.set_bg(theme.ground);
    }
    if cols < 10 || rows < 5 {
        return out;
    }
    let geom = page_geom(cols, rows, view);
    let ws = scene.workspace;

    draw_tabbar(buf, &geom, scene.tabs, scene.daemons, view, theme);
    // A page that owns the whole band is drawn over the columns the rails
    // reserved, so they must not also be painted there — see `page_geom`.
    let rails = !view.page.owns_full_width();
    if rails && geom.left_box.width > 0 {
        if view.zen {
            draw_left_zen(buf, &geom, ws, theme);
        } else {
            out.wants_anim |= draw_left_rail(buf, &geom, ws, scene.system, view, theme);
        }
    }
    match view.page {
        Page::Booth => {
            let page = draw_booth_page(buf, cols, &geom, scene, view, theme);
            out.wants_anim |= page.wants_anim;
            out.wants_fast_anim |= page.wants_fast_anim;
        }
        Page::Agents => draw_stage_box(buf, &geom, ws, view, theme),
        Page::Files => draw_files_page(buf, &geom, Page::Files, scene.files, view, theme),
        Page::Docs => draw_files_page(buf, &geom, Page::Docs, scene.docs, view, theme),
        Page::Diff => draw_diff_page(buf, &geom, scene.diff, theme),
        Page::Docker => draw_docker_page(buf, &geom, scene.system, ws, scene.docker, view, theme),
        Page::Git => draw_git_page(buf, &geom, ws, scene.git, view, theme),
        Page::Settings => settings::draw(buf, &geom, scene.settings, view, theme),
        Page::Help => help::draw(buf, &geom, scene.help, view, theme),
        Page::Usage => usage::draw(buf, &geom, scene.usage, theme),
    }
    if rails && geom.right_box.width > 0 {
        if view.zen {
            draw_right_zen(buf, &geom, ws, theme);
        } else {
            draw_right_rail(buf, &geom, ws, view, theme);
        }
    }
    // The host the active chip carries, so the footer can say which machine
    // its path is on. `None` on a single-daemon client, by the same rule the
    // chip itself uses — see [`Tab::host`].
    let host = scene.tabs.get(view.tab).and_then(|t| t.host);
    draw_footer(buf, &geom.footer, ws, host, view, theme);
    out
}

/// Draw the modal layer, if any.
///
/// Separate from [`draw`] because of z-order: the streamed pane is blitted onto
/// the stage *after* the chrome, so anything drawn during `draw` would be
/// painted over by it. An overlay is on top of everything by definition, so it
/// goes on last — chrome, then pane, then this.
pub fn draw_overlay_layer(buf: &mut Buffer, cols: u16, rows: u16, view: &View, theme: &Theme) {
    if let Some(overlay) = &view.overlay {
        draw_overlay(buf, cols, rows, overlay, theme);
    }
}

/// The rows a modal is showing, and where its box sits.
///
/// Split out of [`draw_overlay`] so a click can be resolved against exactly the
/// rows that were painted. A modal that is hit-tested against a second guess at
/// its own layout is the defect this exists to make impossible — and it is why
/// the daemon needed seven `list_overlay_hit` calls, one per modal, each with
/// its own hard-coded width.
fn overlay_layout(cols: u16, rows: u16, overlay: &Overlay) -> OverlayRows {
    let mut o = overlay_rows(overlay);
    // Wide enough for the title as well as the content: a chooser whose items
    // are short ("sh", "amp") but whose question is long would otherwise cut the
    // question, which is the half that says what the list is for.
    let widest = o.lines.iter().map(|l| l.chars().count()).max().unwrap_or(20);
    let want_w = widest.max(o.title.chars().count()) as u16 + 4;
    let w = want_w.min(cols.saturating_sub(4)).max(12);
    let h = (o.lines.len() as u16 + 2).min(rows.saturating_sub(4)).max(3);
    o.rect = LRect::new(cols.saturating_sub(w) / 2, rows.saturating_sub(h) / 2, w, h);
    o
}

/// One modal, resolved into the rows it draws and the box they go in.
struct OverlayRows {
    title: String,
    lines: Vec<String>,
    /// The highlighted line, as an index into `lines`.
    sel: Option<usize>,
    /// Line and column of the text caret, for the two modals being typed into.
    caret: Option<(usize, usize)>,
    rect: LRect,
}

/// Which line of the open modal is at `(x, y)`, if any.
///
/// Indexes the *lines* the modal drew, which is what the row-to-action mapping
/// in the loop already speaks: `Overlay::List` rows are its items, `Confirm`'s
/// are `no` and `yes` at 2 and 3, and `Search`'s hits start at 2 under the
/// query. `None` means the click missed, which every modal reads as "dismiss".
pub fn overlay_hit(cols: u16, rows: u16, overlay: &Overlay, x: u16, y: u16) -> Option<usize> {
    let o = overlay_layout(cols, rows, overlay);
    let r = o.rect;
    if x <= r.x || x + 1 >= r.right() || y <= r.y || y + 1 >= r.bottom() {
        return None;
    }
    let i = (y - r.y - 1) as usize;
    // Only as far as the box could actually show, which is what the drawing
    // clamps to — a click below the last visible line hits nothing.
    (i < o.lines.len().min(r.height.saturating_sub(2) as usize)).then_some(i)
}

/// The title, lines, cursor row and caret of a modal — everything about it that
/// is not geometry.
fn overlay_rows(overlay: &Overlay) -> OverlayRows {
    let (title, lines, sel, caret) = match overlay {
        Overlay::List(list) => {
            (format!(" {} ", list.title), list.items.clone(), Some(list.sel), None)
        }
        Overlay::Prompt(p) => {
            let mut lines = vec![p.text.clone()];
            if let Some(sub) = &p.subtitle {
                lines.push(String::new());
                lines.push(sub.clone());
            }
            (format!(" {} ", p.title), lines, None, Some((0usize, p.cursor)))
        }
        Overlay::Search(f) => {
            let mut lines = vec![format!("/{}", f.query)];
            lines.push(String::new());
            if f.hits.is_empty() {
                lines.push(if f.searching {
                    "searching…".into()
                } else {
                    "(nothing found)".into()
                });
            } else {
                lines.extend(f.hits.iter().map(SearchOverlay::label));
            }
            // `+2` skips the query row and the blank one under it.
            let sel = (!f.hits.is_empty()).then_some(f.sel + 2);
            // The caret sits after the `/`, so its column is one past the
            // character index.
            (" FIND ".to_string(), lines, sel, Some((0usize, f.cursor + 1)))
        }
        Overlay::Confirm(c) => {
            // "no" first, and selected first, so the keystroke that throws work
            // away is never the one that opened the box.
            let lines = vec![c.header.clone(), String::new(), "no".into(), "yes".into()];
            (format!(" {} ", c.title), lines, Some(2 + usize::from(c.yes)), None)
        }
    };
    OverlayRows { title, lines, sel, caret, rect: LRect::new(0, 0, 0, 0) }
}

/// A centred modal box, sized to its content and clamped to the screen.
fn draw_overlay(buf: &mut Buffer, cols: u16, rows: u16, overlay: &Overlay, theme: &Theme) {
    // Everything is drawn as "a title, some lines, maybe a highlighted one,
    // maybe a caret". Five modals with one renderer rather than five, because
    // the box, the clearing and the clamping are the fiddly part and there is
    // no reason for them to exist five times. The rows and the box both come
    // from `overlay_layout`, which `overlay_hit` also reads, so a click lands
    // on the line it looks like it lands on.
    let OverlayRows { title, lines, sel, caret, rect } = overlay_layout(cols, rows, overlay);
    let (x, y, w, h) = (rect.x, rect.y, rect.width, rect.height);

    // Clear behind it: a modal that lets the rails show through is unreadable
    // over a busy stage.
    for row in y..y + h {
        for col in x..x + w {
            if let Some(cell) = buf.cell_mut((col, row)) {
                cell.set_symbol(" ");
                cell.set_bg(theme.surface);
                cell.set_fg(theme.ink);
            }
        }
    }
    draw_box(buf, rect, &title, theme.rule_focus, theme.surface);

    let inner_w = w.saturating_sub(2);
    let bound = x + w - 1;
    for (i, line) in lines.iter().take(h.saturating_sub(2) as usize).enumerate() {
        let ly = y + 1 + i as u16;
        let cursor = sel == Some(i);
        let bg = if cursor { theme.selection } else { theme.surface };
        if cursor {
            for col in x + 1..bound {
                if let Some(cell) = buf.cell_mut((col, ly)) {
                    cell.set_bg(bg);
                }
            }
        }
        let text = ellipsize(line, inner_w.saturating_sub(2) as usize);
        put_str(buf, x + 2, ly, &text, bound, Pen::new(theme.ink, bg));
        // The caret is a reversed cell rather than the terminal's own: the real
        // cursor is parked on the streamed pane, and moving it here would drag
        // it back the moment the modal closed.
        if let Some((line, col)) = caret {
            if line == i {
                let cx = x + 2 + col as u16;
                if cx < bound {
                    if let Some(cell) = buf.cell_mut((cx, ly)) {
                        cell.set_bg(theme.ink);
                        cell.set_fg(theme.surface);
                    }
                }
            }
        }
    }
}

/// Collapsed left rail: one two-cell marker per agent, then per process.
///
/// No spinner — the zen strip is a glance, not a display, and driving the
/// animation clock from a four-column strip would repaint the screen for
/// something nobody is reading.
fn draw_left_zen(buf: &mut Buffer, geom: &Geom, ws: Option<&WorkspaceDetail>, theme: &Theme) {
    draw_box(buf, geom.left_box, "", theme.rule, theme.ground);
    let x = geom.left_box.x + 1;
    let bottom = geom.left_box.y + geom.left_box.height.saturating_sub(1);
    let mut y = geom.left_box.y + 1;
    let Some(w) = ws else { return };

    for a in &w.agents {
        if y >= bottom {
            return;
        }
        let (glyph, role) = match (a.exited, a.state) {
            (Some(_), _) | (_, AgentState::Exited) => ("Ax", Role::Faint),
            (_, AgentState::Waiting) => ("A!", Role::Danger),
            (_, AgentState::Working) => ("A~", Role::Attention),
            (_, AgentState::Finished) => ("A*", Role::Info),
            (_, AgentState::Idle) => ("A ", Role::Ink),
        };
        put_str(buf, x, y, glyph, x + 2, Pen::new(theme.role(role), theme.ground));
        y += 1;
    }
    for p in &w.processes {
        if y >= bottom {
            return;
        }
        let (status, color) = proc_status(p, theme);
        let glyph = match status.as_str() {
            s if s.starts_with("FAIL") => "P✗",
            "ok" | "done" => "P✓",
            _ => "P·",
        };
        put_str(buf, x, y, glyph, x + 2, Pen::new(color, theme.ground));
        y += 1;
    }
}

/// Collapsed right rail: the change count, and nothing else.
fn draw_right_zen(buf: &mut Buffer, geom: &Geom, ws: Option<&WorkspaceDetail>, theme: &Theme) {
    draw_box(buf, geom.right_box, "", theme.rule, theme.ground);
    let Some(c) = ws.and_then(|w| w.changes.as_ref()) else { return };
    let n = c.staged.len() + c.unstaged.len() + c.conflicted.len();
    let (x, y) = (geom.right_box.x + 1, geom.right_box.y + 1);
    put_str(buf, x, y, &format!("C{n}"), x + 3, Pen::new(theme.attention, theme.ground));
}

// ---- BOOTH -----------------------------------------------------------------

/// Rows the attention tray reserves, separator included.
///
/// **Fixed, and that is the whole point.** A tray that grew with its contents
/// would push the fleet list down every time an agent started waiting, which is
/// the moving list the fixed order exists to prevent. It reserves its space
/// whether it holds three agents or none — and an empty tray is doing real work,
/// since "nothing needs you" is the state this page is in most of the day and it
/// should be an answer rather than an absence.
pub const BOOTH_TRAY_H: u16 = 4;
/// Narrower than [`tree_width`]: BOOTH's middle column is a live pane, and a
/// terminal squeezed under ~60 columns is worse than a short list.
const BOOTH_FLEET_MIN_W: u16 = 22;
const BOOTH_COMPUTE_MIN_W: u16 = 20;

/// The band BOOTH draws into: the whole row between the tab bar and the footer.
///
/// **The one page that takes the rails' columns**, and the exception is
/// principled rather than convenient. Every other page is about one workspace,
/// so AGENTS and CHANGES beside it are about the same thing it is. BOOTH is about
/// the fleet — the left rail would list one workspace's agents beside a column
/// listing everyone's, and SYSTEM would draw the active machine's gauges beside
/// a column drawing every machine's. Keeping them would not be fixed chrome, it
/// would be the same two questions answered twice, differently, side by side.
///
/// The tab bar and the footer do not move, which is the part of the promise
/// that matters: you always know where you are and how to leave.
pub fn booth_area(_cols: u16, geom: &Geom) -> LRect {
    // Already the whole band once `page_geom` has widened it, which is the one
    // place that arithmetic lives.
    geom.stage_box
}

/// The geometry a page is *drawn into*: [`Chrome::compute`]'s rectangles, with
/// the stage widened to the whole band on the pages that own it.
///
/// [`Chrome`] itself stays page-agnostic on purpose — it is shared by both
/// renderers and must not depend on which page is up — so the widening happens
/// here, once, and everything downstream reads it without knowing. That matters
/// most for [`stage_rect`]: it is the measurement the daemon is told to size a
/// pane to, so a page that widened its body without widening that would be
/// streamed frames the wrong shape, which does not fail loudly, it just looks
/// subtly wrong.
pub fn page_geom(cols: u16, rows: u16, view: &View) -> Geom {
    let mut geom = Geom::compute(cols, rows, view.zen, view.geom, system_h_wanted(&view.gauges));
    if !view.page.owns_full_width() {
        return geom;
    }
    // The rails are not drawn on these pages, so their columns are free — and
    // now that the spaces are a tab-bar menu there is nothing else on the band
    // to reserve, so a full-width page really does start at column zero.
    let band = LRect::new(0, geom.stage_box.y, cols, geom.stage_box.height);
    geom.stage_box = band;
    geom.stage_inner = LRect::new(
        band.x + 1,
        band.y + 1,
        band.width.saturating_sub(2),
        band.height.saturating_sub(2),
    );
    geom
}

/// The BOOTH page's three columns, carved out of the band it is given.
pub struct BoothColumns {
    pub fleet_box: LRect,
    /// Fixed-height tray at the top of the fleet column, under its box border.
    pub tray_rows: LRect,
    /// Separator row between the tray and the fleet list.
    pub fleet_sep: u16,
    pub fleet_rows: LRect,
    pub stage_box: LRect,
    pub stage_inner: LRect,
    pub compute_box: LRect,
    pub compute_rows: LRect,
}

/// Split the stage box into fleet | stage | compute.
///
/// Shares of the width rather than constants, for the same reason
/// [`tree_width`] is: a wide terminal should give the pane the room, a narrow
/// one should still list names. Below the point where all three would be
/// unusable the side columns collapse to zero and BOOTH degrades to the pane
/// alone, which is what every other page does when the rails will not fit.
pub fn booth_columns(stage_box: LRect) -> BoothColumns {
    let w = stage_box.width;
    let mut fleet_w = (w / 4).clamp(BOOTH_FLEET_MIN_W, 40);
    // COMPUTE takes the same share as FLEET rather than a fifth. Its rows are
    // now a trace apiece drawn across the full width of the column, and every
    // cell it gains is two more samples of every machine's history — the one
    // column here where width buys information rather than just fewer ellipses.
    let mut compute_w = (w / 4).clamp(BOOTH_COMPUTE_MIN_W, 36);
    if w < fleet_w + compute_w + MIN_STAGE_W {
        fleet_w = 0;
        compute_w = 0;
    }
    let stage_w = w.saturating_sub(fleet_w + compute_w);

    let fleet_box = LRect::new(stage_box.x, stage_box.y, fleet_w, stage_box.height);
    let stage_box_ = LRect::new(stage_box.x + fleet_w, stage_box.y, stage_w, stage_box.height);
    let compute_box =
        LRect::new(stage_box.x + fleet_w + stage_w, stage_box.y, compute_w, stage_box.height);

    // Fleet interior: tray on top, separator, then the list.
    let inner_h = fleet_box.height.saturating_sub(2);
    let inner_w = fleet_w.saturating_sub(2);
    // The tray yields rather than eating a list that would have nothing left.
    let tray_h = if inner_h >= BOOTH_TRAY_H + 3 { BOOTH_TRAY_H } else { 0 };
    let tray_rows = LRect::new(fleet_box.x + 1, fleet_box.y + 1, inner_w, tray_h);
    let fleet_sep = fleet_box.y + 1 + tray_h;
    let fleet_rows =
        LRect::new(fleet_box.x + 1, fleet_sep + 1, inner_w, inner_h.saturating_sub(tray_h + 1));

    BoothColumns {
        fleet_box,
        tray_rows,
        fleet_sep,
        fleet_rows,
        stage_inner: LRect::new(
            stage_box_.x + 1,
            stage_box_.y + 1,
            stage_box_.width.saturating_sub(2),
            stage_box_.height.saturating_sub(2),
        ),
        stage_box: stage_box_,
        compute_rows: LRect::new(
            compute_box.x + 1,
            compute_box.y + 1,
            compute_box.width.saturating_sub(2),
            compute_box.height.saturating_sub(2),
        ),
        compute_box,
    }
}

/// The fleet column's rows: machine, then space, then its agents.
///
/// The order is a pure function of *identity* — daemon index, then tab order,
/// then spawn order — and reads no agent state at all. That is what makes it
/// safe to click: `all_agent_rows` is already in this order, so an agent's row
/// is where it was an hour ago and a state change redraws the glyph in place
/// rather than moving the row out from under the cursor.
///
/// The alternative was measured and rejected. A list re-sorted by urgency on
/// the daemon's ~2s sampler tick travels ~174 positions per ten ticks at 24
/// agents, and banding plus hysteresis only brought that to 169, because
/// damping changes *when* a row moves and not *how far*. Attention
/// is surfaced by [`booth_tray`] copying rows upward instead.
pub fn booth_rows<'a>(
    all: &'a [AllAgentRow<'a>],
    machines: &'a [MachineRow<'a>],
) -> Vec<BoothRow<'a>> {
    let mut out = Vec::new();
    let mut d = None;
    let mut space: Option<&str> = None;
    for (sel, row) in all.iter().enumerate() {
        if d != Some(row.daemon) {
            d = Some(row.daemon);
            space = None;
            let label = machines.get(row.daemon).map(|m| m.label).unwrap_or("local");
            let agents = all.iter().filter(|r| r.daemon == row.daemon).count();
            out.push(BoothRow::Machine { label, agents, daemon: row.daemon });
        }
        if space != Some(row.workspace) {
            space = Some(row.workspace);
            out.push(BoothRow::Space { name: row.workspace });
        }
        out.push(BoothRow::Agent { row: *row, sel });
    }
    out
}

/// How loudly a tray row is asking, lowest first. `None` means it is not asking
/// at all and stays out of the tray.
///
/// The tray shows [`BOOTH_TRAY_H`] rows and does not scroll, so this is not
/// decoration: past the fourth row the answer is invisible, and a blocked agent
/// pushed off the bottom by three finished ones is the one failure this whole
/// surface exists to prevent.
///
/// The gradations, and why they are in this order:
///
/// 1. **Blocked on you** — a question or a bell. Nothing behind it can move
///    until you answer, so it outranks anything that has already stopped.
/// 2. **Died unread** — a non-zero exit you have not seen. Worse news than a
///    completed turn, and unlike a question it will not ask twice.
/// 3. **Landed unread** — a finished turn, or a clean exit. Your move, in your
///    own time.
fn tray_rank(a: &AgentDto) -> Option<u8> {
    match a.state {
        AgentState::Waiting => Some(0),
        _ if !a.unread => None,
        AgentState::Exited if a.exited.is_some_and(|c| c != 0) => Some(1),
        AgentState::Finished | AgentState::Exited => Some(2),
        // `unread` is only ever set on the two states above, so this is
        // unreachable in practice — and is a quiet `None` rather than a panic
        // because a future daemon is allowed to widen the field without this
        // client, built before it, falling over.
        _ => None,
    }
}

/// The agents that need you: blocked, or holding news you have not read.
///
/// **Copies, not moves.** The originals stay exactly where they are in the
/// fleet list, which is why the tray can be sorted to the top without anything
/// below it shifting. It is the same trick the tab bar already plays with its
/// `!` marker — and it is what lets this list be ordered by urgency at all,
/// given [`booth_rows`] measured urgency-sorting the fleet itself and rejected
/// it.
///
/// Sorted by [`tray_rank`] and *stable*, so within one rank the rows stay in
/// fleet order: the sort decides which agents are visible in four rows, not
/// which of two equally urgent ones is on top, and a tie that re-broke itself
/// every tick would put the shuffling back that ranking is here to avoid.
pub fn booth_tray<'a>(all: &'a [AllAgentRow<'a>]) -> Vec<(usize, &'a AllAgentRow<'a>)> {
    let mut out: Vec<_> =
        all.iter().enumerate().filter(|(_, r)| tray_rank(r.agent).is_some()).collect();
    out.sort_by_key(|(_, r)| tray_rank(r.agent).unwrap_or(u8::MAX));
    out
}

/// The per-row jump button on BOOTH's fleet list.
pub const FLEET_OPEN_LABEL: &str = "[open]";

/// The narrowest fleet column that can afford the button: enough for the
/// sprite, a few columns of title and the label itself. Below it the row is all
/// title and the two-step click is the only way, which is what it was before.
const FLEET_OPEN_MIN_W: u16 = SPRITE_W as u16 + 8 + FLEET_OPEN_LABEL.len() as u16;

/// Where `[open]` sits on a fleet row — right-aligned, and the same span the
/// drawing and the hit-test both read.
///
/// `None` when the column is too narrow to spend six characters on it.
pub fn fleet_open_span(area: LRect) -> Option<(u16, u16)> {
    if area.width < FLEET_OPEN_MIN_W {
        return None;
    }
    let end = area.x + area.width;
    Some((end.saturating_sub(FLEET_OPEN_LABEL.len() as u16), end))
}

/// Which fleet row is at `y`, as an *agent* index — the same number
/// `view.all_agents_sel` holds.
///
/// Resolved through the identical scroll arithmetic the drawing uses, and
/// against the same header-interleaved row list, because BOOTH's list is not a
/// flat one: machine and workspace headers sit between the agents, so the
/// row under the pointer and the selection index are different numbers and
/// only this mapping relates them. A header resolves to `None` rather than to
/// the agent above or below it — clicking a machine's name is not a request to
/// open somebody's agent.
pub fn booth_fleet_row_at(
    cols: &BoothColumns,
    all: &[AllAgentRow<'_>],
    machines: &[MachineRow<'_>],
    sel: usize,
    x: u16,
    y: u16,
) -> Option<usize> {
    let area = cols.fleet_rows;
    if area.height == 0 || !area.contains(x, y) {
        return None;
    }
    let rows = booth_rows(all, machines);
    let visible = area.height as usize;
    let cursor_at = rows
        .iter()
        .position(|r| matches!(r, BoothRow::Agent { sel: s, .. } if *s == sel))
        .unwrap_or(0);
    let first = scroll_for(cursor_at, visible, rows.len());
    let i = first + (y - area.y) as usize;
    match rows.get(i)? {
        BoothRow::Agent { sel, .. } => Some(*sel),
        _ => None,
    }
}

/// Which agent the tray row at `y` is a copy *of* — an index into `all`, the
/// same number [`booth_fleet_row_at`] returns.
///
/// The tray is the answer to "what needs me", so the rows in it are the rows
/// worth reaching first, and until this existed they were the only rows on the
/// page you could not point at: the pointer resolved the list underneath and
/// nothing else. It returns the *original's* index rather than a position in
/// the tray, because the tray holds copies and the page has one cursor — see
/// [`booth_tray`].
///
/// Scrolled with the same [`scroll_for`] the drawing uses, from a fixed top:
/// the tray does not follow the cursor, so a row that is off the bottom of four
/// is not clickable, exactly as it is not visible.
pub fn booth_tray_row_at(
    cols: &BoothColumns,
    all: &[AllAgentRow<'_>],
    x: u16,
    y: u16,
) -> Option<usize> {
    let area = cols.tray_rows;
    if area.height == 0 || !area.contains(x, y) {
        return None;
    }
    let tray = booth_tray(all);
    let visible = area.height as usize;
    let first = scroll_for(0, visible, tray.len());
    tray.get(first + (y - area.y) as usize).map(|(sel, _)| *sel)
}

/// Scroll offset that keeps `sel` inside a window of `height` rows.
///
/// Shared by both BOOTH columns so a wheel and a cursor cannot disagree about
/// where the list starts.
fn scroll_for(sel: usize, height: usize, len: usize) -> usize {
    if height == 0 || len <= height {
        return 0;
    }
    sel.saturating_sub(height.saturating_sub(1)).min(len - height)
}

/// The BOOTH page: the fleet on the left, the selected agent's screen in the
/// middle, and every machine's telemetry on the right.
///
/// Returns whether any sprite is moving, which gates the fast clock exactly as
/// the ALL AGENTS panel does — BOOTH is meant to be left open, so it must cost
/// nothing while every agent is resting.
fn draw_booth_page(
    buf: &mut Buffer,
    width: u16,
    geom: &Geom,
    scene: &Scene<'_>,
    view: &View,
    theme: &Theme,
) -> Painted {
    let cols = booth_columns(booth_area(width, geom));
    let focused = view.focus == Focus::AllAgents;
    let rows = booth_rows(scene.all_agents, scene.machines);
    let tray = booth_tray(scene.all_agents);
    let mut out = Painted::default();

    // ---- fleet column ----
    if cols.fleet_box.width > 0 {
        draw_box(
            buf,
            cols.fleet_box,
            &format!(" FLEET ({}) ", scene.all_agents.len()),
            theme.border(focused),
            theme.ground,
        );

        // The tray: a fixed region, whether or not anything is in it.
        if cols.tray_rows.height > 0 {
            let area = cols.tray_rows;
            let bound = area.x + area.width;
            if tray.is_empty() {
                put_str(
                    buf,
                    area.x,
                    area.y,
                    // Short enough to survive a narrow fleet column: at 160
                    // columns the column is 21 cells and a longer sentence
                    // ellipsizes into nonsense.
                    &ellipsize("nothing needs you", area.width as usize),
                    bound,
                    Pen::new(theme.faint, theme.ground),
                );
            } else {
                let visible = area.height as usize;
                let first = scroll_for(0, visible, tray.len());
                for (i, (idx, row)) in tray.iter().skip(first).take(visible).enumerate() {
                    let y = area.y + i as u16;
                    let (sprite, color, animating) = sprite_for(row.agent, view.fast_tick, theme);
                    out.wants_fast_anim |= animating;
                    // The tray holds copies, so it highlights the *selected
                    // agent's* copy rather than owning a cursor of its own —
                    // otherwise every waiting agent is two things you can select.
                    let bg = theme.row_bg(*idx == view.all_agents_sel && focused);
                    fill_row(buf, area.x, y, bound, bg);
                    put_str(buf, area.x, y, &sprite, bound, Pen::new(color, bg));
                    let where_ = match row.host {
                        Some(h) => format!("{h}:{}", row.workspace),
                        None => row.workspace.to_string(),
                    };
                    // The agent's own spinner is pinned beside the sprite for
                    // the reason the rail pins it: it says where the agent is,
                    // and a marker that has to be chased across the row says it
                    // to nobody.
                    let (glyph, title) = split_status_glyph(&row.agent.title);
                    let mut x = area.x + SPRITE_W as u16 + 1;
                    if !glyph.is_empty() {
                        put_str(buf, x, y, glyph, bound, Pen::new(color, bg));
                        x += glyph.chars().count() as u16 + 1;
                    }
                    let (text, moving) = marquee(
                        &format!("{title} · {where_}"),
                        bound.saturating_sub(x) as usize,
                        view.tick,
                    );
                    out.wants_anim |= moving;
                    put_str(buf, x, y, &text, bound, Pen::new(theme.ink, bg));
                }
            }
            let label = if tray.is_empty() {
                " CLEAR ".to_string()
            } else {
                format!(" NEEDS YOU ({}) ", tray.len())
            };
            let color = if tray.is_empty() { theme.rule } else { theme.danger };
            draw_section_sep(buf, cols.fleet_box, cols.fleet_sep, &label, color, theme.ground);
        }

        // The fleet list, scrolled to keep the cursor in view. The cursor counts
        // agents, so it is mapped back onto this header-interleaved list.
        let area = cols.fleet_rows;
        if area.height > 0 {
            let visible = area.height as usize;
            let cursor_at = rows
                .iter()
                .position(
                    |r| matches!(r, BoothRow::Agent { sel, .. } if *sel == view.all_agents_sel),
                )
                .unwrap_or(0);
            let first = scroll_for(cursor_at, visible, rows.len());
            let bound = area.x + area.width;
            for (i, row) in rows.iter().skip(first).take(visible).enumerate() {
                let y = area.y + i as u16;
                match row {
                    BoothRow::Machine { label, agents, .. } => {
                        fill_row(buf, area.x, y, bound, theme.ground);
                        let n = agents.to_string();
                        let nw = n.chars().count() as u16;
                        let (text, moving) = marquee(
                            label,
                            bound.saturating_sub(area.x + nw + 1) as usize,
                            view.tick,
                        );
                        out.wants_anim |= moving;
                        put_str(
                            buf,
                            area.x,
                            y,
                            &text,
                            bound.saturating_sub(nw),
                            Pen::new(theme.ink, theme.ground),
                        );
                        put_str(
                            buf,
                            bound.saturating_sub(nw),
                            y,
                            &n,
                            bound,
                            Pen::new(theme.faint, theme.ground),
                        );
                    }
                    BoothRow::Space { name } => {
                        fill_row(buf, area.x, y, bound, theme.ground);
                        let (text, moving) =
                            marquee(name, bound.saturating_sub(area.x + 1) as usize, view.tick);
                        out.wants_anim |= moving;
                        put_str(
                            buf,
                            area.x + 1,
                            y,
                            &text,
                            bound,
                            Pen::new(theme.muted, theme.ground),
                        );
                    }
                    BoothRow::Agent { row, sel } => {
                        let (sprite, color, animating) =
                            sprite_for(row.agent, view.fast_tick, theme);
                        out.wants_fast_anim |= animating;
                        let cursor = *sel == view.all_agents_sel && focused;
                        let bg = theme.row_bg(cursor);
                        fill_row(buf, area.x, y, bound, bg);
                        let x = area.x + 1;
                        put_str(buf, x, y, &sprite, bound, Pen::new(color, bg));
                        // `[open]` is right-aligned and the title stops short of
                        // it, so a long title cannot run under the button and
                        // leave it unreadable on the row you are aiming at.
                        let open = fleet_open_span(area);
                        let title_end = match open {
                            Some((start, _)) => start.saturating_sub(1),
                            None => bound,
                        };
                        let (glyph, title) = split_status_glyph(&row.agent.title);
                        let mut tx = x + SPRITE_W as u16 + 1;
                        if !glyph.is_empty() {
                            put_str(buf, tx, y, glyph, title_end, Pen::new(color, bg));
                            tx += glyph.chars().count() as u16 + 1;
                        }
                        let (text, moving) =
                            marquee(title, title_end.saturating_sub(tx) as usize, view.tick);
                        out.wants_anim |= moving;
                        put_str(buf, tx, y, &text, title_end, Pen::new(theme.ink, bg));
                        if let Some((start, _)) = open {
                            // Brighter on the row the cursor is on, so the one
                            // that answers Enter is the one that looks pressable
                            // — the others are still clickable, which is the
                            // whole point of the button.
                            let ink = if cursor { theme.ink } else { theme.faint };
                            put_str(buf, start, y, FLEET_OPEN_LABEL, bound, Pen::new(ink, bg));
                        }
                    }
                }
            }
        }
    }

    // ---- the stage ----
    //
    // Titled with the machine as well as the agent. Two projects routinely run
    // an agent of the same name, and on this page the two are one row apart, so
    // an unqualified title is how you type into the wrong host's pane.
    let selected = scene.all_agents.get(view.all_agents_sel);
    let title = match selected {
        Some(r) => {
            let machine = scene.machines.get(r.daemon).map(|m| m.label).unwrap_or("local");
            format!(" {} · {machine}:{} ", r.agent.title, r.workspace)
        }
        None => " STAGE ".to_string(),
    };
    draw_box(
        buf,
        cols.stage_box,
        &ellipsize(&title, cols.stage_box.width.saturating_sub(2) as usize),
        theme.border(view.focus == Focus::Stage),
        theme.ground,
    );

    // ---- compute column ----
    if cols.compute_box.width > 0 {
        draw_box(buf, cols.compute_box, " COMPUTE ", theme.rule, theme.ground);
        let area = cols.compute_rows;
        let bound = area.x + area.width;
        // A name, then two rows per gauge. The column scrolls as a whole,
        // because a machine is a block and splitting one across the fold would
        // put a GPU under the wrong name.
        let mut y = area.y;
        let bottom = area.y + area.height;
        for (i, m) in scene.machines.iter().enumerate().skip(view.booth_compute_scroll) {
            if y >= bottom {
                break;
            }
            // A machine that is away says so where its agent count goes, and
            // its whole row drops to `faint`. Both halves are needed: the word
            // is what you read, and the colour is what you notice without
            // reading — these gauges go on animating from the last telemetry
            // the machine sent, and a moving trace is a strong claim to be
            // alive.
            let n = if m.live { format!("{} agents", m.agents) } else { "away".to_string() };
            let nw = n.chars().count() as u16;
            let (text, moving) =
                marquee(m.label, bound.saturating_sub(area.x + nw + 1) as usize, view.tick);
            out.wants_anim |= moving;
            put_str(
                buf,
                area.x,
                y,
                &text,
                bound.saturating_sub(nw),
                Pen::new(if m.live { theme.ink } else { theme.faint }, theme.ground),
            );
            put_str(
                buf,
                bound.saturating_sub(nw),
                y,
                &n,
                bound,
                Pen::new(if m.live { theme.faint } else { theme.attention }, theme.ground),
            );
            y += 1;
            // The same gauges the SYSTEM rail draws, against this machine's own
            // telemetry. One renderer, so the two cannot drift.
            let gauges = LRect::new(area.x, y, area.width, bottom.saturating_sub(y));
            if gauges.height > 0 {
                // The renderer's own answer, not a recomputation: this
                // arithmetic was `2 + gpus` back when a gauge was one row and
                // network did not exist, then `n * GAUGE_H` until the network
                // gauge stopped being the same height as the rest. Every version
                // of it could disagree with what was drawn; asking cannot. It
                // already stops on whole gauges, which is what keeps the next
                // machine's name off the previous one's trace.
                let gs = system_gauges(m.sys, &view.net, &view.disks);
                y += draw_system(buf, gauges, m.sys, &gs, theme);
            }
            // A blank row between machines, except after the last.
            if i + 1 < scene.machines.len() {
                y += 1;
            }
        }
    }

    out
}

/// Paint one row's background across a span.
fn fill_row(buf: &mut Buffer, x0: u16, y: u16, bound: u16, bg: Color) {
    for x in x0..bound {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(" ");
            cell.set_bg(bg);
        }
    }
}

/// Where the streamed pane goes. The one measurement that crosses the wire.
///
/// Page-dependent, because the Docker page puts the pane in the logs column
/// beside its list rather than across the whole stage. The daemon is told this
/// as the pane connection's `cols`/`rows`, so a page that gets it wrong sends
/// frames the wrong shape.
pub fn stage_rect(cols: u16, rows: u16, view: &View) -> Rect {
    let geom = page_geom(cols, rows, view);
    if view.page == Page::Docker {
        return to_rect(docker_logs_inner(geom.stage_box));
    }
    if view.page == Page::Booth {
        return to_rect(booth_columns(booth_area(cols, &geom)).stage_inner);
    }
    to_rect(geom.stage_inner)
}

/// The interior of the Docker page's logs box: the stage minus the list column,
/// minus the box border, minus the row of action hints.
///
/// Public because a selection is clipped to the column it began in, and this is
/// that column — a drag over the logs that wrapped at the band's edge instead
/// would carry a container name onto the front of every line it copied.
pub fn docker_logs_inner(stage_box: LRect) -> LRect {
    let list_w = tree_width(stage_box.width);
    LRect::new(
        stage_box.x + list_w + 1,
        stage_box.y + 1,
        stage_box.width.saturating_sub(list_w + 2),
        stage_box.height.saturating_sub(3),
    )
}

/// How many rows the diff body gets: the stage interior, less the hint row.
pub fn diff_body_rows(cols: u16, rows: u16, view: &View) -> u16 {
    stage_rect(cols, rows, view).height.saturating_sub(1)
}

/// The GIT page's list column.
///
/// Wider than [`tree_width`]'s 16–40, because a history row is four things
/// where a tree row is one: lane glyphs, a short sha, a ref chip and a summary.
/// At 40 cells the summary was down to a dozen characters, which is a column of
/// ellipses.
fn git_list_width(band_w: u16) -> u16 {
    (band_w / 3).clamp(30, 52).min(band_w.saturating_sub(MIN_STAGE_W))
}

/// The GIT page's three boxes: REFS over HISTORY on the left, the body right.
pub struct GitColumns {
    pub refs_box: LRect,
    pub refs_rows: LRect,
    pub hist_box: LRect,
    pub hist_rows: LRect,
    pub body_box: LRect,
}

/// Split the band into the page's boxes.
///
/// REFS takes a share rather than a constant, for the reason [`tree_width`]
/// does: on a tall terminal the branch list should not be a third of the
/// screen, and on a short one HISTORY must still have rows. The floor is what
/// keeps the graph usable — below it REFS collapses to nothing and the page
/// degrades to history alone, which is the same way every other surface here
/// gives up its side column.
pub fn git_columns(band: LRect) -> GitColumns {
    let list_w = git_list_width(band.width);
    let body_w = band.width.saturating_sub(list_w);

    // The floor check comes first, and it has to: `clamp` panics when its low
    // bound exceeds its high one, and on a short band `band.height / 2` is
    // below `GIT_REFS_MIN_H`. Written the other way round this crashed the TUI
    // out of raw mode on any terminal under 14 rows — every path onto the page
    // reaches here, including the hit test and a resize while already on it.
    let refs_h = if band.height < GIT_REFS_MIN_H + GIT_HIST_MIN_H {
        0
    } else {
        // A third to REFS: enough for the working-tree row, a heading and a few
        // branches, never more than half the column.
        (band.height / 3).clamp(GIT_REFS_MIN_H, band.height / 2)
    };
    let hist_h = band.height.saturating_sub(refs_h);

    let refs_box = LRect::new(band.x, band.y, list_w, refs_h);
    let hist_box = LRect::new(band.x, band.y + refs_h, list_w, hist_h);
    let inner = |b: LRect| {
        LRect::new(b.x + 1, b.y + 1, b.width.saturating_sub(2), b.height.saturating_sub(2))
    };
    GitColumns {
        refs_rows: inner(refs_box),
        refs_box,
        hist_rows: inner(hist_box),
        hist_box,
        body_box: LRect::new(band.x + list_w, band.y, body_w, band.height),
    }
}

/// The diff, drawn from the parsed patch.
///
/// The daemon ran this text through syntect's diff grammar; the marker tint is
/// what that grammar produces for `+`, `-` and `@@` lines, and it is the whole
/// of what a diff's colour says. Real syntax highlighting of the *content*
/// arrives with the editor.
fn draw_diff_page(buf: &mut Buffer, geom: &Geom, diff: Option<&DiffView>, theme: &Theme) {
    draw_diff_in(buf, geom.stage_box, diff, true, theme);
}

/// The diff, in whatever box it was given.
///
/// Split out from [`draw_diff_page`] so the GIT page's body is the *same*
/// renderer rather than a second one: two diff views would drift in their
/// colours, their gutter and their hint row, and the one on the newer page
/// would be the one nobody noticed had fallen behind. `placeholder` is what an
/// empty box says, since "no diff yet" means something different on each page.
pub fn draw_diff_in(
    buf: &mut Buffer,
    outer: LRect,
    diff: Option<&DiffView>,
    focused: bool,
    theme: &Theme,
) {
    let Some(diff) = diff else {
        draw_box(buf, outer, " DIFF ", theme.rule, theme.ground);
        return;
    };
    let title = diff.kind.as_ref().map(DiffKind::title).unwrap_or_else(|| "diff".into());
    draw_box(buf, outer, &format!(" {title} "), theme.border(focused), theme.ground);

    let inner = LRect::new(
        outer.x + 1,
        outer.y + 1,
        outer.width.saturating_sub(2),
        outer.height.saturating_sub(2),
    );
    let bound = inner.x + inner.width;
    // One row at the bottom for the key hints, the way the pane's own footer
    // worked — the keys here are not the workbench's, so the workbench footer
    // cannot carry them.
    let body = inner.height.saturating_sub(1);
    let cursor_row = diff.cursor_row();

    // How wide the two number columns are here, which depends on this patch and
    // on this box. Zero on a narrow body, where the text needs every cell.
    let nums = diff.numbers_w(inner.width);
    let digits = diff.digits as usize;
    let text_x = inner.x + DIFF_GUTTER_W + nums;
    let text_w = inner.width.saturating_sub(DIFF_GUTTER_W + nums) as usize;

    for (row, idx) in (diff.scroll..diff.rows.len()).enumerate().take(body as usize) {
        let y = inner.y + row as u16;
        let line = &diff.rows[idx];
        let anchor = diff.anchors.get(idx).copied().flatten();
        let on_cursor_hunk =
            anchor.is_some_and(|(f, h, _)| (f, h) == (diff.cursor.file, diff.cursor.hunk));
        let is_picked = diff.mode == DiffMode::Lines
            && anchor.is_some_and(|(f, h, l)| {
                (f, h) == (diff.cursor.file, diff.cursor.hunk) && diff.picked.contains(&l)
            });
        let selected = Some(idx) == cursor_row;
        let bg = theme.row_bg(selected);
        for x in inner.x..bound {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_bg(bg);
            }
        }
        // The hunk under the cursor, and any line picked out of it, are marked
        // in the gutter rather than by colour alone: this view is already three
        // colours deep and a fourth would not read.
        //
        // A file card is the exception, and it has to be: its own fold arrow is
        // already `>` when the file is shut, and a cursor marker beside it drew
        // `>>`. The card takes the colour instead — the glyph it wears anyway
        // goes accent when the cursor is on it, so the row is still marked
        // without two arrows meaning two different things one cell apart.
        let on_card = matches!(line, DiffRow::File { .. });
        let gutter = if on_card {
            " "
        } else if is_picked {
            "#"
        } else if selected {
            ">"
        } else if on_cursor_hunk && diff.mutable() {
            "|"
        } else {
            " "
        };
        let gutter_fg = if is_picked { theme.ok } else { theme.accent };
        put_str(buf, inner.x, y, gutter, bound, Pen::new(gutter_fg, bg));

        match line {
            // The file card's head. It spans the whole width rather than
            // starting after the number columns: it is about the file, not
            // about any line in it, and lining it up with the code would hide
            // the one row that says where you are.
            DiffRow::File { path, added, removed, note, folded } => {
                let mark = if *folded { ">" } else { "v" };
                let tail = format!("+{added} -{removed}");
                let tail_w = (tail.width() as u16).min(inner.width);
                let mut x = inner.x + DIFF_GUTTER_W;
                let mark_fg = if selected { theme.accent } else { theme.faint };
                put_str(buf, x, y, mark, bound, Pen { fg: mark_fg, bg, bold: selected });
                x += 2;
                let room = bound.saturating_sub(tail_w + 1).saturating_sub(x) as usize;
                let name = ellipsize(path, room);
                put_str(
                    buf,
                    x,
                    y,
                    &name,
                    bound.saturating_sub(tail_w + 1),
                    Pen { fg: theme.ink, bg, bold: true },
                );
                if let Some(note) = note {
                    let at = x + name.width() as u16 + 1;
                    put_str(
                        buf,
                        at,
                        y,
                        note,
                        bound.saturating_sub(tail_w + 1),
                        Pen::new(theme.attention, bg),
                    );
                }
                // Two colours in one token, so the shape of the change reads
                // before the numbers do.
                let plus = format!("+{added}");
                put_str(buf, bound.saturating_sub(tail_w), y, &plus, bound, Pen::new(theme.ok, bg));
                put_str(
                    buf,
                    bound.saturating_sub(tail_w) + plus.width() as u16,
                    y,
                    &format!(" -{removed}"),
                    bound,
                    Pen::new(theme.danger, bg),
                );
            }
            DiffRow::Rule => {
                let rule: String =
                    "\u{2500}".repeat(inner.width.saturating_sub(DIFF_GUTTER_W) as usize);
                put_str(buf, inner.x + DIFF_GUTTER_W, y, &rule, bound, Pen::new(theme.rule, bg));
            }
            // With numbers on, the `@@` ranges are already drawn down the side
            // of every line below, so the separator carries only the thing they
            // do not: the function the hunk is in. Without them it keeps the
            // literal header, which is then the only orientation there is.
            DiffRow::Hunk { old, new, section } => {
                if nums > 0 {
                    put_str(
                        buf,
                        inner.x + DIFF_GUTTER_W,
                        y,
                        &format!("{:>w$} {:>w$} \u{2502}", "...", "...", w = digits),
                        bound,
                        Pen::new(theme.faint, bg),
                    );
                    // The leading space keeps the section over the *code*, not
                    // over the markers: a context line reads `│ line27`, so a
                    // separator reading `│line27` sits one cell to its left and
                    // the column stops being a column.
                    let text = ellipsize(&format!(" {section}"), text_w);
                    put_str(buf, text_x, y, &text, bound, Pen::new(theme.accent, bg));
                } else {
                    let head = if section.is_empty() {
                        format!("@@ -{old} +{new} @@")
                    } else {
                        format!("@@ -{old} +{new} @@ {section}")
                    };
                    let text = ellipsize(&head, text_w);
                    put_str(buf, text_x, y, &text, bound, Pen::new(theme.accent, bg));
                }
            }
            DiffRow::Line { old, new, origin, text } => {
                if nums > 0 {
                    let side = |n: &Option<usize>| match n {
                        Some(n) => format!("{n:>digits$}"),
                        None => " ".repeat(digits),
                    };
                    put_str(
                        buf,
                        inner.x + DIFF_GUTTER_W,
                        y,
                        &format!("{} {} \u{2502}", side(old), side(new)),
                        bound,
                        Pen::new(theme.faint, bg),
                    );
                }
                let (marker, fg) = match origin {
                    Origin::Context => (' ', theme.ink),
                    Origin::Added => ('+', theme.ok),
                    Origin::Removed => ('-', theme.danger),
                    Origin::NoNewline => ('\\', theme.faint),
                };
                // The marker travels with the text rather than sitting in the
                // gutter, so a line dragged out of this view still pastes as a
                // patch line — `crate::selection` clips the copy to exactly
                // here, which is what `gutter_w` is for.
                let body = ellipsize(&format!("{marker}{text}"), text_w);
                put_str(buf, text_x, y, &body, bound, Pen::new(fg, bg));
            }
            DiffRow::Note(text) => {
                let text = ellipsize(text, text_w);
                put_str(buf, text_x, y, &text, bound, Pen::new(theme.faint, bg));
            }
        }
    }

    if inner.height > 0 {
        let y = inner.y + inner.height - 1;
        let (text, fg) = match &diff.notice {
            Some(n) => (n.clone(), theme.danger),
            None => (diff.hints(), theme.faint),
        };
        for x in inner.x..bound {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_bg(theme.ground);
            }
        }
        put_str(
            buf,
            inner.x,
            y,
            &ellipsize(&text, inner.width as usize),
            bound,
            Pen::new(fg, theme.ground),
        );
    }
}

/// One tab's chip: what it reads and where it sits.
///
/// The label is built here and nowhere else, because the draw and the click
/// have to agree to the column. The daemon's version kept the two apart and
/// left a comment warning that the hit box drifts off the button when they
/// disagree — which is a note about a bug waiting to happen, not a design.
pub fn tab_label(i: usize, tab: &Tab<'_>, active: bool) -> String {
    let s = tab.summary;
    // `!` when something inside wants you — the same signal the tab bar has
    // always carried, now computed from the summary's counts.
    let attention = s.waiting > 0 || s.questions > 0;
    // A host badge only where it disambiguates. With one daemon connected every
    // tab would carry the same word, which is noise in the narrowest row.
    let host = tab.host.map(|h| format!("{h}:")).unwrap_or_default();
    let name = format!("{}:{host}{}{}", i + 1, s.name, if attention { " !" } else { "" });
    // A machine that is not answering takes one of the two padding columns as a
    // marker rather than growing the chip. Width is load-bearing here: chips
    // that changed size as machines came and went would slide the whole strip
    // sideways under the pointer, and `tab_close_span` measures this very
    // string to find the `[x]`. Same reason the `!` is a character and not just
    // a colour — colour is the first thing a terminal, a theme or a screenshot
    // loses.
    let mark = if tab.live { ' ' } else { TAB_AWAY_MARK };
    // Bracketed when it is the one you are on, and only then carrying `[x]`,
    // because it is the only workspace the button could mean.
    if active {
        format!("[{mark}{name} {TAB_CLOSE_MARK} ]")
    } else {
        format!(" {mark}{name}  ")
    }
}

/// What a chip wears while its machine is not answering.
///
/// A dot rather than a word: it sits in a column the chip already reserved for
/// padding, so nothing on the busiest row of the interface moves when a laptop
/// closes.
pub const TAB_AWAY_MARK: char = '·';

/// The close button on the active chip. One press arms it and the next carries
/// it out — see [`Overlay::Confirm`]'s `CloseWorkspace`, which is what the
/// click opens.
pub const TAB_CLOSE_MARK: &str = "[x]";

/// Absolute extent of the active chip's `[x]`, when that chip is on screen.
///
/// Derived from the label [`tab_label`] built, not from a second guess at the
/// layout: the daemon kept the two apart and left a comment warning that the
/// hit box drifts off the button, which is a bug waiting rather than a design.
pub fn tab_close_span(
    area: &LRect,
    tabs: &[Tab<'_>],
    view: &View,
    daemons: usize,
) -> Option<(u16, u16)> {
    let active = active_chip(view)?;
    let (start, end) = (*tab_strip(area, tabs, view, daemons).spans.get(active)?)?;
    let tab = tabs.get(active)?;
    let len = tab_label(active, tab, true).chars().count() as u16;
    let w = TAB_CLOSE_MARK.chars().count() as u16;
    // The label ends ` [x] ]`, so the mark sits two columns in from its right
    // edge. Measured off the string the drawing built rather than guessed at,
    // which is the whole reason both go through `tab_label`.
    let bstart = start + len.saturating_sub(w + 2);
    (bstart + w <= end).then_some((bstart, bstart + w))
}

/// Columns the label field of the spaces button reserves — the longest space
/// name there is, so the control does not change width as you move between
/// them. `docker` at the time of writing.
fn spaces_label_w() -> u16 {
    Page::ORDER.iter().map(|p| p.label().len() as u16).max().unwrap_or(6)
}

/// The chevron that says the button opens a list, in ASCII.
///
/// Not `▾`. That codepoint is East-Asian-ambiguous and renders two cells wide in
/// some terminals, which does not look wrong — it shifts every cell after it on
/// the row, and this row is measured to the column.
pub const SPACES_MARK: &str = "v";

/// Columns the bar holds for the spaces button, whatever it currently reads.
///
/// The button's *ink* is as wide as the space it names — `[git v]` is seven
/// cells and `[agents v]` is ten — but the columns are reserved at the widest,
/// and the ink is right-aligned inside them. So the chip strip's bound does not
/// move as you switch space, and no button is padded to make that true. The
/// blank is outside the brackets, where it reads as the gap before a control
/// rather than a hole inside one.
fn spaces_region_w() -> u16 {
    Page::ORDER.iter().map(|p| p.label().len() as u16).max().unwrap_or(6) + spaces_frame_w()
}

/// `[`, a gap, the chevron, `]` — everything the button is besides its word.
fn spaces_frame_w() -> u16 {
    3 + SPACES_MARK.len() as u16
}

/// The spaces button — `[agents v]` — right-aligned before the machines button.
///
/// **One control where there were two.** The spaces used to be a rail down the
/// left edge at wide terminal sizes and a row of six buttons on this bar at
/// narrow ones, which is two spellings of one piece of state and a layout that
/// rearranged itself as the terminal crossed 154 columns. This is neither: it is
/// the same control at every width, and it costs at most ten columns of a row
/// that has them where the buttons cost 51 and the rail cost 14 columns of
/// *screen* off the one page that could least afford them.
///
/// The span is the ink, not the reservation — a click lands on the button you
/// can see rather than on the blank held open beside it. [`spaces_region_w`] is
/// what [`tabbar_cluster`] reserves.
///
/// `None` when the bar is too narrow to carry it, on the same terms the buttons
/// were dropped on: the chips say where you are, and this is a pointer spelling
/// of keys that already exist.
pub fn spaces_button_span(area: &LRect, view: &View, daemons: usize) -> Option<(u16, u16)> {
    let start = spaces_region_x(area, daemons)?;
    let w = spaces_word(view).len() as u16 + spaces_frame_w();
    let end = start + spaces_region_w();
    Some((end - w, end))
}

/// Where the reserved columns start, and `None` when the bar cannot afford them.
fn spaces_region_x(area: &LRect, daemons: usize) -> Option<u16> {
    /// Columns kept for the chips; below this the row is tabs only.
    const MIN_TABS: u16 = 12;
    // Laid out leftwards from the machines button, the leftmost of the controls
    // that are about the client rather than a project. There is nothing between
    // the two clusters any more: SETTINGS used to sit here, and now sits in the
    // footer.
    let (mx, _) = machines_span(area, daemons);
    let x0 = mx.saturating_sub(2 + spaces_region_w());
    (x0 >= tabbar_chips_x0(area) + MIN_TABS).then_some(x0)
}

/// The word the button carries: the space you are on, or `views` while you are
/// somewhere that is not one — so the control never claims you are in a space
/// you are not.
fn spaces_word(view: &View) -> &'static str {
    if view.page.is_space() {
        view.page.label()
    } else {
        "views"
    }
}

/// What the spaces button reads.
///
/// Built in one place because the drawing and [`spaces_button_span`] have to
/// agree to the column, the same reason [`tab_label`] is.
pub fn spaces_label(view: &View) -> String {
    format!("[{} {SPACES_MARK}]", spaces_word(view))
}

/// The spaces menu's rows: every space, its badge, and a mark on the one you
/// are on. Index-aligned with [`Page::ORDER`], which is what the choice reads.
pub fn spaces_menu_rows(
    view: &View,
    ws: Option<&WorkspaceDetail>,
    usage: Option<&usage::Usage>,
) -> Vec<String> {
    let lw = spaces_label_w() as usize;
    Page::ORDER
        .iter()
        .map(|p| {
            let here = if *p == view.page { ">" } else { " " };
            match page_badge(*p, ws, usage) {
                Some((badge, _)) => format!("{here}{:<lw$}  {badge}", p.label()),
                None => format!("{here}{}", p.label()),
            }
        })
        .collect()
}

/// Which chip the bar draws as the one you are on, if any.
///
/// `None` while BOOTH has the screen: BOOTH and a workspace are alternatives,
/// not layers, so no chip is bracketed there or the bar would claim you were in
/// two places at once. One function because the label, the width the strip
/// reserves for it and the colour it is painted in all have to agree.
pub fn active_chip(view: &View) -> Option<usize> {
    (view.page != Page::Booth).then_some(view.tab)
}

/// Columns the chips keep before the bar hands any of them to the buttons.
///
/// Under this the right-hand cluster is dropped instead and the chips take the
/// whole row: on a bar that narrow the tabs are the only thing left worth
/// drawing, and `[+ new]` is a second spelling of a key.
const MIN_STRIP: u16 = 24;

/// The chip strip's own scroll buttons, at its right end.
pub const TAB_PREV_LABEL: &str = "[<]";
pub const TAB_NEXT_LABEL: &str = "[>]";

/// `[<] [>]` plus the column of gap that separates them from the last chip.
const ARROWS_W: u16 = 8;

/// The narrowest strip worth putting arrows on: below this they would take more
/// than they give back.
const MIN_CHIP: u16 = 12;

/// The bar's fixed right-hand cluster: the machine count, the space buttons
/// where the bar carries them, `[+ host]` and `[+ new]`.
///
/// This is the reservation the chips cannot cross, and it is why they scroll.
/// The rule used to run the other way — the chips took the columns they wanted
/// and each control was dropped as they reached it — so the client with the most
/// projects was the one with no `[+ new]` to open another, and the chips it
/// spent those columns on ran off the right edge where no pointer can reach
/// them. Fixed furniture first; what is left is the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cluster {
    /// Leftmost column anything in it occupies, and so where the chips stop.
    /// The bar's right edge when the whole cluster has been dropped.
    pub left: u16,
    /// The bar is wide enough to keep the machines button and `[+ new]`.
    pub buttons: bool,
}

impl Cluster {
    /// Where the chips stop, exclusive — a column short of the cluster, so the
    /// two can never share a cell.
    pub fn chip_bound(&self, area: &LRect) -> u16 {
        if self.left < area.x + area.width {
            self.left - 1
        } else {
            self.left
        }
    }
}

/// Work out what the bar can afford to keep at this width.
///
/// Each control is weighed against the columns it actually costs the chips, not
/// against the cluster as a whole. The spaces button bounds the strip before it
/// scrolls, so where the bar carries it the machines button and `[+ new]` sit
/// right of that bound and are free — dropping them would hand the chips
/// nothing.
///
/// It used to have a third thing to weigh: the machine count hung off the
/// *left* of whichever cluster was there and had to be dropped separately. It
/// is the machines button now, so there is one less control on the row and one
/// less rule here.
pub fn tabbar_cluster(area: &LRect, daemons: usize) -> Cluster {
    let x0 = tabbar_chips_x0(area);
    let edge = area.x + area.width;
    // The reservation, not the ink: the chips stop where the columns held for
    // the button start, so a shorter space name leaves a wider gap rather than
    // moving the strip.
    let spaces = spaces_region_x(area, daemons);
    let anchor = spaces.unwrap_or_else(|| machines_span(area, daemons).0);
    let buttons = spaces.is_some() || anchor >= x0 + MIN_STRIP;
    if !buttons {
        return Cluster { left: edge, buttons: false };
    }
    Cluster { left: anchor, buttons }
}

/// Where the workspace chips sit, and the arrows that reach the ones that do
/// not fit.
///
/// One structure rather than a span function and a scroll rule kept apart,
/// because the drawing and [`crate::hit::at`] have to agree to the column — the
/// same reason [`tab_label`] is built in one place.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TabStrip {
    /// One entry per tab, in order, and `None` for a chip the strip has
    /// scrolled past or has no room for. Index-aligned so a span still says
    /// *which* workspace it belongs to once the strip is scrolled.
    pub spans: Vec<Option<(u16, u16)>>,
    /// `[<]`, and the workspace it reaches: the last one off the left edge.
    pub prev: Option<((u16, u16), usize)>,
    /// `[>]`, and the first one off the right edge.
    pub next: Option<((u16, u16), usize)>,
}

/// Lay the chips out inside the columns [`tabbar_cluster_left`] leaves them.
///
/// Scrolled to keep the active chip whole, only far enough — the same policy as
/// [`first_visible`], for the same reason: a strip that recentres on every
/// switch is one you cannot read. The scroll is *derived* from which tab is
/// active rather than kept on the [`View`], because the strip has a cursor and
/// deriving it is what stops the drawing and the hit test from drifting apart.
/// That is also what the arrows mean: they are the pointer's spelling of
/// `alt-<` / `alt->`, and each one selects the nearest workspace the strip is
/// not showing, which is the press that brings it into view.
///
/// Takes the whole [`View`] rather than `view.tab`, because a chip's *width*
/// depends on more than which tab is active: while BOOTH is up none of them is
/// bracketed, and sizing one as though it were left every chip right of it
/// hit-testable four columns from where it was painted.
pub fn tab_strip(area: &LRect, tabs: &[Tab<'_>], view: &View, daemons: usize) -> TabStrip {
    let x0 = tabbar_chips_x0(area);
    let bound = tabbar_cluster(area, daemons).chip_bound(area);
    let mut out = TabStrip { spans: vec![None; tabs.len()], ..Default::default() };
    if tabs.is_empty() || bound <= x0 {
        return out;
    }
    // The scroll follows `view.tab` even while BOOTH has the screen — coming
    // back to a workspace should find the strip where you left it — but the
    // bracketing follows what the bar actually paints.
    let active = view.tab.min(tabs.len() - 1);
    let bracketed = active_chip(view).map(|i| i.min(tabs.len() - 1));
    let widths: Vec<u16> = tabs
        .iter()
        .enumerate()
        .map(|(i, t)| tab_label(i, t, Some(i) == bracketed).chars().count() as u16)
        .collect();
    let total: u32 = widths.iter().map(|w| *w as u32).sum();
    let strip_w = bound - x0;

    // The arrows cost columns, so they exist only where they can earn them:
    // more than one workspace, not all of them fitting, and a strip wide enough
    // to still show a chip once they have taken their end of it.
    let arrows = tabs.len() > 1 && total > strip_w as u32 && strip_w > ARROWS_W + MIN_CHIP;
    let usable = if arrows { strip_w - ARROWS_W } else { strip_w };
    let right = x0 + usable;

    let mut first = 0;
    while first < active {
        let used: u32 = widths[first..=active].iter().map(|w| *w as u32).sum();
        if used <= usable as u32 {
            break;
        }
        first += 1;
    }

    // From `first`, in order, until the columns run out. The last one may be
    // clipped exactly as the drawing clips it, so a chip that is half on screen
    // is clickable across the half that is.
    let mut x = x0;
    let mut last = first;
    for (i, w) in widths.iter().enumerate().skip(first) {
        if x >= right {
            break;
        }
        out.spans[i] = Some((x, (x + w).min(right)));
        last = i;
        x += w;
    }

    if arrows {
        let px = bound - (ARROWS_W - 1);
        let nx = px + TAB_PREV_LABEL.len() as u16 + 1;
        out.prev = first.checked_sub(1).map(|i| ((px, px + TAB_PREV_LABEL.len() as u16), i));
        out.next =
            (last + 1 < tabs.len()).then_some(((nx, nx + TAB_NEXT_LABEL.len() as u16), last + 1));
    }
    out
}

/// The `[+ new]` and machines buttons, when there is room for them.
///
/// They sit together because the two are the same gesture at different scales —
/// one adds a project, the other adds a machine full of them — and both put
/// tabs in the bar they sit on. They are dropped together, and only on a bar too
/// narrow for [`tabbar_cluster`] to reserve them a place; above that width the
/// chips scroll rather than push, so how many workspaces are open no longer
/// decides whether you can open another.
///
/// The machines label is owned rather than `&'static` now that it counts, so
/// this hands back strings and the caller matches on which span it is rather
/// than on the text.
pub fn tabbar_buttons(area: &LRect, daemons: usize) -> Vec<(String, u16, u16)> {
    if !tabbar_cluster(area, daemons).buttons {
        return Vec::new();
    }
    let (nx, nend) = tabbar_new_span(area);
    let (mx, mend) = machines_span(area, daemons);
    vec![(TAB_NEW_LABEL.to_string(), nx, nend), (machines_label(daemons), mx, mend)]
}

/// The BOOTH chip, leftmost on the tab bar.
///
/// On the bar rather than in the spaces menu because BOOTH is a peer of the
/// workspaces, not a view of one — the bar answers "which of my things am I
/// looking at", and "all of them, everywhere" is an answer to that question and
/// not to "which view of this project".
pub const TAB_BOOTH_LABEL: &str = "booth";

/// The rule between the BOOTH chip and the workspace chips.
///
/// BOOTH is a peer of the workspaces rather than one of them — it is every
/// project on every machine, and they are one project each — and on a row of
/// look-alike chips that distinction was carried by nothing but a space. The
/// glyph is the one [`draw_box`] rules the rest of the workbench with, so it is
/// single-width wherever the boxes are.
pub const TAB_SEP: &str = "│";

/// The narrowest bar worth ruling.
///
/// The same width the right-hand cluster gives up at, and for the same reason:
/// below it the chips are the only thing left worth drawing, and two columns of
/// rule are two columns of project name. At 24 columns it cost `a-project` its
/// last letter.
const SEP_MIN_W: u16 = 52;

/// Where the workspace chips start: clear of the BOOTH chip and its rule.
///
/// One function because [`tab_strip`] lays the chips out from here and
/// [`tabbar_cluster`] measures the strip against it, and a separator that moved
/// one of them and not the other would put a chip under the rule.
pub fn tabbar_chips_x0(area: &LRect) -> u16 {
    match tabbar_sep_x(area) {
        Some(x) => x + 2,
        None => tabbar_booth_span(area).1 + 1,
    }
}

/// The rule's own column, a space clear of the BOOTH chip, or `None` on a bar
/// too narrow to spend two columns saying what a space already says.
pub fn tabbar_sep_x(area: &LRect) -> Option<u16> {
    (area.width >= SEP_MIN_W).then(|| tabbar_booth_span(area).1 + 1)
}

/// Extent of the BOOTH chip. Always drawn, at every width: it is the only way
/// back to BOOTH with a pointer, since the spaces menu deliberately does not
/// carry it.
pub fn tabbar_booth_span(area: &LRect) -> (u16, u16) {
    let x = area.x + 1;
    // `[ booth ]` / `  booth  ` — same shape as a workspace chip, so the row does
    // not move when it becomes the active one.
    (x, x + TAB_BOOTH_LABEL.len() as u16 + 4)
}

fn booth_chip(active: bool, attention: bool) -> String {
    let mark = if attention { "!" } else { " " };
    if active {
        format!("[ {TAB_BOOTH_LABEL}{mark}]")
    } else {
        format!("  {TAB_BOOTH_LABEL}{mark} ")
    }
}

/// The machines control: what it says depends on how many there are.
///
/// **One control where there were two.** `[+ host]` and an `N hosts` count sat
/// beside each other and both opened the MACHINES picker — the same box, whose
/// rows are the machines already here and what dropping one takes. Two widgets
/// for one action, and the count had to be dropped on a rule of its own because
/// it hung off the left of whatever furniture was there.
///
/// So the label follows the state instead. At one machine it is an offer, and
/// `1 host` would be a label for a fact that needs no label; past one it is the
/// roll call, which is the thing worth pressing — you go there to let a machine
/// go as much as to add one.
pub fn machines_label(daemons: usize) -> String {
    if daemons > 1 {
        format!("[{daemons} hosts]")
    } else {
        TAB_HOST_LABEL.to_string()
    }
}

/// Extent of `[+ new]`, hard against the right edge.
pub fn tabbar_new_span(area: &LRect) -> (u16, u16) {
    let bound = area.x + area.width;
    let x = bound.saturating_sub(TAB_NEW_LABEL.len() as u16 + 1);
    (x, x + TAB_NEW_LABEL.len() as u16)
}

/// Extent of the machines control, one column left of `[+ new]`.
pub fn machines_span(area: &LRect, daemons: usize) -> (u16, u16) {
    let w = machines_label(daemons).chars().count() as u16;
    let x = tabbar_new_span(area).0.saturating_sub(w + 1);
    (x, x + w)
}

/// The tab bar.
fn draw_tabbar(
    buf: &mut Buffer,
    geom: &Geom,
    tabs: &[Tab<'_>],
    daemons: usize,
    view: &View,
    theme: &Theme,
) {
    let area = &geom.tabbar;
    let bound = area.x + area.width;
    for x in area.x..bound {
        if let Some(cell) = buf.cell_mut((x, area.y)) {
            cell.set_symbol(" ");
            cell.set_bg(theme.ground);
        }
    }
    // BOOTH first: it is the leftmost thing on the bar and the only pointer
    // route back to it, since the spaces menu does not carry it.
    {
        let active = view.page == Page::Booth;
        // Machines we are hearing from only, for the same reason the chips
        // below do it: BOOTH's marker is a promise that going there shows you
        // something that needs you now.
        let attention =
            tabs.iter().any(|t| t.live && (t.summary.waiting > 0 || t.summary.questions > 0));
        let (fg, bg) =
            if active { (theme.ground, theme.accent) } else { (theme.muted, theme.ground) };
        let fg = if attention && !active { theme.danger } else { fg };
        let (hx, _) = tabbar_booth_span(area);
        put_str(
            buf,
            hx,
            area.y,
            &booth_chip(active, attention),
            bound,
            Pen { fg, bg, bold: active },
        );
    }
    // The rule that says BOOTH is not one of the chips beside it.
    if let Some(sx) = tabbar_sep_x(area) {
        put_str(buf, sx, area.y, TAB_SEP, bound, Pen::new(theme.rule, theme.ground));
    }
    let strip = tab_strip(area, tabs, view, daemons);
    for ((i, tab), span) in tabs.iter().enumerate().zip(&strip.spans) {
        let Some((x, end)) = *span else { continue };
        let s = tab.summary;
        // Only a machine we are hearing from gets to claim your attention. The
        // counts behind `!` are a snapshot taken when the link died, and a red
        // chip for a workspace on a shut laptop is a summons to somewhere you
        // cannot go.
        let attention = tab.live && (s.waiting > 0 || s.questions > 0);
        let active = active_chip(view) == Some(i);
        let (fg, bg) =
            if active { (theme.ground, theme.accent) } else { (theme.muted, theme.ground) };
        let fg = if attention && !active {
            theme.danger
        } else if !tab.live && !active {
            theme.faint
        } else {
            fg
        };
        // Clipped at the span's own end, not the bar's: the last chip on a full
        // strip stops where the arrows begin.
        put_str(buf, x, area.y, &tab_label(i, tab, active), end, Pen { fg, bg, bold: active });
    }
    // The arrows, and only the one that has somewhere to go. Their columns stay
    // reserved either way, so the strip does not resize under the pointer as it
    // scrolls — an arrow you can see is an arrow that moves you.
    for (label, ((x, _), _)) in [(TAB_PREV_LABEL, strip.prev), (TAB_NEXT_LABEL, strip.next)]
        .into_iter()
        .filter_map(|(l, s)| s.map(|s| (l, s)))
    {
        put_str(buf, x, area.y, label, bound, Pen::new(theme.muted, theme.ground));
    }
    // The spaces, as one control. Drawn at its span, which is the ink rather
    // than the columns reserved for it — a shorter space name leaves the blank
    // outside the brackets instead of inside them.
    if let Some((x, _)) = spaces_button_span(area, view, daemons) {
        let bold = view.page.is_space();
        put_str(
            buf,
            x,
            area.y,
            &spaces_label(view),
            bound,
            Pen { fg: theme.ink, bg: theme.ground, bold },
        );
    }
    for (label, x, _) in tabbar_buttons(area, daemons) {
        put_str(buf, x, area.y, &label, bound, Pen::new(theme.faint, theme.ground));
    }
}

pub const TAB_NEW_LABEL: &str = "[+ new]";
pub const TAB_HOST_LABEL: &str = "[+ host]";

/// What a page wants to say about itself while you are somewhere else.
///
/// Deliberately only the counts a glance can act on: how many agents are
/// blocked, how many files changed, how many services are down. A number that
/// only means "there is stuff here" would be noise on a row this short.
///
/// [`spaces_menu_rows`] is the only caller. The tab bar used to wear the most
/// urgent of these on its spaces button, and before that a rail down the left
/// edge drew one per row; a badge you have to open a menu to see is the trade
/// that made, and what it bought was a control with no hole in it.
fn page_badge(
    page: Page,
    ws: Option<&WorkspaceDetail>,
    usage: Option<&usage::Usage>,
) -> Option<(String, Role)> {
    // The one badge that is not about this workspace, and the only one whose
    // source is a page rather than the workspace detail — an account limit is
    // the same on every tab.
    if page == Page::Usage {
        return usage.and_then(|u| usage::badge(&u.dto));
    }
    let ws = ws?;
    match page {
        Page::Agents => {
            let n = ws
                .agents
                .iter()
                .filter(|a| a.state == butai_protocol::api::AgentState::Waiting)
                .count();
            (n > 0).then(|| (format!("{n}!"), Role::Danger))
        }
        // The one signal on this rail that comes from outside the machine.
        // A conflict is blocking and says so in danger; being behind is not
        // blocking, but "someone pushed and you are working on stale main" has
        // nowhere else to live — every other page's badge is about work you
        // started yourself, and you cannot notice this one by looking.
        Page::Git => {
            let c = ws.changes.as_ref()?;
            if !c.conflicted.is_empty() {
                return Some((format!("{}!", c.conflicted.len()), Role::Danger));
            }
            (c.behind > 0).then(|| (format!("↓{}", c.behind), Role::Attention))
        }
        Page::Files => None,
        Page::Docker => None,
        Page::Docs => None,
        // SETTINGS and HELP badge nothing. Every badge on this rail is a count
        // of work waiting in *this project*; neither the client's configuration
        // nor its reference has such a number, and "there is stuff here" is
        // noise on a strip this wide. USAGE is handled above: its badge comes
        // from the roster rather than from this workspace, because an account
        // limit is the same on every tab. It is in the match only because the
        // table is total.
        Page::Diff | Page::Booth | Page::Settings | Page::Help | Page::Usage => None,
    }
}

/// AGENTS / PROCESSES / SYSTEM. Returns whether anything is marquee-scrolling.
fn draw_left_rail(
    buf: &mut Buffer,
    geom: &Geom,
    ws: Option<&WorkspaceDetail>,
    sys: &SysDto,
    view: &View,
    theme: &Theme,
) -> bool {
    let mut scrolling = false;
    let focused = view.focus == Focus::Agents;
    draw_box(buf, geom.left_box, " AGENTS ", theme.border(focused), theme.ground);
    // `[+ agent]` on the box's own border, right-aligned and clear of the title.
    // The label names the pinned agent when there is one, because a pinned
    // button spawns on a single click and the label is the only place the user
    // can see what that click is about to do — so it is measured, not assumed:
    // a pinned name is a different width and the hit test right-aligns the same
    // string, so drawing the generic span here would offset the two.
    let label = agents_add_label(view.pinned_agent.as_deref(), geom.left_box);
    let (ax, _) = agents_add_span_for(geom, &label);
    put_str(
        buf,
        ax,
        geom.left_box.y,
        &label,
        geom.left_box.right(),
        Pen::new(theme.faint, theme.ground),
    );

    let agents: &[AgentDto] = ws.map(|w| w.agents.as_slice()).unwrap_or(&[]);
    let stage = staged_pane(ws, view);
    let rows = geom.agents_rows;
    // Enough agents to outgrow the section is the normal state of a busy
    // workspace, not an edge case: without the skip the list stopped at the
    // section's height and every agent past it was unreachable — the cursor
    // walked onto rows that were never drawn, so `j` looked like it had stopped
    // working.
    let first = rail_first(view.agent_sel, agents.len(), rows.height);
    for (i, a) in agents.iter().skip(first).take(rows.height as usize).enumerate() {
        let y = rows.y + i as u16;
        let cursor = first + i == view.agent_sel && view.focus == Focus::Agents;
        let (status, status_role, name_role, _) = agent_status(
            a.state,
            a.exited,
            a.working_since_ms.map(secs_since),
            view.tick,
            a.unread,
        );
        // The agent's own spinner is pinned, not scrolled: it is the same kind
        // of token as a file's `M`, and a `◐` towed through the row by the
        // marquee is a moving target where a fixed one said the same thing.
        let (glyph, title) = split_status_glyph(&a.title);
        scrolling |= draw_row(
            buf,
            rows,
            y,
            cursor,
            stage == Some(a.pane),
            glyph,
            title,
            theme.role(name_role),
            &status,
            theme.role(status_role),
            view.tick,
            theme,
        );
    }
    if let Some(hint) = geom.agents_hint {
        let pinned = view.pinned_agent.is_some();
        draw_verb_footer(buf, hint, crate::verbs::agents_verbs(pinned), theme);
    }

    draw_section_sep(
        buf,
        geom.left_box,
        geom.procs_sep,
        " PROCESSES ",
        theme.border(view.focus == Focus::Processes),
        theme.ground,
    );
    // `[+ term]` on the separator itself, opposite the section's name.
    let (px, _) = procs_add_span(geom);
    put_str(
        buf,
        px,
        geom.procs_sep,
        PROCS_ADD_LABEL,
        geom.left_box.right(),
        Pen::new(theme.faint, theme.ground),
    );
    let procs: &[ProcessDto] = ws.map(|w| w.processes.as_slice()).unwrap_or(&[]);
    let rows = geom.procs_rows;
    let first = rail_first(view.proc_sel, procs.len(), rows.height);
    for (i, p) in procs.iter().skip(first).take(rows.height as usize).enumerate() {
        let y = rows.y + i as u16;
        let cursor = first + i == view.proc_sel && view.focus == Focus::Processes;
        let (status, color) = proc_status(p, theme);
        scrolling |= draw_row(
            buf,
            rows,
            y,
            cursor,
            stage == Some(p.pane),
            "",
            &p.name,
            theme.ink,
            &status,
            color,
            view.tick,
            theme,
        );
    }
    if let Some(hint) = geom.procs_hint {
        draw_verb_footer(buf, hint, crate::verbs::procs_verbs(), theme);
    }

    if geom.system_rows.height > 0 {
        draw_section_sep(buf, geom.left_box, geom.system_sep, " SYSTEM ", theme.rule, theme.ground);
        draw_system(buf, geom.system_rows, sys, &view.gauges, theme);
    }
    scrolling
}

/// One entry in the SYSTEM section, in the order it is drawn.
///
/// Carrying what a gauge *is* rather than which row it landed on is what lets
/// the section grow: the rows were `0` and `1` are CPU and RAM, `2` and up are
/// GPUs" until network arrived, at which point that arithmetic silently opened
/// the wrong monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gauge {
    Cpu,
    Ram,
    Gpu(usize),
    /// One network interface, by its index into [`SysDto::net`] — the daemon's
    /// own list, not the filtered one.
    ///
    /// Indexing the daemon's list rather than the drawn subset is what keeps the
    /// policy in one place: [`system_gauges`] decides which interfaces appear,
    /// and everything downstream resolves an index without needing to know how
    /// that decision was made or repeating it.
    Net(usize),
    /// One mounted filesystem, by its index into [`SysDto::disks`] — the
    /// daemon's own list, not the filtered one, and for the same reason
    /// [`Gauge::Net`] carries one: [`disk_mounts`] decides which mounts appear
    /// and everything downstream resolves an index without repeating it.
    Disk(usize),
}

/// Throughput below which the network trace stops autoscaling, so a quiet
/// minute is not amplified into a mountain range.
const NET_FLOOR_BPS: f32 = 64.0 * 1024.0;

/// Throughput below which a link is drawn as silent rather than as traffic.
///
/// A machine that is doing nothing is not sending nothing: ssh keepalives, mDNS,
/// ARP and a VPN's probes keep a few hundred bytes a second moving on every
/// interface that is up. Autoscaling turns that into a full-height trace the
/// moment it is the largest thing in the window, which is why an idle rail drew
/// a line — the fix is a floor in absolute units, below which the row is blank.
///
/// Four kilobytes a second is chosen to sit above that background chatter and
/// below anything a person would call a transfer.
const NET_IDLE_BPS: f32 = 4.0 * 1024.0;

/// Interfaces `NetMode::All` will draw before it stops.
///
/// A laptop has one and this never bites; a docker host here has three real
/// links behind twenty veths and eight bridges, and a Kubernetes node is worse.
/// The rail is shared with two lists, so the cap is what keeps a machine with an
/// unusual number of interfaces from turning SYSTEM into the whole rail. Naming
/// them explicitly overrides it — that is a request, not a discovery.
pub const NET_GAUGE_MAX: usize = 3;

/// Which interfaces are worth drawing, as indices into `sys.net`.
///
/// This is policy, and it lives here rather than in the daemon on purpose: the
/// daemon publishes every interface with what it *is*, and each client decides.
/// Loopback and the container bridges are skipped because their bytes are
/// counted again on whatever they egress from — on a box where the agents talk
/// to a local daemon, `lo` alone would dwarf the real link.
pub fn net_ifaces(sys: &SysDto, select: &NetSelect) -> Vec<usize> {
    let usable = |n: &NetDto| {
        n.carrier && !matches!(n.kind, NetKind::Loopback | NetKind::Bridge | NetKind::Veth)
    };
    let busiest_first = |a: &usize, b: &usize| {
        let (x, y) = (&sys.net[*a], &sys.net[*b]);
        // Default route first whatever the traffic: it is the one a person
        // means by "the network", and a busy VPN must not push it down the rail.
        y.default_route
            .cmp(&x.default_route)
            .then_with(|| (y.rx_bps + y.tx_bps).total_cmp(&(x.rx_bps + x.tx_bps)))
    };
    match select {
        // Honoured literally, in the order asked for, uncapped and unfiltered:
        // naming `docker0` is a decision, not a mistake to correct.
        NetSelect::Named(names) => {
            names.iter().filter_map(|want| sys.net.iter().position(|n| &n.name == want)).collect()
        }
        NetSelect::Mode(NetMode::All) => {
            // "Every link" means every link that is *doing* something, plus the
            // way out whether it is busy or not. A tunnel that has been silent
            // for the whole window is three rows saying nothing — the same
            // complaint the dead band answers one row further down, and a real
            // capture of this machine spent six rows on two idle ones.
            //
            // Judged over the retained history rather than the last sample, so a
            // row does not blink out during a lull and back on the next packet.
            // It takes the whole window of silence — about two and a half
            // minutes — for a link to go.
            let alive = |n: &NetDto| {
                n.default_route
                    || n.rx_bps.max(n.tx_bps) >= NET_IDLE_BPS
                    || n.rx_hist.iter().chain(n.tx_hist.iter()).any(|&v| v >= NET_IDLE_BPS)
            };
            let mut idx: Vec<usize> =
                (0..sys.net.len()).filter(|&i| usable(&sys.net[i]) && alive(&sys.net[i])).collect();
            idx.sort_by(busiest_first);
            idx.truncate(NET_GAUGE_MAX);
            idx
        }
        NetSelect::Mode(NetMode::Auto) => {
            let mut idx: Vec<usize> = (0..sys.net.len()).filter(|&i| usable(&sys.net[i])).collect();
            idx.sort_by(busiest_first);
            idx.truncate(1);
            idx
        }
    }
}

/// Mounts `DiskMode::All` will draw before it stops.
///
/// The daemon publishes up to 64 of them and a docker host reaches that, since
/// every image layer is a mount. Three is what an ordinary workstation has — a
/// root, a project disk and one more — and it is the same bargain
/// [`NET_GAUGE_MAX`] strikes: naming them explicitly overrides the cap, because
/// a list is a request rather than a discovery.
pub const DISK_GAUGE_MAX: usize = 3;

/// Which mounts are worth drawing, as indices into `sys.disks`.
///
/// Policy, and it lives here rather than in the daemon for the same reason
/// [`net_ifaces`] does: the daemon publishes every mount with what it *is*, and
/// each client decides. The default keeps [`DiskKind::Local`] only — a tmpfs is
/// RAM the RAM gauge already counts, an overlay is the image under a container
/// rather than a disk that can fill, and a network mount's capacity is a fact
/// about a machine that has a rail of its own.
pub fn disk_mounts(sys: &SysDto, select: &DiskSelect) -> Vec<usize> {
    let real = |d: &DiskDto| d.kind == DiskKind::Local;
    match select {
        // Honoured literally, in the order asked for, uncapped and unfiltered:
        // naming `/dev/shm` is a decision, not a mistake to correct.
        DiskSelect::Named(mounts) => mounts
            .iter()
            .filter_map(|want| sys.disks.iter().position(|d| &d.mount == want))
            .collect(),
        // The daemon's order is largest-first, and that is the order to cut
        // from: an installed snap is 100% full by construction, so a
        // fullest-first cut would spend the cap on squashfs before naming a
        // real disk. Taking the head of the list keeps the biggest three.
        DiskSelect::Mode(DiskMode::All) => {
            (0..sys.disks.len()).filter(|&i| real(&sys.disks[i])).take(DISK_GAUGE_MAX).collect()
        }
        // The root filesystem whatever its size: it is the one a person means
        // by "the disk", and the one whose filling stops the machine. Falling
        // back to the largest is for a container, where `/` is an overlay the
        // daemon does not call local and the alternative is drawing nothing.
        DiskSelect::Mode(DiskMode::Auto) => (0..sys.disks.len())
            .find(|&i| real(&sys.disks[i]) && sys.disks[i].mount == "/")
            .or_else(|| (0..sys.disks.len()).find(|&i| real(&sys.disks[i])))
            .into_iter()
            .collect(),
    }
}

/// What the SYSTEM section holds for this machine, in drawing order.
///
/// Disks last, below the series that move: they are the slowest thing on the
/// rail, so they are the thing the eye passes on the way somewhere else.
pub fn system_gauges(sys: &SysDto, net: &NetSelect, disks: &DiskSelect) -> Vec<Gauge> {
    let mut g = vec![Gauge::Cpu, Gauge::Ram];
    g.extend((0..sys.gpus.len()).map(Gauge::Gpu));
    g.extend(net_ifaces(sys, net).into_iter().map(Gauge::Net));
    g.extend(disk_mounts(sys, disks).into_iter().map(Gauge::Disk));
    g
}

/// Rows one gauge takes. Not a constant any more: the network gauge draws a
/// trace per direction, so it is a row taller than the rest.
pub const fn gauge_height(g: Gauge) -> u16 {
    match g {
        Gauge::Net(_) => NET_GAUGE_H,
        Gauge::Disk(_) => DISK_GAUGE_H,
        _ => GAUGE_H,
    }
}

/// Rows a set of gauges needs.
pub fn system_rows_used(gauges: &[Gauge]) -> u16 {
    gauges.iter().copied().map(gauge_height).sum()
}

/// The height the SYSTEM section asks the layout for, separator included.
///
/// Taken from the gauge list rather than its length, so a machine whose rail
/// holds a network gauge is measured with that gauge's real height. Both the
/// drawing and the hit testing go through this same list.
pub fn system_h_wanted(gauges: &[Gauge]) -> u16 {
    system_h_for_gauge_rows(gauges.iter().copied().map(gauge_height).sum())
}

/// What the CPU row says it is: the model, and the thread count when there is
/// still room for it.
///
/// The count is appended rather than always shown because `Ryzen 7 5700 16T` is
/// sixteen cells and the default rail has fourteen — so the wide-rail user gets
/// both and the default-rail user gets the name, rather than everyone getting a
/// name cut off mid-word. `gauge_head` does the cutting; this only decides what
/// is worth offering it.
fn cpu_ident(sys: &SysDto) -> String {
    let Some(model) = sys.cpu_model.as_deref().filter(|m| !m.is_empty()) else {
        return String::new();
    };
    match sys.cpu_threads {
        Some(t) => format!("{model} {t}T"),
        None => model.to_string(),
    }
}

/// A used/total pair of capacities in nine cells or fewer.
///
/// One unit for the pair, taken from the total, so the two numbers stay
/// comparable at a glance the way RAM's `19/32G` is. Terabytes above a
/// terabyte, because `3564/3667G` is ten cells of a twenty-six-cell rail spent
/// on four digits nobody reads and `3.5/3.6T` says the same thing in eight.
///
/// **Binary, not decimal.** `SysDto`'s `*_gb` are GiB — `vfs_gb` divides by
/// 1024³ — so a `T` here has to be 1024 of them or the rail prints 3.7 where
/// `df -h` prints 3.6, and `df` is exactly what someone reaches for to check.
fn cap_pair(used_gb: f32, total_gb: f32) -> String {
    const TIB: f32 = 1024.0;
    if total_gb >= TIB {
        format!("{:.1}/{:.1}T", used_gb / TIB, total_gb / TIB)
    } else {
        format!("{used_gb:.0}/{total_gb:.0}G")
    }
}

/// Bytes per second in four cells or fewer.
fn fmt_rate(bps: f32) -> String {
    match bps {
        b if b >= 1e9 => format!("{:.1}G", b / 1e9),
        b if b >= 10e6 => format!("{:.0}M", b / 1e6),
        b if b >= 1e6 => format!("{:.1}M", b / 1e6),
        b if b >= 1e3 => format!("{:.0}k", b / 1e3),
        b => format!("{b:.0}B"),
    }
}

/// A gauge's first row: what it is on the left, what it *is* in the middle,
/// where it stands on the right.
///
/// The middle slot is the hardware's name — `Ryzen 7 5700`, `RTX 4070`,
/// `enp1s0` — and it is the first thing dropped when the rail is too narrow to
/// hold all three. It has to be: the label says which gauge this is and the
/// value is the reading, so an identity that pushed either off the row would
/// have cost more than it told you. At the default 28-cell rail there are
/// fourteen cells for it beside CPU's `34% 61°`.
struct Head<'a> {
    /// Which gauge this is: `CPU`, `RAM`, `GPU0`, `DSK`.
    label: &'a str,
    /// What the hardware is. Empty when there is nothing to say, or nothing
    /// worth the cells.
    ident: &'a str,
    /// How to cut `ident` when it does not fit.
    fit: Fit,
    /// The reading.
    value: &'a str,
    color: Color,
}

/// How an identity is shortened when the rail is narrower than it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fit {
    /// Whole words, or nothing at all. See [`fit_words`].
    Words,
    /// Keep the end. See [`fit_path`].
    Path,
}

/// The longest whole-word prefix of `ident` that fits in `room` cells.
///
/// Whole words, because cutting mid-token is what turns `Ryzen 7 5700 16T` into
/// `Ryzen 7 5700 1` — a number that is not a fact about anything. Dropping the
/// thread count is a smaller loss than printing a wrong one, and a rail wide
/// enough gets both. Nothing fits at all means nothing is drawn, rather than a
/// fragment of the first word.
fn fit_words(ident: &str, room: u16) -> &str {
    let room = room as usize;
    if ident.chars().count() <= room {
        return ident;
    }
    let mut end = 0;
    for (i, _) in ident.match_indices(' ') {
        if ident[..i].chars().count() > room {
            break;
        }
        end = i;
    }
    &ident[..end]
}

/// The tail of `path` in `room` cells, with `…` standing for what was cut.
///
/// Cut from the *left*, which is the opposite of [`fit_words`] and for a reason:
/// a mount point's last segment is the half that identifies it. `/media/fast`
/// and `/media/archive` agree on everything before it, so a right-hand cut would
/// draw the same string for both — the exact failure the words rule avoids by
/// drawing nothing. Whole segments where they fit, so what is left is still a
/// path rather than a fragment of one.
fn fit_path(path: &str, room: u16) -> String {
    let (room, len) = (room as usize, path.chars().count());
    if len <= room {
        return path.to_string();
    }
    if room == 0 {
        return String::new();
    }
    // Whole segments from the right, as many as fit under the ellipsis.
    let mut best = String::new();
    for (i, _) in path.rmatch_indices('/') {
        let cand = format!("…{}", &path[i..]);
        if cand.chars().count() > room {
            break;
        }
        best = cand;
    }
    if !best.is_empty() {
        return best;
    }
    // Not even the last segment fits. Its end is still the part that differs
    // between two mounts under one parent, so that is what survives.
    let tail: String = path.chars().skip(len - (room - 1)).collect();
    format!("…{tail}")
}

fn gauge_head(buf: &mut Buffer, area: LRect, y: u16, h: Head<'_>, theme: &Theme) {
    let Head { label, ident, fit, value, color } = h;
    let bound = area.x + area.width;
    put_str(buf, area.x, y, label, bound, Pen::new(theme.muted, theme.ground));
    if !ident.is_empty() {
        // One space after the label, one before the value: without the second
        // the two run together on a rail that fits the name exactly.
        let x = area.x + label.chars().count() as u16 + 1;
        let room = bound.saturating_sub(x).saturating_sub(value.chars().count() as u16 + 1);
        let cut = match fit {
            Fit::Words => fit_words(ident, room).to_string(),
            Fit::Path => fit_path(ident, room),
        };
        put_str(buf, x, y, &cut, bound, Pen::new(theme.faint, theme.ground));
    }
    let vx = bound.saturating_sub(value.chars().count() as u16);
    put_str(buf, vx, y, value, bound, Pen::new(color, theme.ground));
}

/// A gauge's second row: its trace, across every cell the rail has.
///
/// Drawn on `surface` rather than the ground, which gives the trace a plot area
/// to sit in — without it the braille reads as loose dots rather than a figure,
/// and the empty rows above the line belong to nothing.
fn gauge_trace(buf: &mut Buffer, area: LRect, y: u16, glyphs: &str, color: Color, theme: &Theme) {
    let bound = area.x + area.width;
    fill_row(buf, area.x, y, bound, theme.surface);
    put_str(buf, area.x, y, glyphs, bound, Pen::new(color, theme.surface));
}

/// Draws the gauges, and returns how many rows it actually used.
///
/// The count is returned rather than recomputed because BOOTH stacks a block per
/// machine and has to know where the next one starts: asking the renderer is the
/// only version of that arithmetic which cannot disagree with what was drawn.
/// `gauges` is passed in rather than recomputed from `sys`, because which
/// interfaces appear is a configured choice and the drawing is not the place
/// that reads configuration. The rail hands over the same `view.gauges` the
/// layout was sized with and hit testing resolves against, so all three agree by
/// construction instead of by three copies of one filter.
fn draw_system(
    buf: &mut Buffer,
    area: LRect,
    sys: &SysDto,
    gauges: &[Gauge],
    theme: &Theme,
) -> u16 {
    let bottom = area.y + area.height;
    let cells = area.width as usize;
    let multi_gpu = sys.gpus.len() > 1;
    let multi_net = gauges.iter().filter(|g| matches!(g, Gauge::Net(_))).count() > 1;
    let mut y = area.y;

    for &gauge in gauges {
        // All of a gauge's rows or none: a label stranded above the fold
        // without its trace reads as a half-drawn screen rather than a short
        // one. This is also what makes the section clip cleanly when a
        // configured height is smaller than the machine needs.
        let gauge_h = gauge_height(gauge);
        if y + gauge_h > bottom {
            break;
        }
        match gauge {
            Gauge::Cpu => {
                let temp = sys.cpu_temp.map(|t| format!(" {t:.0}°")).unwrap_or_default();
                let color = theme.role(load_role(sys.cpu_pct));
                let head = Head {
                    label: "CPU",
                    ident: &cpu_ident(sys),
                    fit: Fit::Words,
                    value: &format!("{:.0}%{temp}", sys.cpu_pct),
                    color,
                };
                gauge_head(buf, area, y, head, theme);
                gauge_trace(buf, area, y + 1, &braille_trace(&sys.cpu_hist, cells), color, theme);
            }
            Gauge::Ram => {
                let pct = if sys.ram_total_gb > 0.0 {
                    sys.ram_used_gb / sys.ram_total_gb * 100.0
                } else {
                    0.0
                };
                let color = theme.role(load_role(pct));
                let value = format!("{:.0}/{:.0}G", sys.ram_used_gb, sys.ram_total_gb);
                // Swap only once some is in use. A machine that has swap and is
                // not touching it has nothing to say here, and "swap 0/8G" on
                // every rail forever would be noise rather than a reading.
                let ident = if sys.swap_used_gb >= 0.05 {
                    format!("swap {:.0}/{:.0}G", sys.swap_used_gb, sys.swap_total_gb)
                } else {
                    String::new()
                };
                gauge_head(
                    buf,
                    area,
                    y,
                    Head { label: "RAM", ident: &ident, fit: Fit::Words, value: &value, color },
                    theme,
                );
                gauge_trace(buf, area, y + 1, &braille_trace(&sys.ram_hist, cells), color, theme);
            }
            Gauge::Gpu(i) => {
                let Some(gpu) = sys.gpus.get(i) else { continue };
                let color = theme.role(load_role(gpu.pct));
                // Numbered only when there is more than one, so the ordinary
                // single-GPU machine is not made to look like a rack.
                let label = if multi_gpu { format!("GPU{i}") } else { "GPU".to_string() };
                let value =
                    format!("{:.0}% {:.0}/{:.0}G", gpu.pct, gpu.mem_used_gb, gpu.mem_total_gb);
                gauge_head(
                    buf,
                    area,
                    y,
                    Head { label: &label, ident: &gpu.name, fit: Fit::Words, value: &value, color },
                    theme,
                );
                gauge_trace(buf, area, y + 1, &braille_trace(&gpu.hist, cells), color, theme);
            }
            Gauge::Net(i) => {
                let Some(n) = sys.net.get(i) else { continue };
                // Named only when the rail holds more than one, so the ordinary
                // single-link machine is not made to look like a router.
                let label = if multi_net { n.name.as_str() } else { "" };
                draw_net_gauge(buf, area, y, n, label, cells, theme);
            }
            Gauge::Disk(i) => {
                let Some(d) = sys.disks.get(i) else { continue };
                let pct = if d.total_gb > 0.0 { d.used_gb / d.total_gb * 100.0 } else { 0.0 };
                // A stale mount keeps its last reading and is drawn faint
                // rather than in the colour that reading earned. A filesystem
                // nobody has heard from is not an alarm about how full it is —
                // it is the older news that the answer is out of date, and a
                // red row would say the first thing while meaning the second.
                let color = if d.stale { theme.faint } else { theme.role(load_role(pct)) };
                // The mount is the identity here, and unlike every other
                // gauge's it is never decoration: two disks with their mounts
                // dropped are two identical rows. That is why it is cut rather
                // than abandoned — see [`fit_path`].
                let head = Head {
                    label: "DSK",
                    ident: &d.mount,
                    fit: Fit::Path,
                    value: &cap_pair(d.used_gb, d.total_gb),
                    color,
                };
                gauge_head(buf, area, y, head, theme);
            }
        }
        y += gauge_h;
    }
    y - area.y
}

/// The network gauge. Different enough from the others to be worth its own
/// function: throughput has no denominator, so there is no percentage to take a
/// [`load_role`] from and nothing to divide by for the trace.
///
/// A trace per direction rather than one mirrored around a midline. The mirror
/// fitted in two rows but left two dot rows per direction, and two dot rows is
/// one bit: everything under a quarter of the shared scale collapsed onto the
/// baseline, so a real download running under a heavier upload was drawn as the
/// same single dot an idle link drew. Splitting them costs the row that buys
/// four levels each — and lets each carry its own colour, which one glyph row
/// holding both directions could never do.
///
/// Colour means direction here rather than severity: a saturated link is the
/// machine working, never a warning. The leading arrow says the same thing
/// again, so the rows still read on a monochrome terminal and for a reader who
/// cannot separate the two hues.
fn draw_net_gauge(
    buf: &mut Buffer,
    area: LRect,
    y: u16,
    n: &NetDto,
    name: &str,
    cells: usize,
    theme: &Theme,
) {
    let bound = area.x + area.width;
    let (dn, up) = (format!("↓{}", fmt_rate(n.rx_bps)), format!("↑{}", fmt_rate(n.tx_bps)));
    put_str(buf, area.x, y, "NET", bound, Pen::new(theme.muted, theme.ground));
    // The identity slot, on the same terms as every other gauge's: the
    // interface name once there is more than one, and the negotiated link speed
    // after it when the rail is wide enough to carry both.
    if !name.is_empty() {
        let ident = match n.speed_mbps {
            Some(mbps) if mbps >= 1000 => format!("{name} {}G", mbps / 1000),
            Some(mbps) => format!("{name} {mbps}M"),
            None => name.to_string(),
        };
        let x = area.x + 4;
        let room = bound
            .saturating_sub(x)
            .saturating_sub((dn.chars().count() + up.chars().count() + 2) as u16);
        put_str(buf, x, y, fit_words(&ident, room), bound, Pen::new(theme.faint, theme.ground));
    }
    let w = (dn.chars().count() + 1 + up.chars().count()) as u16;
    let vx = bound.saturating_sub(w);
    put_str(buf, vx, y, &dn, bound, Pen::new(theme.info, theme.ground));
    put_str(
        buf,
        vx + dn.chars().count() as u16 + 1,
        y,
        &up,
        bound,
        Pen::new(theme.accent, theme.ground),
    );

    // Autoscaled to the window peak over the samples actually on screen, and
    // shared between the two directions so the pair compares like with like: a
    // flat ↓ under a full ↑ is the picture of an upload, which is a fact worth
    // keeping. Scaling each against its own peak would make both rows full and
    // throw that away.
    //
    // Below NET_IDLE_BPS a sample is passed through as an exact zero, which is
    // what `braille_traffic` draws as nothing. That floor is in bytes rather
    // than in scale units on purpose — as a fraction of the peak it would move
    // with the traffic and silence would never quite reach it.
    let shown = cells * 2;
    let peak = n
        .rx_hist
        .iter()
        .rev()
        .take(shown)
        .chain(n.tx_hist.iter().rev().take(shown))
        .fold(NET_FLOOR_BPS, |a, &b| a.max(b));
    let scale = |v: &f32| {
        if *v < NET_IDLE_BPS {
            0.0
        } else {
            (v / peak * 100.0).clamp(0.0, 100.0)
        }
    };
    let (rx, tx): (Vec<f32>, Vec<f32>) =
        (n.rx_hist.iter().map(scale).collect(), n.tx_hist.iter().map(scale).collect());
    // The arrow takes the first cell of each trace row; the rest is the plot.
    let plot = LRect::new(area.x + 1, area.y, area.width.saturating_sub(1), area.height);
    let plot_cells = plot.width as usize;
    for (row, (series, color)) in [(&rx, theme.info), (&tx, theme.accent)].iter().enumerate() {
        let ty = y + 1 + row as u16;
        let arrow = if row == 0 { "↓" } else { "↑" };
        gauge_trace(buf, area, ty, "", *color, theme);
        put_str(buf, area.x, ty, arrow, bound, Pen::new(*color, theme.surface));
        put_str(
            buf,
            plot.x,
            ty,
            &braille_traffic(series, plot_cells),
            bound,
            Pen::new(*color, theme.surface),
        );
    }
}

fn draw_stage_box(
    buf: &mut Buffer,
    geom: &Geom,
    ws: Option<&WorkspaceDetail>,
    view: &View,
    theme: &Theme,
) {
    let title = stage_title(ws, view);
    draw_box(buf, geom.stage_box, &title, theme.border(view.focus == Focus::Stage), theme.ground);
}

/// Width of the tree column on the Files page.
///
/// A share of the stage rather than a constant, so a wide terminal gives the
/// file being read the room, and a narrow one still lists names.
fn tree_width(stage_w: u16) -> u16 {
    (stage_w / 3).clamp(16, 40).min(stage_w.saturating_sub(20))
}

/// The Files page: a lazy directory listing beside the file it opens.
/// The Files page's tree column: where its rows are drawn, and where a click
/// on one lands. Both go through this so the two cannot disagree.
pub fn files_row_area(geom: &Geom) -> LRect {
    let outer = geom.stage_box;
    let w = tree_width(outer.width);
    LRect::new(outer.x + 1, outer.y + 1, w.saturating_sub(2), outer.height.saturating_sub(2))
}

/// The `[find]` button on the tree box's top border.
pub const FILES_FIND_LABEL: &str = "[find]";

/// Where `[find]` sits, right-aligned on the tree box's border. Draw and hit
/// test both come through here, so the button cannot be painted in one place
/// and clicked in another.
pub fn files_find_span(tree_box: &LRect) -> (u16, u16) {
    let w = FILES_FIND_LABEL.len() as u16;
    let end = tree_box.right().saturating_sub(1);
    (end.saturating_sub(w), end)
}

/// The tree box on the Files and Docs pages — the outer rectangle whose border
/// carries `[find]`, as against [`files_row_area`]'s rows inside it.
pub fn files_tree_box(geom: &Geom) -> LRect {
    let outer = geom.stage_box;
    LRect::new(outer.x, outer.y, tree_width(outer.width), outer.height)
}

/// The interior of the box the open file is drawn in — the other column of the
/// Files and Docs pages, on the same terms as [`docker_logs_inner`].
///
/// The hint row at the bottom is left in: it is a row of the same box, and a
/// selection that stopped one row short of what is drawn is a clip you can see.
pub fn files_body_inner(geom: &Geom) -> LRect {
    let outer = geom.stage_box;
    let tree_w = tree_width(outer.width);
    LRect::new(
        outer.x + tree_w + 1,
        outer.y + 1,
        outer.width.saturating_sub(tree_w + 2),
        outer.height.saturating_sub(2),
    )
}

/// The Docker page's list column, on the same terms.
pub fn docker_row_area(geom: &Geom) -> LRect {
    let outer = geom.stage_box;
    let w = tree_width(outer.width);
    LRect::new(outer.x + 1, outer.y + 1, w.saturating_sub(2), outer.height.saturating_sub(2))
}

/// The first row of a list that is on screen, given where the cursor is.
///
/// Scrolls only far enough to keep the cursor visible, which is what both pages
/// do — a directory outgrows its column long before the terminal runs out of
/// rows, and a list that recentres on every keypress is hard to read.
///
/// That "only far enough" is the deliberate difference from the daemon's old
/// `Workspace::proc_scroll`, which centred the selection. Both kept the cursor
/// on screen; this one also keeps the rows around it still.
pub fn first_visible(sel: usize, height: u16) -> usize {
    sel.saturating_sub((height as usize).saturating_sub(1))
}

/// The same, for the three rail lists, with the cursor clamped into the list
/// first.
///
/// The rails change under their own cursor in a way the pages do not: an agent
/// exits, a process is killed, a file is staged away — and nothing walks the
/// cursor back, so it can name a row that is no longer there. [`first_visible`]
/// alone would then scroll the list off its own bottom and leave blank rows
/// under the last one. Clamping costs the length, which is why this is separate
/// rather than folded into `first_visible`: the pages pass a cursor that is
/// always in range.
///
/// Both the drawing and [`crate::hit::at`] go through it. A rail that scrolled
/// by one rule and hit-tested by another would select a different row than the
/// one under the pointer, which is the bug the GIT page's own note warns about.
pub fn rail_first(sel: usize, len: usize, height: u16) -> usize {
    first_visible(sel.min(len.saturating_sub(1)), height)
}

fn draw_files_page(
    buf: &mut Buffer,
    geom: &Geom,
    page: Page,
    files: Option<&Files>,
    view: &View,
    theme: &Theme,
) {
    let outer = geom.stage_box;
    let tree_w = tree_width(outer.width);
    let tree_box = files_tree_box(geom);
    let view_box =
        LRect::new(outer.x + tree_w, outer.y, outer.width.saturating_sub(tree_w), outer.height);

    let empty = if page == Page::Docs { " DOCS " } else { " FILES " };
    let Some(files) = files else {
        draw_box(buf, outer, empty, theme.rule, theme.ground);
        return;
    };
    let dir = match files.dir.as_str() {
        "" => "/",
        d => d,
    };
    let dir = if page == Page::Docs { format!(" docs · {dir} ") } else { format!(" {dir} ") };
    draw_box(buf, tree_box, &dir, theme.border(true), theme.ground);
    // `[find]` on the tree box's own border, where the daemon drew it: the
    // search is about the files in front of you, so its button belongs on them
    // rather than on a footer at the other end of the screen.
    let (fx, _) = files_find_span(&tree_box);
    put_str(
        buf,
        fx,
        tree_box.y,
        FILES_FIND_LABEL,
        tree_box.right(),
        Pen::new(theme.faint, theme.ground),
    );

    let rows = files_row_area(geom);
    let bound = rows.x + rows.width;
    let visible = rows.height as usize;
    let first = first_visible(files.sel, rows.height);
    for (i, e) in files.entries.iter().skip(first).take(visible).enumerate() {
        let y = rows.y + i as u16;
        let cursor = first + i == files.sel;
        let bg = theme.row_bg(cursor);
        for x in rows.x..bound {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_bg(bg);
            }
        }
        // `/` for a directory and `●` for a change, both single-cell: the rail
        // rules apply here too.
        let marker = if e.changed { "●" } else { " " };
        let name = if e.is_dir { format!("{}/", e.name) } else { e.name.clone() };
        let fg = if e.is_dir { theme.accent } else { theme.ink };
        let mark_fg = theme.attention;
        put_str(buf, rows.x, y, marker, bound, Pen::new(mark_fg, bg));
        let text = ellipsize(&name, rows.width.saturating_sub(2) as usize);
        put_str(buf, rows.x + 2, y, &text, bound, Pen::new(fg, bg));
    }

    let title = match &files.open {
        // The asterisk is the whole unsaved-changes signal, so it goes where
        // the eye already is rather than into a status line further away.
        Some(f) if f.dirty => format!(" {} * ", f.path),
        Some(f) => format!(" {} ", f.path),
        None => " (no file) ".to_string(),
    };
    let editing = files.open.as_ref().is_some_and(|f| f.mode == EditMode::Edit);
    let focused = editing || view.focus == Focus::Stage;
    draw_box(buf, view_box, &title, theme.border(focused), theme.ground);
    let Some(open) = &files.open else { return };
    let inner = LRect::new(
        view_box.x + 1,
        view_box.y + 1,
        view_box.width.saturating_sub(2),
        view_box.height.saturating_sub(2),
    );
    // One row at the bottom for the notice and the keys, as the diff page does.
    let body = LRect::new(inner.x, inner.y, inner.width, inner.height.saturating_sub(1));
    draw_editor_body(buf, body, open, theme);

    if inner.height > 0 {
        let bound = inner.x + inner.width;
        let y = inner.y + inner.height - 1;
        // On this page `j`/`k` walk the tree until the cursor is moved onto the
        // file, so the hint says which of the two it is about to do.
        let scroll = if view.focus == Focus::Stage { "j/k scroll" } else { "tab to the file" };
        let hints = match (open.mode, open.editable()) {
            (_, false) => format!("read-only   {scroll}   q close"),
            (EditMode::View, _) => format!("e edit   {scroll}   q close"),
            (EditMode::Edit, _) => "C-s save   esc stop editing".to_string(),
        };
        let (text, fg) = match (&open.notice, open.truncated) {
            (Some(n), _) => (n.clone(), theme.attention),
            (None, true) => ("… truncated; download to see the rest".to_string(), theme.attention),
            (None, false) => (hints, theme.faint),
        };
        for x in inner.x..bound {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_bg(theme.ground);
            }
        }
        put_str(
            buf,
            inner.x,
            y,
            &ellipsize(&text, inner.width as usize),
            bound,
            Pen::new(fg, theme.ground),
        );
    }
}

/// How wide the line numbers are, before the space after them.
///
/// Wide enough for the last line's number, so the gutter does not shift under
/// the reader as they scroll into four digits.
fn editor_gutter_digits(open: &Editor) -> usize {
    open.lines().len().max(1).to_string().len().max(3)
}

/// Width of the line-number gutter down the left of a file being read, in cells.
///
/// Public because a copy has to skip it. The gutter is chrome drawn *inside* the
/// text column, so a selection clipped to the column took the line numbers with
/// it and pasted code that no longer compiles — reported exactly that way. Zero
/// while editing: the widget draws its own body with no gutter at all.
///
/// The same function the drawing uses, so the number cannot be right in one
/// place and stale in the other.
pub fn editor_gutter_w(open: &Editor) -> u16 {
    if open.mode == EditMode::Edit {
        return 0;
    }
    editor_gutter_digits(open) as u16 + 1
}

/// The diff's marker column: `#` on a picked line, `>` on the cursor, `|` on the
/// cursor's hunk. One cell, and skipped by a copy for the same reason the
/// editor's line numbers are — it is chrome, not diff.
pub const DIFF_GUTTER_W: u16 = 1;

/// Cells of code the line-number columns must leave behind, or they are not
/// drawn at all.
///
/// The same bargain [`GIT_GRAPH_MIN_W`] strikes for the commit graph: numbers
/// are an orientation aid *over* the text, so a gutter that leaves twenty cells
/// of code has taken more than it gave. The GIT page's body is the narrow case
/// — it is what is left after the refs column — and it is the one that made the
/// floor necessary.
pub const DIFF_TEXT_MIN_W: u16 = 32;

/// The editor's text: highlighted rows while reading, the live widget while
/// editing.
///
/// Two renderers because they answer different questions. Reading wants colour
/// and a line-number gutter; editing wants a cursor that is *actually* the
/// buffer's cursor, with the widget's own scrolling — and reimplementing that
/// on top of a highlighted view is how a cursor ends up one row off its text.
fn draw_editor_body(buf: &mut Buffer, area: LRect, open: &Editor, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if open.mode == EditMode::Edit {
        let rect = to_rect(area);
        for y in rect.y..rect.bottom() {
            for x in rect.x..rect.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_symbol(" ");
                    cell.set_bg(theme.ground);
                }
            }
        }
        open.area.render(rect, buf);
        return;
    }

    let bound = area.x + area.width;
    let digits = editor_gutter_digits(open);
    let gutter_w = editor_gutter_w(open);
    for (i, runs) in
        open.highlighted.iter().skip(open.scroll).take(area.height as usize).enumerate()
    {
        let y = area.y + i as u16;
        for x in area.x..bound {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_bg(theme.ground);
            }
        }
        let n = open.scroll + i + 1;
        put_str(
            buf,
            area.x,
            y,
            &format!("{n:>width$} ", width = digits),
            bound,
            Pen::new(theme.faint, theme.ground),
        );
        let mut x = area.x + gutter_w;
        for (token, text) in runs {
            if x >= bound {
                break;
            }
            let fg = match token {
                Token::Plain => theme.ink,
                Token::Comment => theme.muted,
                Token::Str => theme.ok,
                Token::Number => theme.accent,
                Token::Keyword => theme.attention,
                Token::Type => theme.info,
            };
            // Tabs would advance the cursor by more than the cell they occupy,
            // which puts every run after them in the wrong column.
            let text = text.replace('\t', "    ");
            let text = ellipsize(&text, bound.saturating_sub(x) as usize);
            put_str(buf, x, y, &text, bound, Pen::new(fg, theme.ground));
            x += text.chars().count() as u16;
        }
    }
}

/// Fill a row with the cursor background and return the pen to write it in.
///
/// Every list on this page does the same three lines before writing anything;
/// doing it once means a row cannot be highlighted in one branch and left
/// transparent in another.
fn row_ground(buf: &mut Buffer, area: LRect, y: u16, cursor: bool, theme: &Theme) -> Color {
    let bg = theme.row_bg(cursor);
    for x in area.x..area.x + area.width {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(" ");
            cell.set_bg(bg);
        }
    }
    bg
}

/// `↑2 ↓1`, or nothing when a branch is level with its upstream.
fn drift(entry: &BranchDto) -> String {
    match (entry.ahead, entry.behind) {
        (0, 0) => String::new(),
        (a, 0) => format!("↑{a}"),
        (0, b) => format!("↓{b}"),
        (a, b) => format!("↑{a}↓{b}"),
    }
}

/// The GIT page: the repository's refs over its history, beside a diff.
///
/// Two cursors, as the Docker page has two: REFS chooses what the history is
/// *about*, HISTORY chooses what the body shows. Neither moves anything in the
/// repository — see [`Page::Git`].
fn draw_git_page(
    buf: &mut Buffer,
    geom: &Geom,
    ws: Option<&WorkspaceDetail>,
    git: Option<&Git>,
    view: &View,
    theme: &Theme,
) {
    let cols = git_columns(geom.stage_box);
    let changes = ws.and_then(|w| w.changes.as_ref());
    let here = ws.map(|w| w.id);
    let empty = Git::default();
    let git = git.unwrap_or(&empty);

    draw_git_refs(buf, &cols, git, changes, here, view, theme);
    draw_git_history(buf, &cols, git, view, theme);

    // The body is the diff widget the DIFF page uses, in another box. A commit
    // is history, so nothing in it can be staged and its hint row says so on
    // its own — `DiffKind::Commit` already carries that.
    match &git.body {
        Some(diff) => {
            draw_diff_in(buf, cols.body_box, Some(diff), view.focus == Focus::Stage, theme)
        }
        None => {
            draw_box(buf, cols.body_box, " COMMIT ", theme.rule, theme.ground);
            if cols.body_box.width > 4 && cols.body_box.height > 2 {
                let msg = if git.loaded {
                    "Enter on a commit to read it"
                } else if ws.is_none() {
                    "no workspace open"
                } else {
                    "loading…"
                };
                put_str(
                    buf,
                    cols.body_box.x + 2,
                    cols.body_box.y + 1,
                    msg,
                    cols.body_box.x + cols.body_box.width,
                    Pen::new(theme.faint, theme.ground),
                );
            }
        }
    }
}

/// The REFS list: the working tree, then everything that names a commit.
fn draw_git_refs(
    buf: &mut Buffer,
    cols: &GitColumns,
    git: &Git,
    changes: Option<&ChangesDto>,
    here: Option<SessionId>,
    view: &View,
    theme: &Theme,
) {
    if cols.refs_box.height == 0 {
        return;
    }
    let focused = view.focus == Focus::Refs;
    draw_box(buf, cols.refs_box, " REFS ", theme.border(focused), theme.ground);
    let area = cols.refs_rows;
    let bound = area.x + area.width;
    let rows = ref_rows(git, changes, here);
    // The footer is drawn before the empty check, not after it: the hit test
    // measures these rows unconditionally, so a box that skipped drawing them
    // left invisible buttons over its blank space.
    let verbs = crate::verbs::git_footer(ref_row_kind(&rows, git.refs_sel));
    let (list_h, footer_h) = git_split(area, &verbs);
    if footer_h > 0 {
        let footer = LRect::new(area.x, area.y + list_h, area.width, footer_h);
        draw_verb_footer(buf, footer, &verbs, theme);
    }
    let area = LRect::new(area.x, area.y, area.width, list_h);
    if rows.is_empty() {
        let msg = if git.loaded { "not a git repository" } else { "loading…" };
        put_str(buf, area.x, area.y, msg, bound, Pen::new(theme.faint, theme.ground));
        return;
    }

    let visible = area.height as usize;
    // Derived from the cursor rather than stored, which is what keeps the
    // selection on screen: `j` past the last visible row scrolls the list
    // instead of losing the highlight off the bottom of the box.
    let first = first_visible(git.refs_sel.min(rows.len().saturating_sub(1)), area.height);
    for (i, row) in rows.iter().skip(first).take(visible).enumerate() {
        let y = area.y + i as u16;
        let cursor = first + i == git.refs_sel && focused;
        let bg = row_ground(buf, area, y, cursor, theme);
        match row {
            RefRow::Header(name) => {
                put_str(buf, area.x, y, name, bound, Pen::new(theme.muted, bg));
            }
            RefRow::WorkingTree { dirty } => {
                let (text, fg) = match dirty {
                    0 => ("working tree clean".to_string(), theme.faint),
                    n => (format!("working tree · {n} changed"), theme.attention),
                };
                put_str(
                    buf,
                    area.x,
                    y,
                    &ellipsize(&text, area.width as usize),
                    bound,
                    Pen { fg, bg, bold: *dirty > 0 },
                );
            }
            // The status code is pinned between the marker and the name and only
            // the path is allowed to be cut, for the reason the rail pins it:
            // a row narrow enough to lose its tail is exactly the row that still
            // has to say whether the file was modified, added or untracked.
            RefRow::Change(ChangeRow::Conflicted { path }) => {
                put_str(buf, area.x, y, "!", bound, Pen { fg: theme.danger, bg, bold: true });
                put_str(
                    buf,
                    area.x + 2,
                    y,
                    &ellipsize(path, area.width.saturating_sub(2) as usize),
                    bound,
                    Pen::new(theme.danger, bg),
                );
            }
            RefRow::Change(ChangeRow::File { change, staged }) => {
                let code = change.code.chars().next().unwrap_or(' ').to_string();
                let code_fg = if *staged {
                    theme.ok
                } else if code == "?" {
                    theme.danger
                } else {
                    theme.attention
                };
                let tail = format!("+{} -{}", change.added, change.deleted);
                let tail_w = (tail.width() as u16).min(area.width);
                put_str(buf, area.x + 1, y, &code, bound, Pen::new(code_fg, bg));
                let name_w = area.width.saturating_sub(tail_w + 4) as usize;
                put_str(
                    buf,
                    area.x + 3,
                    y,
                    &ellipsize(&change.path, name_w),
                    bound.saturating_sub(tail_w),
                    Pen::new(theme.ink, bg),
                );
                put_str(
                    buf,
                    bound.saturating_sub(tail_w),
                    y,
                    &tail,
                    bound,
                    Pen::new(theme.faint, bg),
                );
            }
            RefRow::Change(_) => {}
            RefRow::Branch { entry, current, elsewhere } => {
                // The marker is `>`, not a pointing glyph: those are
                // East-Asian-ambiguous and render two cells wide in some
                // terminals, shifting every cell after them on the row.
                let mark = if *current { ">" } else { " " };
                let right = match elsewhere {
                    Some(path) => {
                        let leaf = path.rsplit('/').next().unwrap_or(path);
                        format!("⇢{leaf}")
                    }
                    None => drift(entry),
                };
                // Truncated first: `⇢{leaf}` is an unbounded worktree name, and
                // subtracting a wider-than-the-column string from the right
                // edge underflowed — a panic in debug, a vanished marker in
                // release. `name_w` was already guarded; this half was not.
                let right = ellipsize(&right, area.width.saturating_sub(2) as usize);
                let right_w = (right.width() as u16).min(area.width);
                let name_w = area.width.saturating_sub(right_w + 2) as usize;
                let fg = if *current {
                    theme.ok
                } else if entry.remote {
                    theme.faint
                } else {
                    theme.ink
                };
                put_str(buf, area.x, y, mark, bound, Pen::new(theme.ok, bg));
                put_str(
                    buf,
                    area.x + 1,
                    y,
                    &ellipsize(&entry.name, name_w),
                    bound.saturating_sub(right_w),
                    Pen { fg, bg, bold: *current },
                );
                if !right.is_empty() {
                    let colour = if elsewhere.is_some() { theme.muted } else { theme.attention };
                    put_str(
                        buf,
                        bound.saturating_sub(right_w),
                        y,
                        &right,
                        bound,
                        Pen::new(colour, bg),
                    );
                }
            }
            RefRow::Remote { name, url } => {
                let text =
                    ellipsize(&format!("{name}  {url}"), area.width.saturating_sub(1) as usize);
                put_str(buf, area.x + 1, y, &text, bound, Pen::new(theme.faint, bg));
            }
            RefRow::Tag(name) => {
                put_str(
                    buf,
                    area.x + 1,
                    y,
                    &ellipsize(name, area.width.saturating_sub(1) as usize),
                    bound,
                    Pen::new(theme.accent, bg),
                );
            }
            RefRow::Stash(dto) => {
                let text = format!("{}  {}", dto.index, dto.message);
                put_str(
                    buf,
                    area.x + 1,
                    y,
                    &ellipsize(&text, area.width.saturating_sub(1) as usize),
                    bound,
                    Pen::new(theme.ink, bg),
                );
            }
            RefRow::Worktree { dto, here } => {
                let leaf = dto.path.rsplit('/').next().unwrap_or(&dto.path);
                let tail = if *here {
                    "here".to_string()
                } else if dto.workspace.is_some() {
                    "open".to_string()
                } else {
                    String::new()
                };
                let tail_w = (tail.width() as u16).min(area.width);
                let label = format!("{leaf}  {}", dto.branch.as_deref().unwrap_or("detached"));
                put_str(
                    buf,
                    area.x + 1,
                    y,
                    &ellipsize(&label, area.width.saturating_sub(tail_w + 2) as usize),
                    bound.saturating_sub(tail_w),
                    Pen::new(if *here { theme.faint } else { theme.ink }, bg),
                );
                if !tail.is_empty() {
                    put_str(
                        buf,
                        bound.saturating_sub(tail_w),
                        y,
                        &tail,
                        bound,
                        Pen::new(theme.muted, bg),
                    );
                }
            }
        }
    }
}

/// The HISTORY list: one page of the log, scoped by what REFS chose.
fn draw_git_history(buf: &mut Buffer, cols: &GitColumns, git: &Git, view: &View, theme: &Theme) {
    if cols.hist_box.height == 0 {
        return;
    }
    let focused = view.focus == Focus::History;
    let title = format!(" HISTORY · {} ", git.scope.label());
    draw_box(
        buf,
        cols.hist_box,
        &ellipsize(&title, cols.hist_box.width.saturating_sub(2) as usize),
        theme.border(focused),
        theme.ground,
    );
    let area = cols.hist_rows;
    let bound = area.x + area.width;
    // Before the empty check — see `draw_git_refs`. The kind must match what
    // the hit test computes for an empty log, or the two disagree about which
    // rows are buttons.
    let kind =
        if git.log.is_empty() { crate::verbs::GitRow::None } else { crate::verbs::GitRow::Commit };
    let verbs = crate::verbs::git_footer(kind);
    let (list_h, footer_h) = git_split(area, &verbs);
    if footer_h > 0 {
        let footer = LRect::new(area.x, area.y + list_h, area.width, footer_h);
        draw_verb_footer(buf, footer, &verbs, theme);
    }
    let area = LRect::new(area.x, area.y, area.width, list_h);
    if git.log.is_empty() {
        let msg = if git.loaded { "no commits" } else { "loading…" };
        put_str(buf, area.x, area.y, msg, bound, Pen::new(theme.faint, theme.ground));
        return;
    }

    // Lanes are computed over the *whole* page, not the visible slice: a lane
    // opened by a merge above the fold still has to be drawn passing through
    // the rows on screen, and a graph that restarted at the scroll offset would
    // invent a different shape at every scroll position.
    let lanes = if area.width >= GIT_GRAPH_MIN_W {
        crate::graph::graph_rows(
            git.log.iter().map(|c| (c.id.as_str(), c.parents.as_slice())),
            GIT_MAX_LANES,
        )
    } else {
        Vec::new()
    };

    let visible = area.height as usize;
    let first = first_visible(git.hist_sel.min(git.log.len().saturating_sub(1)), area.height);
    // One column for every lane the page uses, so the sha and summary line up
    // down the list instead of stepping in and out with the branching.
    let graph_w = lanes.iter().map(|r| r.width().min(GIT_MAX_LANES)).max().unwrap_or(0) as u16;
    let graph_w = if lanes.iter().any(|r| r.overflow) { graph_w + 1 } else { graph_w };

    for (i, entry) in git.log.iter().skip(first).take(visible).enumerate() {
        let y = area.y + i as u16;
        let cursor = first + i == git.hist_sel && focused;
        let bg = row_ground(buf, area, y, cursor, theme);

        let short: String = entry.id.chars().take(7).collect();
        match lanes.get(first + i) {
            Some(row) => {
                let g = crate::graph::glyphs(row, GIT_MAX_LANES);
                // The node takes the accent so the eye can follow one branch
                // down the page; the lines around it stay quiet, or the column
                // reads as a wall.
                for (n, ch) in g.chars().enumerate() {
                    let fg = if n == row.lane { theme.accent } else { theme.rule };
                    put_str(buf, area.x + n as u16, y, &ch.to_string(), bound, Pen::new(fg, bg));
                }
            }
            None => put_str(buf, area.x, y, "●", bound, Pen::new(theme.accent, bg)),
        }
        let sha_x = area.x + graph_w.max(1) + 1;
        put_str(buf, sha_x, y, &short, bound, Pen::new(theme.muted, bg));

        // Ref chips sit between the sha and the summary, because that is where
        // the eye already is after reading the sha, and they are what makes one
        // commit in a page of them worth stopping at.
        let mut x = sha_x + 8;
        for r in &entry.refs {
            if r.kind == butai_protocol::api::RefKind::Head {
                continue; // Drawn as boldness on the branch beside it, not twice.
            }
            let head_here = entry.refs.iter().any(|o| o.kind == butai_protocol::api::RefKind::Head);
            let colour = match r.kind {
                butai_protocol::api::RefKind::Tag => theme.accent,
                butai_protocol::api::RefKind::Remote => theme.faint,
                _ => theme.ok,
            };
            let w = r.name.width() as u16 + 1;
            if x + w >= bound.saturating_sub(6) {
                break;
            }
            put_str(
                buf,
                x,
                y,
                &r.name,
                bound,
                Pen {
                    fg: colour,
                    bg,
                    bold: head_here && r.kind == butai_protocol::api::RefKind::Branch,
                },
            );
            x += w;
        }

        if x < bound {
            let summary = ellipsize(&entry.summary, bound.saturating_sub(x) as usize);
            put_str(buf, x, y, &summary, bound, Pen::new(theme.ink, bg));
        }
    }
}

/// The Docker page: stacks and their containers, beside the logs of the one
/// under the cursor.
fn draw_docker_page(
    buf: &mut Buffer,
    geom: &Geom,
    sys: &SysDto,
    ws: Option<&WorkspaceDetail>,
    docker: Option<&Docker>,
    view: &View,
    theme: &Theme,
) {
    let outer = geom.stage_box;
    let list_w = tree_width(outer.width);
    let list_box = LRect::new(outer.x, outer.y, list_w, outer.height);
    let logs_box =
        LRect::new(outer.x + list_w, outer.y, outer.width.saturating_sub(list_w), outer.height);

    let cwd = ws.map(|w| w.cwd.as_str()).unwrap_or("");
    let stacks = project_stacks(sys, cwd);
    let rows = docker_rows(&stacks);
    let sel = docker.map(|d| d.sel).unwrap_or(0).min(rows.len().saturating_sub(1));

    draw_box(buf, list_box, " DOCKER ", theme.border(view.focus != Focus::Stage), theme.ground);
    let inner = LRect::new(
        list_box.x + 1,
        list_box.y + 1,
        list_w.saturating_sub(2),
        list_box.height.saturating_sub(2),
    );
    let bound = inner.x + inner.width;
    if rows.is_empty() {
        put_str(
            buf,
            inner.x,
            inner.y,
            "no running containers",
            bound,
            Pen::new(theme.faint, theme.ground),
        );
    }
    // Keep the cursor in view: a machine with many stacks outgrows the column
    // long before the terminal runs out of rows.
    let visible = inner.height as usize;
    let first = sel.saturating_sub(visible.saturating_sub(1));
    for (i, row) in rows.iter().skip(first).take(visible).enumerate() {
        let y = inner.y + i as u16;
        let cursor = first + i == sel && view.focus != Focus::Stage;
        let bg = theme.row_bg(cursor);
        for x in inner.x..bound {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_bg(bg);
            }
        }
        match row {
            DockerRow::Stack(si) => {
                let s = &stacks[*si];
                let status = if s.dto.running == s.dto.total {
                    "up".to_string()
                } else {
                    format!("{}/{}", s.dto.running, s.dto.total)
                };
                // A stack that is not this project's is dimmed rather than
                // hidden: it only shows at all when nothing here is ours.
                let fg = if s.mine { theme.ink } else { theme.faint };
                // A one-container stack stands for its container, so it wears
                // the same dot a container row does — without it a standalone
                // container was the one row on the page whose state you could
                // only read off the status column. A compose project gets the
                // marker that says it has rows underneath instead. Both are two
                // cells, so every header's label starts in the same column.
                let (marker, marker_fg) = match (s.expands(), s.dto.running > 0) {
                    (true, _) => ("▾", theme.faint),
                    (false, true) => ("●", theme.ok),
                    (false, false) => ("○", theme.faint),
                };
                put_str(buf, inner.x, y, marker, bound, Pen::new(marker_fg, bg));
                let status_w = status.width() as u16;
                let label =
                    ellipsize(&s.dto.label, inner.width.saturating_sub(status_w + 3) as usize);
                put_str(
                    buf,
                    inner.x + 2,
                    y,
                    &label,
                    bound.saturating_sub(status_w),
                    Pen::new(fg, bg),
                );
                put_str(
                    buf,
                    bound.saturating_sub(status_w),
                    y,
                    &status,
                    bound,
                    Pen::new(theme.ok, bg),
                );
            }
            DockerRow::Container { name, running, .. } => {
                let (glyph, fg) = if *running { ("●", theme.ok) } else { ("○", theme.faint) };
                put_str(buf, inner.x + 1, y, glyph, bound, Pen::new(fg, bg));
                let name = ellipsize(name, inner.width.saturating_sub(3) as usize);
                put_str(buf, inner.x + 3, y, &name, bound, Pen::new(theme.ink, bg));
            }
        }
    }

    let title = match docker.and_then(|d| d.following.as_deref()) {
        Some(name) => format!(" {name} · logs "),
        None => " logs ".to_string(),
    };
    draw_box(buf, logs_box, &title, theme.border(view.focus == Focus::Stage), theme.ground);
    // The pane's cells are blitted over the interior after this — see
    // `stage_rect`, which measures exactly that hole — so nothing is drawn
    // there except the placeholder before one is following.
    if docker.and_then(|d| d.logs).is_none() && logs_box.width > 4 {
        put_str(
            buf,
            logs_box.x + 2,
            logs_box.y + 1,
            "Enter to follow a container's logs",
            logs_box.x + logs_box.width,
            Pen::new(theme.faint, theme.ground),
        );
    }
    if logs_box.height >= 3 {
        put_str(
            buf,
            logs_box.x + 2,
            logs_box.y + logs_box.height - 2,
            "r restart · x stop · s shell · enter follow · q close",
            logs_box.x + logs_box.width,
            Pen::new(theme.faint, theme.ground),
        );
    }
}

/// Which pane the stage is showing: this client's choice, or the workspace's
/// own default when it has not made one.
///
/// One function because three things read it — the title, the rail's marker and
/// the connection itself — and a screen where they disagree is worse than any
/// of them being wrong alone: it says the stage holds one pane while showing
/// another.
pub fn staged_pane(ws: Option<&WorkspaceDetail>, view: &View) -> Option<PaneId> {
    let w = ws?;
    let known =
        |p: PaneId| w.agents.iter().any(|a| a.pane == p) || w.processes.iter().any(|q| q.pane == p);
    view.staged.filter(|p| known(*p)).or(w.stage)
}

/// What the stage box calls what it is showing, resolved from the DTOs rather
/// than from a pane the client cannot see inside.
fn stage_title(ws: Option<&WorkspaceDetail>, view: &View) -> String {
    let Some(w) = ws else { return " STAGE ".into() };
    let Some(stage) = staged_pane(ws, view) else { return " STAGE ".into() };
    let named = |p: PaneId| -> Option<String> {
        w.agents
            .iter()
            .find(|a| a.pane == p)
            .map(|a| a.title.clone())
            .or_else(|| w.processes.iter().find(|x| x.pane == p).map(|x| x.name.clone()))
    };
    match named(stage) {
        Some(name) => format!(" STAGE · {name} "),
        None => " STAGE ".into(),
    }
}

/// One row of the CHANGES rail, headings included.
///
/// The rail is a flat list with section headings in it, and *the same list* is
/// what the cursor walks and what Enter reads. Drawing from one place and
/// dispatching from another is how "diff the row I am on" ends up off by the
/// number of headings above it — the daemon's `GitPane` builds exactly this
/// list for the same reason.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChangeRow<'a> {
    /// A section heading. Selectable: it diffs the whole section.
    Header(&'a str),
    Conflicted {
        path: &'a str,
    },
    File {
        change: &'a FileChange,
        staged: bool,
    },
    Commit {
        id: &'a str,
        summary: &'a str,
    },
}

/// Lay the CHANGES rail out as rows, in the order they are drawn.
pub fn change_rows(c: &ChangesDto) -> Vec<ChangeRow<'_>> {
    let mut rows = Vec::new();
    // Conflicts first: they are what is blocking you, and unlike the other
    // sections a conflicted file is never also listed as ordinary unstaged
    // work — which would offer `s` on something that cannot be staged.
    if !c.conflicted.is_empty() {
        rows.push(ChangeRow::Header("Conflicts"));
        rows.extend(c.conflicted.iter().map(|f| ChangeRow::Conflicted { path: &f.path }));
    }
    if !c.unstaged.is_empty() {
        rows.push(ChangeRow::Header("Unstaged"));
        rows.extend(c.unstaged.iter().map(|f| ChangeRow::File { change: f, staged: false }));
    }
    if !c.staged.is_empty() {
        rows.push(ChangeRow::Header("Staged"));
        rows.extend(c.staged.iter().map(|f| ChangeRow::File { change: f, staged: true }));
    }
    if !c.recent_commits.is_empty() {
        rows.push(ChangeRow::Header("Commits"));
        rows.extend(
            c.recent_commits.iter().map(|k| ChangeRow::Commit { id: &k.id, summary: &k.summary }),
        );
    }
    rows
}

/// A resizable band of a rail, named by the focus that selects it.
///
/// Layout mode resizes *a section*, not "the thing under the cursor": which
/// rail the arrows widen follows from which section you are in, so the two
/// questions have one answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Agents,
    Processes,
    Changes,
}

impl Section {
    pub fn of(focus: Focus) -> Self {
        match focus {
            Focus::Processes => Section::Processes,
            Focus::Changes => Section::Changes,
            _ => Section::Agents,
        }
    }

    pub fn on_right_rail(self) -> bool {
        matches!(self, Section::Changes)
    }

    /// The section's name in the layout HUD, matching its rail header.
    pub fn label(self) -> &'static str {
        match self {
            Section::Agents => "AGENTS",
            Section::Processes => "PROCESSES",
            Section::Changes => "CHANGES",
        }
    }

    /// How tall it is right now, from the same `sections` split the rail draws.
    pub fn height(self, geom: RailGeom, rows: u16, want_system_h: u16) -> u16 {
        let s = crate::chrome::sections(geom, rows, want_system_h);
        match self {
            Section::Agents => s.agents_h,
            Section::Processes => s.procs_h,
            Section::Changes => s.changes_h,
        }
    }
}

/// Widen or narrow a rail by `delta` cells.
///
/// Clamped to the configured bounds and, on top of that, capped so the two
/// rails always leave [`MIN_STAGE_W`] for the stage — otherwise growing a rail
/// on a narrow terminal trips the fallback that collapses both to nothing, and
/// the key appears to do the opposite of what it says.
pub fn resize_rail(geom: &mut RailGeom, cols: u16, left: bool, delta: i16) {
    use crate::chrome::{MIN_STAGE_W, RAIL_MAX_W, RAIL_MIN_W};
    let other = if left { geom.right_w } else { geom.left_w };
    // Never let the cap drop below the floor: `clamp` requires `lo <= hi`, and
    // on a genuinely tiny terminal the drawing's own fallback still applies.
    let hi = RAIL_MAX_W.min(cols.saturating_sub(MIN_STAGE_W + other)).max(RAIL_MIN_W);
    let w = if left { &mut geom.left_w } else { &mut geom.right_w };
    *w = w.saturating_add_signed(delta).clamp(RAIL_MIN_W, hi);
}

/// Grow or shrink one section by `delta` rows.
///
/// The heights start out automatic, so the first keypress seeds them from what
/// is on screen — after that they are literal row counts and `Chrome::compute`
/// only fits them to the rail.
pub fn resize_section(
    geom: &mut RailGeom,
    rows: u16,
    section: Section,
    delta: i16,
    want_system_h: u16,
) {
    use crate::chrome::{SECTION_MIN_H, SYSTEM_MAX_H};
    let seed = crate::chrome::sections(*geom, rows, want_system_h);
    // The upper bound only stops a runaway key-repeat from storing an absurd
    // number; the real fitting happens at draw time, against a rail height this
    // client may not share with the next one. It has to stay above the floor,
    // since `clamp` requires `lo <= hi`.
    let rail_h = rows.saturating_sub(4).max(SECTION_MIN_H);
    let resize = |cur: u16, by: i16| cur.saturating_add_signed(by).clamp(SECTION_MIN_H, rail_h);
    match section {
        // AGENTS has nothing above it, so it grows by taking from PROCESSES —
        // the mirror image of PROCESSES' own move.
        Section::Agents => geom.procs_h = Some(resize(seed.procs_h, -delta)),
        Section::Processes => {
            // What PROCESSES gains comes out of SYSTEM, so AGENTS stays exactly
            // where the user last put it. SYSTEM may have less to give than was
            // asked for, so PROCESSES takes what it actually got rather than
            // assuming the full delta.
            let system = seed.system_h.saturating_add_signed(-delta).min(SYSTEM_MAX_H);
            let moved = seed.system_h as i32 - system as i32;
            geom.system_h = Some(system);
            geom.procs_h = Some(resize(seed.procs_h, moved as i16));
        }
        // CHANGES is the whole right rail: there is no neighbour to take rows
        // from, so its height is the rail's and only the rail's width moves.
        Section::Changes => {}
    }
}

/// A rail band whose height `[ui]` stores.
///
/// [`Section`] is the same rail seen from LAYOUT mode, where a drag moves a
/// *boundary* and one gesture therefore writes two fields. The SETTINGS page
/// names the stored keys instead — a row that says `[ui] system_height` had
/// better set `system_height` — so it needs the bands rather than the
/// boundaries between them. Both land in the same struct through the same
/// floor, which is the part that must not be duplicated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    Procs,
    System,
}

impl Band {
    fn slot(self, geom: &mut RailGeom) -> &mut Option<u16> {
        match self {
            Band::Procs => &mut geom.procs_h,
            Band::System => &mut geom.system_h,
        }
    }

    /// The gauges top out where the block becomes padding; the other two are
    /// bounded only by the rail they live in.
    fn ceiling(self) -> u16 {
        match self {
            Band::System => SYSTEM_MAX_H,
            Band::Procs => u16::MAX,
        }
    }

    /// What the band is drawing at right now, stored or automatic — which is
    /// what a nudge has to start from, so the first `+` grows what is on screen
    /// rather than jumping to a number nobody chose.
    pub fn height(self, geom: RailGeom, rows: u16, want_system_h: u16) -> u16 {
        let s = crate::chrome::sections(geom, rows, want_system_h);
        match self {
            Band::Procs => s.procs_h,
            Band::System => s.system_h,
        }
    }
}

/// Pin a band's height, or clear it back to sizing itself to the terminal.
///
/// The clamp is [`resize_section`]'s, deliberately: the two gestures write the
/// same fields, and a floor that held for a drag but not for a keypress would
/// be a rail you could type into a state you could not drag it into.
pub fn set_band(geom: &mut RailGeom, rows: u16, band: Band, value: Option<u16>) {
    let rail_h = rows.saturating_sub(4).max(SECTION_MIN_H);
    let hi = rail_h.min(band.ceiling()).max(SECTION_MIN_H);
    *band.slot(geom) = value.map(|v| v.clamp(SECTION_MIN_H, hi));
}

/// Grow or shrink a band from whatever it is drawing at.
pub fn nudge_band(geom: &mut RailGeom, rows: u16, band: Band, delta: i16, want_system_h: u16) {
    let from = band.height(*geom, rows, want_system_h);
    set_band(geom, rows, band, Some(from.saturating_add_signed(delta)));
}

/// What the footer says while layout mode is on.
pub fn layout_hud(view: &View, cols: u16, rows: u16) -> String {
    let section = Section::of(view.focus);
    let (rail, w) = if section.on_right_rail() {
        ("right", view.geom.right_w)
    } else {
        ("left", view.geom.left_w)
    };
    let _ = cols;
    let h = section.height(view.geom, rows, system_h_wanted(&view.gauges));
    // Two hints where there was one: kept short enough that the footer's
    // buttons do not paint over "esc save" on a 100-column terminal.
    format!("LAYOUT (every tab)  ←/→ {rail} {w}  ↑/↓ {} {h}  esc save", section.label())
}

pub const PROCS_ADD_LABEL: &str = "[+ term]";
pub const AGENTS_ADD_LABEL: &str = "[+ agent]";

/// The `[+ agent]` label, naming the pinned agent when there is one.
///
/// A pinned button spawns on a single click with nothing in between, so the
/// label is the only place the user can see what that click is about to do —
/// `[+ claude]` rather than a `[+ agent]` that quietly stopped asking. Falls
/// back to the generic word when the rail is too narrow to spell the name
/// without running into the ` AGENTS ` title, which starts at `x + 2`.
pub fn agents_add_label(pinned: Option<&str>, left_box: LRect) -> String {
    let room = left_box.width.saturating_sub(12) as usize;
    match pinned {
        Some(name) if name.width() + 4 <= room => format!("[+ {name}]"),
        _ => AGENTS_ADD_LABEL.to_string(),
    }
}

/// Where that label sits — the draw and the click both go through this, and
/// both pass the label they mean, because a pinned name is a different width
/// from `[+ agent]` and a right-aligned button measured against the wrong one
/// lands a column off.
pub fn agents_add_span_for(geom: &Geom, label: &str) -> (u16, u16) {
    let w = label.width() as u16;
    let end = geom.left_box.right().saturating_sub(1);
    (end.saturating_sub(w), end)
}

/// The `[+ term]` button on the PROCESSES separator.
pub fn procs_add_span(geom: &Geom) -> (u16, u16) {
    let w = PROCS_ADD_LABEL.width() as u16;
    let end = geom.left_box.right().saturating_sub(1);
    (end.saturating_sub(w), end)
}

/// Draw a section's verbs into `area`, the dangerous ones in the danger colour.
///
/// All three verb footers — AGENTS, PROCESSES and CHANGES — go through this,
/// because the text and the colouring both have to come from the same
/// [`crate::verbs::layout`] the hit-test reads back. A surface that decided for
/// itself where a verb had been drawn is how a button ends up one column off
/// the word it is under.
fn draw_verb_footer(buf: &mut Buffer, area: LRect, verbs: &[crate::verbs::Verb], theme: &Theme) {
    let bound = area.x + area.width;
    let (width, rows) = (area.width as usize, area.height as usize);
    for (i, line) in crate::verbs::lines(verbs, width, rows).into_iter().enumerate() {
        let y = area.y + i as u16;
        put_str(buf, area.x, y, &line, bound, Pen::new(theme.faint, theme.ground));
    }
    for span in crate::verbs::layout(verbs, width, rows) {
        let Some(v) = verbs.iter().find(|v| v.key == span.key) else { continue };
        if !v.danger {
            continue;
        }
        let y = area.y + span.row as u16;
        for x in span.start..span.end {
            if let Some(cell) = buf.cell_mut((area.x + x as u16, y)) {
                cell.set_fg(theme.danger);
            }
        }
    }
}

/// Which verb a click at `col` in a left-rail section's footer row lands on.
///
/// Reads back the same packing [`draw_verb_footer`] drew, so the word and the
/// button are the same columns by construction.
pub fn rail_verb_at(verbs: &[crate::verbs::Verb], width: u16, col: u16) -> Option<char> {
    crate::verbs::hit(verbs, width as usize, crate::verbs::RAIL_FOOTER_ROWS, 0, col as usize)
}

/// Which kind of row the CHANGES cursor is on, in the verb table's vocabulary.
///
/// The table keys off *what is selected* — that is the whole point of it: the
/// rail used to offer `s stage` with a commit selected, and nothing at all with
/// a conflict.
pub fn changes_row_kind(c: &ChangesDto, sel: usize) -> crate::verbs::ChangesRow {
    use crate::verbs::ChangesRow;
    match change_rows(c).get(sel) {
        Some(ChangeRow::Conflicted { .. }) => ChangesRow::Conflict,
        Some(ChangeRow::File { staged: false, .. }) => ChangesRow::Unstaged,
        Some(ChangeRow::File { staged: true, .. }) => ChangesRow::Staged,
        Some(ChangeRow::Commit { .. }) => ChangesRow::Commit,
        // A header, or a cursor past the end of a rail that has shrunk.
        _ => ChangesRow::None,
    }
}

/// What the CHANGES rail can do right now, for the row the cursor is on.
pub fn changes_verbs(c: &ChangesDto, sel: usize) -> Vec<crate::verbs::Verb> {
    crate::verbs::changes_footer(changes_row_kind(c, sel), c.ahead)
}

/// How the rail splits between its list and its verb row(s).
///
/// Returns the list's height; the rest of `changes_rows` is the footer. Both
/// the drawing and the hit-test go through this, so a click on a verb cannot
/// land on the file above it.
pub fn changes_split(geom: &Geom) -> (u16, u16) {
    let rows = geom.changes_rows;
    let footer = crate::verbs::changes_footer_rows(rows.width as usize) as u16;
    let footer = footer.min(rows.height);
    (rows.height - footer, footer)
}

fn draw_right_rail(
    buf: &mut Buffer,
    geom: &Geom,
    ws: Option<&WorkspaceDetail>,
    view: &View,
    theme: &Theme,
) {
    let changes = ws.and_then(|w| w.changes.as_ref());
    let label = changes
        .map(|c| changes_label(c, geom.right_box.width))
        .unwrap_or_else(|| " CHANGES ".into());
    draw_box(buf, geom.right_box, &label, theme.border(view.focus == Focus::Changes), theme.ground);
    let Some(c) = changes else { return };

    let area = geom.changes_rows;
    let bound = area.x + area.width;
    // The verb rows are pinned to the bottom and the list stops above them.
    let (list_h, footer_h) = changes_split(geom);
    let bottom = area.y + list_h;
    if footer_h > 0 {
        let footer = LRect::new(area.x, area.y + list_h, area.width, footer_h);
        draw_verb_footer(buf, footer, &changes_verbs(c, view.changes_sel), theme);
    }

    // The headings count as rows here, because they are rows the cursor can sit
    // on — so the list this scrolls is the built one, not the file count.
    let list = change_rows(c);
    let first = rail_first(view.changes_sel, list.len(), list_h);
    for (i, row) in list.into_iter().skip(first).enumerate() {
        let y = area.y + i as u16;
        if y >= bottom {
            break;
        }
        let cursor = first + i == view.changes_sel && view.focus == Focus::Changes;
        match row {
            ChangeRow::Header(name) => {
                // A heading has no status token and no marquee, so it is written
                // rather than drawn as a row — but it still takes the cursor
                // background, because a section is something you can act on.
                let bg = theme.row_bg(cursor);
                for x in area.x..bound {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_symbol(" ");
                        cell.set_bg(bg);
                    }
                }
                put_str(buf, area.x, y, name, bound, Pen::new(theme.muted, bg));
            }
            ChangeRow::Conflicted { path } => {
                let bg = theme.row_bg(cursor);
                for x in area.x..bound {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_symbol(" ");
                        cell.set_bg(bg);
                    }
                }
                put_str(buf, area.x + 2, y, path, bound, Pen::new(theme.danger, bg));
            }
            ChangeRow::File { change: f, staged } => {
                let stat = format!("+{} -{}", f.added, f.deleted);
                // The status code is pinned and only the path scrolls: a row
                // whose `M` slides away is a row that has stopped saying what
                // happened to the file, and the path is the only part of it too
                // long to fit in the first place.
                draw_row(
                    buf,
                    area,
                    y,
                    cursor,
                    false,
                    &f.code,
                    &f.path,
                    if staged { theme.ok } else { theme.attention },
                    &stat,
                    theme.faint,
                    view.tick,
                    theme,
                );
            }
            ChangeRow::Commit { id, summary } => {
                let bg = theme.row_bg(cursor);
                for x in area.x..bound {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_symbol(" ");
                        cell.set_bg(bg);
                    }
                }
                let short: String = id.chars().take(7).collect();
                let text =
                    ellipsize(&format!("{short} {summary}"), area.width.saturating_sub(2) as usize);
                put_str(buf, area.x + 2, y, &text, bound, Pen::new(theme.faint, bg));
            }
        }
    }
}

/// The footer's right-hand buttons, as one string because that is how they are
/// drawn — one `put_str` rather than four, so they cannot drift apart.
///
/// `[settings]` sits here rather than on the tab bar, where it used to be a
/// chip. The bar is *which project*, and this client's own configuration is not
/// one of them; the footer is already where the things that are about the
/// client and not the workspace live — the layout, the connection, the
/// reference. It replaced `[:cmd]`, which advertised a keystroke rather than a
/// place and was the one button here that opened a box to type in.
pub const FOOTER_BUTTONS: &str = "[layout] [detach] [help] [settings]";

/// Each footer button and the columns it occupies, or nothing when the screen
/// is too narrow for the row (in which case none of them was drawn either).
pub fn footer_button_spans(width: u16) -> Vec<(&'static str, u16, u16)> {
    if FOOTER_BUTTONS.len() as u16 + 2 > width {
        return Vec::new();
    }
    let start = width - FOOTER_BUTTONS.len() as u16;
    let mut out = Vec::new();
    let mut at = start;
    for label in FOOTER_BUTTONS.split(' ') {
        let w = label.len() as u16;
        out.push((
            match label {
                "[layout]" => "[layout]",
                "[detach]" => "[detach]",
                "[help]" => "[help]",
                _ => "[settings]",
            },
            at,
            at + w,
        ));
        at += w + 1;
    }
    out
}

/// The footer: what you are looking at on the left, an attention notice in the
/// middle, buttons on the right.
///
/// All three zones are measured before any of them is written. The daemon's
/// version bounds each one only by the screen edge, so below about 88 columns
/// they overwrite each other — which is the phone-width path, and visible on the
/// landing page today (see `TODO/tui-showpiece.md`).
fn draw_footer(
    buf: &mut Buffer,
    area: &LRect,
    ws: Option<&WorkspaceDetail>,
    host: Option<&str>,
    view: &View,
    theme: &Theme,
) {
    let y = area.y;
    let width = area.width;
    for x in area.x..area.x + width {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(" ");
            cell.set_bg(theme.status_bg);
        }
    }
    let buttons = FOOTER_BUTTONS;
    let notice = view.flash.clone().or_else(|| attention_notice(ws, view.page));

    // Right zone first: its start column is a hard bound for everything else.
    let mut right_start = width;
    if buttons.len() as u16 + 2 <= width {
        right_start = width - buttons.len() as u16;
        put_str(buf, right_start, y, buttons, width, Pen::new(theme.status_fg, theme.status_bg));
        // Two of these buttons name a page rather than an action, so those two
        // can be lit — and being lit is what says the second press is the way
        // back out. Overdrawn rather than split out of the string, because the
        // string is what the hit test measures.
        let lit = match view.page {
            Page::Settings => Some("[settings]"),
            Page::Help => Some("[help]"),
            _ => None,
        };
        if let Some(lit) = lit {
            if let Some((label, x, _)) =
                footer_button_spans(width).into_iter().find(|(l, _, _)| *l == lit)
            {
                let pen = Pen { fg: theme.ground, bg: theme.accent, bold: true };
                put_str(buf, x, y, label, width, pen);
            }
        }
    }

    // Centre the notice in what is left; its start bounds the left string.
    let mut mid_start = right_start;
    if let Some(text) = notice.as_deref() {
        let avail = right_start.saturating_sub(1);
        let want = text.chars().count() as u16;
        // Cut it down to what there is room for. Only when that leaves no room
        // for a readable fragment does it degrade to the bare marker — the
        // phone-width path, where the glyph is the part that matters and the
        // detail is what you go and look at.
        //
        // The cut is by *available width* rather than by the text's own length:
        // a long error on a wide screen used to take the same branch as a short
        // one on a phone and come out as a lone dot, which told you something
        // had happened and nothing about what.
        const MIN_READABLE: u16 = 12;
        let shortened;
        let (text, want) = if want + 2 <= avail {
            (text, want)
        } else if avail >= MIN_READABLE + 2 {
            shortened = ellipsize(text, avail.saturating_sub(2) as usize);
            let w = shortened.chars().count() as u16;
            (shortened.as_str(), w)
        } else {
            ("●", 1)
        };
        if want + 2 <= avail {
            mid_start = avail.saturating_sub(want) / 2;
            put_str(
                buf,
                mid_start,
                y,
                text,
                right_start,
                Pen::new(theme.attention, theme.status_bg),
            );
        }
    }

    // Layout mode takes the left zone outright: while it is on, every arrow
    // key means something else and the footer is where that is said.
    let left = match &view.layout {
        Some(_) => format!(" {}", layout_hud(view, width, area.y + 1)),
        None => ws
            .map(|w| {
                // The branch belongs here rather than only in the CHANGES
                // label: which branch you are on is the thing you most want to
                // be sure of before an agent starts writing, and the rail can
                // be scrolled, narrow, or shut.
                let branch =
                    w.changes.as_ref().map(|c| format!(" ({})", c.branch)).unwrap_or_default();
                // `host:/path` — scp's own spelling of "that path, on that
                // machine", which reads as such without being explained.
                //
                // A bare path stopped being unambiguous the moment a second
                // machine could be in the tab bar: `/home/me/proj` exists on all
                // of them. The tab chip already qualifies itself this way; the
                // footer was the last strip still implying there is only one
                // machine, and it is the strip that survives every page. `host`
                // is already `None` for a single-daemon client, so nothing
                // changes for anyone who has not connected a second.
                let at = match host {
                    Some(h) => format!("{h}:{}", w.cwd),
                    None => w.cwd.clone(),
                };
                format!(" {} {at}{branch}", w.name)
            })
            .unwrap_or_else(|| " butai".to_string()),
    };
    // The armed prefix goes on the end, bold, because it is a mode: until the
    // next keystroke the whole keyboard means something else, and the one thing
    // worse than not showing that is showing it quietly.
    let left = if view.prefix_armed { format!("{left} {}", view.prefix) } else { left };
    let left = ellipsize(&left, mid_start.saturating_sub(1) as usize);
    let pen = Pen { fg: theme.status_fg, bg: theme.status_bg, bold: view.prefix_armed };
    put_str(buf, area.x, y, &left, mid_start, pen);
}

/// "● claude is waiting" — the first agent that wants you, or nothing.
fn attention_notice(ws: Option<&WorkspaceDetail>, page: Page) -> Option<String> {
    let w = ws?;
    let a = w.agents.iter().find(|a| a.state == butai_protocol::api::AgentState::Waiting)?;
    // On a page that has hidden the AGENTS rail, this line is the only thing on
    // screen naming *which* agent is blocked. The rail's badge says that one is,
    // which is a bit; a two-cell badge cannot carry an identity, and the footer
    // is the other strip that survives every page. So it says more here, and
    // says how to get to it without losing the page you are on.
    if page.owns_full_width() && page != Page::Booth {
        Some(format!("● {} is waiting · alt-w", a.title))
    } else {
        Some(format!("● {} is waiting", a.title))
    }
}

/// The ALL AGENTS sprite for one row, resolved to a colour.
///
/// Public because the panel that uses it is drawn from the cross-workspace agent
/// list rather than from one `WorkspaceDetail`.
pub fn sprite_for(a: &AgentDto, fast_tick: u64, theme: &Theme) -> (String, Color, bool) {
    let (text, role, moving) = agent_sprite(a.state, a.exited, secs_since(a.started_ms), fast_tick);
    debug_assert_eq!(text.chars().count(), SPRITE_W);
    (text, theme.role(role), moving)
}

#[cfg(test)]
mod tests {

    /// Whatever the policy, the cursor has to be on screen — and it must not
    /// scroll when the whole list already fits, or a four-row rail would jump
    /// every time you moved down it.
    #[test]
    fn the_selection_is_always_inside_the_window() {
        for height in 1u16..8 {
            for sel in 0usize..12 {
                let first = first_visible(sel, height);
                assert!(first <= sel, "scrolled past the cursor: {sel} {height} -> {first}");
                assert!(
                    sel < first + height as usize,
                    "cursor below the window: {sel} {height} -> {first}"
                );
                if sel < height as usize {
                    assert_eq!(first, 0, "scrolled while the list still fit: {sel} {height}");
                }
            }
        }
    }
    use super::*;
    use butai_protocol::api::{AgentState, CommitDto, RepoState};
    use butai_protocol::SessionId;

    /// A scene with only the parts a test cares about filled in.
    fn scene<'a>(
        tabs: &'a [Tab<'a>],
        ws: Option<&'a WorkspaceDetail>,
        sys: &'a SysDto,
        all: &'a [AllAgentRow<'a>],
    ) -> Scene<'a> {
        Scene { workspace: ws, all_agents: all, ..Scene::new(tabs, sys) }
    }

    /// A BOOTH fleet: two machines, three workspaces, in connection order.
    fn booth_fleet<'a>(agents: &'a [AgentDto]) -> Vec<AllAgentRow<'a>> {
        vec![
            AllAgentRow {
                workspace: "butai",
                workspace_id: SessionId(1),
                agent: &agents[0],
                host: None,
                daemon: 0,
            },
            AllAgentRow {
                workspace: "butai",
                workspace_id: SessionId(1),
                agent: &agents[1],
                host: None,
                daemon: 0,
            },
            AllAgentRow {
                workspace: "caliper",
                workspace_id: SessionId(2),
                agent: &agents[2],
                host: None,
                daemon: 0,
            },
            AllAgentRow {
                workspace: "diffusion",
                workspace_id: SessionId(1),
                agent: &agents[3],
                host: Some("gpu-box"),
                daemon: 1,
            },
        ]
    }

    /// A fleet of any size, all on one machine in one workspace — for the tests
    /// that care about *which* rows the tray picks and in what order, not about
    /// the two-machine layout [`booth_fleet`] pins down.
    fn fleet_of(agents: &[AgentDto]) -> Vec<AllAgentRow<'_>> {
        agents
            .iter()
            .map(|a| AllAgentRow {
                workspace: "butai",
                workspace_id: SessionId(1),
                agent: a,
                host: None,
                daemon: 0,
            })
            .collect()
    }

    fn machines<'a>(sys: &'a SysDto, all: &[AllAgentRow<'a>]) -> Vec<MachineRow<'a>> {
        ["local", "gpu-box"]
            .iter()
            .enumerate()
            .map(|(d, label)| MachineRow {
                label,
                sys,
                agents: all.iter().filter(|r| r.daemon == d).count(),
                live: true,
            })
            .collect()
    }

    /// The fleet list groups machine → workspace → agent, and **the order does
    /// not read agent state at all**.
    ///
    /// That is the property the whole page rests on: an urgency-sorted list was
    /// measured travelling ~174 row-positions per ten sampler ticks, which is a
    /// list you cannot click. Mutation-checked below by flipping every state and
    /// asserting the sequence is byte-identical — sort the rows by urgency in
    /// `booth_rows` and this fails.
    #[test]
    fn the_fleet_list_groups_by_machine_and_never_reorders() {
        let calm = [
            agent(1, "claude", AgentState::Idle),
            agent(2, "codex", AgentState::Idle),
            agent(3, "aider", AgentState::Idle),
            agent(4, "gemini", AgentState::Idle),
        ];
        let all = booth_fleet(&calm);
        let sys = SysDto::default();
        let ms = machines(&sys, &all);
        let shape = |rows: &[BoothRow<'_>]| -> Vec<String> {
            rows.iter()
                .map(|r| match r {
                    BoothRow::Machine { label, .. } => format!("machine:{label}"),
                    BoothRow::Space { name } => format!("space:{name}"),
                    BoothRow::Agent { row, sel } => format!("agent:{}:{sel}", row.agent.title),
                })
                .collect()
        };
        let before = shape(&booth_rows(&all, &ms));
        assert_eq!(
            before,
            vec![
                "machine:local",
                "space:butai",
                "agent:claude:0",
                "agent:codex:1",
                "space:caliper",
                "agent:aider:2",
                "machine:gpu-box",
                "space:diffusion",
                "agent:gemini:3",
            ]
        );

        // Same agents, every one of them now in a different state — including
        // the two that a sort would drag to the top.
        let stirred = [
            agent(1, "claude", AgentState::Exited),
            agent(2, "codex", AgentState::Waiting),
            agent(3, "aider", AgentState::Working),
            agent(4, "gemini", AgentState::Waiting),
        ];
        let all2 = booth_fleet(&stirred);
        let ms2 = machines(&sys, &all2);
        assert_eq!(shape(&booth_rows(&all2, &ms2)), before, "state changed the order");
    }

    /// The tray *copies* the waiting agents upward and leaves the originals
    /// where they are — so the top of the page is a queue and the rest is a map,
    /// without the map moving.
    #[test]
    fn the_tray_copies_what_is_waiting_without_moving_it() {
        let agents = [
            agent(1, "claude", AgentState::Idle),
            agent(2, "codex", AgentState::Waiting),
            agent(3, "aider", AgentState::Working),
            agent(4, "gemini", AgentState::Waiting),
        ];
        let all = booth_fleet(&agents);
        let tray = booth_tray(&all);
        assert_eq!(
            tray.iter().map(|(i, r)| (*i, r.agent.title.as_str())).collect::<Vec<_>>(),
            vec![(1, "codex"), (3, "gemini")],
            "the tray holds the waiting agents, keyed by their real row index"
        );
        // And the fleet list still has all four, in their original places.
        let sys = SysDto::default();
        let ms = machines(&sys, &all);
        let seats: Vec<usize> = booth_rows(&all, &ms)
            .iter()
            .filter_map(|r| match r {
                BoothRow::Agent { sel, .. } => Some(*sel),
                _ => None,
            })
            .collect();
        assert_eq!(seats, vec![0, 1, 2, 3], "a copied agent left its seat");
    }

    /// A turn that landed while you were away belongs in the tray; the same turn
    /// once read does not. This is the whole read/unread distinction, seen from
    /// the one surface that is meant to answer "what needs me".
    #[test]
    fn the_tray_holds_unread_news_and_lets_go_of_it_when_read() {
        let mut agents = [
            agent(1, "claude", AgentState::Idle),
            unread_agent(2, "codex", None),
            agent(3, "aider", AgentState::Working),
            // Finished, but already read: your move, and you know it.
            agent(4, "gemini", AgentState::Finished),
        ];
        assert_eq!(
            booth_tray(&fleet_of(&agents))
                .iter()
                .map(|(_, r)| r.agent.title.as_str())
                .collect::<Vec<_>>(),
            vec!["codex"],
            "only the unread turn is news"
        );

        // Read it: the tray empties, though the agent is still `Finished` and
        // still sitting in the fleet list saying `done`.
        agents[1].unread = false;
        assert!(booth_tray(&fleet_of(&agents)).is_empty(), "reading it left the tray full");
        assert_eq!(agents[1].state, AgentState::Finished, "reading is not a state change");
    }

    /// The tray draws [`BOOTH_TRAY_H`] rows and does not scroll, so ranking is
    /// what decides which of them you actually see. A blocked agent must survive
    /// a crowd of finished ones.
    #[test]
    fn a_blocked_agent_outranks_news_and_a_crash_outranks_a_turn() {
        let agents = [
            unread_agent(1, "landed-a", None),
            unread_agent(2, "landed-b", None),
            unread_agent(3, "landed-c", None),
            unread_agent(4, "crashed", Some(2)),
            agent(5, "blocked", AgentState::Waiting),
        ];
        let all = fleet_of(&agents);
        let order: Vec<&str> =
            booth_tray(&all).iter().map(|(_, r)| r.agent.title.as_str()).collect();
        assert_eq!(
            order,
            vec!["blocked", "crashed", "landed-a", "landed-b", "landed-c"],
            "blocked first, then the crash, then the turns in fleet order"
        );
        // The failure this ranking exists to prevent: the blocked agent is last
        // in fleet order, and only ranking keeps it inside the visible four.
        assert!(
            order.iter().take(BOOTH_TRAY_H as usize).any(|t| *t == "blocked"),
            "the blocked agent fell off the bottom of the tray: {order:?}"
        );
    }

    /// A clean exit is news once; a crash is news *louder*. Both are news only
    /// until looked at — an exited agent you have read is not a standing alarm.
    #[test]
    fn a_read_corpse_leaves_the_tray() {
        let read = [AgentDto { exited: Some(1), ..agent(1, "dead", AgentState::Exited) }];
        assert!(booth_tray(&fleet_of(&read)).is_empty(), "a read crash is history, not a queue");

        // The same corpse before anyone looked at it is the loudest kind of news
        // short of a live question — which is what makes the assertion above a
        // statement about `unread` and not just about `Exited`.
        let fresh = [AgentDto { unread: true, ..read[0].clone() }];
        assert_eq!(booth_tray(&fleet_of(&fresh)).len(), 1, "an unread crash belongs in the tray");
    }

    /// The three columns exist, add up to the stage, and the middle one is what
    /// the daemon is told to render into.
    #[test]
    fn booth_splits_the_stage_into_three_columns() {
        let booth = View { page: Page::Booth, ..Default::default() };
        let geom = page_geom(160, 40, &booth);
        let band = booth_area(160, &geom);
        let c = booth_columns(band);
        assert!(c.fleet_box.width > 0 && c.compute_box.width > 0);
        // BOOTH takes the rails' columns too, so the band is the whole width.
        assert_eq!(band.width, 160);
        assert_eq!(
            c.fleet_box.width + c.stage_box.width + c.compute_box.width,
            band.width,
            "the columns must tile the band exactly"
        );
        assert_eq!(c.compute_box.right(), band.right());
        // The tray reserves its rows above the list, separator included.
        assert_eq!(c.tray_rows.height, BOOTH_TRAY_H);
        assert_eq!(c.fleet_sep, c.tray_rows.y + c.tray_rows.height);
        assert_eq!(c.fleet_rows.y, c.fleet_sep + 1);

        // The pane is measured to the middle column, not the whole stage.
        let view = View { page: Page::Booth, ..Default::default() };
        assert_eq!(stage_rect(160, 40, &view), to_rect(c.stage_inner));
        let work = View { page: Page::Agents, ..Default::default() };
        assert_ne!(stage_rect(160, 40, &work), stage_rect(160, 40, &view));
    }

    /// A terminal too narrow for three columns gives them up rather than
    /// squeezing the pane, the way the rails already do.
    #[test]
    fn booth_gives_up_its_columns_before_it_squeezes_the_pane() {
        let narrow = LRect::new(0, 1, 40, 20);
        let c = booth_columns(narrow);
        assert_eq!(c.fleet_box.width, 0);
        assert_eq!(c.compute_box.width, 0);
        assert_eq!(c.stage_box.width, narrow.width);
    }

    /// The page draws all three columns: the fleet grouped by machine, the
    /// selected agent's title over the stage, and every connected machine's
    /// telemetry — not only the active tab's.
    #[test]
    fn the_booth_page_draws_the_fleet_the_stage_and_every_machine() {
        let agents = [
            agent(1, "claude", AgentState::Idle),
            agent(2, "codex", AgentState::Waiting),
            agent(3, "aider", AgentState::Working),
            agent(4, "gemini", AgentState::Waiting),
        ];
        let all = booth_fleet(&agents);
        let sys =
            SysDto { cpu_pct: 42.0, ram_used_gb: 8.0, ram_total_gb: 32.0, ..Default::default() };
        let ms = machines(&sys, &all);
        let view = View { page: Page::Booth, focus: Focus::AllAgents, ..Default::default() };
        let mut b = buf(160, 40);
        let scene = Scene { machines: &ms, ..scene(&[], None, &sys, &all) };
        draw(&mut b, 160, 40, &scene, &view, &Theme::default());
        let screen: String = (0..40).map(|y| text_of(&b, y)).collect::<Vec<_>>().join("\n");

        assert!(screen.contains("FLEET (4)"), "{screen}");
        // Grouped: both machines and all three workspaces are headers.
        for want in ["local", "gpu-box", "butai", "caliper", "diffusion"] {
            assert!(screen.contains(want), "`{want}` missing from:\n{screen}");
        }
        // The tray counts what is waiting, and says so where a count belongs.
        assert!(screen.contains("NEEDS YOU (2)"), "{screen}");
        // The stage names the selected agent *and* its machine, because two
        // machines may run an agent of the same name one row apart.
        assert!(screen.contains("claude · local:butai"), "{screen}");
        // Compute is per machine, so the column carries both names and gauges.
        assert!(screen.contains("COMPUTE"), "{screen}");
        assert!(screen.contains("CPU") && screen.contains("RAM"), "{screen}");
    }

    /// One interface, `carrier` up and carrying the default route, with the
    /// history the rail autoscales against.
    fn net(name: &str, rx: &[f32], tx: &[f32]) -> NetDto {
        NetDto {
            name: name.into(),
            rx_bps: rx.last().copied().unwrap_or(0.0),
            tx_bps: tx.last().copied().unwrap_or(0.0),
            rx_hist: rx.to_vec(),
            tx_hist: tx.to_vec(),
            kind: NetKind::Wired,
            carrier: true,
            default_route: true,
            speed_mbps: Some(1000),
            driver: Some("r8169".into()),
        }
    }

    /// Draw just the SYSTEM section into a rail-width buffer and hand back the
    /// rows, so a gauge can be read the way it is seen.
    fn system_rows(sys: &SysDto, rows: u16) -> (Vec<String>, Buffer) {
        system_rows_with(sys, rows, &NetSelect::default())
    }

    fn system_rows_with(sys: &SysDto, rows: u16, net: &NetSelect) -> (Vec<String>, Buffer) {
        let mut b = buf(LEFT_W - 2, rows);
        let area = LRect::new(0, 0, LEFT_W - 2, rows);
        let gs = system_gauges(sys, net, &DiskSelect::default());
        draw_system(&mut b, area, sys, &gs, &Theme::default());
        ((0..rows).map(|y| text_of(&b, y)).collect(), b)
    }

    /// One local disk, sized in gigabytes.
    fn disk(mount: &str, used_gb: f32, total_gb: f32) -> DiskDto {
        DiskDto {
            mount: mount.into(),
            source: format!("/dev/{}", mount.trim_start_matches('/').replace('/', "-")),
            fstype: "ext4".into(),
            kind: DiskKind::Local,
            used_gb,
            total_gb,
            stale: false,
        }
    }

    /// The reported bug, pinned. 230 B/s of ssh keepalives and mDNS is not
    /// traffic, and it drew a solid two-row axis indistinguishable from a
    /// saturated link. Both trace rows must now be blank.
    #[test]
    fn an_idle_link_draws_nothing_at_all() {
        let quiet: Vec<f32> = (0..60).map(|i| 180.0 + (i % 7) as f32 * 20.0).collect();
        let sys = SysDto { net: vec![net("eth0", &quiet, &quiet)], ..Default::default() };
        let (rows, _) = system_rows(&sys, 9);
        // Row 4 is the NET head (cpu head+trace, ram head+trace), 5 and 6 its
        // two traces.
        assert!(rows[4].starts_with("NET"), "{rows:?}");
        assert_eq!(inked(&rows[5]), 0, "the download trace should be empty: {:?}", rows[5]);
        assert_eq!(inked(&rows[6]), 0, "the upload trace should be empty: {:?}", rows[6]);
    }

    /// Braille cells in a trace row that actually have a dot lit. U+2800 is a
    /// *blank* braille cell, so a row of them is a row of nothing — and it does
    /// not trim away like whitespace, which is what makes counting necessary.
    fn inked(row: &str) -> usize {
        row.chars().filter(|c| ('\u{2801}'..='\u{28FF}').contains(c)).count()
    }

    /// The other half of the same bug: a real download running under a heavier
    /// upload used to collapse onto the identical baseline dot. Each direction
    /// now gets a row of its own, and the busy one has to show texture the quiet
    /// one does not.
    #[test]
    fn a_download_under_a_bigger_upload_is_still_legible() {
        let rx: Vec<f32> = (0..60).map(|i| 52_000.0 + (i % 5) as f32 * 3_000.0).collect();
        let tx: Vec<f32> = (0..60).map(|i| 420_000.0 + (i % 11) as f32 * 15_000.0).collect();
        let sys = SysDto { net: vec![net("eth0", &rx, &tx)], ..Default::default() };
        let (rows, b) = system_rows(&sys, 9);
        let (dn, up) = (&rows[5], &rows[6]);
        assert!(dn.starts_with('↓') && up.starts_with('↑'), "{rows:?}");
        // Both directions have ink across the whole window — the download is
        // 10% of the shared peak, which is exactly where the old two-level
        // mirror rounded it to nothing and floored it back onto the baseline.
        assert_eq!(inked(dn), LEFT_W as usize - 3, "the download vanished: {dn:?}");
        assert_eq!(inked(up), LEFT_W as usize - 3, "the upload vanished: {up:?}");
        // ... and they are drawn differently, which two dot rows per direction
        // could not manage at this ratio.
        assert_ne!(dn, up, "the two directions came out identical");
        // Direction is also carried by colour, so the pair must not share one.
        let t = Theme::default();
        assert_eq!(b.cell((0, 5)).unwrap().fg, t.info, "the ↓ row is not `info`");
        assert_eq!(b.cell((0, 6)).unwrap().fg, t.accent, "the ↑ row is not `accent`");
        assert_ne!(t.info, t.accent, "the palette cannot tell the directions apart");
    }

    /// The network gauge is three rows and the others are two, so whatever
    /// follows it has to start three rows down. Getting this wrong is what
    /// opened the wrong monitor when the section was indexed by row pairs.
    #[test]
    fn the_net_gauge_is_a_row_taller_than_the_rest() {
        let sys = SysDto {
            gpus: vec![butai_protocol::api::GpuDto {
                pct: 7.0,
                mem_used_gb: 4.0,
                mem_total_gb: 12.0,
                hist: vec![7.0; 60],
                name: "RTX 4070".into(),
                temp_c: Some(48.0),
                power_w: Some(31.0),
            }],
            net: vec![net("eth0", &[9e6; 60], &[2e5; 60])],
            ..Default::default()
        };
        assert_eq!(gauge_height(Gauge::Net(0)), GAUGE_H + 1);
        // cpu 2 + ram 2 + gpu 2 + net 3.
        let gs = system_gauges(&sys, &NetSelect::default(), &DiskSelect::default());
        assert_eq!(system_rows_used(&gs), 9);
        assert_eq!(system_h_wanted(&gs), 10, "plus the separator row");
        let (rows, _) = system_rows(&sys, 12);
        assert!(rows[6].starts_with("NET"), "NET should follow the GPU: {rows:?}");
        assert!(rows[7].starts_with('↓') && rows[8].starts_with('↑'), "{rows:?}");
    }

    /// A section too short for the whole gauge draws none of it, rather than a
    /// label stranded above a missing trace or one direction without the other.
    #[test]
    fn a_gauge_that_does_not_fit_is_not_half_drawn() {
        let sys = SysDto { net: vec![net("eth0", &[9e6; 60], &[2e5; 60])], ..Default::default() };
        // Four rows holds cpu and ram exactly; NET needs three more.
        let (rows, _) = system_rows(&sys, 6);
        assert!(rows[4].is_empty() && rows[5].is_empty(), "NET was half-drawn: {rows:?}");
        let (rows, _) = system_rows(&sys, 7);
        assert!(rows[4].starts_with("NET"), "NET should fit in seven rows: {rows:?}");
    }

    /// The gap this closes: the daemon has published `disks` since it learned to
    /// read the mount table, and no client drew a single one of them.
    ///
    /// One row, because a disk is a level and not a series — so the assertion
    /// that matters is not "there is a DSK row" but "two disks are two rows".
    #[test]
    fn a_disk_is_one_row_naming_its_mount_and_its_capacity() {
        let sys = SysDto {
            disks: vec![disk("/media/fast", 898.7, 915.8), disk("/", 202.0, 215.4)],
            ..Default::default()
        };
        assert_eq!(gauge_height(Gauge::Disk(0)), 1, "a level has no trend to trace");
        let gs = system_gauges(&sys, &NetSelect::default(), &DiskSelect::default());
        // cpu 2 + ram 2 + a row each.
        assert_eq!(system_rows_used(&gs), 6);
        assert_eq!(system_h_wanted(&gs), 7, "plus the separator row");

        let (rows, _) = system_rows(&sys, 8);
        assert!(rows[4].starts_with("DSK "), "the disks follow cpu and ram: {rows:?}");
        assert!(rows[4].contains("/media/fast"), "{rows:?}");
        assert!(rows[4].ends_with("899/916G"), "used/total, right-aligned: {rows:?}");
        assert!(rows[5].starts_with("DSK ") && rows[5].contains('/'), "{rows:?}");
        assert!(rows[5].ends_with("202/215G"), "{rows:?}");
        // The row below the last disk is the section's own padding, not a trace.
        assert!(rows[6].is_empty(), "a disk drew a second row: {rows:?}");
    }

    /// Two things a 26-cell rail forces, in the one row that has to hold both.
    ///
    /// `3564/3667G` is ten cells spent on four digits nobody reads, and
    /// `/media/archive` is a cell longer than the room a mount gets — cut from
    /// the right, as every other identity is, it would read `/media/` and be
    /// indistinguishable from the disk beside it.
    #[test]
    fn a_big_disk_keeps_its_units_and_its_mount_keeps_its_tail() {
        let sys = SysDto {
            disks: vec![
                disk("/media/archive", 3564.4, 3667.4),
                disk("/media/fast-scratch", 100.0, 200.0),
            ],
            ..Default::default()
        };
        let (rows, _) = system_rows(&sys, 7);
        // What `df -h` prints for this filesystem, which is where anyone
        // will go to check it.
        assert!(rows[4].ends_with("3.5/3.6T"), "a 3.6 TiB disk in gigabytes: {rows:?}");
        assert!(rows[4].contains("…/archive"), "the tail is what names it: {rows:?}");
        assert!(rows[5].contains("scratch"), "{rows:?}");
        assert_ne!(
            rows[4].trim_end_matches(|c: char| c.is_ascii_digit() || "./GT".contains(c)),
            rows[5].trim_end_matches(|c: char| c.is_ascii_digit() || "./GT".contains(c)),
            "two mounts under one parent drew the same name: {rows:?}"
        );
    }

    /// The rail is not `df`. A docker host's mount table is mostly image layers
    /// and a workstation's is mostly tmpfs, and neither is a disk that fills.
    #[test]
    fn a_docker_host_draws_its_disks_and_not_its_layers() {
        let mut ds = vec![
            disk("/media/archive", 3300.0, 3667.0),
            disk("/media/fast", 853.0, 916.0),
            disk("/", 191.0, 215.0),
            DiskDto { kind: DiskKind::Memory, ..disk("/dev/shm", 0.0, 39.0) },
            DiskDto { kind: DiskKind::Network, ..disk("/mnt/nas", 4.0, 8.0) },
            disk("/home", 1.0, 2.0),
        ];
        // Thirty snaps and container layers, each 100% full by construction.
        ds.extend((0..30).map(|i| DiskDto {
            kind: DiskKind::Layer,
            ..disk(&format!("/snap/thing{i}"), 0.2, 0.2)
        }));
        let sys = SysDto { disks: ds, ..Default::default() };
        let drawn = |sel: &DiskSelect| -> Vec<String> {
            disk_mounts(&sys, sel).into_iter().map(|i| sys.disks[i].mount.clone()).collect()
        };

        // `all` is every real disk, largest first, capped — and `/home` is the
        // fourth, so the cap is doing something here rather than being spare.
        assert_eq!(drawn(&DiskSelect::Mode(DiskMode::All)), ["/media/archive", "/media/fast", "/"]);
        assert_eq!(drawn(&DiskSelect::Mode(DiskMode::All)).len(), DISK_GAUGE_MAX);
        // `auto` is the root filesystem, whatever its size.
        assert_eq!(drawn(&DiskSelect::Mode(DiskMode::Auto)), ["/"]);
        // A list is honoured literally and in the order given, tmpfs included,
        // and it is not capped: naming four is a request for four.
        assert_eq!(
            drawn(&DiskSelect::Named(vec![
                "/dev/shm".into(),
                "/".into(),
                "/home".into(),
                "/media/fast".into(),
            ])),
            ["/dev/shm", "/", "/home", "/media/fast"]
        );
        // A mount nothing matches is dropped rather than drawn empty.
        assert_eq!(drawn(&DiskSelect::Named(vec!["/nope".into()])), Vec::<String>::new());
        assert!(drawn(&DiskSelect::Named(vec![])).is_empty(), "an empty list draws none");
    }

    /// A mount that missed the sweep keeps its last reading, and the row says
    /// which of the two facts it is carrying.
    #[test]
    fn a_stale_mount_keeps_its_reading_without_raising_an_alarm() {
        let t = Theme::default();
        let sys = SysDto {
            disks: vec![DiskDto { stale: true, ..disk("/mnt/nas-backup", 99.0, 100.0) }],
            ..Default::default()
        };
        let (rows, b) = system_rows(&sys, 6);
        assert!(rows[4].ends_with("99/100G"), "the last reading is still drawn: {rows:?}");
        // 99% full and not red: the news is that nothing has answered for a
        // minute, not that this filesystem is about to stop the machine.
        let vx = (LEFT_W - 2) - "99/100G".chars().count() as u16;
        let fg = b.cell((vx, 4)).unwrap().fg;
        assert_ne!(fg, t.danger, "a stale reading was drawn as an alarm");
        assert_eq!(fg, t.faint, "{rows:?}");
    }

    /// A docker host is the case this exists for: the real links have to come
    /// out from behind the veths and bridges, the default route has to lead, and
    /// the rail must not grow a row per container.
    #[test]
    fn a_docker_host_draws_its_real_links_and_not_its_plumbing() {
        let mut ifs = vec![
            NetDto { default_route: false, ..net("lo", &[9e6; 8], &[9e6; 8]) },
            NetDto {
                kind: NetKind::Vpn,
                default_route: false,
                ..net("vpn-tunnel", &[3e5; 8], &[1e5; 8])
            },
            NetDto {
                kind: NetKind::Bridge,
                default_route: false,
                ..net("docker0", &[8e6; 8], &[8e6; 8])
            },
            net("enp1s0", &[1e3; 8], &[1e3; 8]),
        ];
        ifs[0].kind = NetKind::Loopback;
        // Twenty veths, as busy as the traffic they are carrying for.
        ifs.extend((0..20).map(|i| NetDto {
            kind: NetKind::Veth,
            default_route: false,
            ..net(&format!("veth{i:04x}"), &[5e6; 8], &[5e6; 8])
        }));
        let sys = SysDto { net: ifs, ..Default::default() };

        let drawn = |sel: &NetSelect| -> Vec<String> {
            net_ifaces(&sys, sel).into_iter().map(|i| sys.net[i].name.clone()).collect()
        };
        // `all` skips the double-counted kinds and leads with the default route
        // even though it is the quietest thing on the box.
        assert_eq!(drawn(&NetSelect::Mode(NetMode::All)), ["enp1s0", "vpn-tunnel"]);
        // `auto` is the old behaviour: one gauge, the default route.
        assert_eq!(drawn(&NetSelect::Mode(NetMode::Auto)), ["enp1s0"]);
        // A list is honoured literally and in the order given, bridge included.
        assert_eq!(
            drawn(&NetSelect::Named(vec!["docker0".into(), "enp1s0".into()])),
            ["docker0", "enp1s0"]
        );
        // A name nothing matches is dropped rather than drawn empty.
        assert_eq!(drawn(&NetSelect::Named(vec!["wlan9".into()])), Vec::<String>::new());
    }

    /// A link that has been silent for the whole window is not worth three rows.
    /// The default route is, busy or not — it is the way out either way.
    #[test]
    fn all_leaves_out_a_link_that_is_doing_nothing() {
        let quiet = NetDto {
            kind: NetKind::Vpn,
            default_route: false,
            ..net("tun0", &[0.0; 60], &[0.0; 60])
        };
        let route = net("enp1s0", &[0.0; 60], &[0.0; 60]);
        let sys = SysDto { net: vec![quiet.clone(), route], ..Default::default() };
        let drawn = |s: &SysDto| -> Vec<String> {
            net_ifaces(s, &NetSelect::Mode(NetMode::All))
                .into_iter()
                .map(|i| s.net[i].name.clone())
                .collect()
        };
        assert_eq!(drawn(&sys), ["enp1s0"], "an idle tunnel should not take three rows");

        // One burst anywhere in the window is enough to keep it, so a row does
        // not blink out between packets.
        let mut hist = vec![0.0; 60];
        hist[3] = 200_000.0;
        let busy = NetDto { rx_hist: hist, ..quiet.clone() };
        let sys =
            SysDto { net: vec![busy, net("enp1s0", &[0.0; 60], &[0.0; 60])], ..Default::default() };
        assert_eq!(drawn(&sys), ["enp1s0", "tun0"]);

        // Naming it explicitly still draws it: a request is not a discovery.
        let sys = SysDto { net: vec![quiet], ..Default::default() };
        assert_eq!(net_ifaces(&sys, &NetSelect::Named(vec!["tun0".into()])).len(), 1);
    }

    /// The cap is what keeps SYSTEM from eating the rail on a machine with an
    /// unusual number of real links.
    #[test]
    fn all_stops_at_the_cap_but_an_explicit_list_does_not() {
        let ifs: Vec<NetDto> = (0..6)
            .map(|i| NetDto {
                default_route: i == 0,
                ..net(&format!("eth{i}"), &[1e6; 8], &[1e6; 8])
            })
            .collect();
        let names: Vec<String> = ifs.iter().map(|n| n.name.clone()).collect();
        let sys = SysDto { net: ifs, ..Default::default() };
        assert_eq!(net_ifaces(&sys, &NetSelect::Mode(NetMode::All)).len(), NET_GAUGE_MAX);
        assert_eq!(net_ifaces(&sys, &NetSelect::Named(names)).len(), 6, "a list is a request");
    }

    /// One link is unnamed and several are named, because a name that is always
    /// there is a cell of noise on the machine that only has one interface.
    #[test]
    fn an_interface_is_named_only_when_there_is_more_than_one() {
        let one = SysDto { net: vec![net("enp1s0", &[9e6; 60], &[9e6; 60])], ..Default::default() };
        let (rows, _) = system_rows(&one, 9);
        assert!(rows[4].starts_with("NET"), "{rows:?}");
        assert!(!rows[4].contains("enp1s0"), "one link should not be named: {:?}", rows[4]);

        let two = SysDto {
            net: vec![
                net("enp1s0", &[9e6; 60], &[9e6; 60]),
                NetDto {
                    kind: NetKind::Vpn,
                    default_route: false,
                    speed_mbps: None,
                    ..net("vpn-tunnel", &[1e5; 60], &[1e5; 60])
                },
            ],
            ..Default::default()
        };
        let (rows, _) = system_rows(&two, 12);
        assert!(rows[4].contains("enp1s0"), "{:?}", rows[4]);
        assert!(rows[7].contains("vpn-tunnel"), "{:?}", rows[7]);
        // The link speed rides along when the rail has room for it.
        assert!(rows[4].contains("1G"), "the link speed is missing: {:?}", rows[4]);
    }

    /// The identity slot is the first thing to go, because the label says which
    /// gauge this is and the value is the reading — losing either would cost
    /// more than the name is worth.
    #[test]
    fn a_narrow_rail_drops_the_identity_before_the_reading() {
        let sys = SysDto {
            cpu_pct: 34.0,
            cpu_temp: Some(61.0),
            cpu_hist: vec![34.0; 60],
            cpu_model: Some("Ryzen 7 5700".into()),
            cpu_threads: Some(16),
            ..Default::default()
        };
        let wide = {
            let mut b = buf(40, 2);
            draw_system(&mut b, LRect::new(0, 0, 40, 2), &sys, &[Gauge::Cpu], &Theme::default());
            text_of(&b, 0)
        };
        assert!(wide.contains("Ryzen 7 5700 16T"), "{wide:?}");
        assert!(wide.ends_with("34% 61°"), "{wide:?}");

        let narrow = {
            let mut b = buf(14, 2);
            draw_system(&mut b, LRect::new(0, 0, 14, 2), &sys, &[Gauge::Cpu], &Theme::default());
            text_of(&b, 0)
        };
        assert!(narrow.starts_with("CPU"), "{narrow:?}");
        assert!(narrow.ends_with("34% 61°"), "the reading was cut: {narrow:?}");
        assert!(!narrow.contains("Ryzen"), "the name should have gone first: {narrow:?}");

        // The default rail is the interesting width: the model fits and the
        // thread count does not, so the count goes rather than being cut into
        // `Ryzen 7 5700 1` — which is what a live rail actually drew.
        let default_rail = {
            let mut b = buf(LEFT_W - 2, 2);
            draw_system(
                &mut b,
                LRect::new(0, 0, LEFT_W - 2, 2),
                &sys,
                &[Gauge::Cpu],
                &Theme::default(),
            );
            text_of(&b, 0)
        };
        assert_eq!(default_rail, "CPU Ryzen 7 5700   34% 61°");
    }

    /// Whole words or nothing — a truncated token reads as a fact and is not
    /// one.
    #[test]
    fn an_identity_is_elided_at_a_word_boundary() {
        assert_eq!(fit_words("Ryzen 7 5700 16T", 16), "Ryzen 7 5700 16T");
        assert_eq!(fit_words("Ryzen 7 5700 16T", 14), "Ryzen 7 5700");
        assert_eq!(fit_words("Ryzen 7 5700 16T", 12), "Ryzen 7 5700");
        assert_eq!(fit_words("Ryzen 7 5700 16T", 8), "Ryzen 7");
        // One word too long for the room leaves nothing, rather than a stump.
        assert_eq!(fit_words("Ryzen", 3), "");
        assert_eq!(fit_words("", 10), "");
    }

    /// A title too long for the fleet column scrolls, the way every rail row
    /// already does — an ellipsis there is a name you can never finish reading,
    /// and the column is narrow enough (~21 cells at 160) that most real agent
    /// titles hit it.
    ///
    /// Asserted as "the tail reaches the screen at some phase", because that is
    /// the property; the exact window at a given tick is `marquee`'s business
    /// and is covered by its own tests. Mutation-checked by putting `ellipsize`
    /// back — the tail never appears and this fails.
    #[test]
    fn a_long_fleet_title_scrolls_instead_of_ellipsizing() {
        let agents = [
            agent(1, "claude-refactoring-the-client-daemon-boundary", AgentState::Idle),
            agent(2, "codex", AgentState::Idle),
            agent(3, "aider", AgentState::Idle),
            agent(4, "gemini", AgentState::Idle),
        ];
        let all = booth_fleet(&agents);
        let sys = SysDto::default();
        let ms = machines(&sys, &all);
        let scene = Scene { machines: &ms, ..scene(&[], None, &sys, &all) };

        let mut seen = String::new();
        let mut ever_wanted_anim = false;
        for tick in 0..80 {
            let view =
                View { page: Page::Booth, focus: Focus::AllAgents, tick, ..Default::default() };
            let mut b = buf(160, 40);
            ever_wanted_anim |= draw(&mut b, 160, 40, &scene, &view, &Theme::default()).wants_anim;
            seen.push_str(&(0..40).map(|y| text_of(&b, y)).collect::<Vec<_>>().join("\n"));
            seen.push('\n');
        }

        assert!(seen.contains("boundary"), "the title's tail never scrolled into view");
        assert!(!seen.contains('…'), "a scrolling title should not also ellipsize:\n{seen}");
        // The clock has to be told, or the row scrolls only when something else
        // happens to repaint.
        assert!(ever_wanted_anim, "BOOTH never asked the slow clock to keep running");
    }

    /// BOOTH pins the agent's own spinner too — both in the fleet list and in
    /// the tray, which draw the same agent through two different code paths.
    ///
    /// The sprite says the same thing more loudly, so the glyph is kept rather
    /// than dropped only because it is the agent's word for its own state and
    /// the sprite is ours; what it must not do is march through the row.
    #[test]
    fn booth_pins_an_agents_own_spinner_in_both_columns() {
        let sys = SysDto::default();
        let agents = [
            agent(1, "✳ Wire the export watcher up to the daemon", AgentState::Waiting),
            agent(2, "codex", AgentState::Idle),
            agent(3, "aider", AgentState::Idle),
            agent(4, "gemini", AgentState::Idle),
        ];
        let all = booth_fleet(&agents);
        let ms = machines(&sys, &all);
        let scene = Scene { machines: &ms, ..scene(&[], None, &sys, &all) };

        // Only the fleet column: the stage box is titled with the same agent,
        // and a box title is ellipsized rather than scrolled, so its copy of
        // the glyph is not what this is about.
        let view = View { page: Page::Booth, focus: Focus::AllAgents, ..Default::default() };
        let fleet_w =
            booth_columns(booth_area(160, &page_geom(160, 40, &view))).fleet_box.width as usize;

        // Every column the glyph lands on, over enough ticks for the name to
        // have scrolled and wrapped: two rows draw this agent (the tray copy
        // and the fleet row) and both must hold still.
        let mut columns: BTreeSet<Vec<usize>> = BTreeSet::new();
        let mut moved = false;
        for tick in 0..80 {
            let view = View { tick, ..view.clone() };
            let mut b = buf(160, 40);
            draw(&mut b, 160, 40, &scene, &view, &Theme::default());
            let rows: Vec<String> =
                (0..40).map(|y| text_of(&b, y).chars().take(fleet_w).collect::<String>()).collect();
            let at: Vec<usize> =
                rows.iter().filter_map(|r| r.chars().position(|c| c == '✳')).collect();
            assert_eq!(at.len(), 2, "tray copy and fleet row, tick {tick}:\n{}", rows.join("\n"));
            moved |= !rows.iter().any(|r| r.contains("Wire the export"));
            columns.insert(at);
        }
        assert_eq!(columns.len(), 1, "the glyph changed column: {columns:?}");
        assert!(moved, "the name never scrolled, so this proves nothing about the glyph");
    }

    /// With nothing waiting the tray still holds its rows, and says so — and
    /// the fleet list below starts on the same screen row either way.
    ///
    /// This is the fixed-tray promise measured on the painted buffer rather
    /// than asserted about the geometry: shrink the tray to its contents and
    /// the second half of this fails, because every row below it moves up by
    /// two the moment nothing is waiting.
    #[test]
    fn an_empty_tray_keeps_its_space_and_answers_the_question() {
        let sys = SysDto::default();
        let view = View { page: Page::Booth, focus: Focus::AllAgents, ..Default::default() };

        // The row the fleet list's first workspace header lands on. `caliper`
        // is unique to the fleet column, so it cannot match the compute side.
        let first_space_row = |states: [AgentState; 4]| -> (usize, String) {
            let agents = [
                agent(1, "claude", states[0]),
                agent(2, "codex", states[1]),
                agent(3, "aider", states[2]),
                agent(4, "gemini", states[3]),
            ];
            let all = booth_fleet(&agents);
            let ms = machines(&sys, &all);
            let mut b = buf(160, 40);
            let scene = Scene { machines: &ms, ..scene(&[], None, &sys, &all) };
            draw(&mut b, 160, 40, &scene, &view, &Theme::default());
            let rows: Vec<String> = (0..40).map(|y| text_of(&b, y)).collect();
            let y =
                rows.iter().position(|r| r.contains("caliper")).expect("caliper header on screen");
            (y, rows.join("\n"))
        };

        use AgentState::*;
        let (calm_y, calm) = first_space_row([Idle, Idle, Working, Idle]);
        assert!(calm.contains("nothing needs you"), "{calm}");

        let (busy_y, busy) = first_space_row([Idle, Waiting, Working, Waiting]);
        assert!(busy.contains("NEEDS YOU (2)"), "{busy}");
        assert_eq!(calm_y, busy_y, "the fleet list moved when the tray filled up");
    }

    /// The columns `start..end` of a painted row, as text.
    ///
    /// Not `&row[start..end]`: a span is in *cells* and a `String` indexes in
    /// bytes, and the two stopped agreeing the moment the tab bar grew a `│`.
    /// Every assertion that read a span off the bar was silently a byte slice.
    fn cells(row: &str, start: u16, end: u16) -> String {
        row.chars().skip(start as usize).take((end - start) as usize).collect()
    }

    /// Which cell `needle` starts at, counting cells rather than bytes.
    fn cell_of(row: &str, needle: &str) -> Option<usize> {
        let at = row.find(needle)?;
        Some(row[..at].chars().count())
    }

    fn ws_summary(name: &str) -> WorkspaceSummary {
        WorkspaceSummary {
            id: SessionId(1),
            name: name.into(),
            cwd: "/tmp".into(),
            agents: 0,
            waiting: 0,
            working: 0,
            finished: 0,
            questions: 0,
            exited: 0,
            unread: 0,
            processes: 0,
            changes: 0,
            conflicts: 0,
            repo_state: RepoState::Clean,
            attached_clients: 1,
        }
    }

    /// Chrome plus the modal layer, i.e. what a caller draws when there is no
    /// pane to blit between them.
    fn draw_all(
        b: &mut Buffer,
        cols: u16,
        rows: u16,
        scene: &Scene<'_>,
        view: &View,
        theme: &Theme,
    ) -> Painted {
        let out = draw(b, cols, rows, scene, view, theme);
        draw_overlay_layer(b, cols, rows, view, theme);
        out
    }

    fn buf(cols: u16, rows: u16) -> Buffer {
        Buffer::empty(Rect::new(0, 0, cols, rows))
    }

    fn text_of(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width)
            .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    fn agent(pane: u64, title: &str, state: AgentState) -> AgentDto {
        AgentDto {
            pane: PaneId(pane),
            title: title.into(),
            state,
            exited: None,
            question: false,
            started_ms: now_ms(),
            working_since_ms: None,
            unread: false,
        }
    }

    /// An agent holding news you have not read. `code` is its exit status —
    /// `None` for a finished turn, `Some` for a corpse.
    fn unread_agent(pane: u64, title: &str, code: Option<u32>) -> AgentDto {
        let state = if code.is_some() { AgentState::Exited } else { AgentState::Finished };
        AgentDto { exited: code, unread: true, ..agent(pane, title, state) }
    }

    fn detail(agents: Vec<AgentDto>, changes: Option<ChangesDto>) -> WorkspaceDetail {
        WorkspaceDetail {
            id: SessionId(1),
            name: "proj".into(),
            cwd: "/tmp/proj".into(),
            agents,
            processes: vec![],
            changes,
            stage: None,
        }
    }

    fn changes(ahead: usize, behind: usize, state: RepoState) -> ChangesDto {
        ChangesDto {
            branch: "main".into(),
            staged: vec![],
            unstaged: vec![FileChange {
                path: "a.rs".into(),
                code: "M".into(),
                added: 1,
                deleted: 0,
            }],
            recent_commits: vec![],
            conflicted: vec![],
            upstream: Some("origin/main".into()),
            ahead,
            behind,
            state,
            detached: false,
        }
    }

    #[test]
    fn the_stage_is_the_screen_minus_the_rails_and_borders() {
        // The one measurement that crosses the wire: the daemon sizes the pane
        // it streams to exactly this, so it has to be the box interior.
        let view = View::default();
        let r = stage_rect(120, 40, &view);
        let geom = Geom::compute(120, 40, false, view.geom, system_h_wanted(&view.gauges));
        assert_eq!(r.width, geom.stage_box.width - 2);
        assert_eq!(r.height, geom.stage_box.height - 2);
        assert_eq!(r.x, geom.stage_box.x + 1);
    }

    #[test]
    fn a_narrow_terminal_drops_the_rails_and_gives_the_stage_the_screen() {
        let r = stage_rect(60, 24, &View::default());
        assert_eq!(r.width, 58, "rails should have yielded the whole width");
    }

    #[test]
    fn agent_rows_carry_their_status_token() {
        let mut b = buf(120, 40);
        let ws = detail(
            vec![agent(1, "claude", AgentState::Waiting), agent(2, "codex", AgentState::Idle)],
            None,
        );
        draw(
            &mut b,
            120,
            40,
            &scene(&[], Some(&ws), &SysDto::default(), &[]),
            &View::default(),
            &Theme::default(),
        );
        let joined: String = (0..40).map(|y| text_of(&b, y)).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("claude"), "{joined}");
        assert!(joined.contains("WAIT"), "a waiting agent should say so:\n{joined}");
        assert!(joined.contains("idle"), "{joined}");
    }

    /// An agent's own spinner holds one column while the name scrolls past it.
    ///
    /// Claude Code rewrites its OSC title every frame — `◐ Fix the parser`,
    /// `◑ Fix the parser` — and that title *is* the agent's name here. Fed to
    /// the marquee whole, the glyph set off across the row with the text and
    /// came round again a second later, which is what this was reported as: a
    /// list scrolling "with the star at the beginning".
    #[test]
    fn an_agents_own_spinner_does_not_travel_with_its_name() {
        let title = "◐ Fix scrolling behaviour in agents and files";
        let ws = detail(vec![agent(1, title, AgentState::Working)], None);
        let rows = page_geom(120, 40, &View::default()).agents_rows;

        let at = |tick: u64| -> (Option<u16>, String) {
            let view = View { tick, ..View::default() };
            let mut b = buf(120, 40);
            draw(
                &mut b,
                120,
                40,
                &scene(&[], Some(&ws), &SysDto::default(), &[]),
                &view,
                &Theme::default(),
            );
            let row = text_of(&b, rows.y);
            (row.chars().position(|c| c == '◐').map(|c| c as u16), row)
        };

        let (home, first) = at(0);
        assert!(home.is_some(), "the glyph should be on the row at all:\n{first}");
        assert!(first.contains("Fix scrolling"), "the name should start at the start:\n{first}");

        // Long enough for the marquee to have left the hold and wrapped twice.
        let mut moved = false;
        for tick in 1..80 {
            let (col, row) = at(tick);
            assert_eq!(col, home, "the glyph moved at tick {tick}:\n{row}");
            moved |= !row.contains("Fix scrolling");
        }
        assert!(moved, "the name never scrolled, so this proves nothing about the glyph");
    }

    /// More agents than the section has rows, and the cursor still gets to all
    /// of them.
    ///
    /// The rail used to draw `agents.iter().take(height)` and stop: every agent
    /// past the fold was undrawable, and since `move_sel` walks the whole list
    /// the cursor went there anyway — off screen, with the highlight nowhere and
    /// `j` looking dead. Reported as "I can't scroll my agents", and that is
    /// exactly what it was.
    #[test]
    fn a_rail_longer_than_its_section_scrolls_to_the_cursor() {
        let agents: Vec<AgentDto> =
            (0..30).map(|i| agent(i, &format!("agent-{i:02}"), AgentState::Idle)).collect();
        let ws = detail(agents, None);
        let view = View::default();
        let rows = page_geom(120, 40, &view).agents_rows;
        assert!(
            (rows.height as usize) < 30,
            "{} rows fits all 30 agents, so this proves nothing",
            rows.height
        );

        let rail = |sel: usize| -> String {
            let view = View { agent_sel: sel, focus: Focus::Agents, ..View::default() };
            let mut b = buf(120, 40);
            draw(
                &mut b,
                120,
                40,
                &scene(&[], Some(&ws), &SysDto::default(), &[]),
                &view,
                &Theme::default(),
            );
            (0..rows.height).map(|i| text_of(&b, rows.y + i)).collect::<Vec<_>>().join("\n")
        };

        // At the top the list is unscrolled, and the last agent is over the fold.
        let top = rail(0);
        assert!(top.contains("agent-00"), "{top}");
        assert!(!top.contains("agent-29"), "the list should not fit yet:\n{top}");

        // On the last agent, it is on screen — and the first has scrolled away,
        // which is the half a `take()` could never fail.
        let bottom = rail(29);
        assert!(bottom.contains("agent-29"), "the cursor's own row is off screen:\n{bottom}");
        assert!(!bottom.contains("agent-00"), "the list did not scroll:\n{bottom}");

        // And it scrolls only far enough: the cursor is the *last* visible row,
        // not a recentred one, so the rows above it stay where the eye left them.
        let lines: Vec<&str> = bottom.lines().collect();
        assert!(lines[rows.height as usize - 1].contains("agent-29"), "{bottom}");
    }

    /// A cursor left past the end of a list that has shrunk — kill four agents
    /// and nothing walks it back — scrolls to the end of what is there rather
    /// than off the bottom of it. Without the clamp the last rows come up blank.
    #[test]
    fn a_stale_cursor_does_not_scroll_a_rail_off_its_own_end() {
        let view = View { agent_sel: 40, focus: Focus::Agents, ..View::default() };
        let rows = page_geom(120, 40, &view).agents_rows;
        let agents: Vec<AgentDto> = (0..rows.height as u64 + 3)
            .map(|i| agent(i, &format!("agent-{i:02}"), AgentState::Idle))
            .collect();
        let last = format!("agent-{:02}", agents.len() - 1);
        assert!(view.agent_sel >= agents.len(), "the cursor has to be past the end to be stale");
        let ws = detail(agents, None);
        let mut b = buf(120, 40);
        draw(
            &mut b,
            120,
            40,
            &scene(&[], Some(&ws), &SysDto::default(), &[]),
            &view,
            &Theme::default(),
        );
        let lines: Vec<String> = (0..rows.height).map(|i| text_of(&b, rows.y + i)).collect();
        assert!(
            lines.join("\n").contains(&last),
            "the end of the list should be shown:\n{lines:?}"
        );
        assert!(
            lines.iter().all(|l| l.contains("agent-")),
            "a clamped scroll leaves no blank rows:\n{lines:?}"
        );
    }

    /// The CHANGES rail scrolls on the same rule, and the rows it counts are the
    /// built ones — a heading is a row the cursor sits on, so a scroll measured
    /// in files alone would be short by one per section.
    #[test]
    fn the_changes_rail_scrolls_past_the_bottom_of_the_right_rail() {
        let view = View::default();
        let (list_h, _) = changes_split(&page_geom(120, 40, &view));
        let mut c = changes(0, 0, RepoState::Clean);
        // A heading plus this many files, comfortably past the rail.
        c.unstaged = (0..list_h as usize + 10)
            .map(|i| FileChange {
                path: format!("file-{i:02}.rs"),
                code: "M".into(),
                added: 1,
                deleted: 0,
            })
            .collect();
        let last = format!("file-{}.rs", c.unstaged.len() - 1);
        let rows = change_rows(&c).len();
        let ws = detail(vec![], Some(c));

        let rail = |sel: usize| -> String {
            let view = View { changes_sel: sel, focus: Focus::Changes, ..View::default() };
            let mut b = buf(120, 40);
            draw(
                &mut b,
                120,
                40,
                &scene(&[], Some(&ws), &SysDto::default(), &[]),
                &view,
                &Theme::default(),
            );
            let area = page_geom(120, 40, &view).changes_rows;
            (0..list_h).map(|i| text_of(&b, area.y + i)).collect::<Vec<_>>().join("\n")
        };

        let top = rail(0);
        assert!(top.contains("Unstaged"), "the heading is the first row:\n{top}");
        assert!(!top.contains(&last), "the list should not fit:\n{top}");

        let bottom = rail(rows - 1);
        assert!(bottom.contains(&last), "the last file is unreachable:\n{bottom}");
        assert!(!bottom.contains("Unstaged"), "the heading should have scrolled away:\n{bottom}");
    }

    #[test]
    fn the_changes_label_carries_the_branch_divergence_and_sequence_state() {
        assert_eq!(
            changes_label(&changes(2, 1, RepoState::Clean), 38),
            " CHANGES (1) · main ↑2↓1 "
        );
        // Mid-sequence the divergence is not the actionable fact.
        assert_eq!(
            changes_label(&changes(2, 1, RepoState::Rebase), 38),
            " CHANGES (1) · main · REBASING "
        );
        assert_eq!(changes_label(&changes(0, 0, RepoState::Clean), 38), " CHANGES (1) · main ");
    }

    /// The branch is the only unbounded part of the title, so it is the part
    /// that gives way — letting `draw_box` cut the label would have taken the
    /// arrows, which are the half that says whether you can push.
    #[test]
    fn a_long_branch_yields_before_the_divergence_does() {
        let long = |width| {
            let mut c = changes(2, 1, RepoState::Clean);
            c.branch = "refactor/client-daemon-boundary".into();
            changes_label(&c, width)
        };
        let label = long(38);
        assert!(label.ends_with("↑2↓1 "), "the divergence should have survived: {label}");
        assert!(label.contains("refactor/"), "as much branch as fits, not none: {label}");
        assert_eq!(label.chars().count(), 34, "wider than the box will draw: {label}");
        // Narrow enough that no useful part of the name would get through, so
        // the title says nothing about the branch rather than saying `ref…`.
        assert_eq!(long(24), " CHANGES (1) ↑2↓1 ");
    }

    #[test]
    fn the_changes_rail_lists_its_files() {
        let mut b = buf(120, 40);
        let ws = detail(vec![], Some(changes(0, 0, RepoState::Clean)));
        draw(
            &mut b,
            120,
            40,
            &scene(&[], Some(&ws), &SysDto::default(), &[]),
            &View::default(),
            &Theme::default(),
        );
        let joined: String = (0..40).map(|y| text_of(&b, y)).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("Unstaged"), "{joined}");
        assert!(joined.contains("M a.rs"), "{joined}");
        assert!(joined.contains("+1 -0"), "{joined}");
    }

    /// A path too long for the rail marquees — and the status code stays where it
    /// is while it does. The code is what happened to the file, and one that
    /// slides off the left edge leaves a scrolling row that no longer says
    /// whether it is modified, added or untracked. Only the path is too long to
    /// fit, so only the path is what moves.
    #[test]
    fn a_long_path_scrolls_under_a_pinned_status_code() {
        let mut c = changes(0, 0, RepoState::Clean);
        c.unstaged[0].path = "crates/butai-client/src/chrome/an-extremely-long-name.rs".into();
        let ws = detail(vec![], Some(c));
        // Sliced to the rail's own columns: the left rail draws SYSTEM gauges on
        // these same rows, so a whole-screen row is two lists side by side.
        let rail = |b: &Buffer, y: u16, area: LRect| -> String {
            (area.x..area.x + area.width)
                .filter_map(|x| b.cell((x, y)).map(|c| c.symbol().to_string()))
                .collect::<String>()
        };
        let row_at = |tick: u64| -> String {
            let view = View { tick, ..View::default() };
            let mut b = buf(120, 40);
            draw(
                &mut b,
                120,
                40,
                &scene(&[], Some(&ws), &SysDto::default(), &[]),
                &view,
                &Theme::default(),
            );
            let area = page_geom(120, 40, &view).changes_rows;
            let heading = (0..area.height)
                .find(|i| rail(&b, area.y + i, area).contains("Unstaged"))
                .expect("no Unstaged heading");
            rail(&b, area.y + heading + 1, area)
        };
        // `marquee` holds for three ticks before it starts, so these are one
        // frame before the move and several after it.
        let held = row_at(0);
        let moved = row_at(9);
        assert_ne!(held, moved, "the path should have scrolled by now:\n{held}\n{moved}");
        for (tick, row) in [(0, &held), (9, &moved)] {
            assert!(
                row.starts_with("  M "),
                "tick {tick}: the code should still lead the row: {row:?}"
            );
            assert!(row.trim_end().ends_with("+1 -0"), "tick {tick}: lost the stat: {row:?}");
        }
    }

    /// A conflict is what is blocking you, so the rail leads with it — above
    /// the unstaged work, and never *as* unstaged work, which would offer `s`
    /// on a file that cannot be staged. The box label says what git is in the
    /// middle of, so the state is readable without opening anything.
    #[test]
    fn a_conflict_leads_the_changes_rail_and_is_not_listed_as_unstaged() {
        let mut c = changes(0, 0, RepoState::Merge);
        c.conflicted = vec![butai_protocol::api::ConflictFile {
            path: "conflict.txt".into(),
            base: true,
            ours: true,
            theirs: true,
        }];
        let ws = detail(vec![], Some(c.clone()));
        let mut b = buf(120, 40);
        draw(
            &mut b,
            120,
            40,
            &scene(&[], Some(&ws), &SysDto::default(), &[]),
            &View::default(),
            &Theme::default(),
        );
        let joined: String = (0..40).map(|y| text_of(&b, y)).collect::<Vec<_>>().join("\n");
        let conflicts = joined.find("Conflicts").expect("no Conflicts heading");
        let unstaged = joined.find("Unstaged").expect("no Unstaged heading");
        assert!(conflicts < unstaged, "conflicts should lead:\n{joined}");
        assert!(joined.contains("conflict.txt"), "{joined}");
        assert!(
            joined.contains("MERGING"),
            "the label should say what git is mid-way through:\n{joined}"
        );

        // And the row model agrees, which is what the verbs are chosen from.
        let rows = change_rows(&c);
        assert!(matches!(rows[0], ChangeRow::Header("Conflicts")), "{rows:?}");
        assert!(matches!(rows[1], ChangeRow::Conflicted { path } if path == "conflict.txt"));
        assert!(
            !rows.iter().any(
                |r| matches!(r, ChangeRow::File { change, .. } if change.path == "conflict.txt")
            ),
            "a conflicted file must not also be an unstaged row: {rows:?}"
        );
    }

    /// The bug the daemon's footer has today: below ~88 columns its three zones
    /// are each bounded only by the screen edge, so they overwrite each other.
    #[test]
    fn the_footer_zones_do_not_collide_at_any_width() {
        for cols in [60u16, 72, 80, 88, 96, 104, 120] {
            let mut b = buf(cols, 24);
            let ws = detail(vec![agent(1, "codex", AgentState::Waiting)], None);
            draw(
                &mut b,
                cols,
                24,
                &scene(&[], Some(&ws), &SysDto::default(), &[]),
                &View::default(),
                &Theme::default(),
            );
            let footer = text_of(&b, 23);
            assert!(footer.chars().count() <= cols as usize, "footer overran {cols}: {footer:?}");
            // Whatever fits, the workspace name must never run into the notice.
            if let Some(dot) = footer.chars().position(|c| c == '●') {
                let left: String = footer.chars().take(dot).collect();
                assert!(
                    left.trim_end().chars().count() < dot,
                    "left zone touched the notice at {cols}: {footer:?}"
                );
            }
        }
    }

    /// The footer names the branch, not only the CHANGES label.
    ///
    /// Which branch you are on is the thing to be sure of before an agent
    /// starts writing, and the rail can be scrolled, narrow, or shut — so it
    /// goes in the one row that is always there. Found missing by running the
    /// workbench: the daemon's footer had it and the port dropped it.
    /// The footer's path has to say *which machine* it is on once there is more
    /// than one, or it names a directory that exists on all of them.
    ///
    /// Read off the painted row, not built from the same pieces the drawing
    /// used, because agreeing with itself would prove nothing about what is on
    /// screen.
    #[test]
    fn the_footer_path_names_its_machine_when_there_is_more_than_one() {
        let ws = detail(vec![], Some(changes(0, 0, RepoState::Clean)));
        let summary = ws_summary("proj");

        // One machine: nothing to qualify, and the bare path stays bare. This
        // half is the regression guard — qualifying always would put a hostname
        // in front of every single-machine user's footer.
        let local = [Tab { summary: &summary, host: None, live: true }];
        let mut b = buf(120, 24);
        draw(
            &mut b,
            120,
            24,
            &scene(&local, Some(&ws), &SysDto::default(), &[]),
            &View::default(),
            &Theme::default(),
        );
        let footer = text_of(&b, 23);
        assert!(footer.contains(" /tmp/proj"), "{footer:?}");
        assert!(!footer.contains(":/tmp/proj"), "a lone machine needs no qualifying: {footer:?}");

        // Two machines, showing the remote one: the path says where it is, in
        // scp's spelling.
        let remote = [Tab { summary: &summary, host: Some("gpu-box"), live: true }];
        let mut b = buf(120, 24);
        draw(
            &mut b,
            120,
            24,
            &scene(&remote, Some(&ws), &SysDto::default(), &[]),
            &View::default(),
            &Theme::default(),
        );
        let footer = text_of(&b, 23);
        assert!(
            footer.contains("gpu-box:/tmp/proj"),
            "the footer must name the machine its path is on: {footer:?}"
        );
        assert!(footer.contains("(main)"), "and keep the branch: {footer:?}");
    }

    #[test]
    fn the_footer_says_which_branch_you_are_on() {
        let ws = detail(vec![], Some(changes(0, 0, RepoState::Clean)));
        let mut b = buf(120, 24);
        draw(
            &mut b,
            120,
            24,
            &scene(&[], Some(&ws), &SysDto::default(), &[]),
            &View::default(),
            &Theme::default(),
        );
        assert!(text_of(&b, 23).contains("(main)"), "{:?}", text_of(&b, 23));

        // A workspace outside a repository has no branch to name, and must not
        // grow an empty pair of brackets.
        let bare = detail(vec![], None);
        let mut b = buf(120, 24);
        draw(
            &mut b,
            120,
            24,
            &scene(&[], Some(&bare), &SysDto::default(), &[]),
            &View::default(),
            &Theme::default(),
        );
        assert!(!text_of(&b, 23).contains('('), "{:?}", text_of(&b, 23));
    }

    /// A long message is cut down, not thrown away.
    ///
    /// Found live: `ssh` failing to resolve a host produces a ~150-character
    /// error, and the footer took the same branch as a phone-width screen and
    /// showed a lone `●` — which said something had happened and nothing about
    /// what. Only a genuinely tiny middle zone degrades that far now.
    #[test]
    fn a_long_notice_is_shortened_rather_than_swallowed() {
        let long = "nosuchhost.invalid: forward failed: ssh: Could not resolve \
                    hostname nosuchhost.invalid: Name or service not known";
        let view = View { flash: Some(long.to_string()), ..Default::default() };
        let mut b = buf(160, 24);
        draw(&mut b, 160, 24, &scene(&[], None, &SysDto::default(), &[]), &view, &Theme::default());
        let footer = text_of(&b, 23);
        assert!(footer.contains("Could not resolve"), "the message was swallowed: {footer:?}");
        assert!(footer.chars().count() <= 160, "footer overran: {footer:?}");

        // At every width it either says something or shows the marker, and
        // never overruns.
        for cols in [40u16, 60, 80, 100, 120, 160, 200] {
            let mut b = buf(cols, 24);
            draw(
                &mut b,
                cols,
                24,
                &scene(&[], None, &SysDto::default(), &[]),
                &view,
                &Theme::default(),
            );
            let footer = text_of(&b, 23);
            assert!(footer.chars().count() <= cols as usize, "overran at {cols}: {footer:?}");
            assert!(
                footer.contains('●') || footer.contains("nosuchhost"),
                "said nothing at {cols}: {footer:?}"
            );
        }
    }

    #[test]
    fn zen_collapses_the_rails_to_status_markers() {
        let mut b = buf(120, 24);
        let ws = detail(
            vec![agent(1, "claude", AgentState::Waiting), agent(2, "codex", AgentState::Working)],
            Some(changes(0, 0, RepoState::Clean)),
        );
        let view = View { zen: true, ..Default::default() };
        draw(
            &mut b,
            120,
            24,
            &scene(&[], Some(&ws), &SysDto::default(), &[]),
            &view,
            &Theme::default(),
        );
        let joined: String = (0..24).map(|y| text_of(&b, y)).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("A!"), "a waiting agent should mark the strip:\n{joined}");
        assert!(joined.contains("A~"), "a working agent should too:\n{joined}");
        assert!(joined.contains("C1"), "the change count should survive zen:\n{joined}");
        // Zen is four columns a side, so the stage takes almost everything.
        let stage = stage_rect(120, 24, &view);
        assert_eq!(stage.width, 120 - 4 - 4 - 2);
    }

    #[test]
    fn an_overlay_covers_what_is_behind_it() {
        // A modal that lets the rails show through is unreadable over a busy
        // stage, so it must clear its own rectangle.
        let mut b = buf(100, 30);
        let ws = detail(vec![agent(1, "claude", AgentState::Idle)], None);
        let plain = View::default();
        draw_all(
            &mut b,
            100,
            30,
            &scene(&[], Some(&ws), &SysDto::default(), &[]),
            &plain,
            &Theme::default(),
        );
        let before: String = (0..30).map(|y| text_of(&b, y)).collect::<Vec<_>>().join("\n");
        assert!(before.contains("claude"));

        let view = View {
            overlay: Some(Overlay::List(ListOverlay {
                title: "AGENTS".into(),
                items: vec!["codex".into(), "gemini".into()],
                values: None,
                sel: 0,
                kind: ListKind::SpawnAgent,
            })),
            ..Default::default()
        };
        draw_all(
            &mut b,
            100,
            30,
            &scene(&[], Some(&ws), &SysDto::default(), &[]),
            &view,
            &Theme::default(),
        );
        let after: String = (0..30).map(|y| text_of(&b, y)).collect::<Vec<_>>().join("\n");
        assert!(after.contains("AGENTS"), "{after}");
        assert!(after.contains("gemini"), "the modal should list its items:\n{after}");
    }

    #[test]
    fn an_overlay_never_overruns_a_small_screen() {
        for (cols, rows) in [(40u16, 12u16), (60, 20), (100, 30), (200, 60)] {
            let mut b = buf(cols, rows);
            let view = View {
                overlay: Some(Overlay::Confirm(ConfirmOverlay {
                    title: "DISCARD".into(),
                    header: "M src/main.rs  +12 -3".into(),
                    yes: false,
                    kind: ConfirmKind::Discard { path: "src/main.rs".into() },
                })),
                ..Default::default()
            };
            draw_all(
                &mut b,
                cols,
                rows,
                &scene(&[], None, &SysDto::default(), &[]),
                &view,
                &Theme::default(),
            );
            for y in 0..rows {
                assert!(
                    text_of(&b, y).chars().count() <= cols as usize,
                    "overlay overran {cols}x{rows} on row {y}"
                );
            }
        }
    }

    #[test]
    fn a_list_overlay_marks_and_moves_its_cursor() {
        let mut list = ListOverlay {
            title: "SPAWN AGENT".into(),
            items: vec!["claude".into(), "codex".into(), "aider".into()],
            values: None,
            sel: 0,
            kind: ListKind::SpawnAgent,
        };
        list.move_sel(-1);
        assert_eq!(list.sel, 0, "should not walk off the top");
        list.move_sel(1);
        assert_eq!(list.chosen(), Some("codex"));
        for _ in 0..10 {
            list.move_sel(1);
        }
        assert_eq!(list.chosen(), Some("aider"), "should not walk off the end");

        let mut b = buf(80, 24);
        let view = View { overlay: Some(Overlay::List(list)), ..Default::default() };
        draw_all(
            &mut b,
            80,
            24,
            &scene(&[], None, &SysDto::default(), &[]),
            &view,
            &Theme::default(),
        );
        let joined: String = (0..24).map(|y| text_of(&b, y)).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("SPAWN AGENT"), "{joined}");
        assert!(joined.contains("claude") && joined.contains("aider"), "{joined}");
    }

    /// Wide enough for the labelled rail, given the default rails.
    const WIDE: u16 = 200;

    /// The spaces are one control on the bar, and it says which one you are on.
    ///
    /// Read off the painted buffer rather than from [`spaces_button_span`], for
    /// the same reason the chip test is: a span and a drawing that agree with
    /// each other prove nothing about where the control actually landed.
    #[test]
    fn the_tab_bar_carries_one_spaces_button_and_names_the_one_you_are_on() {
        let sys = SysDto::default();
        for page in Page::ORDER {
            let view = View { page, ..Default::default() };
            let mut b = buf(WIDE, 30);
            draw(&mut b, WIDE, 30, &scene(&[], None, &sys, &[]), &view, &Theme::default());
            let bar = text_of(&b, 0);
            let geom = Geom::compute(WIDE, 30, false, view.geom, system_h_wanted(&view.gauges));
            let (start, end) =
                spaces_button_span(&geom.tabbar, &view, 1).expect("wide enough for it");
            let drawn: String =
                bar.chars().skip(start as usize).take((end - start) as usize).collect();
            assert_eq!(drawn, spaces_label(&view), "the spaces button at {start}..{end} of: {bar}");
            assert!(drawn.contains(page.label()), "it should name `{}`: {drawn:?}", page.label());
            // The other five are *not* on the row. That is the difference
            // between this and the six buttons it replaces, and it is what the
            // 36 columns it gives back are made of.
            for other in Page::ORDER.iter().filter(|p| **p != page) {
                assert!(
                    !bar.contains(other.label()),
                    "`{}` should be in the menu, not on the bar: {bar}",
                    other.label()
                );
            }
        }
    }

    /// **The ink has no hole in it, on any page.**
    ///
    /// This is the whole point of reserving columns rather than padding the
    /// label: `[agents       v]` was a button with a five-column badge field and
    /// a padded name inside its own brackets, and it read as a rendering fault.
    /// Nothing between the brackets may be two spaces wide, on the longest space
    /// name or the shortest.
    #[test]
    fn the_spaces_button_has_no_gap_inside_its_brackets() {
        for page in Page::ORDER.iter().chain([&Page::Booth, &Page::Settings]) {
            let view = View { page: *page, ..Default::default() };
            let label = spaces_label(&view);
            assert!(!label.contains("  "), "`{}` draws a hole: {label:?}", page.label());
            assert!(label.starts_with('[') && label.ends_with(&format!(" {SPACES_MARK}]")));
        }
    }

    /// The reservation is what does not move — not the button.
    ///
    /// `docker` is six characters and `git` is three, so the ink is a different
    /// width on each. What has to stay put is where the chips stop, or the strip
    /// beside it would reflow every time you changed page.
    #[test]
    fn the_columns_held_for_the_spaces_button_do_not_move() {
        let geom = Geom::compute(WIDE, 30, false, default_geom(), 0);
        let left = tabbar_cluster(&geom.tabbar, 1).left;
        for page in Page::ORDER.iter().chain([&Page::Booth]) {
            let view = View { page: *page, ..Default::default() };
            assert_eq!(
                tabbar_cluster(&geom.tabbar, 1).left,
                left,
                "`{}` moved where the chips stop",
                page.label()
            );
            // ...and the ink stays inside the columns held for it, right-aligned
            // so its closing bracket is where every other page's is.
            let (bs, be) = spaces_button_span(&geom.tabbar, &view, 1).expect("wide enough");
            assert!(bs >= left, "`{}` overflows its reservation", page.label());
            assert_eq!(
                be,
                spaces_button_span(&geom.tabbar, &View::default(), 1).unwrap().1,
                "`{}` does not end where the others do",
                page.label()
            );
        }
        // Off a space the button says so rather than naming one.
        let booth = View { page: Page::Booth, ..Default::default() };
        assert!(spaces_label(&booth).contains("views"), "{}", spaces_label(&booth));
    }

    /// The badge is gone from the bar and kept in the menu.
    ///
    /// It used to ride the button, and before that the view rail, on the
    /// argument that a signal from a space you are not on needs somewhere that
    /// survives every page. A waiting agent already says so five other ways —
    /// its rail row, its workspace chip, the booth chip, the footer and a bell —
    /// so the bar stays quiet and the menu carries the counts.
    #[test]
    fn the_bar_carries_no_badge_and_the_menu_still_does() {
        let sys = SysDto::default();
        let ws = detail(vec![agent(1, "claude", AgentState::Waiting)], None);
        for page in [Page::Docs, Page::Docker, Page::Agents] {
            let view = View { page, ..Default::default() };
            let mut b = buf(WIDE, 30);
            draw(&mut b, WIDE, 30, &scene(&[], Some(&ws), &sys, &[]), &view, &Theme::default());
            let bar = text_of(&b, 0);
            assert!(!bar.contains('!'), "the bar should stay quiet, read: {bar}");
            // The signal is in the menu instead, on the row it belongs to.
            let rows = spaces_menu_rows(&view, Some(&ws), None);
            assert!(rows[0].contains("1!"), "AGENTS' row should carry it: {:?}", rows[0]);
        }
    }

    /// The menu lists every space with its badge, and opens on the one you are
    /// on — the rail's row order and marking, in a box.
    #[test]
    fn the_spaces_menu_lists_them_all_with_their_badges() {
        let ws = detail(vec![agent(1, "claude", AgentState::Waiting)], None);
        let view = View { page: Page::Docker, ..Default::default() };
        let rows = spaces_menu_rows(&view, Some(&ws), None);
        assert_eq!(rows.len(), Page::ORDER.len());
        for (row, p) in rows.iter().zip(Page::ORDER) {
            assert!(row.contains(p.label()), "row {row:?} should name `{}`", p.label());
            assert_eq!(
                row.starts_with('>'),
                p == Page::Docker,
                "the cursor mark belongs to the page you are on: {row:?}"
            );
        }
        assert!(rows[0].ends_with("1!"), "AGENTS should carry its badge: {:?}", rows[0]);
    }

    /// The whole point of the exercise: a page that owns the width gets the
    /// columns the rails were holding, and the pane measurement the daemon is
    /// told about follows it rather than being left behind.
    #[test]
    fn a_page_that_owns_the_width_gets_the_rails_columns_and_tells_the_daemon() {
        let work = View::default();
        let work_geom = page_geom(WIDE, 40, &work);

        for page in Page::ORDER.iter().filter(|p| p.owns_full_width()) {
            let view = View { page: *page, ..Default::default() };
            let geom = page_geom(WIDE, 40, &view);
            assert_eq!(
                (geom.stage_box.x, geom.stage_box.width),
                (0, WIDE),
                "`{}` should own the whole band, edge to edge",
                page.label()
            );
            assert!(
                geom.stage_box.width > work_geom.stage_box.width,
                "`{}` should be wider than WORK's stage, not narrower",
                page.label()
            );
            // The measurement that crosses the wire has to be inside the band it
            // was widened to, or the daemon renders frames the wrong shape.
            let pane = stage_rect(WIDE, 40, &view);
            assert!(pane.x >= geom.stage_box.x, "`{}` pane starts left of its band", page.label());
            assert!(
                pane.x + pane.width <= geom.stage_box.x + geom.stage_box.width,
                "`{}` pane runs past its band",
                page.label()
            );
        }

        // WORK and DIFF are not widened: the rails are what those pages are for.
        for page in [Page::Agents, Page::Diff] {
            let view = View { page, ..Default::default() };
            assert_eq!(
                page_geom(WIDE, 40, &view).stage_box,
                work_geom.stage_box,
                "`{}` must keep the stage between the rails",
                page.label()
            );
        }
    }

    /// BOOTH belongs to the tab bar, beside the workspaces it is a peer of —
    /// never to the spaces menu, which lists ways of looking at *one* workspace.
    #[test]
    fn booth_is_on_the_tab_bar_and_never_among_the_spaces() {
        assert!(!Page::ORDER.contains(&Page::Booth), "BOOTH is not a view of a workspace");

        let sys = SysDto::default();
        for page in [Page::Booth, Page::Agents, Page::Files] {
            let view = View { page, ..Default::default() };
            let mut b = buf(WIDE, 30);
            draw(&mut b, WIDE, 30, &scene(&[], None, &sys, &[]), &view, &Theme::default());
            let geom = page_geom(WIDE, 30, &view);

            // On the bar, at its own span, bracketed when it is the one you are on.
            let bar = text_of(&b, 0);
            let (hx, hend) = tabbar_booth_span(&geom.tabbar);
            let drawn: String = bar.chars().skip(hx as usize).take((hend - hx) as usize).collect();
            assert!(
                drawn.contains(TAB_BOOTH_LABEL),
                "the BOOTH chip should be at {hx}..{hend} of: {bar}"
            );
            assert_eq!(
                drawn.starts_with('['),
                page == Page::Booth,
                "the chip is bracketed exactly when BOOTH is showing: {drawn:?}"
            );

            // And never among the spaces — not on the button, not in the menu.
            assert!(!spaces_label(&view).contains(TAB_BOOTH_LABEL));
            for row in spaces_menu_rows(&view, None, None) {
                assert!(
                    !row.contains(TAB_BOOTH_LABEL),
                    "the menu must not carry BOOTH, read {row:?}"
                );
            }
        }
    }

    /// The control is the same at every width the bar can carry it at, and on a
    /// bar too narrow it is dropped rather than shrunk.
    ///
    /// There is no second layout any more: the rail-versus-buttons switch used
    /// to rearrange the whole screen as the terminal crossed 154 columns, and
    /// the button either fits or it does not.
    #[test]
    fn the_spaces_button_is_dropped_whole_on_a_bar_that_cannot_hold_it() {
        let view = View::default();
        let mut narrowest = None;
        for cols in 40u16..=WIDE {
            let geom = Geom::compute(cols, 30, false, view.geom, system_h_wanted(&view.gauges));
            match spaces_button_span(&geom.tabbar, &view, 1) {
                Some((s, e)) => {
                    assert_eq!(
                        e - s,
                        spaces_label(&view).chars().count() as u16,
                        "at {cols} cols the button was resized rather than kept or dropped"
                    );
                    narrowest.get_or_insert(cols);
                }
                // Once dropped it must stay dropped as the terminal narrows —
                // no width where it flickers back.
                None => assert!(
                    narrowest.is_none_or(|n| cols < n),
                    "the button came back at {cols} cols after being dropped"
                ),
            }
        }
        assert!(narrowest.is_some(), "it should fit somewhere in 40..={WIDE}");
    }

    /// The `[x]` on the active chip is painted at the columns the hit test
    /// reports, which is the whole point of the two coming from one place.
    #[test]
    fn the_active_chip_carries_a_close_button_where_it_is_hit() {
        let sys = SysDto::default();
        let (a, b_) = (ws_summary("alpha"), ws_summary("beta"));
        let tabs = [
            Tab { summary: &a, host: None, live: true },
            Tab { summary: &b_, host: None, live: true },
        ];
        for active in 0..2usize {
            let view = View { tab: active, ..Default::default() };
            let mut b = buf(120, 30);
            draw(&mut b, 120, 30, &scene(&tabs, None, &sys, &[]), &view, &Theme::default());
            let bar = text_of(&b, 0);
            let geom = Geom::compute(120, 30, false, view.geom, system_h_wanted(&view.gauges));
            let (start, end) = tab_close_span(&geom.tabbar, &tabs, &view, 1).unwrap();
            let drawn: String =
                bar.chars().skip(start as usize).take((end - start) as usize).collect();
            assert_eq!(drawn, TAB_CLOSE_MARK, "tab {active}'s close button in: {bar}");
            // Exactly one on the bar, so the mark cannot be read as belonging
            // to the chip beside it.
            assert_eq!(bar.matches(TAB_CLOSE_MARK).count(), 1, "{bar}");
        }
    }

    /// The Docs page is the Files page over a different listing: the same tree
    /// column and the same reader, saying which of the two it is.
    #[test]
    fn the_docs_page_is_the_files_widget_over_markdown() {
        let sys = SysDto::default();
        let docs = Files {
            dir: String::new(),
            entries: vec![
                FileEntry {
                    name: "README.md".into(),
                    path: "README.md".into(),
                    is_dir: false,
                    changed: false,
                },
                FileEntry {
                    name: "docs".into(),
                    path: "docs".into(),
                    is_dir: true,
                    changed: false,
                },
            ],
            sel: 0,
            open: None,
        };
        let view = View { page: Page::Docs, ..Default::default() };
        let mut b = buf(120, 30);
        let sc = Scene { docs: Some(&docs), ..scene(&[], None, &sys, &[]) };
        draw(&mut b, 120, 30, &sc, &view, &Theme::default());
        let screen: String = (0..30).map(|y| text_of(&b, y)).collect::<Vec<_>>().join("\n");
        assert!(screen.contains("docs · /"), "the tree box should name the space: {screen}");
        assert!(screen.contains("README.md"), "{screen}");
        assert!(screen.contains(FILES_FIND_LABEL), "the [find] button is missing: {screen}");
    }

    /// The markdown filter is the Docs page's whole difference from Files.
    #[test]
    fn the_docs_filter_keeps_writing_and_drops_code() {
        for (name, dir) in [("README", false), ("notes.md", false), ("A.MARKDOWN", false)] {
            assert!(is_doc(name, dir), "{name} should be a doc");
        }
        for (name, dir) in [("main.rs", false), ("Cargo.toml", false)] {
            assert!(!is_doc(name, dir), "{name} should not be a doc");
        }
        // Directories stay, because the markdown is inside them — except the
        // two that are only ever build output.
        assert!(is_doc("src", true));
        assert!(!is_doc("target", true));
        assert!(!is_doc("node_modules", true));
    }

    #[test]
    fn the_tab_bar_offers_a_machine_as_well_as_a_project() {
        let sys = SysDto::default();
        let mut b = buf(120, 30);
        draw(&mut b, 120, 30, &scene(&[], None, &sys, &[]), &View::default(), &Theme::default());
        let bar = text_of(&b, 0);
        let host = bar.find(TAB_HOST_LABEL).expect("the machines button is missing");
        let new = bar.find(TAB_NEW_LABEL).expect("the [+ new] button is missing");
        // Left of `[+ new]`, and clear of it: the two must not overlap or share
        // a column, or one is unclickable.
        assert!(host + TAB_HOST_LABEL.len() < new, "{bar}");
    }

    /// One machines control, whatever the count, painted where it is hit.
    ///
    /// There were two: `[+ host]` and an `N hosts` count beside it, both opening
    /// the same picker. The count was painted last and had to be positioned
    /// against whichever furniture happened to be on the right, which is how it
    /// once landed straight through `docs`.
    #[test]
    fn the_machines_button_counts_and_lands_on_nothing_else() {
        let sys = SysDto::default();
        let view = View::default();
        for daemons in [1usize, 2, 3, 12] {
            let mut b = buf(120, 30);
            let sc = Scene { daemons, ..scene(&[], None, &sys, &[]) };
            draw(&mut b, 120, 30, &sc, &view, &Theme::default());
            let bar = text_of(&b, 0);
            let label = machines_label(daemons);
            // One machine is an offer; past one it is the roll call and says how
            // many.
            assert_eq!(
                label.contains(&daemons.to_string()),
                daemons > 1,
                "the label should count only past one machine: {label:?}"
            );
            let geom = Chrome::compute(120, 30, false, view.geom, system_h_wanted(&view.gauges));
            let (bx, bend) = machines_span(&geom.tabbar, daemons);
            assert_eq!(cells(&bar, bx, bend), label, "the span is off the button: {bar}");
            // Exactly one of it on the row, and clear of everything else.
            assert_eq!(bar.matches(&label).count(), 1, "{bar}");
            let mut controls: Vec<(u16, u16)> =
                spaces_button_span(&geom.tabbar, &view, daemons).into_iter().collect();
            controls.push(tabbar_new_span(&geom.tabbar));
            controls.push(tabbar_booth_span(&geom.tabbar));
            for (s, e) in controls {
                assert!(bend <= s || bx >= e, "the machines button overlaps ({s}, {e}): {bar}");
            }
        }
    }

    /// The rule between BOOTH and the chips is drawn, and no chip is under it.
    #[test]
    fn a_rule_separates_booth_from_the_workspace_chips() {
        let sys = SysDto::default();
        let (a, b_) = (ws_summary("alpha"), ws_summary("beta"));
        let tabs = [
            Tab { summary: &a, host: None, live: true },
            Tab { summary: &b_, host: None, live: true },
        ];
        let view = View::default();
        let mut b = buf(120, 30);
        draw(&mut b, 120, 30, &scene(&tabs, None, &sys, &[]), &view, &Theme::default());
        let bar = text_of(&b, 0);
        let geom = Chrome::compute(120, 30, false, view.geom, system_h_wanted(&view.gauges));
        let sep = tabbar_sep_x(&geom.tabbar).expect("120 columns can afford a rule");
        assert_eq!(
            cells(&bar, sep, sep + 1),
            TAB_SEP,
            "the rule should be at column {sep} of: {bar}"
        );
        // Clear of the BOOTH chip on one side and the first chip on the other.
        assert!(sep >= tabbar_booth_span(&geom.tabbar).1, "{bar}");
        let strip = tab_strip(&geom.tabbar, &tabs, &view, 1);
        for span in strip.spans.iter().flatten() {
            assert!(span.0 > sep, "a chip at {span:?} runs under the rule: {bar}");
        }

        // And on a bar too narrow to spend the columns, there is no rule and the
        // chips take them back.
        let narrow = Chrome::compute(40, 30, false, view.geom, system_h_wanted(&view.gauges));
        assert!(tabbar_sep_x(&narrow.tabbar).is_none(), "40 columns should go unruled");
        assert_eq!(tabbar_chips_x0(&narrow.tabbar), tabbar_booth_span(&narrow.tabbar).1 + 1);
    }

    /// A diff is what is on the stage, not a place you go — so it has no row
    /// and the cycle never lands on it.
    #[test]
    fn the_spaces_menu_does_not_offer_diff() {
        assert!(
            !Page::ORDER.contains(&Page::Diff),
            "diff is stage content; a row makes it look like a seventh space"
        );
        for label in Page::ORDER.iter().map(|p| p.label()) {
            assert_ne!(label, "diff");
        }
        // Cycling from a diff leaves it for a real space rather than sitting on
        // it: `Diff` is not in the order, so `next`/`prev` resolve from the
        // start of the list instead of from a position it does not have.
        assert!(Page::ORDER.contains(&Page::Diff.next()), "alt-. off a diff must reach a space");
        assert!(Page::ORDER.contains(&Page::Diff.prev()), "alt-, off a diff must reach a space");
        // And the spaces that remain still cycle through each other exactly.
        // Walked from the order's own head rather than from a page named here,
        // so adding one to the front stays a one-line change to `ORDER`.
        let head = Page::ORDER[0];
        let mut seen = vec![head];
        let mut p = head;
        for _ in 1..Page::ORDER.len() {
            p = p.next();
            seen.push(p);
        }
        assert_eq!(seen, Page::ORDER.to_vec(), "the cycle must visit every space once");
        assert_eq!(p.next(), head, "and wrap");
    }

    /// The `..` row is built from `parent_of` and `Backspace` from
    /// `Files::parent`, so they must give the same answer for every directory.
    /// They do because one calls the other — this fails if someone re-inlines it.
    #[test]
    fn the_dotdot_row_and_backspace_agree_about_up() {
        for dir in ["", "src", "src/pane", "a/b/c"] {
            let files = Files { dir: dir.to_string(), ..Default::default() };
            assert_eq!(files.parent(), parent_of(dir), "disagreed about up from {dir:?}");
        }
        assert_eq!(parent_of(""), None, "the root must not escape the workspace");
        assert_eq!(parent_of("src"), Some(String::new()), "one level up is the root");
    }

    #[test]
    fn a_narrow_tab_bar_drops_the_buttons_rather_than_the_tabs() {
        // The buttons are convenience; the tabs are what tells you where you
        // are. Where the two want the same columns the tab keeps them, so a
        // button that would land on one is not drawn at all.
        let sys = SysDto::default();
        let summary = WorkspaceSummary {
            id: SessionId(1),
            name: "a-project-with-a-long-name".into(),
            cwd: "/tmp".into(),
            agents: 0,
            waiting: 0,
            working: 0,
            finished: 0,
            questions: 0,
            exited: 0,
            unread: 0,
            processes: 0,
            changes: 0,
            conflicts: 0,
            repo_state: RepoState::Clean,
            attached_clients: 1,
        };
        let tabs = [Tab { summary: &summary, host: None, live: true }];
        // Where the chip ends: one column of margin, then the label the bar
        // actually draws for the tab you are on — bracketed, with its `[x]`.
        let chip_end = 1 + tab_label(0, &Tab { summary: &summary, host: None, live: true }, true)
            .chars()
            .count();
        for cols in [24u16, 30, 40, 60, 120] {
            let mut b = buf(cols, 20);
            draw(
                &mut b,
                cols,
                20,
                &scene(&tabs, None, &sys, &[]),
                &View::default(),
                &Theme::default(),
            );
            let bar = text_of(&b, 0);
            assert!(bar.chars().count() <= cols as usize, "the bar overran {cols} cols: {bar}");
            for label in [TAB_HOST_LABEL, TAB_NEW_LABEL] {
                if let Some(at) = cell_of(&bar, label) {
                    assert!(
                        at >= chip_end,
                        "{label} landed on the tab at {cols} cols (col {at}, chip ends {chip_end}): {bar}"
                    );
                }
            }
            // And the chip is intact as far as the screen goes.
            let visible = chip_end.min(cols as usize) as u16;
            assert!(cells(&bar, 0, visible).contains("1:a-project"), "{bar}");
        }
    }

    /// A dozen workspaces on one client: the bar keeps every fixed control and
    /// scrolls the chips under them, instead of letting the chips push them off.
    ///
    /// This is the bug the strip exists for. The rule ran the other way — the
    /// chips took the columns they wanted and `[+ new]`, `[+ host]` and the
    /// machine count were dropped one at a time as they grew — so the client
    /// with the most projects was the one with no button to open another, and
    /// the chips it spent them on ran off the right edge, where no pointer can
    /// reach them.
    #[test]
    fn a_bar_full_of_workspaces_keeps_its_controls_and_scrolls_the_chips() {
        let sys = SysDto::default();
        let names: Vec<WorkspaceSummary> =
            (0..12).map(|i| ws_summary(&format!("project-{i}"))).collect();
        let tabs: Vec<Tab<'_>> =
            names.iter().map(|s| Tab { summary: s, host: None, live: true }).collect();
        for cols in [80u16, 120, 200] {
            for daemons in [1usize, 3] {
                let view = View::default();
                let mut b = buf(cols, 30);
                let sc = Scene { daemons, ..scene(&tabs, None, &sys, &[]) };
                draw(&mut b, cols, 30, &sc, &view, &Theme::default());
                let bar = text_of(&b, 0);
                let at = format!("{cols} cols, {daemons} machines: {bar}");

                // BOOTH and both buttons are still there.
                assert!(bar.contains(TAB_BOOTH_LABEL), "BOOTH went under the chips — {at}");
                for label in [TAB_NEW_LABEL.to_string(), machines_label(daemons)] {
                    assert!(bar.contains(&label), "`{label}` went under the chips — {at}");
                }
                let geom = page_geom(cols, 30, &view);
                let c = tabbar_cluster(&geom.tabbar, daemons);

                // Not merely on screen: no chip shares a column with any of them.
                let strip = tab_strip(&geom.tabbar, &tabs, &view, daemons);
                for (i, (s, e)) in
                    strip.spans.iter().enumerate().filter_map(|(i, s)| Some((i, (*s)?)))
                {
                    assert!(e <= c.left, "chip {i} at {s}..{e} crosses {} — {at}", c.left);
                }

                // What does not fit is reachable rather than gone: the arrow
                // names a workspace, and that workspace has no chip to click.
                //
                // Except at 80 columns, where the fixed furniture leaves the
                // strip a handful of columns — too few to spend on arrows. That
                // width is the reservation's problem, not the strip's, and it
                // reads the same as it always did.
                match strip.next {
                    Some((_, next)) => {
                        assert!(
                            strip.spans[next].is_none(),
                            "`{TAB_NEXT_LABEL}` points at a chip already on screen — {at}"
                        );
                        assert!(bar.contains(TAB_NEXT_LABEL), "the arrow is not painted — {at}");
                    }
                    None => assert!(
                        c.left - (tabbar_booth_span(&geom.tabbar).1 + 1) < MIN_STRIP,
                        "a strip with room for the arrows went without them — {at}"
                    ),
                }
                // Nothing on the row ran past the terminal.
                assert!(bar.chars().count() <= cols as usize, "the bar overran — {at}");
            }
        }
    }

    /// The strip scrolls to keep the chip you are on whole, and only that far.
    ///
    /// Derived from the active tab rather than kept as its own offset, so the
    /// drawing and the hit test cannot disagree about where a chip is. The two
    /// halves of that are asserted here: every active chip is on screen and
    /// unclipped, and walking the bar never scrolls it backwards.
    #[test]
    fn the_strip_scrolls_only_far_enough_to_keep_the_active_chip_whole() {
        let names: Vec<WorkspaceSummary> =
            (0..14).map(|i| ws_summary(&format!("project-{i}"))).collect();
        let tabs: Vec<Tab<'_>> =
            names.iter().map(|s| Tab { summary: s, host: None, live: true }).collect();
        for cols in [80u16, 120, 200] {
            let mut walked = Vec::new();
            for tab in 0..tabs.len() {
                let view = View { tab, ..Default::default() };
                let geom = page_geom(cols, 30, &view);
                let strip = tab_strip(&geom.tabbar, &tabs, &view, 1);
                let (s, e) = strip.spans[tab].expect("the chip you are on must be on the strip");
                let want = tab_label(tab, &tabs[tab], true).chars().count() as u16;
                let alone = strip.spans.iter().flatten().count() == 1;
                assert!(
                    e - s == want || alone,
                    "the active chip is clipped at {cols} cols, tab {tab}, beside {} others",
                    strip.spans.iter().flatten().count() - 1
                );
                walked.push(strip.spans.iter().position(|s| s.is_some()).unwrap());
            }
            assert!(
                walked.windows(2).all(|w| w[0] <= w[1]),
                "walking right scrolled the strip back at {cols} cols: {walked:?}"
            );
            assert_eq!(walked[0], 0, "the strip starts scrolled at {cols} cols");
            assert!(walked.last() > Some(&0), "the strip never scrolled at {cols} cols");
        }
    }

    /// While BOOTH has the screen no chip is bracketed, so none of them may be
    /// *sized* as though it were: the bar would then paint every chip right of
    /// the active one four columns left of the span the pointer is tested
    /// against, and clicking a workspace would open its neighbour.
    #[test]
    fn no_chip_is_sized_as_the_active_one_while_booth_has_the_screen() {
        let sys = SysDto::default();
        let names: Vec<WorkspaceSummary> =
            ["alpha", "beta", "gamma"].iter().map(|n| ws_summary(n)).collect();
        let tabs: Vec<Tab<'_>> =
            names.iter().map(|s| Tab { summary: s, host: None, live: true }).collect();
        for page in [Page::Agents, Page::Booth] {
            let view = View { page, tab: 0, ..Default::default() };
            let mut b = buf(120, 30);
            draw(&mut b, 120, 30, &scene(&tabs, None, &sys, &[]), &view, &Theme::default());
            let bar = text_of(&b, 0);
            let geom = page_geom(120, 30, &view);
            let strip = tab_strip(&geom.tabbar, &tabs, &view, 1);
            for (i, span) in strip.spans.iter().enumerate() {
                let (s, e) = span.expect("three chips fit at 120 columns");
                let drawn: String = bar.chars().skip(s as usize).take((e - s) as usize).collect();
                assert_eq!(
                    drawn,
                    tab_label(i, &tabs[i], active_chip(&view) == Some(i)),
                    "chip {i} is painted somewhere other than its span on `{}`: {bar}",
                    page.label()
                );
            }
        }
    }

    /// A pin turns the AGENTS `+` into a one-click spawn, so the label is the
    /// only place the user can see what that click is about to do. It has to
    /// name the agent, and the span the click is tested against has to follow
    /// the label it drew — a button hit in one place and painted in another is
    /// the defect this guards.
    #[test]
    fn the_agents_button_names_the_pin_it_will_spawn() {
        let ws = detail(vec![], None);
        let sys = SysDto::default();
        let view = View { pinned_agent: Some("beta".into()), ..View::default() };
        let mut b = buf(120, 40);
        draw(&mut b, 120, 40, &scene(&[], Some(&ws), &sys, &[]), &view, &Theme::default());
        let joined: String = (0..40).map(|y| text_of(&b, y)).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("[+ beta]"), "the button should name the pin:\n{joined}");
        assert!(
            !joined.contains(AGENTS_ADD_LABEL),
            "and not the generic word beside it:\n{joined}"
        );

        let geom = Geom::compute(120, 40, false, view.geom, system_h_wanted(&view.gauges));
        let label = agents_add_label(view.pinned_agent.as_deref(), geom.left_box);
        let (start, end) = agents_add_span_for(&geom, &label);
        let row = text_of(&b, geom.left_box.y);
        let painted: String = row.chars().skip(start as usize).take(label.width()).collect();
        assert_eq!(painted, label, "row {row:?} does not carry {label:?} at {start}..{end}");

        // Too narrow to spell it and the generic word comes back, rather than a
        // name clipped into something that names a different agent.
        let narrow = Geom::compute(64, 40, false, view.geom, system_h_wanted(&view.gauges));
        assert_eq!(agents_add_label(Some("a-long-agent-name"), narrow.left_box), AGENTS_ADD_LABEL);
    }

    #[test]
    fn an_empty_list_overlay_does_not_panic() {
        let list = ListOverlay {
            title: "NOTHING".into(),
            items: vec![],
            values: None,
            sel: 0,
            kind: ListKind::SpawnAgent,
        };
        assert_eq!(list.chosen(), None);
        let mut b = buf(80, 24);
        let view = View { overlay: Some(Overlay::List(list)), ..Default::default() };
        draw_all(
            &mut b,
            80,
            24,
            &scene(&[], None, &SysDto::default(), &[]),
            &view,
            &Theme::default(),
        );
    }

    fn files_fixture() -> Files {
        Files {
            dir: "src".into(),
            entries: vec![
                FileEntry {
                    name: "core".into(),
                    path: "src/core".into(),
                    is_dir: true,
                    changed: false,
                },
                FileEntry {
                    name: "main.rs".into(),
                    path: "src/main.rs".into(),
                    is_dir: false,
                    changed: true,
                },
            ],
            sel: 1,
            open: Some(Editor::new(
                "src/main.rs".into(),
                "fn main() {\n    println!(\"hi\");\n}\n",
                false,
            )),
        }
    }

    #[test]
    fn the_files_page_lists_a_directory_beside_the_open_file() {
        let mut b = buf(120, 30);
        let view = View { page: Page::Files, ..Default::default() };
        let sys = SysDto::default();
        let files = files_fixture();
        let scene = Scene { files: Some(&files), diff: None, ..Scene::new(&[], &sys) };
        draw(&mut b, 120, 30, &scene, &view, &Theme::default());
        let joined: String = (0..30).map(|y| text_of(&b, y)).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("core/"), "a directory should be marked:\n{joined}");
        assert!(joined.contains("main.rs"), "{joined}");
        assert!(joined.contains("fn main()"), "the open file should show:\n{joined}");
        assert!(joined.contains("src/main.rs"), "the viewer should be titled:\n{joined}");
        // FILES owns the whole band, so the workspace rails are *not* here:
        // a file is not about this workspace's agents or its diff, and the
        // body they crowded got less width than the two of them together.
        // What does not move is the chrome that tells you where you are and
        // how to leave — the tab bar, and the spaces button on it.
        assert!(!joined.contains("AGENTS"), "the agents rail should be gone:\n{joined}");
        assert!(!joined.contains("CHANGES"), "the changes rail should be gone:\n{joined}");
        assert!(joined.contains("[+ new]"), "the tab bar must not move:\n{joined}");
    }

    #[test]
    fn the_files_page_never_overruns_its_column() {
        for cols in [60u16, 80, 120, 200] {
            let mut b = buf(cols, 24);
            let view = View { page: Page::Files, ..Default::default() };
            let sys = SysDto::default();
            let files = files_fixture();
            let scene = Scene { files: Some(&files), diff: None, ..Scene::new(&[], &sys) };
            draw(&mut b, cols, 24, &scene, &view, &Theme::default());
            for y in 0..24 {
                assert!(text_of(&b, y).chars().count() <= cols as usize, "overran at {cols}");
            }
        }
    }

    #[test]
    fn the_tree_cursor_stops_at_both_ends() {
        let mut files = files_fixture();
        files.sel = 0;
        files.move_sel(-1);
        assert_eq!(files.sel, 0);
        for _ in 0..5 {
            files.move_sel(1);
        }
        assert_eq!(files.sel, 1, "two entries, so the last index is 1");
    }

    #[test]
    fn walking_up_stops_at_the_workspace_root() {
        let mut files = files_fixture();
        files.dir = "src/core/deep".into();
        assert_eq!(files.parent().as_deref(), Some("src/core"));
        files.dir = "src".into();
        // One level above `src` is the root, spelled as the empty path.
        assert_eq!(files.parent().as_deref(), Some(""));
        files.dir = String::new();
        assert_eq!(files.parent(), None, "the root must not escape the workspace");
    }

    /// The buffer is client-side now, so the guarantee that replaced
    /// "server-side buffers survive detach" has to actually hold: the first
    /// close refuses, and only the second discards.
    #[test]
    fn a_changed_buffer_will_not_close_on_the_first_try() {
        let mut e = Editor::new("a.rs".into(), "fn main() {}\n", false);
        assert!(e.may_close(), "an unchanged buffer closes without asking");

        e.touch();
        assert!(e.dirty);
        assert!(!e.may_close(), "a changed buffer must refuse once");
        assert!(e.notice.is_some(), "and say why");
        assert!(e.may_close(), "the second press discards");
    }

    /// Saving disarms the refusal — otherwise the very next close would be
    /// treated as the confirming press and skip the warning on later edits.
    #[test]
    fn saving_clears_the_armed_discard() {
        let mut e = Editor::new("a.rs".into(), "x\n", false);
        e.touch();
        assert!(!e.may_close());
        e.saved();
        assert!(!e.dirty);
        assert!(!e.discard_armed, "a saved buffer is not armed to discard");
        e.touch();
        assert!(!e.may_close(), "the next edit warns again");
    }

    /// A truncated read is a prefix of the file. Saving it would drop
    /// everything past the daemon's cap.
    #[test]
    fn a_truncated_file_is_read_only() {
        let mut e = Editor::new("big.log".into(), "first line\n", true);
        assert!(!e.editable());
        e.edit();
        assert_eq!(e.mode, EditMode::View, "editing a partial read must be refused");
        assert!(e.notice.as_deref().is_some_and(|n| n.contains("read-only")), "{:?}", e.notice);
    }

    #[test]
    fn a_saved_buffer_ends_with_a_newline() {
        let e = Editor::new("a.rs".into(), "one\ntwo", false);
        assert_eq!(e.contents(), "one\ntwo\n");
        // And a file that already ended with one does not grow a second.
        let e = Editor::new("a.rs".into(), "one\ntwo\n", false);
        assert_eq!(e.contents(), "one\ntwo\n");
    }

    #[test]
    fn the_open_file_is_highlighted_and_says_when_it_is_dirty() {
        let mut files = files_fixture();
        let mut b = buf(120, 30);
        let view = View { page: Page::Files, ..Default::default() };
        let sys = SysDto::default();
        let draw_it = |b: &mut Buffer, files: &Files| {
            let scene = Scene { files: Some(files), ..Scene::new(&[], &sys) };
            draw(b, 120, 30, &scene, &view, &Theme::default());
            (0..30).map(|y| text_of(b, y)).collect::<Vec<_>>().join("\n")
        };

        let joined = draw_it(&mut b, &files);
        assert!(joined.contains("fn main()"), "{joined}");
        assert!(joined.contains("e edit"), "the keys should be offered:\n{joined}");
        assert!(!joined.contains("src/main.rs *"), "a fresh buffer is not dirty:\n{joined}");

        // `fn` is a keyword and `main` is not, so they must be different
        // colours — the whole point of highlighting here.
        let row = (0..30).find(|y| text_of(&b, *y).contains("fn main()")).expect("no code row");
        let line = text_of(&b, row);
        // Column, not byte offset: the row is full of multi-byte box-drawing
        // characters, so `find` lands well past the text it matched.
        let at = line.find("fn main()").unwrap();
        let col = line[..at].chars().count() as u16;
        let fg = |x: u16| b.cell((x, row)).unwrap().fg;
        assert_eq!(b.cell((col, row)).unwrap().symbol(), "f", "column arithmetic is off");
        assert_ne!(fg(col), fg(col + 3), "`fn` and `main` came out the same colour");

        files.open.as_mut().unwrap().touch();
        let joined = draw_it(&mut b, &files);
        assert!(joined.contains("src/main.rs *"), "an unsaved buffer must say so:\n{joined}");
    }

    /// Two hunks in one file, far enough apart that git prints them separately.
    /// The shape the whole of partial staging turns on.
    const TWO_HUNKS: &str = "\
diff --git a/a.txt b/a.txt
index 1111111..2222222 100644
--- a/a.txt
+++ b/a.txt
@@ -1,5 +1,5 @@
 line1
-line2
+CHANGED-EARLY
 line3
 line4
 line5
@@ -16,5 +16,5 @@
 line16
 line17
-line18
+CHANGED-LATE
 line19
 line20
";

    fn unstaged() -> DiffView {
        DiffView::new(DiffKind::Unstaged { path: Some("a.txt".into()) }, TWO_HUNKS)
    }

    #[test]
    fn the_rows_on_screen_point_back_into_the_patch() {
        let d = unstaged();
        // Every line row has an anchor; the file card, its rule and the hunk
        // separators have none.
        for (i, row) in d.rows.iter().enumerate() {
            let anchored = d.anchors[i].is_some();
            let is_line = matches!(row, DiffRow::Line { .. });
            assert_eq!(anchored, is_line, "row {i} ({row:?}) anchored={anchored}");
        }
        // And an anchor names a line that really is in that hunk.
        for (f, h, l) in d.anchors.iter().flatten().copied() {
            assert!(d.patch.files[f].hunks[h].lines.get(l).is_some(), "dangling anchor");
        }
    }

    /// Staging the second hunk must send *only* the second hunk. This is the
    /// feature: a file with one finished change and one debug line should not
    /// have to be staged whole.
    #[test]
    fn staging_the_second_hunk_sends_only_the_second_hunk() {
        let mut d = unstaged();
        assert_eq!(d.patch.hunk_count(), 2, "{:?}", d.rows);
        d.step_hunk(1);
        assert_eq!(d.cursor, DiffCursor { file: 0, hunk: 1 });

        let (patch, target, reverse) = d.selection(false).expect("nothing to apply");
        assert_eq!(target, ApplyTarget::Index);
        assert!(!reverse, "an unstaged diff goes into the index forwards");
        assert!(patch.contains("CHANGED-LATE"), "the chosen hunk is missing:\n{patch}");
        assert!(!patch.contains("CHANGED-EARLY"), "the other hunk leaked in:\n{patch}");
        // And it is a valid one-hunk patch, not a slice of text.
        assert_eq!(Patch::parse(&patch).hunk_count(), 1, "{patch}");
    }

    /// Line-select sends the picked lines and nothing else — the case that
    /// proves the `@@` arithmetic, since the unpicked lines have to change the
    /// counts in the header.
    #[test]
    fn line_select_sends_only_the_picked_lines() {
        let mut d = unstaged();
        d.line_select();
        assert_eq!(d.mode, DiffMode::Lines);
        // `+CHANGED-EARLY` and `-line2` are the two changed lines of hunk 0.
        d.pick_line();
        let (patch, ..) = d.selection(false).expect("nothing to apply");
        // One of the pair was picked, so the other must appear as context —
        // dropped `+` lines vanish, dropped `-` lines stay as context because
        // the file being patched still contains them.
        let picked_plus = patch.contains("+CHANGED-EARLY");
        let picked_minus = patch.lines().any(|l| l == "-line2");
        assert!(picked_plus ^ picked_minus, "expected exactly one side:\n{patch}");
        assert!(!patch.contains("CHANGED-LATE"), "the other hunk leaked in:\n{patch}");
    }

    #[test]
    fn a_staged_diff_applies_backwards_and_refuses_to_discard() {
        let mut d = DiffView::new(DiffKind::Staged { path: Some("a.txt".into()) }, TWO_HUNKS);
        let (_, target, reverse) = d.selection(false).expect("nothing to apply");
        assert_eq!(target, ApplyTarget::Index);
        assert!(reverse, "the same key has to unstage on a staged diff");
        // There is no worktree change on this side to throw away, and
        // reverse-applying one would undo an edit the reader can see.
        assert!(d.selection(true).is_none(), "a staged diff must not offer discard");
    }

    #[test]
    fn discarding_reverse_applies_to_the_worktree() {
        let mut d = unstaged();
        let (_, target, reverse) = d.selection(true).expect("nothing to apply");
        assert_eq!(target, ApplyTarget::Worktree);
        assert!(reverse);
    }

    #[test]
    fn a_commit_diff_will_not_stage_anything() {
        let mut d = DiffView::new(
            DiffKind::Commit { id: "abc1234def".into(), summary: "c".into() },
            TWO_HUNKS,
        );
        assert!(d.selection(false).is_none(), "history is not stageable");
        assert!(d.selection(true).is_none());
        assert!(d.notice.is_none(), "a refusal is not an error to report");
        assert_eq!(d.kind.as_ref().unwrap().title(), "commit abc1234 c");
    }

    #[test]
    fn picking_nothing_refuses_rather_than_staging_the_hunk() {
        let mut d = unstaged();
        d.line_select();
        assert!(d.selection(false).is_none());
        assert_eq!(d.notice.as_deref(), Some("no lines picked"));
    }

    /// After staging, the diff is re-read and the hunk that moved is gone. The
    /// cursor was on it, so it has to come back somewhere real.
    #[test]
    fn the_cursor_survives_the_hunk_under_it_disappearing() {
        let mut d = unstaged();
        d.step_hunk(1);
        assert_eq!(d.cursor.hunk, 1);
        // What the daemon would return once the late hunk is in the index.
        let one_hunk = TWO_HUNKS.split("@@ -16,5 +16,5 @@").next().unwrap();
        d.set_patch(one_hunk);
        assert_eq!(d.patch.hunk_count(), 1);
        assert_eq!(d.cursor.hunk, 0, "the cursor walked off the end of the file");
        assert!(d.selection(false).is_some(), "and it can still stage what is left");
    }

    #[test]
    fn an_empty_diff_says_so_instead_of_showing_nothing() {
        let d = DiffView::new(DiffKind::Unstaged { path: None }, "");
        assert_eq!(d.rows, vec![DiffRow::Note("(no differences)".to_string())]);
        assert_eq!(d.patch.hunk_count(), 0);
    }

    /// One card head per file, in place of the four lines git writes above it.
    ///
    /// `diff --git`, `index`, `---` and `+++` are three restatements of the path
    /// and a pair of blob hashes — four rows per file to say what one row says
    /// better, and on a working tree of twenty files that is eighty rows of
    /// nothing between you and the code.
    #[test]
    fn a_file_card_replaces_the_four_lines_git_writes_above_each_file() {
        let d = unstaged();
        assert_eq!(
            d.rows[0],
            DiffRow::File { path: "a.txt".into(), added: 2, removed: 2, note: None, folded: false },
            "the head should carry the path and the counts"
        );
        assert_eq!(d.rows[1], DiffRow::Rule);
        // And git's own header is gone rather than merely reordered.
        for row in &d.rows {
            if let DiffRow::Line { text, .. } | DiffRow::Note(text) = row {
                assert!(!text.starts_with("diff --git"), "the raw header survived: {text:?}");
                assert!(!text.starts_with("index "), "the raw header survived: {text:?}");
            }
        }
    }

    /// The two sides count independently, and getting that wrong is the classic
    /// way a diff view lies: a removed line has no number on the new side
    /// because it is not in the new file, and an added one has none on the old.
    /// Off by one here and every number below the first hunk is wrong.
    #[test]
    fn every_line_wears_the_number_it_has_in_its_own_side() {
        let d = unstaged();
        let numbered: Vec<(Option<usize>, Option<usize>, String)> = d
            .rows
            .iter()
            .filter_map(|r| match r {
                DiffRow::Line { old, new, text, .. } => Some((*old, *new, text.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            numbered[..6].to_vec(),
            vec![
                (Some(1), Some(1), "line1".to_string()),
                (Some(2), None, "line2".to_string()),
                (None, Some(2), "CHANGED-EARLY".to_string()),
                (Some(3), Some(3), "line3".to_string()),
                (Some(4), Some(4), "line4".to_string()),
                (Some(5), Some(5), "line5".to_string()),
            ]
        );
        // The second hunk restarts from its own `@@`, not from where the first
        // one left off.
        assert_eq!(numbered[6], (Some(16), Some(16), "line16".to_string()));
        assert_eq!(numbered[8], (Some(18), None, "line18".to_string()));
        assert_eq!(numbered[9], (None, Some(18), "CHANGED-LATE".to_string()));
    }

    /// The numbers are the first thing a narrow body gives up. The GIT page is
    /// the case that needs it: its body is what is left after the refs column,
    /// and a ten-cell gutter there would leave the code unreadable.
    #[test]
    fn the_gutter_gives_up_its_numbers_before_the_text_does() {
        let d = unstaged();
        assert!(d.gutter_w(120) > DIFF_GUTTER_W, "a wide body can afford the numbers");
        assert_eq!(d.gutter_w(20), DIFF_GUTTER_W, "a narrow one draws the marker and nothing else");
        // And the threshold is the floor, not a number picked twice: at exactly
        // enough room for the numbers plus the minimum text, they appear.
        let need = DIFF_GUTTER_W + 2 * 3 + 3 + DIFF_TEXT_MIN_W;
        assert!(d.gutter_w(need) > DIFF_GUTTER_W, "{need} cells is enough");
        assert_eq!(d.gutter_w(need - 1), DIFF_GUTTER_W, "one cell short and they go");
    }

    /// Shutting a file hides its hunks and nothing else. The cursor still names
    /// the hunk it named — folding is a view, not a move — and it still has a
    /// row on screen to be drawn on, which is the file's own head.
    #[test]
    fn folding_a_file_hides_its_hunks_without_moving_the_cursor() {
        let mut d = unstaged();
        let open = d.rows.len();
        d.step_hunk(1);
        let was = d.cursor;

        d.toggle_fold();
        assert_eq!(d.rows.len(), 2, "a shut file is its head and its rule: {:?}", d.rows);
        assert!(d.anchors.iter().all(Option::is_none), "a hidden line kept an anchor");
        assert_eq!(d.cursor, was, "folding moved the cursor");
        assert_eq!(d.cursor_row(), Some(0), "the cursor lost the only row it had left");
        assert!(matches!(d.rows[0], DiffRow::File { folded: true, .. }));

        d.toggle_fold();
        assert_eq!(d.rows.len(), open, "unfolding did not put the hunks back");
        assert_eq!(d.cursor, was);
        // And what it stages is still the hunk it was on before any of this.
        let (patch, ..) = d.selection(false).expect("nothing to apply");
        assert!(patch.contains("CHANGED-LATE"), "{patch}");
    }

    /// Walking into a file you had shut opens it. The alternative is a cursor on
    /// a hunk that is not on screen with `space` about to stage it, which is the
    /// one thing a staging UI must never do.
    #[test]
    fn stepping_into_a_folded_file_opens_it() {
        let two_files = format!(
            "{TWO_HUNKS}diff --git a/b.txt b/b.txt\nindex 3333333..4444444 100644\n\
             --- a/b.txt\n+++ b/b.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n"
        );
        let mut d = DiffView::new(DiffKind::Unstaged { path: None }, &two_files);
        d.cursor = DiffCursor { file: 1, hunk: 0 };
        d.toggle_fold();
        assert!(matches!(d.rows.last(), Some(DiffRow::Rule)), "b.txt should be shut");

        d.cursor = DiffCursor { file: 0, hunk: 0 };
        d.step_hunk(2); // past a.txt's two hunks, into b.txt
        assert_eq!(d.cursor, DiffCursor { file: 1, hunk: 0 });
        assert!(
            d.rows.iter().any(|r| matches!(r, DiffRow::Line { text, .. } if text == "new")),
            "the file the cursor walked into is still shut: {:?}",
            d.rows
        );
    }

    /// The working tree's files are on the GIT page, under the headings the
    /// CHANGES rail uses, and each row answers the rail's own verb for its side
    /// of the index. This is what "stage it where you are reading it" means; the
    /// rail keeps everything it had, and these are the same `ChangeRow`s.
    #[test]
    fn the_refs_list_carries_the_working_trees_files_with_the_rails_verbs() {
        use crate::verbs::GitRow;
        let mut c = changes(0, 0, RepoState::Clean);
        c.staged =
            vec![FileChange { path: "staged.rs".into(), code: "M".into(), added: 3, deleted: 1 }];
        c.conflicted = vec![butai_protocol::api::ConflictFile {
            path: "clash.rs".into(),
            base: true,
            ours: true,
            theirs: true,
        }];
        // A real repository always has these, and they are what the dropped
        // `Commits` heading used to be drawn over.
        c.recent_commits = vec![butai_protocol::api::CommitDto {
            id: "abc1234".into(),
            summary: "a commit".into(),
        }];
        let git = Git::default();
        let rows = ref_rows(&git, Some(&c), Some(SessionId(1)));

        assert!(matches!(rows[0], RefRow::WorkingTree { dirty: 3 }), "{:?}", rows[0]);
        let labels: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                RefRow::Header(h) => Some(*h),
                _ => None,
            })
            .collect();
        // No `Commits`: the whole log is in the box directly below, so those
        // rows are dropped — and the heading has to go with them, or the list
        // carries a label over nothing. A real repository always has recent
        // commits, so this is the ordinary case rather than an edge one.
        assert_eq!(labels, vec!["Conflicts", "Unstaged", "Staged"], "{rows:?}");
        assert!(
            !rows.iter().any(|r| matches!(r, RefRow::Change(ChangeRow::Commit { .. }))),
            "the log is the box below; these would be the same commits twice"
        );

        // The verb each row offers follows from which side it is on — the whole
        // reason the table is keyed off the selection.
        let kinds: Vec<GitRow> = (0..rows.len()).map(|i| ref_row_kind(&rows, i)).collect();
        assert!(kinds.contains(&GitRow::ChangeConflicted), "{kinds:?}");
        assert!(kinds.contains(&GitRow::ChangeUnstaged), "{kinds:?}");
        assert!(kinds.contains(&GitRow::ChangeStaged), "{kinds:?}");
        let verb =
            |kind| crate::verbs::git_row_verbs(kind).iter().map(|v| v.label).collect::<Vec<_>>();
        assert!(verb(GitRow::ChangeUnstaged).contains(&"stage"));
        assert!(verb(GitRow::ChangeStaged).contains(&"unstage"));
        assert!(
            !verb(GitRow::ChangeStaged).contains(&"discard"),
            "there is nothing on disk to lose"
        );
        assert!(verb(GitRow::ChangeConflicted).contains(&"ours"));
        assert!(
            !verb(GitRow::ChangeConflicted).contains(&"stage"),
            "an unmerged file cannot be staged, and offering it advertises a failure"
        );
    }

    /// A clean repository is one row, not three empty headings — and a page with
    /// no workspace at all is still just the refs.
    #[test]
    fn a_clean_working_tree_is_one_row_on_the_git_page() {
        let mut c = changes(0, 0, RepoState::Clean);
        c.unstaged.clear();
        let git = Git::default();
        let rows = ref_rows(&git, Some(&c), Some(SessionId(1)));
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(matches!(rows[0], RefRow::WorkingTree { dirty: 0 }));
        assert!(ref_rows(&git, None, None).is_empty());
    }

    /// A rename, a creation and a deletion each say so on the card. The paths
    /// alone cannot: git names the real file on both sides of `diff --git` even
    /// when one side is `/dev/null`, so only the header knows.
    #[test]
    fn a_file_card_says_what_happened_to_the_file_itself() {
        let created = DiffView::new(
            DiffKind::Unstaged { path: None },
            "diff --git a/new.txt b/new.txt\nnew file mode 100644\nindex 0000000..3333333\n\
             --- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,1 @@\n+alpha\n",
        );
        assert_eq!(
            created.rows[0],
            DiffRow::File {
                path: "new.txt".into(),
                added: 1,
                removed: 0,
                note: Some("new file".into()),
                folded: false,
            }
        );

        let moved = DiffView::new(
            DiffKind::Unstaged { path: None },
            "diff --git a/old.txt b/new.txt\nsimilarity index 90%\nrename from old.txt\n\
             rename to new.txt\nindex 1111111..2222222 100644\n--- a/old.txt\n+++ b/new.txt\n\
             @@ -1,1 +1,1 @@\n-a\n+b\n",
        );
        let DiffRow::File { path, note, .. } = &moved.rows[0] else { panic!("not a card") };
        assert_eq!(path, "old.txt -> new.txt", "a rename has to name both ends");
        assert_eq!(note.as_deref(), Some("renamed"));
    }

    #[test]
    fn the_diff_page_marks_the_cursor_hunk_and_the_picked_lines() {
        let mut d = unstaged();
        d.line_select();
        d.pick_line();
        let mut b = buf(120, 30);
        let view = View { page: Page::Diff, ..Default::default() };
        let sys = SysDto::default();
        let scene = Scene { files: None, diff: Some(&d), ..Scene::new(&[], &sys) };
        draw(&mut b, 120, 30, &scene, &view, &Theme::default());
        let rows: Vec<String> = (0..30).map(|y| text_of(&b, y)).collect();
        let joined = rows.join("\n");
        assert!(joined.contains("diff a.txt"), "the box should be titled:\n{joined}");
        assert!(joined.contains("CHANGED-EARLY"), "{joined}");
        // The gutter marks: `#` on a picked line, `|` on the cursor's hunk.
        let gutters: String =
            rows.iter().filter_map(|r| r.chars().find(|c| *c == '#' || *c == '>')).collect();
        assert!(gutters.contains('#'), "no picked line was marked:\n{joined}");
        assert!(joined.contains("space pick"), "the hints should follow the mode:\n{joined}");
        // Fixed chrome: the rails do not move between pages. DIFF keeps them on
        // purpose — it is what is *on* the stage, and the CHANGES rail beside it
        // is how you walk to the next file.
        assert!(joined.contains("AGENTS") && joined.contains("CHANGES"), "{joined}");
    }

    #[test]
    fn the_diff_page_never_overruns_its_box() {
        let d = unstaged();
        for cols in [60u16, 80, 120, 200] {
            let mut b = buf(cols, 24);
            let view = View { page: Page::Diff, ..Default::default() };
            let sys = SysDto::default();
            let scene = Scene { files: None, diff: Some(&d), ..Scene::new(&[], &sys) };
            draw(&mut b, cols, 24, &scene, &view, &Theme::default());
            for y in 0..24 {
                assert!(text_of(&b, y).chars().count() <= cols as usize, "overran at {cols}");
            }
        }
    }

    fn stack(label: &str, workdir: &str, containers: &[(&str, &str)]) -> StackDto {
        StackDto {
            label: label.into(),
            project: label.into(),
            workdir: workdir.into(),
            running: containers.iter().filter(|(_, s)| *s == "running").count(),
            total: containers.len(),
            containers: containers
                .iter()
                .map(|(n, s)| butai_protocol::api::ContainerDto {
                    name: (*n).into(),
                    state: (*s).into(),
                })
                .collect(),
        }
    }

    fn sys_with(stacks: Vec<StackDto>) -> SysDto {
        SysDto { stacks, ..SysDto::default() }
    }

    /// The page is a workbench view, not a machine inspector: when anything is
    /// this project's, everything else goes.
    #[test]
    fn the_docker_page_keeps_this_projects_stacks_and_drops_the_rest() {
        let sys = sys_with(vec![
            stack("elsewhere", "/other", &[("e-1", "running")]),
            stack("mine", "/proj/app", &[("m-1", "running")]),
        ]);
        let stacks = project_stacks(&sys, "/proj/app");
        assert_eq!(stacks.len(), 1, "an unrelated stack should not be listed");
        assert_eq!(stacks[0].dto.label, "mine");
        assert!(stacks[0].mine);

        // A stack whose workdir is *above* the workspace counts as ours: a
        // compose file at the repo root serves a workspace opened in a subdir.
        let stacks = project_stacks(&sys, "/proj/app/crates/thing");
        assert_eq!(stacks.len(), 1);
        assert_eq!(stacks[0].dto.label, "mine");

        // And with nothing of ours, the page shows what there is rather than
        // being mysteriously empty.
        let stacks = project_stacks(&sys, "/somewhere/else");
        assert_eq!(stacks.len(), 2, "the fallback should list everything");
        assert!(stacks.iter().all(|s| !s.mine));
    }

    #[test]
    fn a_stopped_stack_is_not_listed() {
        let sys = sys_with(vec![
            stack("up", "", &[("a", "running")]),
            stack("down", "", &[("b", "exited")]),
        ]);
        let stacks = project_stacks(&sys, "/anywhere");
        assert_eq!(stacks.len(), 1);
        assert_eq!(stacks[0].dto.label, "up");
    }

    /// A one-container stack is its own header. Listing the container under it
    /// would make every standalone container two identical rows.
    #[test]
    fn a_standalone_container_is_one_row_and_a_compose_project_is_several() {
        let sys = sys_with(vec![
            stack("alone", "", &[("alone", "running")]),
            stack("app", "", &[("app-web-1", "running"), ("app-db-1", "exited")]),
        ]);
        let stacks = project_stacks(&sys, "/anywhere");
        let rows = docker_rows(&stacks);
        assert_eq!(
            rows,
            vec![
                DockerRow::Stack(0),
                DockerRow::Stack(1),
                DockerRow::Container { stack: 1, name: "app-web-1", running: true },
                DockerRow::Container { stack: 1, name: "app-db-1", running: false },
            ]
        );
    }

    /// That one row is the container, so it wears the container's dot. Without
    /// it, a machine of standalone containers was a page of bare labels whose
    /// state you could only read off the status column — and the web client has
    /// always drawn the dot there, so the TUI was the odd one out.
    #[test]
    fn a_standalone_containers_row_wears_its_status_dot() {
        let sys = sys_with(vec![
            stack("alone", "/proj", &[("alone", "running")]),
            stack("app", "/proj", &[("app-web-1", "running"), ("app-db-1", "exited")]),
        ]);
        let mut b = buf(160, 30);
        let view = View { page: Page::Docker, ..Default::default() };
        let docker = Docker::default();
        let scene = Scene { docker: Some(&docker), ..Scene::new(&[], &sys) };
        draw(&mut b, 160, 30, &scene, &view, &Theme::default());
        let screen: Vec<String> = (0..30).map(|y| text_of(&b, y)).collect();
        let row = |needle: &str| {
            screen
                .iter()
                .find(|r| r.contains(needle))
                .unwrap_or_else(|| panic!("no row holding {needle:?}:\n{}", screen.join("\n")))
                .clone()
        };
        // Columns, not byte offsets: the borders left of the label are multibyte.
        let col = |row: &str, ch: char| row.chars().position(|c| c == ch);

        let alone = row("● alone");
        let compose = row("▾ app");
        let child = row("app-web-1");
        assert!(alone.contains("● alone"), "the standalone row needs its dot: {alone:?}");
        // Both markers are two cells, so the two kinds of header line up, and a
        // container listed under its project still sits one column further in.
        assert_eq!(col(&alone, '●'), col(&compose, '▾'), "{alone:?} / {compose:?}");
        assert_eq!(
            col(&child, '●').zip(col(&alone, '●')).map(|(c, h)| c - h),
            Some(1),
            "a child container should be indented past its project: {child:?}"
        );
    }

    #[test]
    fn the_docker_page_lists_stacks_beside_a_logs_column() {
        let sys = sys_with(vec![stack(
            "app",
            "/proj",
            &[("app-web-1", "running"), ("app-db-1", "running")],
        )]);
        let mut b = buf(160, 30);
        let view = View { page: Page::Docker, ..Default::default() };
        let docker = Docker { sel: 1, ..Docker::default() };
        let scene = Scene { docker: Some(&docker), ..Scene::new(&[], &sys) };
        draw(&mut b, 160, 30, &scene, &view, &Theme::default());
        let joined: String = (0..30).map(|y| text_of(&b, y)).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("DOCKER"), "{joined}");
        assert!(joined.contains("app-web-1"), "containers should be listed:\n{joined}");
        assert!(joined.contains("up"), "a fully-running stack says up:\n{joined}");
        assert!(joined.contains("enter follow"), "the actions should be offered:\n{joined}");
        // DOCKER owns the whole band: a container's logs have nothing to do with
        // this workspace's agents or its diff, and the logs column was what the
        // rails beside it were starving. The tab bar stays, and with it the
        // spaces button — which is how you leave.
        assert!(
            !joined.contains("AGENTS") && !joined.contains("CHANGES"),
            "the workspace rails should be gone:\n{joined}"
        );
        assert!(
            joined.contains(&spaces_label(&view)),
            "the spaces button is how you leave:\n{joined}"
        );
    }

    #[test]
    fn the_docker_page_never_overruns_its_box() {
        let sys = sys_with(vec![stack(
            "a-very-long-compose-project-name",
            "/proj",
            &[("a-very-long-container-name-1", "running")],
        )]);
        let docker = Docker::default();
        for cols in [60u16, 80, 120, 200] {
            let mut b = buf(cols, 24);
            let view = View { page: Page::Docker, ..Default::default() };
            let scene = Scene { docker: Some(&docker), ..Scene::new(&[], &sys) };
            draw(&mut b, cols, 24, &scene, &view, &Theme::default());
            for y in 0..24 {
                assert!(text_of(&b, y).chars().count() <= cols as usize, "overran at {cols}");
            }
        }
    }

    /// The pane is sized to the logs column, not to the whole stage. This is
    /// the measurement that crosses the wire, so getting it wrong sends frames
    /// the wrong shape.
    #[test]
    fn the_docker_page_sizes_the_pane_to_its_logs_column() {
        let docker = View { page: Page::Docker, ..Default::default() };
        let band = page_geom(120, 40, &docker).stage_box;
        let logs = stage_rect(120, 40, &docker);
        // Measured against the band DOCKER is actually drawn in, not against
        // WORK's stage: DOCKER owns the whole width now, so its logs column is
        // *wider* than the stage between the rails, and comparing the two would
        // assert the opposite of what this is guarding.
        assert!(logs.width < band.width, "the list column should have taken some width");
        assert!(logs.x > band.x, "the pane starts after the list");
        assert!(logs.height < band.height, "the action-hint row should have taken a row");
        // And it stays inside the band at every width the rails allow.
        for cols in [60u16, 80, 120, 200] {
            let geom = page_geom(cols, 40, &docker);
            let r = stage_rect(cols, 40, &docker);
            assert!(r.x >= geom.stage_box.x, "at {cols}");
            assert!(r.x + r.width <= geom.stage_box.x + geom.stage_box.width, "at {cols}");
        }
    }

    /// The caret is counted in characters and the string is indexed in bytes.
    /// Mixing the two panics on the first accented character someone types
    /// into a commit message — which is the first thing a non-English commit
    /// message contains.
    #[test]
    fn a_prompt_edits_by_character_not_by_byte() {
        let mut p = PromptOverlay {
            title: "COMMIT".into(),
            text: String::new(),
            cursor: 0,
            kind: PromptKind::Commit { all: false },
            subtitle: None,
        };
        for c in "héllo wörld".chars() {
            p.insert(c);
        }
        assert_eq!(p.text, "héllo wörld");
        assert_eq!(p.cursor, 11, "the caret counts characters, not bytes");

        p.backspace();
        assert_eq!(p.text, "héllo wörl");
        p.to_start();
        p.delete();
        assert_eq!(p.text, "éllo wörl", "delete takes the character after the caret");
        p.move_cursor(1);
        p.insert('X');
        assert_eq!(p.text, "éXllo wörl");

        // The caret never leaves the string.
        p.move_cursor(-100);
        assert_eq!(p.cursor, 0);
        p.backspace();
        assert_eq!(p.text, "éXllo wörl", "backspace at the start is a no-op");
        p.move_cursor(100);
        assert_eq!(p.cursor, p.text.chars().count());
        p.delete();
        assert_eq!(p.text, "éXllo wörl", "delete at the end is a no-op");
    }

    #[test]
    fn a_confirm_opens_on_the_safe_answer() {
        let c = ConfirmOverlay {
            title: "DISCARD".into(),
            header: "throw away changes to a.rs".into(),
            yes: false,
            kind: ConfirmKind::Discard { path: "a.rs".into() },
        };
        assert!(!c.yes, "the box must not open on the answer that destroys work");

        let mut b = buf(100, 24);
        let view = View { overlay: Some(Overlay::Confirm(c)), ..Default::default() };
        draw_overlay_layer(&mut b, 100, 24, &view, &Theme::default());
        let joined: String = (0..24).map(|y| text_of(&b, y)).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("DISCARD"), "{joined}");
        assert!(joined.contains("throw away changes to a.rs"), "{joined}");
        assert!(joined.contains("yes") && joined.contains("no"), "{joined}");
    }

    #[test]
    fn a_prompt_draws_its_text_and_what_it_is_about() {
        let mut b = buf(100, 24);
        let view = View {
            overlay: Some(Overlay::Prompt(PromptOverlay {
                title: "COMMIT".into(),
                text: "fix the thing".into(),
                cursor: 3,
                kind: PromptKind::Commit { all: false },
                subtitle: Some("2 staged file(s)".into()),
            })),
            ..Default::default()
        };
        draw_overlay_layer(&mut b, 100, 24, &view, &Theme::default());
        let joined: String = (0..24).map(|y| text_of(&b, y)).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("COMMIT"), "{joined}");
        assert!(joined.contains("fix the thing"), "{joined}");
        assert!(joined.contains("2 staged file(s)"), "the subtitle is the check:\n{joined}");
    }

    /// The rail's rows are one list, headings included, and the cursor indexes
    /// it. If drawing and dispatch disagreed about what row 3 is, Enter would
    /// open the wrong diff.
    #[test]
    fn the_changes_rail_is_one_list_with_its_headings_in_it() {
        let mut c = changes(0, 0, RepoState::Clean);
        c.staged = vec![FileChange { path: "b.rs".into(), code: "A".into(), added: 5, deleted: 0 }];
        c.recent_commits = vec![CommitDto { id: "abcdef1234".into(), summary: "first".into() }];
        let rows = change_rows(&c);
        assert_eq!(
            rows,
            vec![
                ChangeRow::Header("Unstaged"),
                ChangeRow::File { change: &c.unstaged[0], staged: false },
                ChangeRow::Header("Staged"),
                ChangeRow::File { change: &c.staged[0], staged: true },
                ChangeRow::Header("Commits"),
                ChangeRow::Commit { id: "abcdef1234", summary: "first" },
            ]
        );
        // An empty section contributes no heading, so the indices stay tight.
        c.staged.clear();
        assert_eq!(change_rows(&c).len(), 4);
    }

    /// `clamp` panics when its low bound exceeds its high one, and on a short
    /// band `height / 2` falls under `GIT_REFS_MIN_H`. Written with the floor
    /// check *after* the clamp this crashed the TUI out of raw mode on any
    /// terminal under 14 rows — and every route onto the page reaches here, so
    /// a click, a keypress and a resize all did it too.
    #[test]
    fn the_git_columns_survive_a_short_terminal() {
        for h in 0..40u16 {
            let c = git_columns(LRect::new(0, 1, 100, h));
            assert_eq!(c.refs_box.height + c.hist_box.height, h, "at height {h}");
            // Below the two floors REFS yields entirely rather than shrinking
            // into a box with no rows in it.
            if h < GIT_REFS_MIN_H + GIT_HIST_MIN_H {
                assert_eq!(c.refs_box.height, 0, "REFS did not yield at height {h}");
            } else {
                assert!(c.refs_box.height >= GIT_REFS_MIN_H, "at height {h}");
            }
        }
    }

    /// A branch checked out in a worktree with a long directory name puts a
    /// marker wider than the column on the row. Subtracting that from the right
    /// edge underflowed: a panic in debug, a vanished marker in release.
    #[test]
    fn a_long_worktree_marker_does_not_underflow_the_column() {
        use butai_protocol::api::{BranchDto, BranchesDto};
        let git = Git {
            branches: Some(BranchesDto {
                current: Some("main".into()),
                branches: vec!["main".into(), "spike".into()],
                entries: vec![
                    BranchDto {
                        name: "main".into(),
                        remote: false,
                        upstream: None,
                        ahead: 0,
                        behind: 0,
                        tip: "0".repeat(40),
                    },
                    BranchDto {
                        name: "spike".into(),
                        remote: false,
                        upstream: None,
                        ahead: 0,
                        behind: 0,
                        tip: "1".repeat(40),
                    },
                ],
            }),
            worktrees: vec![WorktreeDto {
                path: "/x/butai-refactor-client-daemon-boundary-and-then-some".into(),
                branch: Some("spike".into()),
                head: "1".repeat(40),
                is_main: false,
                detached: false,
                locked: false,
                prunable: false,
                workspace: None,
            }],
            loaded: true,
            ..Git::default()
        };
        // The narrowest column the page will ever draw.
        let mut b = buf(60, 30);
        let cols = git_columns(LRect::new(0, 1, 50, 28));
        let view = View { page: Page::Git, focus: Focus::Refs, ..View::default() };
        draw_git_refs(&mut b, &cols, &git, None, None, &view, &Theme::default());
    }

    /// The list offset is derived from the cursor, so walking past the last
    /// visible row scrolls rather than losing the highlight off the bottom.
    /// Stored separately, `j` moved a selection nobody could see and `Enter`
    /// opened a commit that was not on screen.
    #[test]
    fn the_history_cursor_stays_on_screen() {
        let rows = 6u16;
        // Cursor above the fold: the list has not moved.
        assert_eq!(first_visible(0, rows), 0);
        assert_eq!(first_visible(5, rows), 0);
        // Past it: the offset follows, keeping the cursor on the last row.
        assert_eq!(first_visible(6, rows), 1);
        assert_eq!(first_visible(199, rows), 194);
        assert!(199 - first_visible(199, rows) < rows as usize, "cursor off screen");
    }

    #[test]
    fn a_tab_that_wants_you_is_marked() {
        let mut b = buf(100, 24);
        let tabs = [WorkspaceSummary {
            id: SessionId(1),
            name: "webapp".into(),
            cwd: "/tmp".into(),
            agents: 1,
            waiting: 1,
            working: 0,
            finished: 0,
            questions: 1,
            exited: 0,
            unread: 0,
            processes: 0,
            changes: 0,
            conflicts: 0,
            repo_state: RepoState::Clean,
            attached_clients: 1,
        }];
        let tabs = [Tab { summary: &tabs[0], host: None, live: true }];
        draw(
            &mut b,
            100,
            24,
            &scene(&tabs, None, &SysDto::default(), &[]),
            &View::default(),
            &Theme::default(),
        );
        let bar = text_of(&b, 0);
        assert!(bar.contains("1:webapp"), "{bar}");
        assert!(bar.contains('!'), "an attention tab should carry a marker: {bar}");
    }

    /// **The width is the assertion.** A chip that grew when its machine went
    /// away would slide every chip to its right, and `tab_close_span` measures
    /// this same string to find the `[x]` — so a wider label puts the close
    /// button's hit box off the button, on the one row where a stray click
    /// closes a workspace.
    #[test]
    fn a_chip_keeps_its_width_when_its_machine_goes_away() {
        let s = ws_summary("proj");
        for active in [false, true] {
            let up = tab_label(0, &Tab { summary: &s, host: None, live: true }, active);
            let down = tab_label(0, &Tab { summary: &s, host: None, live: false }, active);
            assert_eq!(
                up.chars().count(),
                down.chars().count(),
                "active={active}: {up:?} vs {down:?}"
            );
            assert!(!up.contains(TAB_AWAY_MARK), "a live chip carries no marker: {up:?}");
            assert!(down.contains(TAB_AWAY_MARK), "a downed chip carries one: {down:?}");
        }
    }

    /// A workspace on a machine that is not answering keeps its `!` — something
    /// *was* waiting there — but stops being painted as a summons.
    ///
    /// **The `!` cannot go.** It is two columns of the label, and dropping it
    /// would narrow the chip the moment a laptop closed, which is the strip-slide
    /// that [`a_chip_keeps_its_width_when_its_machine_goes_away`] exists to
    /// prevent. So staleness is said by the leading marker and by taking the
    /// urgent colour away, and this reads both off the painted cells — the
    /// character and the colour are decided in two different places.
    #[test]
    fn a_downed_chip_stops_being_painted_as_a_summons() {
        let mut s = ws_summary("webapp");
        s.waiting = 1;
        s.questions = 1;
        let theme = Theme::default();
        // On BOOTH, so no workspace chip is the active one — the active chip is
        // painted on `accent` and would answer a different question.
        let painted = |live: bool| {
            let tabs = [Tab { summary: &s, host: None, live }];
            let mut b = buf(100, 24);
            draw(
                &mut b,
                100,
                24,
                &scene(&tabs, None, &SysDto::default(), &[]),
                &View { page: Page::Booth, ..Default::default() },
                &theme,
            );
            let row = text_of(&b, 0);
            let at = row.find("1:webapp").expect("no chip on the bar") as u16;
            (row, b.cell((at, 0)).expect("chip cell").fg)
        };
        let (up, up_fg) = painted(true);
        assert!(up.contains("webapp !"), "a live tab with a question marks it: {up}");
        assert_eq!(up_fg, theme.danger, "and paints it as urgent");

        let (away, away_fg) = painted(false);
        assert!(away.contains("webapp !"), "the chip must not narrow when it goes away: {away}");
        assert_ne!(away_fg, theme.danger, "a downed tab kept its urgent colour: {away}");
        assert!(away.contains(TAB_AWAY_MARK), "and it must say it is away: {away}");
    }

    /// The notice names the machine and says how long — and only claims there is
    /// a last frame behind it when there is one. A stage opened straight onto a
    /// machine that is already down has nothing behind the card, and pointing at
    /// it would send someone looking for a screen that was never drawn.
    #[test]
    fn the_stage_notice_says_who_went_away_and_for_how_long() {
        let with = StageDown { host: Some("gpu-box"), secs: 12, has_frame: true };
        let lines: Vec<String> = stage_down_lines(&with, 0).into_iter().map(|(s, _)| s).collect();
        let text = lines.join(" | ");
        assert!(text.contains("gpu-box went away"), "{text}");
        assert!(text.contains("12s"), "the age is the point of the notice: {text}");
        assert!(text.contains("last frame"), "{text}");

        // The local daemon has no host to name, and "localhost went away" is a
        // stranger sentence than the situation deserves.
        let local = StageDown { host: None, secs: 3, has_frame: false };
        let text: String =
            stage_down_lines(&local, 0).into_iter().map(|(s, _)| s).collect::<Vec<_>>().join(" | ");
        assert!(text.contains("the daemon went away"), "{text}");
        assert!(!text.contains("last frame"), "there is nothing behind this one: {text}");
    }

    /// Minutes and hours, because "down 4s" and "down 2h10m" call for completely
    /// different reactions and a notice that flattened both to "a while" would
    /// answer neither.
    #[test]
    fn the_age_stays_readable_as_it_grows() {
        assert_eq!(down_for(4), "4s");
        assert_eq!(down_for(59), "59s");
        assert_eq!(down_for(60), "1m00s");
        assert_eq!(down_for(3599), "59m59s");
        assert_eq!(down_for(3600), "1h00m");
        assert_eq!(down_for(7 * 3600 + 25 * 60), "7h25m");
    }

    /// The notice covers the pane's cells and dims the rest of them, so what is
    /// left on the stage is legibly a photograph rather than a live screen.
    #[test]
    fn the_notice_dims_the_frame_it_covers_and_names_the_machine() {
        let view = View { page: Page::Agents, ..Default::default() };
        let (cols, rows) = (120u16, 40u16);
        let area = stage_rect(cols, rows, &view);
        let mut b = buf(cols, rows);
        // A pane's worth of live-looking text, in a colour that is not `faint`.
        let theme = Theme::default();
        for y in area.y..area.y + area.height {
            put_str(
                &mut b,
                area.x,
                y,
                &"x".repeat(area.width as usize),
                cols,
                Pen { fg: theme.ok, bg: theme.ground, bold: true },
            );
        }
        let down = StageDown { host: Some("gpu-box"), secs: 5, has_frame: true };
        draw_stage_down(&mut b, area, &down, &theme, 0);

        let text: String =
            (area.y..area.y + area.height).map(|y| text_of(&b, y)).collect::<Vec<_>>().join("\n");
        assert!(text.contains("gpu-box went away"), "the notice is not on the stage:\n{text}");
        // The corners are far from the centred card, so they are the surviving
        // frame — and they must have stopped looking live.
        let corner = b.cell((area.x, area.y)).expect("stage corner");
        assert_eq!(corner.symbol(), "x", "the last frame was cleared instead of kept");
        assert_eq!(corner.fg, theme.faint, "the last frame was left looking live");
    }
}
