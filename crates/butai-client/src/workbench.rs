//! The client-rendered workbench: the loop that ties a [`crate::daemon::Daemon`]
//! to [`crate::chrome`] and one streamed pane.
//!
//! This is the TUI as an ordinary client. It holds exactly what the web client
//! holds — REST plus an event stream for everything structured, and one framed
//! connection for the pane on the stage — and draws the rest itself.
//!
//! The shape of the loop follows from that split. Structured state arrives on
//! its own clock and only when it changed; pane cells arrive on theirs; the
//! terminal produces input; two timers drive the marquee and the sprites.
//! Anything that moves marks the screen dirty and one repaint at the top of the
//! loop reconciles it, so a burst of pane output costs one paint rather than one
//! per message.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use butai_protocol::api::{ApiEvent, ApplyTarget, WorkspaceDetail};
use butai_protocol::{
    AttachTarget, ClientMsg, Color as PColor, Encoding, FrameUpdate, InputEvent, KeyEvent, PaneId,
    ServerMsg, DETACH_SERVER_SHUTDOWN, PROTOCOL_VERSION,
};
use crossterm::style::{Attribute, SetAttribute, SetBackgroundColor, SetForegroundColor};
use crossterm::{cursor, event, queue, terminal};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::chrome::{
    self, DiffKind, DiffMode, DiffView, Docker, DockerRow, EditMode, Editor, Files, Focus,
    ListKind, ListOverlay, Overlay, Page, Theme, View,
};
use crate::daemon::{Daemon, DaemonEvent};
use crate::hit;
use crate::keymap::{Keymap, ViewVerb};
use crate::keys;
use crate::links;
use crate::selection::{self, Drag};

/// Marquee clock. Slow enough that a scrolling title is readable.
const TICK: std::time::Duration = std::time::Duration::from_millis(250);
/// Sprite clock, gated on something actually animating.
const FAST_TICK: std::time::Duration = std::time::Duration::from_millis(120);
/// How often a running client asks whether a newer release exists.
///
/// The first tick is immediate, so this is both the launch check and the
/// one that catches a release cut while a workbench has been up for days —
/// which is the normal state of one, since the daemon outlives the terminal.
/// Six hours because the answer changes about that often at best, and the
/// unauthenticated GitHub API allows sixty requests an hour per address.
const UPDATE_CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// How long between attempts to re-open a stage connection that dropped.
///
/// A second is short enough that a daemon restarting under a running client is
/// back before you finish reading the notice, and long enough that a machine
/// that is off costs one connect a second rather than one per repaint. Unlike
/// the event stream's backoff this does not grow: the far daemon is already
/// being re-dialled with a backoff of its own, and this connect is to a local
/// socket path that either answers or refuses immediately.
const STAGE_RETRY: Duration = Duration::from_secs(1);

/// One connection streaming one pane, with the pane it is pointed at.
struct Stage {
    transport: crate::conn::Transport,
    /// Whether the program in this pane asked for mouse reporting, as the last
    /// frame reported it. Decides who a drag over the stage belongs to.
    wants_mouse: bool,
    /// Where the program's cursor is, pane-relative, as the last frame reported
    /// it — `None` when the program hid it, the pane is scrolled back, or the
    /// command has exited.
    ///
    /// Only the daemon can know: it is the one parsing the program's output,
    /// and the escape sequences that move a cursor never reach this terminal.
    /// So the position crosses the wire on every frame and the client puts its
    /// own cursor there — see [`stage_caret`].
    cursor: Option<(u16, u16)>,
    /// Which daemon this connection is to. Watching only re-points within one.
    daemon: usize,
    pane: PaneId,
    buf: Buffer,
    /// When this connection dropped, if it has.
    ///
    /// **A dropped connection is not a closed pane, and the two used to be the
    /// same line.** `ServerMsg::Detached` means the program exited and there is
    /// genuinely nothing to show; end-of-stream means this client stopped
    /// hearing from a machine whose pane is almost certainly still running. Both
    /// cleared the stage, so a laptop losing its link drew a black rectangle
    /// that read as "your agent is gone".
    ///
    /// While this is `Some` the `Stage` is kept — with its last frame — and
    /// [`chrome::StageDown`] draws over it. The transport underneath is dead;
    /// [`Stage::reopen_due`] is what eventually replaces it.
    lost: Option<Instant>,
    /// When to next try re-opening. `None` while the connection is live.
    retry_at: Option<Instant>,
}

/// What to tell the user when the daemon's build is not this client's.
///
/// `None` for a matching pair, which is every ordinary run.
///
/// The *absent* case is the one this exists for. A daemon too old to send
/// `server_version` is older than the field itself, and therefore older than any
/// client able to read it — which is precisely the situation that produced this:
/// a daemon left running across an upgrade, answering a client many commits
/// ahead of it. Every symptom then points at a feature and none of them point at
/// the version, so the user goes looking for bugs that are not there.
fn skew_notice(server_version: Option<&str>) -> Option<String> {
    let mine = env!("CARGO_PKG_VERSION");
    match server_version {
        Some(theirs) if theirs == mine => None,
        Some(theirs) => {
            Some(format!("daemon is {theirs}, client is {mine} — restart it: butai kill-server"))
        }
        // Not "predates {mine}": the daemon may *be* {mine}. An unreleased fix
        // does not bump `CARGO_PKG_VERSION`, so the common case here is two
        // builds both calling themselves the same version, where only one has
        // the field.
        // What is certainly true is the comparison — a daemon missing a field
        // this client knows about was built before it.
        None => {
            Some(format!("daemon predates this client ({mine}) — restart it: butai kill-server"))
        }
    }
}

impl Stage {
    /// Open a pane connection sized to `rect`.
    async fn open(socket: &std::path::Path, pane: PaneId, rect: Rect) -> Result<Self> {
        let stream = crate::conn::connect_existing(socket).await?;
        let transport = crate::conn::into_transport(stream, Encoding::Msgpack);
        transport
            .to_server
            .send(ClientMsg::Hello {
                proto_version: PROTOCOL_VERSION,
                encoding: Encoding::Msgpack,
                cols: rect.width,
                rows: rect.height,
                target: AttachTarget::Pane { pane },
                cwd: std::env::current_dir().unwrap_or_else(|_| "/".into()),
            })
            .ok();
        Ok(Self {
            transport,
            wants_mouse: false,
            cursor: None,
            daemon: 0,
            pane,
            buf: Buffer::empty(rect),
            lost: None,
            retry_at: None,
        })
    }

    /// A stage for a pane on a machine that is not answering.
    ///
    /// Same shape as a live one so nothing downstream has to ask which it is
    /// holding, but with a transport whose far end is dropped before it is
    /// returned: sends go nowhere and there is nothing to receive. That is safe
    /// only because `lost` is set, which is what parks [`recv_stage`] on a
    /// pending future — see the warning there.
    ///
    /// This exists so switching to a tab on a downed machine names *that*
    /// machine. Keeping the previous tab's stage and marking it would put the
    /// wrong host in the notice, which is worse than a black screen: it is a
    /// screen that says something false.
    fn down(pane: PaneId, rect: Rect, now: Instant) -> Self {
        let (to_server, _) = tokio::sync::mpsc::unbounded_channel();
        let (_, from_server) = tokio::sync::mpsc::unbounded_channel();
        Self {
            transport: crate::conn::Transport { to_server, from_server },
            wants_mouse: false,
            cursor: None,
            daemon: 0,
            pane,
            buf: Buffer::empty(rect),
            lost: Some(now),
            // A connect was just attempted and refused; the next one waits.
            retry_at: Some(now + STAGE_RETRY),
        }
    }

    /// Whether any cells were ever received for this stage.
    ///
    /// A buffer of blanks is what [`Stage::down`] starts with, and telling the
    /// user that what is behind the notice is "its last frame" when nothing is
    /// behind it would be a lie the notice can avoid.
    fn has_frame(&self) -> bool {
        self.buf.content.iter().any(|c| c.symbol() != " ")
    }

    /// Mark the connection lost, keeping the cells it had.
    ///
    /// Idempotent on the timestamp: the age on the notice is time since the
    /// link went, not time since the last failed retry, and restarting it every
    /// second would leave it reading "down 0s" forever.
    fn mark_lost(&mut self, now: Instant) {
        if self.lost.is_none() {
            self.lost = Some(now);
            // The program's caret belongs to a screen we are no longer being
            // told about. Leaving it parks this terminal's cursor on a
            // photograph, blinking as though something were typing into it.
            self.cursor = None;
        }
        self.retry_at.get_or_insert(now);
    }

    /// Whether to spend another connection attempt now, arming the next wait.
    ///
    /// **The retry used to ride on the repaint.** Anything animating makes that
    /// every 120ms, each attempt wrote its own `stage: …` into the footer, and a
    /// machine that was simply off turned the one line that could have explained
    /// the situation into a strobe. Takes `now` so the interval is testable
    /// without sleeping through it.
    fn reopen_due(&mut self, now: Instant) -> bool {
        if self.lost.is_none() {
            return false;
        }
        if self.retry_at.is_some_and(|t| now < t) {
            return false;
        }
        self.retry_at = Some(now + STAGE_RETRY);
        true
    }

    /// Point at a different pane without reconnecting.
    ///
    /// The daemon answers with a full frame, so the local buffer is cleared to
    /// match: applying that frame over the previous pane's cells would leave
    /// whatever the new one does not overwrite.
    fn watch(&mut self, pane: PaneId) {
        if self.pane == pane {
            return;
        }
        self.pane = pane;
        self.buf.reset();
        // For the same reason the buffer is: the pane being left behind put the
        // cursor somewhere, and leaving it there parks a caret on the new
        // pane's screen at a position that belongs to the old one's.
        self.cursor = None;
        self.transport.to_server.send(ClientMsg::Watch { pane }).ok();
    }

    fn resize(&mut self, rect: Rect) {
        if self.buf.area == rect {
            return;
        }
        self.buf = Buffer::empty(rect);
        self.transport
            .to_server
            .send(ClientMsg::Resize { cols: rect.width, rows: rect.height })
            .ok();
    }
}

/// One daemon this client is connected to, and what to call it.
pub struct Endpoint {
    /// Tab badge. `None` for the local daemon, which needs no qualifying.
    pub host: Option<String>,
    pub socket: PathBuf,
}

/// A machine to reach over ssh at start — a remembered `[+ host]` connection.
///
/// Distinct from [`Endpoint`] because it is not reachable yet: it needs an
/// `ssh -L` forward before there is a socket to connect to, and that is slow
/// enough that it cannot happen before the first frame.
pub struct RemoteDial {
    /// ssh destination: an alias from `~/.ssh/config`, or `user@host`.
    pub target: String,
    /// Tab badge; the destination unless `name` overrode it.
    pub label: String,
    pub args: Vec<String>,
    /// `BUTAI_SOCKET` on the far side. Normally `None`.
    pub socket_path: Option<String>,
}

/// Every workspace across every connected daemon, in connection order.
///
/// Flattened rather than nested because that is what the tab bar is: one row of
/// projects, and which machine each lives on is a badge rather than a level of
/// hierarchy. It is also what makes several daemons cost the client nothing
/// structurally — a second machine is a second `Daemon` in this list, not a
/// relay in the middle.
fn tab_index(daemons: &[Daemon], hosts: &[Option<String>]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (d, daemon) in daemons.iter().enumerate() {
        let _ = &hosts;
        for (t, _) in daemon.state.tabs.iter().enumerate() {
            out.push((d, t));
        }
    }
    out
}

/// Run the client-rendered workbench until it detaches, or until somebody
/// accepts an update.
///
/// `updates` is whether this client may offer one. `butai standalone` passes
/// `false`: its daemon is in-process, so the `kill-server` an update ends with
/// would take the client down with it, and its socket is a private path that
/// goes away with the process — there is no session on the other side to come
/// back to.
pub async fn run(
    endpoints: Vec<Endpoint>,
    remotes: Vec<RemoteDial>,
    ws_name: Option<String>,
    updates: bool,
) -> Result<crate::Exit> {
    anyhow::ensure!(!endpoints.is_empty(), "no daemon to connect to");
    let mut hosts: Vec<Option<String>> = endpoints.iter().map(|e| e.host.clone()).collect();
    let mut sockets: Vec<PathBuf> = endpoints.iter().map(|e| e.socket.clone()).collect();
    // Live `ssh -L` forwards, one per adopted machine. Held for the session:
    // dropping one kills its ssh and takes the far daemon out of the tab bar.
    let mut forwards: Vec<crate::ssh::Forward> = Vec::new();
    // Machines being dialled right now, so a second announcement from the same
    // one does not open a second connection to it.
    let mut dialling: HashSet<String> = HashSet::new();
    // What each in-flight dial should do when it lands.
    let mut dial_meta: HashMap<String, DialMeta> = HashMap::new();
    // How each machine in the bar was reached, so a link that drops can be
    // rebuilt the same way. See [`DialSpec`].
    let mut dial_specs: HashMap<String, DialSpec> = HashMap::new();
    // Machines whose link has dropped, and when to try them again.
    let mut downed: HashMap<String, Downed> = HashMap::new();
    let (adopt_tx, mut adopt_rx) = unbounded_channel::<(String, Result<crate::ssh::Forward>)>();
    // The GIT page's reads, which run off the loop so they cannot freeze it.
    let (git_tx, mut git_rx) = unbounded_channel::<GitLoad>();
    // The update check, off the loop for the same reason and one more: it is
    // the only thing in a butai client that leaves the machine, so it is the
    // only one whose slowest case is a network that never answers.
    let (update_tx, mut update_rx) = unbounded_channel::<Result<Option<crate::update::Offer>>>();
    // Which GIT read is the current one. See [`GitLoad::generation`].
    let mut git_generation: u64 = 0;

    let mut daemons = Vec::new();
    let mut failures = Vec::new();
    for e in &endpoints {
        // A badge means another machine's socket, forwarded here by us or by
        // somebody else. Silence on one of those means the tunnel is down, and
        // starting a daemon on this machine is the answer to no version of
        // that — see [`crate::api::Api::remote`].
        let opened = match &e.host {
            Some(_) => Daemon::connect_remote(e.socket.clone()).await,
            None => Daemon::connect(e.socket.clone()).await,
        };
        match opened {
            Ok(mut d) => {
                // The event stream only sends what changed, so a fresh
                // subscriber knows nothing until something moves. This is the
                // one-time catch-up that makes the first frame complete.
                d.prime().await?;
                daemons.push(d);
            }
            // One unreachable machine must not stop the others: a forwarded
            // socket whose tunnel is down is the ordinary case, not a fatal one.
            Err(err) => failures.push(format!("{}: {err:#}", e.socket.display())),
        }
    }
    anyhow::ensure!(!daemons.is_empty(), "no daemon answered ({})", failures.join("; "));

    // Remembered machines are dialled *after* the local daemon has answered and
    // before the loop starts drawing, on their own tasks. Nothing waits on them:
    // one asleep machine must cost a tab that fills in late, not twenty seconds
    // of blank screen — which is the whole reason these are not `endpoints`.
    for r in remotes {
        if !should_dial(&r.target, &hosts, &dialling) {
            continue;
        }
        spawn_dial(
            r.target.clone(),
            DialMeta {
                label: r.label,
                // Already in the file; landing must not write it a second time.
                remember: false,
                reconnect: false,
                args: r.args,
                socket_path: r.socket_path,
            },
            &mut dialling,
            &mut dial_meta,
            &adopt_tx,
        );
    }

    let (mut cols, mut rows) = terminal::size().context("query terminal size")?;
    let mut view = View::default();
    // The prefix table is the user's, and this is the only thing that reads it:
    // the daemon stopped parsing `[keys]` when it stopped resolving keystrokes.
    let (config, config_warnings) = crate::config::Config::load();
    failures.extend(config_warnings);
    // The palette the file names.
    //
    // This was `Theme::default()`, which meant `[theme]` was parsed, resolved
    // and then thrown away: every client drew `blueprint-dark` whatever the
    // file said, and `:theme` answered with a flash telling you to edit a key
    // that would not have been read either. Mutable because the SETTINGS page
    // applies a palette the moment the cursor reaches it — which is the only
    // way to choose one, and needs no reload to be worth having.
    let (palette, theme_warnings) =
        crate::theme::Palette::resolve(&config.theme.name, &config.theme.role_overrides());
    failures.extend(theme_warnings);
    let mut theme = Theme::from_palette(&palette);
    // Which palette the screen is currently wearing, so a preview only costs a
    // resolve when it actually changes.
    let mut showing_theme = config.theme.name.clone();
    let (keymap, key_warnings) =
        crate::keymap::Keymap::from_config(&config.general.prefix, &config.keys);
    view.prefix = keys::key_label(&keymap.prefix);
    // What `a` spawns without asking. Held rather than re-read, so pinning takes
    // effect on the next keystroke instead of the next start.
    let mut pinned = config.general.default_agent.clone();
    view.pinned_agent = pinned.clone();
    // Whether an announcement is allowed to bring a machine in on its own. The
    // daemon owned this until it stopped dialling; the decision is this side's
    // now, so the setting has to be read here or it stops meaning anything.
    let mut auto_attach = config.general.remote_auto_attach;
    // Whether to look at all: the config key, the environment opt-out, and
    // whether this client is one that could carry an update out.
    let mut updates_enabled = updates && crate::update::enabled(config.update.check);
    // The newest release, once a check has found one. Held so `:update` and the
    // SETTINGS row can reopen the question without asking GitHub again.
    let mut update_offer: Option<crate::update::Offer> = None;
    // The version already turned down — from the file, then from this session's
    // own answer, so declining does not have to survive a reload to take effect.
    let mut declined_update = config.update.declined_version.clone();
    // Whether the box has been put up yet. The launch check raises it; a later
    // one leaves a footer notice instead, because a modal takes the keyboard
    // (see [`handle_input`]) and doing that to somebody mid-sentence in an agent
    // pane is not a thing to do unprompted.
    let mut update_prompted = false;
    // Set by `:update`: report what a silent check swallows, ask again about a
    // version that was declined, and raise the box rather than a notice.
    let mut update_forced = false;
    // Same story for `[ui]`: the daemon read it while it owned the layout. It
    // is this side's now, and `save_ui` has been writing to it all along — so
    // without this read, resizing a rail with Alt-l saved and then came back at
    // the default on the next start.
    view.geom = chrome::geom_from_config(&config.ui);
    view.net = config.ui.net.clone();
    view.disks = config.ui.disks.clone();
    view.links = config.ui.links;
    if !key_warnings.is_empty() {
        // A mistyped binding used to be a log line on the daemon. It is the
        // user's own config and the reason a key does nothing, so it says so.
        failures.push(key_warnings.join("; "));
    }
    if !failures.is_empty() {
        view.flash = Some(failures.join("; "));
    }
    if let Some(name) = ws_name {
        let index = tab_index(&daemons, &hosts);
        if let Some(i) = index.iter().position(|(d, t)| daemons[*d].state.tabs[*t].name == name) {
            view.tab = i;
        }
    }

    let _guard = crate::tui::TerminalGuard::enter()?;
    let mut input = spawn_raw_input();

    let mut stage: Option<Stage> = None;
    let mut files = Files::default();
    // The Docs page is the same widget over a filtered listing, so it is the
    // same type with its own cursor and its own open buffer: switching spaces
    // must not lose where you were in the other one.
    let mut docs = Files::default();
    let mut diff = DiffView::default();
    let mut docker = Docker::default();
    let mut git = chrome::Git::default();
    // The SETTINGS page's cursor and the lists it loads on arrival. Seeded with
    // what the file said, so the page opens showing the configuration that is
    // actually in force rather than the defaults.
    let mut settings = chrome::Settings {
        saved_theme: config.theme.name.clone(),
        auto_attach,
        remotes: config.remote.iter().map(remote_label).collect(),
        bindings: (keymap.len(), config.keys.len()),
        update_check: config.update.check,
        ..Default::default()
    };
    // The HELP page's topic and reading position. Nothing to seed and nothing to
    // load: its text is compiled into the binary, which is why the page opens
    // with no daemon in the loop and reads the same over ssh.
    let mut help = chrome::Help::default();
    // The USAGE page's roster. Loaded on arrival and on `r`, and kept when the
    // page is left so the rail badge survives — the badge is the reason the
    // page can be machine-scoped and still live in a rail of workspace views.
    let mut usage = chrome::usage::Usage::default();
    let mut drag = Drag::default();
    let mut painted = Buffer::empty(Rect::new(0, 0, cols, rows));
    // The URLs on the screen as it was last painted, from the same pass that
    // marked them up for the terminal. The picker reads this rather than
    // scanning again: a link you can see is a link that has been drawn, and two
    // scans of two different buffers would be two answers.
    let mut screen_links = links::ScreenLinks::default();
    let mut dirty = true;
    // The first tick fires immediately, so this is the launch check too —
    // one clock rather than a spawn before the loop and a timer inside it.
    let mut update_tick = tokio::time::interval(UPDATE_CHECK_EVERY);
    let mut slow = tokio::time::interval(TICK);
    let mut fast = tokio::time::interval(FAST_TICK);
    // Which workspace the page state on screen belongs to.
    //
    // The trees, the diff and the docker cursor are all *about* one workspace,
    // and none of them lives in `View` — so `reset_sel`, which is where "the tab
    // changed, drop what belonged to the old one" is written, cannot reach them.
    // Nor can the seven places that move the tab: a click, `alt-1`..`alt-9`,
    // `alt-<`/`alt->`, a tab closing under you, a machine disconnecting.
    // Noticing the change here covers all of them at once, and covers the ones
    // that do not exist yet.
    //
    // Keyed on the workspace's identity rather than the tab index, because
    // closing a tab shifts every index after it: the same number is then a
    // different project, which is exactly when stale contents would be worst.
    let mut showing = active_ws_key(&daemons, &hosts, &view);
    // Which page the loop last saw, so *arriving* on one can do work. Noticed
    // here for the same reason `showing` is: there are ten ways to reach a page
    // — the spaces menu, `alt-g`, `alt-,`/`alt-.`, a click, a jump from another
    // page — and putting the fetch on each of them means the eleventh forgets.
    let mut showed_page = view.page;

    let reason = loop {
        let now_showing = active_ws_key(&daemons, &hosts, &view);
        if view.page != showed_page {
            // Leaving SETTINGS with a theme list open would strand the preview:
            // the palette on screen is whichever row the cursor was on, and
            // nothing would ever recompute it. Closing the list here puts the
            // file's own theme back, whichever of the ten ways out was taken.
            if showed_page == Page::Settings && settings.open.take().is_some() {
                let (p, _) = crate::theme::Palette::resolve(
                    &settings.saved_theme,
                    &config.theme.role_overrides(),
                );
                theme = Theme::from_palette(&p);
                showing_theme = settings.saved_theme.clone();
                dirty = true;
            }
            showed_page = view.page;
            // GIT reads six endpoints, so it loads when you arrive rather than
            // on a timer: nothing here changes unless you or an agent runs git,
            // and `r` re-reads it.
            if view.page == Page::Git && !git.loaded {
                spawn_git_refresh(&daemons, &hosts, &view, &mut git, &mut git_generation, &git_tx);
                dirty = true;
            }
            // SETTINGS reads two lists on arrival: the palettes that resolve
            // right now, which means scanning `~/.butai/themes`, and the agent
            // types this daemon has. Neither changes unless a file does, so a
            // timer would be asking a question nobody changed the answer to.
            if view.page == Page::Usage && !usage.loaded {
                if let Some(err) = refresh_usage(&daemons, &hosts, &view, &mut usage).await {
                    view.flash = Some(err);
                }
                // Marked loaded either way: a page that retried a failing call
                // on every frame would hammer the socket for as long as it was
                // open. `r` is the retry.
                usage.loaded = true;
                dirty = true;
            }
            if view.page == Page::Settings && !settings.loaded {
                settings.themes = crate::theme::available();
                settings.agents = match daemons[active_daemon(&daemons, &hosts, &view)]
                    .api
                    .get_as::<Vec<String>>("/v1/agents")
                    .await
                {
                    Ok(names) => names,
                    // Not fatal: every other row on the page still works, and
                    // the one that does not says "none configured" rather than
                    // throwing the page away over it.
                    Err(e) => {
                        view.flash = Some(format!("agents: {e:#}"));
                        Vec::new()
                    }
                };
                settings.loaded = true;
                dirty = true;
            }
        }
        if now_showing != showing {
            showing = now_showing;
            // Leaving this up was the bug: the chip and the footer moved to the
            // new workspace while the tree went on listing the old one's files,
            // so clicking the tab read as having done nothing at all.
            files = Files::default();
            docs = Files::default();
            diff = DiffView::default();
            docker.sel = 0;
            // The GIT page is about *this* repository; keeping its branches
            // across a tab change would list another project's.
            git = chrome::Git::default();
            if view.page == Page::Git {
                spawn_git_refresh(&daemons, &hosts, &view, &mut git, &mut git_generation, &git_tx);
            }
            if view.page.is_tree() {
                match fetch_dir(&daemons, &hosts, &view, view.page, "").await {
                    Ok(entries) => {
                        let entries = tree_rows(view.page, entries, "");
                        page_tree(view.page, &mut files, &mut docs).entries = entries;
                    }
                    Err(e) => view.flash = Some(format!("tree: {e:#}")),
                }
            }
            dirty = true;
        }
        if dirty {
            let rect = chrome::stage_rect(cols, rows, &view);
            // Follow the stage. `watch` is what makes this cost no reconnect —
            // switching panes is a message, not a redial.
            match (current_stage(&daemons, &hosts, &view, &docker), stage.as_mut()) {
                // Re-point only within the same daemon; a tab on another
                // machine is a different socket, so that is a reconnect.
                (Some((d, pane)), Some(s)) if s.daemon == d && s.lost.is_none() => {
                    s.resize(rect);
                    s.watch(pane);
                }
                // The same daemon, but the link to it dropped. Retry on a clock
                // of its own rather than on the repaint, and keep the old
                // `Stage` — and its cells — until one succeeds.
                (Some((d, pane)), Some(s)) if s.daemon == d => {
                    if s.reopen_due(Instant::now()) {
                        if let Ok(mut new) = Stage::open(&sockets[d], pane, rect).await {
                            new.daemon = d;
                            stage = Some(new);
                        }
                        // A failure is silent. The notice on the stage already
                        // says the machine is away and how long for, which is
                        // strictly more than `stage: connect to …` told anyone,
                        // and it says it without overwriting the footer once a
                        // second for as long as the laptop stays shut.
                    }
                }
                // A different machine, or nothing staged yet. Either way this is
                // a fresh connection, and one that fails leaves a marked stage
                // for the machine that was asked for — so opening a tab on a
                // laptop that is shut explains itself instead of going black.
                (Some((d, pane)), _) => {
                    let mut new = match Stage::open(&sockets[d], pane, rect).await {
                        Ok(s) => s,
                        Err(_) => Stage::down(pane, rect, Instant::now()),
                    };
                    new.daemon = d;
                    stage = Some(new);
                }
                (None, _) => stage = None,
            }
            // Before painting, so the section is sized for the machine that is
            // about to be drawn — and so the pointer, which resolves against
            // this same list between frames, is testing the rail that is
            // actually on screen.
            sync_gauges(&mut view, &daemons, &hosts);
            screen_links = paint(
                &mut painted,
                cols,
                rows,
                &daemons,
                &hosts,
                &view,
                &theme,
                stage.as_ref(),
                Some(&files),
                Some(&docs),
                Some(&diff),
                Some(&docker),
                Some(&git),
                Some(&settings),
                Some(&help),
                Some(&usage),
                &drag,
            )?;
            dirty = false;
        }

        tokio::select! {
            ev = next_any_event(&mut daemons) => match ev {
                // The stream task retries the socket for as long as the client
                // runs, which is the whole answer when the far daemon merely
                // restarted. It is no answer at all when the *forward* died:
                // that socket was ssh's, it went with it, and nothing else
                // re-runs `ssh -L`. So a link of ours gets rebuilt here.
                Some((d, DaemonEvent::Lost(why))) => {
                    view.flash = Some(format!("daemon: {why}"));
                    redial_lost(
                        d,
                        &hosts,
                        &sockets,
                        &mut forwards,
                        &dial_specs,
                        &mut downed,
                        &mut dialling,
                        &mut dial_meta,
                        &mut view,
                        &adopt_tx,
                    );
                    dirty = true;
                }
                // Back — by our re-dial, or by the far daemon returning on a
                // socket that was there all along. Either way the machine is
                // answering, so the backoff starts over.
                Some((d, DaemonEvent::Connected)) => {
                    if let Some(host) = hosts.get(d).and_then(Option::as_ref) {
                        downed.remove(host);
                    }
                    dirty = true;
                }
                // A `butai` typed after `ssh` announced where it is. The daemon
                // detected it — only it can, it reads every byte the pane
                // writes — and this client decides what to do about it, which
                // is to put that machine in *its* tab bar.
                Some((_, DaemonEvent::Api(event))) => {
                    if let ApiEvent::RemoteAnnounce(a) = &*event {
                        // Dialled on its own task. An ssh connection is seconds
                        // of DNS, TCP and key exchange, and doing it here would
                        // stop the screen repainting and the keyboard
                        // responding for all of them — which is exactly what
                        // the first live run of this did.
                        match announced_target(a) {
                            Ok(target)
                                if announcement_dials(
                                    target,
                                    &hosts,
                                    &dialling,
                                    auto_attach,
                                ) => {
                                view.flash = Some(format!("{target} announced itself — connecting"));
                                spawn_dial(
                                    target.to_string(),
                                    DialMeta {
                                        label: target.to_string(),
                                        // Adopted, not adopted-and-kept: an
                                        // announcement is a machine you
                                        // happened to ssh into, not one you
                                        // chose to keep.
                                        remember: false,
                                        reconnect: false,
                                        args: a.ssh_args.clone(),
                                        socket_path: Some(a.socket.clone()),
                                    },
                                    &mut dialling,
                                    &mut dial_meta,
                                    &adopt_tx,
                                );
                            }
                            // Already here, or already on its way. A pane can
                            // announce more than once and each one must not add
                            // a tab.
                            Ok(_) => {}
                            Err(e) => view.flash = Some(format!("{e:#}")),
                        }
                    }
                    dirty = true;
                }
                None => break crate::Exit::Detached("daemon connection lost".to_string()),
            },
            msg = recv_stage(stage.as_mut()) => match msg {
                Some(ServerMsg::Frame(frame)) => {
                    if let Some(s) = stage.as_mut() {
                        s.wants_mouse = frame.wants_mouse;
                        s.cursor = frame.cursor;
                        apply_frame(&mut s.buf, &frame);
                    }
                    dirty = true;
                }
                Some(ServerMsg::Bell) => {
                    let mut out = io::stdout();
                    out.write_all(b"\x07").ok();
                    out.flush().ok();
                }
                Some(ServerMsg::Error(e)) => {
                    view.flash = Some(e);
                    dirty = true;
                }
                // A pane that went away leaves the stage empty rather than the
                // client wedged on a dead connection. The pane exited, the
                // workspace closed, we asked to detach — the thing being
                // watched is gone, and an empty stage is the honest picture.
                Some(ServerMsg::Detached { reason }) if reason != DETACH_SERVER_SHUTDOWN => {
                    stage = None;
                    dirty = true;
                }
                // The daemon going down, and end of stream, are the same event:
                // nobody said the *pane* was going. It is very likely still
                // there — a `kill-server` restores every workspace on the next
                // start, and a link that dropped never touched the far machine
                // at all — so the last frame stays on screen under a notice
                // rather than being cleared to a black rectangle.
                //
                // Both arms are needed. A daemon shutting down says so first
                // and closes second; one that is killed, or a forward that
                // dies, only ever produces the silence.
                Some(ServerMsg::Detached { .. }) | None => {
                    if let Some(s) = stage.as_mut() {
                        s.mark_lost(Instant::now());
                    }
                    dirty = true;
                }
                // The daemon names its own build in the handshake. A mismatch
                // is not fatal — the wire is additive and most of it still
                // works — but it is the whole difference between "butai is
                // broken" and "restart the daemon", and the client is the only
                // one holding both numbers.
                Some(ServerMsg::Hello { server_version, .. }) => {
                    if let Some(notice) = skew_notice(server_version.as_deref()) {
                        view.flash = Some(notice);
                        dirty = true;
                    }
                }
                Some(_) => {}
            },
            Some(load) = git_rx.recv() => {
                // Anything but the newest answer is about a repository, a scope
                // or a tab the page has already left.
                if load.generation == git_generation {
                    let ws = active_workspace(&daemons, &hosts, &view).cloned();
                    let changes = ws.as_ref().and_then(|w| w.changes.clone());
                    apply_git_load(&mut git, load, changes.as_ref(), ws.as_ref().map(|w| w.id));
                    dirty = true;
                }
            }
            Some((target, dialled)) = adopt_rx.recv() => {
                dialling.remove(&target);
                let meta = dial_meta.remove(&target).unwrap_or_else(|| DialMeta {
                    label: target.clone(),
                    remember: false,
                    reconnect: false,
                    args: Vec::new(),
                    socket_path: None,
                });
                match dialled {
                    Ok(forward) => {
                        let socket = forward.socket().to_path_buf();
                        // Hold the forward for the session: dropping it kills
                        // the ssh and takes the far daemon out of the bar.
                        forwards.push(forward);
                        match Daemon::connect_remote(socket.clone()).await {
                            Ok(mut d) => match d.prime().await {
                                Ok(()) => {
                                    // Where the machine already sits, when this
                                    // is rebuilding a link rather than adding a
                                    // machine. `None` if it was disconnected by
                                    // hand while the dial was in flight, which
                                    // makes this an ordinary adoption again.
                                    let slot = meta.reconnect.then(|| {
                                        hosts.iter().position(|h| h.as_deref() == Some(&*meta.label))
                                    }).flatten();
                                    // Written only once the machine has actually
                                    // answered. Remembering a host that turned
                                    // out not to have a daemon on it would put a
                                    // failure in the config and re-run it every
                                    // morning.
                                    let kept = if meta.remember {
                                        match crate::config::Config::save_remote(
                                            None,
                                            &target,
                                            &[],
                                        ) {
                                            Ok(()) => " — remembered",
                                            Err(e) => {
                                                tracing::warn!("save remote: {e}");
                                                ""
                                            }
                                        }
                                    } else {
                                        ""
                                    };
                                    // Reached, so it can be reached again the
                                    // same way. Recorded here rather than at the
                                    // dial because a spec for a machine that
                                    // never answered is a reconnect loop.
                                    dial_specs.insert(
                                        meta.label.clone(),
                                        DialSpec {
                                            target: target.clone(),
                                            args: meta.args.clone(),
                                            socket_path: meta.socket_path.clone(),
                                        },
                                    );
                                    downed.remove(&meta.label);
                                    match slot {
                                        // **In place**: replacing the entries
                                        // rather than removing and appending is
                                        // what keeps the tab where it was.
                                        // `Vec::remove` would shift every index
                                        // after it, moving other machines' tabs
                                        // and invalidating `view.tab` and
                                        // `view.browse_daemon` — for a machine
                                        // that never actually left the bar.
                                        //
                                        // Dropping the old `Daemon` here also
                                        // drops its event-stream receiver, which
                                        // is what stops the task still retrying
                                        // the socket ssh took with it.
                                        Some(at) => {
                                            daemons[at] = d;
                                            sockets[at] = socket;
                                            view.flash =
                                                Some(format!("{} is back", meta.label));
                                        }
                                        None => {
                                            view.flash = Some(format!(
                                                "{} connected — its projects are in the tab bar{kept}",
                                                meta.label
                                            ));
                                            daemons.push(d);
                                            hosts.push(Some(meta.label));
                                            sockets.push(socket);
                                        }
                                    }
                                }
                                Err(e) => view.flash = Some(format!("{}: {e:#}", meta.label)),
                            },
                            Err(e) => view.flash = Some(format!("{}: {e:#}", meta.label)),
                        }
                    }
                    Err(e) => view.flash = Some(format!("{e:#}")),
                }
                dirty = true;
            }
            Some(ev) = input.recv() => {
                match handle_input(
                    ev,
                    &mut view,
                    &daemons,
                    &hosts,
                    stage.as_ref(),
                    &mut files,
                    &mut docs,
                    &mut diff,
                    &mut docker,
                    &mut git,
                    &mut settings,
                    &mut help,
                    &mut usage,
                    &keymap,
                    &mut drag,
                    config.general.option_as_alt,
                    stage.as_ref().is_some_and(|s| s.wants_mouse),
                    &mut cols,
                    &mut rows,
                ) {
                    Flow::Continue => dirty = true,
                    Flow::Detach => break crate::Exit::Detached("detached".to_string()),
                    Flow::Update => match update_offer.take() {
                        Some(offer) => break crate::Exit::Update(offer),
                        // The offer went away under the box — a `:update` that
                        // found nothing, most likely. Nothing to do but close.
                        None => dirty = true,
                    },
                    Flow::DeclineUpdate(version) => {
                        match crate::config::Config::save_declined_version(&version) {
                            Ok(()) => {
                                view.flash = Some(format!(
                                    "butai {version} declined — `butai update` to change your mind"
                                ));
                            }
                            // A config that cannot be written is worth saying
                            // out loud: the answer looked like it took, and on
                            // the next start it would ask again.
                            Err(e) => view.flash = Some(format!("update: {e}")),
                        }
                        // Either way it does not ask again *this* session.
                        declined_update = Some(version);
                        update_offer = None;
                        dirty = true;
                    }
                    Flow::CheckUpdate => {
                        match &update_offer {
                            // Already know about one: reopen the question
                            // rather than asking GitHub the same thing twice.
                            Some(offer) => {
                                view.overlay = Some(update_overlay(&offer.version));
                                view.flash = Some(format!(
                                    "no won't ask again for {} — esc asks next launch",
                                    offer.version
                                ));
                            }
                            None if updates => {
                                view.flash = Some("checking for updates…".to_string());
                                update_forced = true;
                                spawn_update_check(&update_tx);
                            }
                            None => {
                                view.flash =
                                    Some("standalone does not update itself".to_string());
                            }
                        }
                        dirty = true;
                    }
                    Flow::PickAgent => {
                        match agent_picker(&daemons, &hosts, &view, pinned.as_deref()).await {
                            Ok(overlay) => view.overlay = Some(overlay),
                            Err(e) => view.flash = Some(format!("agents: {e:#}")),
                        }
                        dirty = true;
                    }
                    Flow::SpawnAgent(name) => {
                        match spawn_agent(&daemons, &hosts, &view, &name).await {
                            Ok(pane) => stage_new_pane(&mut view, pane),
                            Err(e) => view.flash = Some(format!("{e:#}")),
                        }
                        dirty = true;
                    }
                    // The pin short-circuits the picker; without one this is
                    // the picker, which is what makes `a` one key rather than
                    // one key whose meaning you have to remember.
                    Flow::NewAgent => {
                        match pinned.clone() {
                            Some(name) => {
                                match spawn_agent(&daemons, &hosts, &view, &name).await {
                                    Ok(pane) => stage_new_pane(&mut view, pane),
                                    Err(e) => view.flash = Some(format!("{e:#}")),
                                }
                            }
                            None => {
                                match agent_picker(&daemons, &hosts, &view, None).await {
                                    Ok(overlay) => view.overlay = Some(overlay),
                                    Err(e) => view.flash = Some(format!("agents: {e:#}")),
                                }
                            }
                        }
                        dirty = true;
                    }
                    // Everything the SETTINGS page does lands here, because
                    // every one of them either writes the config file or
                    // repaints the screen in a different palette — and the key
                    // handler owns neither.
                    Flow::SettingsEdit(edit) => {
                        use chrome::settings::Edit;
                        match edit {
                            Edit::Moved => {}
                            Edit::Theme(name) => {
                                settings.saved_theme = name.clone();
                                match crate::config::Config::save_theme_name(&name) {
                                    Ok(()) => view.flash = Some(format!("theme: {name}")),
                                    Err(e) => view.flash = Some(format!("config.toml: {e}")),
                                }
                            }
                            Edit::DefaultAgent(name) => {
                                match pin_agent(&daemons, &hosts, &view, name).await {
                                    Ok(next) => {
                                        view.flash = Some(match &next {
                                            Some(n) => format!("{n} is pinned — A still picks"),
                                            None => "unpinned — a asks again".into(),
                                        });
                                        view.pinned_agent = next.clone();
                                        pinned = next;
                                    }
                                    Err(e) => view.flash = Some(format!("{e:#}")),
                                }
                            }
                            Edit::AutoAttach(on) => {
                                settings.auto_attach = on;
                                auto_attach = on;
                                if let Err(e) = crate::config::Config::save_remote_auto_attach(on) {
                                    view.flash = Some(format!("config.toml: {e}"));
                                }
                            }
                            // Straight onto the view: the painter reads it
                            // every frame, so the next one is already drawn the
                            // new way — and the file is written so it stays
                            // that way tomorrow.
                            Edit::Links(on) => {
                                view.links = on;
                                if let Err(e) = crate::config::Config::save_links(on) {
                                    view.flash = Some(format!("config.toml: {e}"));
                                }
                            }
                            // The clock keeps ticking either way; the flag
                            // is what its arm consults, so turning the check
                            // off takes effect at the next tick rather than at
                            // the next start.
                            Edit::UpdateCheck(on) => {
                                settings.update_check = on;
                                updates_enabled = updates && crate::update::enabled(on);
                                if let Err(e) = crate::config::Config::save_update_check(on) {
                                    view.flash = Some(format!("config.toml: {e}"));
                                }
                            }
                            Edit::Geom => {
                                if let Err(e) = crate::config::Config::save_ui(view.geom) {
                                    view.flash = Some(format!("config.toml: {e}"));
                                }
                            }
                        }
                        // The palette on screen is a function of where the
                        // cursor is: an open theme list previews the row it is
                        // on, and closing it without choosing must put the file's
                        // own theme back. Recomputed after every edit rather
                        // than at each of the six places one can move.
                        let want = settings_palette(&settings, &view);
                        if want != showing_theme {
                            let (p, _) = crate::theme::Palette::resolve(
                                &want,
                                &config.theme.role_overrides(),
                            );
                            theme = Theme::from_palette(&p);
                            showing_theme = want;
                        }
                        dirty = true;
                    }
                    // The reference, as a page of its own. It remembers where it
                    // was entered from for the reason SETTINGS does — you did
                    // not navigate here, you looked something up, and the way
                    // out is back to what you were doing.
                    Flow::OpenHelp => {
                        help.ret = view.page;
                        view.page = Page::Help;
                        dirty = true;
                    }
                    Flow::CloseHelp => {
                        view.page = help.ret;
                        dirty = true;
                    }
                    Flow::OpenSettings => {
                        settings.ret = view.page;
                        settings.saved_theme = showing_theme.clone();
                        view.page = Page::Settings;
                        dirty = true;
                    }
                    Flow::CloseSettings => {
                        view.page = settings.ret;
                        dirty = true;
                    }
                    Flow::PinAgent(name) => {
                        match pin_agent(&daemons, &hosts, &view, name).await {
                            Ok(next) => {
                                view.flash = Some(match &next {
                                    Some(n) => format!("{n} is pinned — A still picks"),
                                    None => "unpinned — a asks again".into(),
                                });
                                view.pinned_agent = next.clone();
                                pinned = next;
                            }
                            Err(e) => view.flash = Some(format!("{e:#}")),
                        }
                        dirty = true;
                    }
                    // Straight down the connection already streaming the pane.
                    // A route would be a second way to say the same thing to
                    // the same pane over a second socket.
                    Flow::Scroll(pages) => {
                        match stage.as_ref() {
                            Some(s) => {
                                let cmd = butai_protocol::Command::ScrollPage(pages);
                                s.transport.to_server.send(ClientMsg::Command(cmd)).ok();
                            }
                            None => view.flash = Some("nothing on the stage".into()),
                        }
                        dirty = true;
                    }
                    Flow::PasteImage => {
                        // Read here rather than answering a daemon that asked:
                        // this machine is the one with the clipboard, and the
                        // round trip existed only because the daemon used to
                        // own the keybinding.
                        match (crate::clipboard::image_as_put_file(), stage.as_ref()) {
                            (Ok(cmd), Some(s)) => {
                                s.transport.to_server.send(ClientMsg::Command(cmd)).ok();
                            }
                            (Ok(_), None) => view.flash = Some("nothing on the stage".into()),
                            (Err(why), _) => view.flash = Some(why),
                        }
                        dirty = true;
                    }
                    Flow::GitMenu => {
                        view.overlay =
                            Some(git_menu_overlay(None, active_workspace(&daemons, &hosts, &view)));
                        dirty = true;
                    }
                    Flow::PickHost => {
                        view.overlay =
                            Some(host_picker(&hosts, &sockets, &forwards, &dialling));
                        dirty = true;
                    }
                    Flow::PickSpace => {
                        view.overlay = Some(space_picker(
                            &view,
                            active_workspace(&daemons, &hosts, &view),
                            Some(&usage),
                        ));
                        dirty = true;
                    }
                    Flow::DialHost(target) => {
                        connect_machine(
                            target,
                            &hosts,
                            &mut view,
                            &mut dialling,
                            &mut dial_meta,
                            &adopt_tx,
                        );
                        dirty = true;
                    }
                    // The three that need the tab bar the loop holds: which
                    // workspace is active, where it lives, and how many there
                    // are across every connected daemon.
                    Flow::BrowseHere => {
                        let here = active_workspace(&daemons, &hosts, &view)
                            .map(|w| w.cwd.clone())
                            .unwrap_or_default();
                        browse_into(&daemons, &hosts, &mut view, &here).await;
                        dirty = true;
                    }
                    Flow::AskCloseWorkspace => {
                        if let Some(ws) = active_workspace(&daemons, &hosts, &view) {
                            view.overlay = Some(close_workspace_confirm(ws));
                        }
                        dirty = true;
                    }
                    Flow::GoTab(m) => {
                        go_tab(&mut view, m, tab_index(&daemons, &hosts).len());
                        dirty = true;
                    }
                    // The same thing Enter does on the focused rail, reached by
                    // clicking the row that was already selected.
                    Flow::StageSelected => {
                        // No call: which pane this client looks at is this
                        // client's business, and the next repaint re-points the
                        // open connection with `watch`.
                        if let Some(pane) = selected_pane(&daemons, &hosts, &view) {
                            view.staged = Some(pane);
                            view.focus = Focus::Stage;
                            // Staging means "show me this", which is only true
                            // on the agents page. Reached from a diff or a tree it
                            // changed what the stage held and left the
                            // full-screen page in front of it, so the click
                            // looked like it had done nothing.
                            //
                            // Done here rather than at each caller because the
                            // click and the Enter path both arrive here, and a
                            // third would have to remember.
                            view.page = Page::Agents;
                        }
                        dirty = true;
                    }
                    Flow::OpenFleetAgent(sel) => {
                        if !open_fleet_agent(&daemons, &hosts, &mut view, sel) {
                            view.flash = Some("that agent has gone".into());
                        }
                        dirty = true;
                    }
                    Flow::OpenSelectedDiff => {
                        let ws = active_workspace(&daemons, &hosts, &view);
                        if let Some(kind) = diff_under_cursor(ws, view.changes_sel) {
                            match fetch_diff(&daemons, &hosts, &view, &kind).await {
                                Ok(text) => {
                                    diff = DiffView::new(kind, &text);
                                    diff.set_view_rows(chrome::diff_body_rows(cols, rows, &view));
                                    view.page = Page::Diff;
                                }
                                Err(e) => view.flash = Some(format!("diff: {e:#}")),
                            }
                        }
                        dirty = true;
                    }
                    // Entering snapshots the geometry; leaving writes it out,
                    // but only if it moved — an accidental Alt-l should not
                    // rewrite the user's config file.
                    Flow::ToggleLayout => {
                        match view.layout.take() {
                            Some(before) if before != view.geom => {
                                if let Err(e) = crate::config::Config::save_ui(view.geom) {
                                    view.flash = Some(format!("rail geometry not saved: {e}"));
                                } else {
                                    view.flash = Some("layout saved".into());
                                }
                            }
                            Some(_) => {}
                            None => view.layout = Some(view.geom),
                        }
                        dirty = true;
                    }
                    // The links are the ones on the screen as it stands, which
                    // is why the list is built here and not in the key handler:
                    // this is where the painted buffer lives.
                    //
                    // Nothing to offer is said out loud. A picker that opens
                    // empty reads as a broken key, and "no links on this
                    // screen" is a fact about the screen rather than a failure.
                    Flow::PickLinks => {
                        if screen_links.is_empty() {
                            view.flash = Some("no links on this screen".into());
                        } else {
                            view.overlay = Some(Overlay::List(chrome::ListOverlay {
                                title: if links::can_open() {
                                    "LINKS — enter opens · y copies".into()
                                } else {
                                    // No browser here, so Enter copies too.
                                    // Said in the title rather than discovered
                                    // by pressing it: this is the ssh case, and
                                    // it is the normal one for a TUI.
                                    "LINKS — no browser here · enter copies".into()
                                },
                                items: screen_links.urls().to_vec(),
                                values: None,
                                sel: 0,
                                kind: chrome::ListKind::Links,
                            }));
                        }
                        dirty = true;
                    }
                    Flow::CopyLink(url) => {
                        crate::tui::set_clipboard(&url).ok();
                        view.flash = Some(format!("copied {url}"));
                        dirty = true;
                    }
                    // The text only exists on the painted screen, so the copy
                    // happens here rather than where the drag ended.
                    Flow::CopySelection => {
                        if let Some(text) = drag.finish(&painted) {
                            let lines = text.lines().count();
                            crate::tui::set_clipboard(&text).ok();
                            view.flash = Some(match lines {
                                1 => "copied 1 line".into(),
                                n => format!("copied {n} lines"),
                            });
                            dirty = true;
                        }
                    }
                    Flow::RestartProcess => {
                        match restart_process(&daemons, &hosts, &mut view).await {
                            Ok(()) => {}
                            Err(e) => view.flash = Some(format!("{e:#}")),
                        }
                        dirty = true;
                    }
                    Flow::CloseStagePane => {
                        match stage.as_ref().map(|s| s.pane) {
                            Some(pane) => {
                                if let Err(e) =
                                    kill_process(&daemons, &hosts, &view, pane).await
                                {
                                    view.flash = Some(format!("{e:#}"));
                                }
                            }
                            None => view.flash = Some("nothing on the stage".into()),
                        }
                        dirty = true;
                    }
                    // The row under the cursor, not the one on the stage: `x`
                    // is a verb of the list it is drawn under, and the two are
                    // routinely different rows.
                    Flow::KillSelected => {
                        match selected_route(&daemons, &hosts, &view) {
                            Some(at) => {
                                if let Err(e) = kill_pane(&daemons, at).await {
                                    view.flash = Some(format!("{e:#}"));
                                }
                            }
                            None => view.flash = Some("nothing selected".into()),
                        }
                        dirty = true;
                    }
                    Flow::Control(cmd) => {
                        let d = active_daemon(&daemons, &hosts, &view);
                        match crate::conn::control_request(&sockets[d], cmd).await {
                            Ok(ServerMsg::Error(e)) => view.flash = Some(e),
                            Ok(_) => {}
                            Err(e) => view.flash = Some(format!("{e:#}")),
                        }
                        dirty = true;
                    }
                    Flow::ListDir(dir) => {
                        load_tree(&daemons, &hosts, &mut view, &mut files, &mut docs, dir).await;
                        dirty = true;
                    }
                    Flow::OpenFile(path) => {
                        // Opening over a changed buffer would lose it silently,
                        // so it refuses once and arms the discard, exactly as
                        // closing does.
                        let page = view.page;
                        let blocked = page_tree(page, &mut files, &mut docs)
                            .open
                            .as_mut()
                            .is_some_and(|f| !f.may_close());
                        if !blocked {
                            match fetch_file(&daemons, &hosts, &view, &path).await {
                                Ok(open) => {
                                    page_tree(page, &mut files, &mut docs).open = Some(open)
                                }
                                Err(e) => view.flash = Some(format!("file: {e:#}")),
                            }
                        }
                        dirty = true;
                    }
                    Flow::OpenFileAt { path, line } => {
                        let blocked = files.open.as_mut().is_some_and(|f| !f.may_close());
                        // A search hit is always a code file, so it lands on
                        // the Files page whichever space you searched from.
                        view.page = Page::Files;
                        if !blocked {
                            match fetch_file(&daemons, &hosts, &view, &path).await {
                                Ok(mut open) => {
                                    // Put the match a few rows down rather than
                                    // at the very top, so the lines above it —
                                    // which are usually why you were looking —
                                    // are on screen too.
                                    if let Some(n) = line {
                                        open.scroll = (n as usize).saturating_sub(4);
                                    }
                                    files.open = Some(open);
                                }
                                Err(e) => view.flash = Some(format!("file: {e:#}")),
                            }
                        }
                        dirty = true;
                    }
                    Flow::SaveFile => {
                        let tree = page_tree(view.page, &mut files, &mut docs);
                        if let Some(open) = tree.open.as_mut() {
                            let (path, contents) = (open.path.clone(), open.contents());
                            match save_file(&daemons, &hosts, &view, &path, &contents).await {
                                Ok(()) => open.saved(),
                                Err(e) => open.notice = Some(format!("save failed: {e:#}")),
                            }
                        }
                        dirty = true;
                    }
                    Flow::DeleteFile(path) => {
                        match delete_file(&daemons, &hosts, &view, &path).await {
                            Ok(()) => {
                                // The viewer is showing a file that no longer
                                // exists, so it goes with it. Only when it is
                                // *that* file: deleting one row should not
                                // close a different file you were reading.
                                let page = view.page;
                                let tree = page_tree(page, &mut files, &mut docs);
                                if tree.open.as_ref().is_some_and(|o| o.path == path) {
                                    tree.open = None;
                                }
                                // Re-list rather than drop the row locally: the
                                // listing also carries the `changed` markers,
                                // and deleting a tracked file changes them.
                                let dir = tree.dir.clone();
                                load_tree(&daemons, &hosts, &mut view, &mut files, &mut docs, dir)
                                    .await;
                            }
                            Err(e) => view.flash = Some(format!("delete: {e:#}")),
                        }
                        dirty = true;
                    }
                    Flow::GitRefresh => {
                        spawn_git_refresh(&daemons, &hosts, &view, &mut git, &mut git_generation, &git_tx);
                        dirty = true;
                    }
                    Flow::ConfirmPick { target, value } => {
                        // Named in the question: "DELETE BRANCH" over the
                        // branch's own name is the only form of the question
                        // that says what is about to go.
                        view.overlay = Some(Overlay::Confirm(chrome::ConfirmOverlay {
                            title: target.title().into(),
                            header: value.clone(),
                            yes: false,
                            kind: chrome::ConfirmKind::Pick {
                                target,
                                value: value.clone(),
                                label: value,
                            },
                        }));
                        dirty = true;
                    }
                    Flow::GitFetch(remote) => {
                        let body = serde_json::json!({ "remote": remote, "prune": true });
                        if let Err(e) = post_git(&daemons, &hosts, &view, "git/fetch", &body).await {
                            view.flash = Some(format!("fetch: {e:#}"));
                        }
                        // Fetch is the one verb here whose whole point is that
                        // the refs moved, so the page re-reads itself.
                        spawn_git_refresh(&daemons, &hosts, &view, &mut git, &mut git_generation, &git_tx);
                        dirty = true;
                    }
                    Flow::GitCopySha => {
                        match git.commit() {
                            Some(c) => {
                                // The same OSC-52 the drag-selection copy uses,
                                // so a sha lands wherever a selection would —
                                // including through ssh, where there is no
                                // local clipboard to reach for.
                                let id = c.id.clone();
                                crate::tui::set_clipboard(&id).ok();
                                view.flash = Some(format!("copied {}", &id[..7.min(id.len())]));
                            }
                            None => view.flash = Some("no commit selected".into()),
                        }
                        dirty = true;
                    }
                    Flow::GitScope(scope) => {
                        git.scope = scope;
                        // A new scope is a new list: leaving the cursor where
                        // it was points it at a commit from the old one.
                        git.hist_sel = 0;
                        spawn_git_refresh(&daemons, &hosts, &view, &mut git, &mut git_generation, &git_tx);
                        dirty = true;
                    }
                    Flow::GitOpenCommit => {
                        if let Some(c) = git.commit() {
                            let kind =
                                DiffKind::Commit { id: c.id.clone(), summary: c.summary.clone() };
                            match fetch_diff(&daemons, &hosts, &view, &kind).await {
                                Ok(text) => {
                                    let mut d = DiffView::new(kind, &text);
                                    d.set_view_rows(git_body_rows(cols, rows, &view));
                                    git.body = Some(d);
                                }
                                Err(e) => view.flash = Some(format!("show: {e:#}")),
                            }
                        }
                        dirty = true;
                    }
                    Flow::GitShowRev { rev, title } => {
                        let kind = DiffKind::Commit { id: rev, summary: title };
                        match fetch_diff(&daemons, &hosts, &view, &kind).await {
                            Ok(text) => {
                                let mut d = DiffView::new(kind, &text);
                                d.set_view_rows(git_body_rows(cols, rows, &view));
                                git.body = Some(d);
                            }
                            Err(e) => view.flash = Some(format!("show: {e:#}")),
                        }
                        dirty = true;
                    }
                    Flow::GitOpenDiff { kind, keep_cursor } => {
                        match fetch_diff(&daemons, &hosts, &view, &kind).await {
                            Ok(text) => {
                                // Re-reading in place keeps the cursor, the
                                // scroll and the folds — which is the whole
                                // point after an apply, since the diff you are
                                // reading has just lost the hunk you staged.
                                match git.body.as_mut().filter(|_| keep_cursor) {
                                    Some(body) => body.set_patch(&text),
                                    None => {
                                        let mut d = DiffView::new(kind, &text);
                                        d.set_view_rows(git_body_rows(cols, rows, &view));
                                        git.body = Some(d);
                                    }
                                }
                                // A diff you asked for is a diff you want to
                                // read, so the keyboard goes to it.
                                if !keep_cursor {
                                    view.focus = Focus::Stage;
                                }
                            }
                            Err(e) => view.flash = Some(format!("diff: {e:#}")),
                        }
                        dirty = true;
                    }
                    Flow::OpenDiff { kind, keep_cursor } => {
                        match fetch_diff(&daemons, &hosts, &view, &kind).await {
                            Ok(text) => {
                                if keep_cursor {
                                    diff.set_patch(&text);
                                } else {
                                    diff = DiffView::new(kind, &text);
                                }
                                diff.set_view_rows(chrome::diff_body_rows(cols, rows, &view));
                                view.page = Page::Diff;
                            }
                            Err(e) => view.flash = Some(format!("diff: {e:#}")),
                        }
                        dirty = true;
                    }
                    Flow::ApplyDiff { discard } => {
                        // Whichever diff the page in front of you owns. The two
                        // are the same widget over the same patch model, so the
                        // apply is one piece of code and only the view it acts
                        // on differs — a second copy for the GIT page is how the
                        // two would come to disagree about what `space` does.
                        let target_view = if view.page == Page::Git {
                            git.body.as_mut()
                        } else {
                            Some(&mut diff)
                        };
                        if let Some(d) = target_view {
                            if let Some((patch, at, reverse)) = d.selection(discard) {
                                match apply_diff(&daemons, &hosts, &view, &patch, at, reverse).await
                                {
                                    // The diff has changed under us — the hunk
                                    // just staged is no longer in it — so re-read
                                    // it rather than showing a patch that no
                                    // longer describes the repository.
                                    Ok(()) => {
                                        d.cancel_line_select();
                                        if let Some(kind) = d.kind.clone() {
                                            match fetch_diff(&daemons, &hosts, &view, &kind).await {
                                                Ok(text) => d.set_patch(&text),
                                                Err(e) => {
                                                    d.notice = Some(format!("refresh: {e:#}"))
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => d.notice = Some(format!("{e:#}")),
                                }
                            }
                        }
                        // The file lists beside the body have just moved a file
                        // from one section to the other, and the counts on the
                        // rows are stale until the workspace is read again.
                        if view.page == Page::Git {
                            refresh_git_if_showing(
                                &daemons,
                                &hosts,
                                &view,
                                &mut git,
                                &mut git_generation,
                                &git_tx,
                            );
                        }
                        dirty = true;
                    }
                    Flow::RunProcess { name, command, then } => {
                        // Following something else replaces the follower: two
                        // `docker logs -f` processes on one page is a leak the
                        // user cannot see to clean up.
                        if matches!(then, Spawned::Follow(_)) {
                            if let Some(old) = docker.logs.take() {
                                kill_process(&daemons, &hosts, &view, old).await.ok();
                                stage = None;
                            }
                            docker.following = None;
                        }
                        match run_process(&daemons, &hosts, &view, &name, &command).await {
                            Ok(pane) => match then {
                                Spawned::Follow(label) => {
                                    docker.logs = Some(pane);
                                    docker.following = Some(label);
                                    view.focus = Focus::Stage;
                                }
                                Spawned::Stage => stage_new_pane(&mut view, pane),
                                Spawned::Rail => {}
                            },
                            // Named for what was asked for, not for the page
                            // that asks most often: `[+ term]` and the SYSTEM
                            // gauges come this way too, and both reported their
                            // failures as `docker:`.
                            Err(e) => view.flash = Some(format!("{name}: {e:#}")),
                        }
                        dirty = true;
                    }
                    Flow::RefreshUsage => {
                        if let Some(err) =
                            refresh_usage(&daemons, &hosts, &view, &mut usage).await
                        {
                            view.flash = Some(err);
                        }
                        dirty = true;
                    }
                    Flow::PickBranch => {
                        match branch_picker(&daemons, &hosts, &view, ListKind::Branch, "CHECK OUT")
                            .await
                        {
                            Ok(overlay) => view.overlay = Some(overlay),
                            Err(e) => view.flash = Some(format!("branches: {e:#}")),
                        }
                        dirty = true;
                    }
                    Flow::Search(query) => {
                        match fetch_search(&daemons, &hosts, &view, &query).await {
                            Ok(hits) => apply_search(&mut view, &query, hits),
                            Err(e) => {
                                if let Some(Overlay::Search(f)) = view.overlay.as_mut() {
                                    f.searching = false;
                                }
                                view.flash = Some(format!("search: {e:#}"));
                            }
                        }
                        dirty = true;
                    }
                    Flow::Browse(dir) => {
                        browse_into(&daemons, &hosts, &mut view, &dir).await;
                        dirty = true;
                    }
                    Flow::MakeFolder { dir, name } => {
                        match make_folder(&daemons, &hosts, &view, &dir, &name).await {
                            // Inside it, with `[open this folder]` under the
                            // cursor: naming a folder and opening a workspace in
                            // it is one gesture, and this is the second half of
                            // it already selected.
                            Ok(dto) => view.overlay = Some(browse_overlay(dto)),
                            Err(e) => {
                                view.flash = Some(format!("new folder: {e:#}"));
                                // Back into the picker rather than out of it. A
                                // name the daemon refused is a typo to correct,
                                // and closing the overlay would make the whole
                                // gesture — machine, then folder — start again.
                                browse_into(&daemons, &hosts, &mut view, &dir).await;
                            }
                        }
                        dirty = true;
                    }
                    Flow::CloseWorkspace(id) => {
                        let d = active_daemon(&daemons, &hosts, &view);
                        if let Err(e) =
                            daemons[d].api.delete(&format!("/v1/workspaces/{id}")).await
                        {
                            view.flash = Some(format!("close: {e:#}"));
                        }
                        // The tab that was under the cursor has gone; the tab
                        // list shrinks on the next push, so step back now
                        // rather than paint one frame past the end.
                        view.tab = view.tab.saturating_sub(1);
                        reset_sel(&mut view);
                        dirty = true;
                    }
                    Flow::Pick { target, value } => {
                        if let Err(e) =
                            run_pick_confirmed(&daemons, &hosts, &view, target, &value).await
                        {
                            view.flash = Some(format!("{e:#}"));
                        }
                        refresh_git_if_showing(
                            &daemons,
                            &hosts,
                            &view,
                            &mut git,
                            &mut git_generation,
                            &git_tx,
                        );
                        dirty = true;
                    }
                    Flow::MenuAction(action) => {
                        if let Err(e) =
                            run_menu_action(&daemons, &hosts, &mut view, action).await
                        {
                            view.flash = Some(format!("{e:#}"));
                        }
                        refresh_git_if_showing(
                            &daemons,
                            &hosts,
                            &view,
                            &mut git,
                            &mut git_generation,
                            &git_tx,
                        );
                        dirty = true;
                    }
                    Flow::Git(action) => {
                        if let Err(e) = run_git(&daemons, &hosts, &view, &action).await {
                            view.flash = Some(format!("{e:#}"));
                        }
                        refresh_git_if_showing(
                            &daemons,
                            &hosts,
                            &view,
                            &mut git,
                            &mut git_generation,
                            &git_tx,
                        );
                        dirty = true;
                    }
                    Flow::Choose => {
                        if let Some(Overlay::List(list)) = view.overlay.take() {
                            let choice = list.chosen().map(str::to_string);
                            if let Some(choice) = choice {
                                // Browsing answers itself: a directory row
                                // re-opens the list one level along, and only
                                // "[open this folder]" leaves it.
                                if let ListKind::Space = &list.kind {
                                    // By row index, not by label: the row is
                                    // built from `Page::ORDER` and carries a
                                    // cursor mark and a badge, so reading the
                                    // page back out of the string would be
                                    // parsing a thing we already have.
                                    //
                                    // Through `run_view`, which is what makes
                                    // choosing `git` here and pressing `alt-r`
                                    // the same act — including the toggle back
                                    // to work when you pick the space you are
                                    // already on.
                                    if let Some(page) = Page::ORDER.get(list.sel).copied() {
                                        // FILES and DOCS answer `ListDir`, and
                                        // this is already inside the arm that
                                        // would have handled it.
                                        if let Flow::ListDir(dir) =
                                            run_view(ViewVerb::Space(page), &mut view)
                                        {
                                            load_tree(
                                                &daemons, &hosts, &mut view, &mut files,
                                                &mut docs, dir,
                                            )
                                            .await;
                                        }
                                    }
                                } else if let ListKind::Machine = &list.kind {
                                    // Answered where; now ask what. The browse
                                    // that follows is on the chosen machine,
                                    // and so is the workspace it opens.
                                    view.browse_daemon = choice.parse::<usize>().ok();
                                    match open_browser(&daemons, &hosts, &view, "").await {
                                        Ok(overlay) => view.overlay = Some(overlay),
                                        Err(e) => view.flash = Some(format!("browse: {e:#}")),
                                    }
                                } else if let ListKind::GitGroups = &list.kind {
                                    // A group row opens its own list; the
                                    // label carries the ellipsis, so match on
                                    // what is left of it.
                                    let name = choice.trim_end_matches('…');
                                    let ws = active_workspace(&daemons, &hosts, &view);
                                    let group = crate::git_menu::MenuGroup::ALL
                                        .into_iter()
                                        .find(|g| g.label() == name);
                                    if let Some(g) = group {
                                        view.overlay = Some(git_menu_overlay(Some(g), ws));
                                    }
                                } else if let ListKind::GitGroup(group) = &list.kind {
                                    if choice == chrome::BROWSE_UP {
                                        let ws = active_workspace(&daemons, &hosts, &view);
                                        view.overlay = Some(git_menu_overlay(None, ws));
                                    } else {
                                        let ws = active_workspace(&daemons, &hosts, &view);
                                        match menu_action(*group, &choice, ws) {
                                            Some(action) => {
                                                if let Err(e) = run_menu_action(
                                                    &daemons, &hosts, &mut view, action,
                                                )
                                                .await
                                                {
                                                    view.flash = Some(format!("{e:#}"));
                                                }
                                            }
                                            None => {
                                                view.flash =
                                                    Some(format!("{choice} is not wired yet"))
                                            }
                                        }
                                    }
                                } else if let ListKind::Pick(target) = list.kind {
                                    if target.destroys() {
                                        let label = list
                                            .chosen_label()
                                            .map(str::to_string)
                                            .unwrap_or_else(|| choice.clone());
                                        view.overlay =
                                            Some(Overlay::Confirm(chrome::ConfirmOverlay {
                                                title: target.title().into(),
                                                header: label.clone(),
                                                yes: false,
                                                kind: chrome::ConfirmKind::Pick {
                                                    target,
                                                    value: choice.clone(),
                                                    label,
                                                },
                                            }));
                                    } else if let Err(e) =
                                        run_pick_confirmed(&daemons, &hosts, &view, target, &choice)
                                            .await
                                    {
                                        view.flash = Some(format!("{e:#}"));
                                    }
                                } else if let ListKind::Menu(target) = list.kind {
                                    // By row index rather than by label: the
                                    // table in `MenuTarget::rows` is the one
                                    // source for both, so matching on the
                                    // string would be a second copy of it.
                                    let row = list.sel;
                                    if let Err(e) = run_menu_row(
                                        &mut daemons,
                                        &mut hosts,
                                        &mut view,
                                        &mut forwards,
                                        &mut sockets,
                                        target,
                                        row,
                                    )
                                    .await
                                    {
                                        view.flash = Some(format!("{e:#}"));
                                    }
                                } else if let ListKind::Host = &list.kind {
                                    // The last row asks instead of answering —
                                    // it opens a box to type a destination in,
                                    // which lands back here as `DialHost`.
                                    if choice == TYPE_DESTINATION {
                                        view.overlay = Some(destination_prompt());
                                    } else if let Some(host) =
                                        choice.strip_prefix(DISCONNECT)
                                    {
                                        // A machine already here: the row is
                                        // the answer to "which ones am I
                                        // holding open", and choosing it lets
                                        // one go — for good, not until the
                                        // next attach.
                                        match disconnect_host(
                                            host,
                                            &mut daemons,
                                            &mut hosts,
                                            &mut sockets,
                                            &mut forwards,
                                            &mut view,
                                        ) {
                                            Ok(host) => forget_machine(&host, &mut view),
                                            Err(e) => view.flash = Some(format!("{e:#}")),
                                        }
                                    } else if let Some(host) =
                                        choice.strip_prefix(CONNECTING)
                                    {
                                        // Nothing to do but say so. Dialling it
                                        // again would be the second ssh to one
                                        // machine that `dialling` exists to
                                        // prevent.
                                        view.flash =
                                            Some(format!("{host} is still connecting…"));
                                    } else if let Some(host) = choice.strip_prefix(KEEP) {
                                        // Reached through a forward this client
                                        // did not open — a `[[remote]] socket`
                                        // block. Closing it means stopping
                                        // whatever set it up.
                                        view.flash = Some(format!(
                                            "{host} is on a forward of its own — \
                                             close that to disconnect it"
                                        ));
                                    } else if !choice.is_empty() {
                                        connect_machine(
                                            choice,
                                            &hosts,
                                            &mut view,
                                            &mut dialling,
                                            &mut dial_meta,
                                            &adopt_tx,
                                        );
                                    }
                                } else if let ListKind::Links = &list.kind {
                                    // Opened here rather than in
                                    // `act_on_choice`: a browser is this
                                    // machine's, and that function is the one
                                    // that only ever calls the daemon.
                                    //
                                    // Over ssh there is no browser to open —
                                    // the common case for a TUI — so the
                                    // clipboard is the answer instead. It is
                                    // not a consolation prize: OSC 52 lands on
                                    // the terminal emulator's machine, which is
                                    // the one with the browser on it.
                                    match links::open(&choice) {
                                        Ok(()) => view.flash = Some(format!("opened {choice}")),
                                        Err(e) => {
                                            crate::tui::set_clipboard(&choice).ok();
                                            view.flash = Some(format!("{e} — copied it instead"));
                                        }
                                    }
                                } else if let ListKind::Browse { dir } = &list.kind {
                                    match browse_step(dir, &choice) {
                                        BrowseStep::Descend(next) => {
                                            match open_browser(&daemons, &hosts, &view, &next).await
                                            {
                                                Ok(overlay) => view.overlay = Some(overlay),
                                                Err(e) => {
                                                    view.flash = Some(format!("{e:#}"))
                                                }
                                            }
                                        }
                                        BrowseStep::OpenHere(path) => {
                                            if let Err(e) =
                                                open_workspace(&daemons, &hosts, &view, &path).await
                                            {
                                                view.flash = Some(format!("{e:#}"));
                                            }
                                        }
                                        BrowseStep::NewHere(here) => {
                                            view.overlay = Some(new_folder_prompt(&here));
                                        }
                                    }
                                } else {
                                    match act_on_choice(
                                        &daemons, &hosts, &view, &list.kind, &choice,
                                    )
                                    .await
                                    {
                                        // The agent picker is the one list that
                                        // makes a pane; it goes on the stage,
                                        // exactly as `a` and `[+ agent]` do.
                                        Ok(Some(pane)) => stage_new_pane(&mut view, pane),
                                        Ok(None) => {}
                                        Err(e) => view.flash = Some(format!("{e:#}")),
                                    }
                                }
                            }
                        }
                        dirty = true;
                    }
                }
            }
            Some(found) = update_rx.recv() => {
                // `:update` asked for this one, so it reports rather than
                // shrugging, and it overrides a previous no.
                let forced = std::mem::take(&mut update_forced);
                match found {
                    Ok(Some(offer)) => {
                        let declined =
                            !forced && declined_update.as_deref() == Some(offer.version.as_str());
                        if !declined {
                            // Never over another modal: one at a time is the
                            // rule the overlay type is built on, and stealing
                            // the branch picker somebody just opened is worse
                            // than telling them later.
                            if view.overlay.is_none() && (forced || !update_prompted) {
                                view.overlay = Some(update_overlay(&offer.version));
                                view.flash = Some(format!(
                                    "no won't ask again for {} — esc asks next launch",
                                    offer.version
                                ));
                                update_prompted = true;
                            } else {
                                view.flash =
                                    Some(format!("butai {} is available — :update", offer.version));
                            }
                            settings.update_available = Some(offer.version.clone());
                            update_offer = Some(offer);
                            dirty = true;
                        }
                    }
                    Ok(None) => {
                        if forced {
                            view.flash =
                                Some(format!("butai {} is the latest", crate::update::CURRENT));
                            dirty = true;
                        }
                    }
                    // A laptop on a train has no network, and that is the
                    // ordinary case rather than something worth a footer. Only
                    // somebody who typed `:update` gets told.
                    Err(e) => {
                        if forced {
                            view.flash = Some(format!("update: {e:#}"));
                            dirty = true;
                        } else {
                            tracing::debug!("update check: {e:#}");
                        }
                    }
                }
            }
            _ = update_tick.tick(), if updates_enabled => {
                spawn_update_check(&update_tx);
            }
            _ = slow.tick() => {
                view.tick = view.tick.wrapping_add(1);
                dirty = true;
            }
            _ = fast.tick() => {
                view.fast_tick = view.fast_tick.wrapping_add(1);
            }
        }
    };
    // The log follower belongs to this client's Docker page, not to the
    // workspace, so it goes when the page's owner does. Without this a
    // `docker logs -f` outlives every detach and the PROCESSES rail fills up
    // with followers nobody asked for — which is what the first live run of
    // this page left behind.
    if let Some(pane) = docker.logs.take() {
        kill_process(&daemons, &hosts, &view, pane).await.ok();
    }
    drop(_guard);
    Ok(reason)
}

/// The box that asks. One builder because three routes open it: the launch
/// check, `:update`, and the SETTINGS row.
///
/// `yes: false` like every other confirm — but for a different reason. The rest
/// preselect "no" so the keystroke that throws work away is never the one that
/// opened the box; this one does it so an update is something you agree to
/// rather than something that happens while you are reaching for the keyboard.
fn update_overlay(version: &str) -> Overlay {
    Overlay::Confirm(chrome::ConfirmOverlay {
        title: "UPDATE".into(),
        header: format!("butai {version} is available — you have {}", crate::update::CURRENT),
        yes: false,
        kind: chrome::ConfirmKind::Update { version: version.to_string() },
    })
}

/// Ask GitHub whether there is a newer release, off the event loop.
///
/// Same shape as [`spawn_git_refresh`], and for the same reason its doc comment
/// gives: awaited here, a network call stops the client dead for as long as it
/// takes. `ureq` is blocking on top of that, so the work goes to a blocking
/// thread rather than parking a runtime worker on a socket.
fn spawn_update_check(tx: &UnboundedSender<Result<Option<crate::update::Offer>>>) {
    let tx = tx.clone();
    tokio::spawn(async move {
        let found = tokio::task::spawn_blocking(crate::update::check)
            .await
            .unwrap_or_else(|e| Err(anyhow::anyhow!("the update check did not finish: {e}")));
        tx.send(found).ok();
    });
}

/// The rows a tree page shows for `dir`: the listing, with a `..` on top
/// wherever there is somewhere to go back to.
///
/// **The Docs filter used to be here**, and moved into the request. It was the
/// better-looking arrangement — one route, and each client decided what its
/// pages showed — but the `●` markers arrive in the same reply, computed over
/// the whole change set, and filtering the rows afterwards left directories
/// marked for files this page had just dropped. Following one down landed on an
/// empty listing every time. A filter is only sound where the marker is
/// decided; `fetch_dir` asks for `?filter=docs` and what comes back is already
/// this page's.
///
/// **It used to carry butai's own reference too**, as a `butai://reference`
/// folder at the root whose rows were topics rather than files. That is gone:
/// the reference is [`Page::Help`] now, and a rail that answered both "which of
/// this project's writing" and "which page of the manual" was answering two
/// questions with one list — which is what made pressing help rearrange the
/// file screen. What is left here is a project's own files, on every page that
/// shows files.
///
/// `..` is an ordinary directory row rather than a special case, so Enter and
/// the mouse take the path they already take for a directory. `Backspace`
/// already walked up, but nothing on screen said so, so descending into a folder
/// read as a one-way trip.
fn tree_rows(
    _page: Page,
    mut entries: Vec<chrome::FileEntry>,
    dir: &str,
) -> Vec<chrome::FileEntry> {
    if let Some(up) = chrome::parent_of(dir) {
        entries.insert(
            0,
            chrome::FileEntry { name: "..".into(), path: up, is_dir: true, changed: false },
        );
    }
    entries
}

/// The tree state the page on screen is driving.
///
/// Files and Docs are one widget over two listings, so everything that acts on
/// "the tree" — a key, a click, a fetch — asks this which one it means rather
/// than each site deciding for itself.
fn page_tree<'a>(page: Page, files: &'a mut Files, docs: &'a mut Files) -> &'a mut Files {
    if page == Page::Docs {
        docs
    } else {
        files
    }
}

/// The box that asks before a workspace and everything in it goes away.
///
/// One builder because three routes open it — `X`, `alt-x`, and the `[x]` on
/// the active chip — and a confirm that words itself differently depending on
/// how you got there is three chances to word one of them wrongly.
fn close_workspace_confirm(ws: &WorkspaceDetail) -> Overlay {
    Overlay::Confirm(chrome::ConfirmOverlay {
        title: "CLOSE WORKSPACE".into(),
        header: format!("close {} and kill what is running in it", ws.name),
        yes: false,
        kind: chrome::ConfirmKind::CloseWorkspace { id: ws.id, name: ws.name.clone() },
    })
}

/// What arriving at `view.page` has to fetch.
///
/// A tree page is empty until its directory is listed, so switching to one is a
/// request as well as a state change. Every route onto a page — the button, the
/// cycle keys, `alt-o` — goes through this rather than remembering to list the
/// root itself, which is how the Docs page would otherwise open blank from one
/// of the three and not the others.
fn open_page(view: &mut View) -> Flow {
    // Arriving on BOOTH points the keyboard at the fleet, because that is the
    // only thing on the page there is to walk — landing on the stage would make
    // `j`/`k` type into whichever agent happened to be selected. Leaving hands
    // the keyboard back, so the stage-focused default is restored the moment you
    // are on a page that has a pane to type into.
    if view.page == Page::Booth {
        view.focus = Focus::AllAgents;
    } else if view.page == Page::Git {
        // The history is what you came for, and it is the list `j`/`k` should
        // walk on arrival; REFS is one Tab away.
        view.focus = Focus::History;
    } else if matches!(view.focus, Focus::Refs | Focus::History) {
        // Those two exist only on GIT: anywhere else they are a cursor in a
        // list the page does not draw.
        view.focus = Focus::Stage;
    } else if view.focus == Focus::AllAgents && view.page != Page::Booth {
        // The fleet list is BOOTH's, so off that page this focus has nowhere to
        // live — it would be a cursor you cannot see.
        view.focus = Focus::Stage;
    }
    if view.page.is_tree() {
        Flow::ListDir(String::new())
    } else {
        Flow::Continue
    }
}

/// Fill the FILES or DOCS tree from `dir`, whichever page is up.
///
/// A function rather than only the loop's `Flow::ListDir` arm because arriving
/// on a tree page is what fills it, and there is now more than one way to
/// arrive: a key, and a row of the spaces menu. Both reach `open_page`, which
/// answers `Flow::ListDir` — but a flow produced *inside* the loop's own match
/// has nobody left to dispatch it, so the menu calls this directly.
async fn load_tree(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &mut View,
    files: &mut Files,
    docs: &mut Files,
    dir: String,
) {
    let page = view.page;
    match fetch_dir(daemons, hosts, view, page, &dir).await {
        Ok(entries) => {
            let entries = tree_rows(page, entries, &dir);
            let tree = page_tree(page, files, docs);
            tree.dir = dir;
            tree.entries = entries;
            tree.sel = 0;
        }
        Err(e) => view.flash = Some(format!("tree: {e:#}")),
    }
}

/// Open the folder browser at `dir`, asking which machine first when there is
/// more than one and nowhere to start from.
///
/// Where before what. With more than one machine connected, "open a workspace"
/// is two questions and this is the first — asked once, at the start, rather
/// than discovered after you have picked a directory that does not exist on the
/// machine it landed on. A `dir` that names somewhere has already answered it.
async fn browse_into(daemons: &[Daemon], hosts: &[Option<String>], view: &mut View, dir: &str) {
    if daemons.len() > 1 && view.browse_daemon.is_none() && dir.is_empty() {
        view.overlay = Some(machine_picker(daemons, hosts));
        return;
    }
    match open_browser(daemons, hosts, view, dir).await {
        Ok(overlay) => view.overlay = Some(overlay),
        Err(e) => view.flash = Some(format!("browse: {e:#}")),
    }
}

/// The daemon and pane the active tab has on its stage, if any.
fn current_stage(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    docker: &Docker,
) -> Option<(usize, PaneId)> {
    // BOOTH is not about the active tab at all: its stage follows the fleet
    // cursor, and that agent may be on a different machine from the tab you were
    // last on. Resolved before the tab lookup because on BOOTH there may be no
    // meaningful active tab to resolve — and returning that agent's *own* daemon
    // index is what makes the middle column reconnect to the right socket.
    if view.page == Page::Booth {
        let row = all_agent_rows(daemons, hosts).get(view.all_agents_sel).copied()?;
        return Some((row.daemon, row.agent.pane));
    }
    let (d, t) = *tab_index(daemons, hosts).get(view.tab)?;
    let id = daemons[d].state.tabs.get(t)?.id;
    // The Docker page streams the logs it started, not the workspace's stage.
    if view.page == Page::Docker {
        return docker.logs.map(|pane| (d, pane));
    }
    // This client's own choice first. `Watch` makes changing it free — it
    // re-points the open connection rather than redialling — which is what lets
    // the stage be a viewport rather than something the daemon has to agree to.
    //
    // Through the same resolver the title and the rail's marker use, so the
    // three cannot disagree — a live run of the first version drew `STAGE · two`
    // over the contents of `one`.
    Some((d, chrome::staged_pane(daemons[d].state.workspace(id), view)?))
}

/// The workspace behind the active tab.
fn active_workspace<'a>(
    daemons: &'a [Daemon],
    hosts: &[Option<String>],
    view: &View,
) -> Option<&'a butai_protocol::api::WorkspaceDetail> {
    let (d, t) = *tab_index(daemons, hosts).get(view.tab)?;
    let id = daemons[d].state.tabs.get(t)?.id;
    daemons[d].state.workspace(id)
}

/// The workspace behind the active tab, as a value that can be compared across
/// repaints: which daemon it is on, and its id there.
///
/// An id alone is not enough — two machines both number their first workspace
/// `1`, and switching between them is exactly when page state must be dropped.
fn active_ws_key(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
) -> Option<(usize, butai_protocol::SessionId)> {
    let (d, t) = *tab_index(daemons, hosts).get(view.tab)?;
    Some((d, daemons[d].state.tabs.get(t)?.id))
}

/// Which daemon the active tab belongs to.
fn active_daemon(daemons: &[Daemon], hosts: &[Option<String>], view: &View) -> usize {
    tab_index(daemons, hosts).get(view.tab).map(|(d, _)| *d).unwrap_or(0)
}

/// Carry out what an overlay chose.
///
/// Every one of these is an ordinary REST call — the same route a GUI or a
/// script would use. That is the test of whether the boundary is real: if the
/// TUI needed a private message to spawn an agent, it would not be a client.
async fn act_on_choice(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    kind: &ListKind,
    choice: &str,
) -> Result<Option<PaneId>> {
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace to act in")
    };
    match kind {
        // The only choice that makes a pane, and therefore the only one with
        // anything to stage.
        ListKind::SpawnAgent => return spawn_agent(daemons, hosts, view, choice).await.map(Some),
        ListKind::Branch => {
            let body = serde_json::json!({ "branch": choice });
            daemons[d].api.post(&format!("/v1/workspaces/{}/checkout", ws.id), &body).await?;
        }
        // Handled by the loop, which is where the chosen row's *label* is
        // still in hand — a destructive pick needs it to name what is about to
        // go, and by the time it reaches here only the value is left.
        ListKind::Pick(_) => {}
        // Handled by the loop: these rows are navigation, not actions.
        // `Space` most of all — choosing a view is this client moving its own
        // eyes, and the daemon refuses that whole family on purpose.
        ListKind::Space | ListKind::Browse { .. } | ListKind::GitGroups | ListKind::GitGroup(_) => {
        }
        // Handled by the loop, and not a daemon call at all: connecting a
        // machine is this client opening a second connection of its own.
        // Both are answered by the loop: one dials a machine, the other picks
        // among the machines already dialled, and neither is a call on the
        // daemon this function talks to.
        ListKind::Host | ListKind::Machine => {}
        // Handled by the loop: its rows are four different calls, and it is
        // the loop that knows which daemon a tab belongs to.
        ListKind::Menu(_) => {}
        // Themes are client-side now, so this is not a daemon call at all.
        ListKind::Theme => {}
        // Neither is following a link: the browser is on the machine this
        // client runs on, and the daemon may not even be on it.
        ListKind::Links => {}
    }
    Ok(None)
}

/// Spawn an agent and report the pane it became.
///
/// `POST .../agents` answers `200 OK` with no body — the route says a thing was
/// done, not which pane it is, and changing that would change `/v1/*` for every
/// consumer of it. So the pane is recovered the way [`run_process`] recovers
/// its own: the list is read before and after, and the row that is new is the
/// one that was just made. Client-side entirely, which is what keeps the wire
/// out of it.
async fn spawn_agent(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    name: &str,
) -> Result<PaneId> {
    use butai_protocol::api::AgentDto;
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace to act in")
    };
    let route = format!("/v1/workspaces/{}/agents", ws.id);
    let before: Vec<PaneId> =
        daemons[d].api.get_as::<Vec<AgentDto>>(&route).await?.iter().map(|a| a.pane).collect();

    daemons[d].api.post(&route, &serde_json::json!({ "type": name })).await?;

    // The spawn is synchronous in the daemon, but the agent list is rebuilt on
    // its own tick, so the new row can be a moment behind the reply.
    for _ in 0..40 {
        let now: Vec<AgentDto> = daemons[d].api.get_as(&route).await?;
        if let Some(a) = now.iter().find(|a| !before.contains(&a.pane)) {
            return Ok(a.pane);
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    anyhow::bail!("{name} did not appear in the agent list")
}

/// Install a search answer, unless the query has moved on since it went out.
///
/// Typing runs a search per keystroke, so several are in flight at once and a
/// slow one can land after a fast one — putting the answer to `need` under a
/// box that now says `needle`.
fn apply_search(view: &mut View, query: &str, hits: Vec<chrome::SearchHit>) {
    let Some(Overlay::Search(f)) = view.overlay.as_mut() else { return };
    if f.query != query {
        return;
    }
    f.hits = hits;
    f.sel = 0;
    f.searching = false;
}

/// Run a search against the workspace.
async fn fetch_search(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    query: &str,
) -> Result<Vec<chrome::SearchHit>> {
    use butai_protocol::api::SearchDto;
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace open")
    };
    let route = format!("/v1/workspaces/{}/search?q={}", ws.id, urlencode(query));
    let dto: SearchDto = daemons[d].api.get_as(&route).await?;
    Ok(dto
        .hits
        .into_iter()
        .map(|h| chrome::SearchHit { path: h.path, line: h.line, preview: h.preview })
        .collect())
}

/// Where a new worktree's checkout goes: beside the current one, named after
/// its branch.
///
/// Pure, because it is path arithmetic and path arithmetic fails quietly. A
/// branch with slashes in it (`feature/x`) flattens, or the "sibling" would be
/// two directories down.
fn worktree_path(cwd: &str, branch: &str) -> String {
    let cwd = std::path::Path::new(cwd);
    let base = cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let safe = branch.replace('/', "-");
    let parent = cwd.parent().unwrap_or(std::path::Path::new("."));
    parent.join(format!("{base}-{safe}")).to_string_lossy().into_owned()
}

/// Build the chooser a pick target needs.
///
/// One fetch each, all of them routes that already existed for the web client.
async fn pick_overlay(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    target: chrome::PickTarget,
) -> Result<Overlay> {
    use butai_protocol::api::{RemoteDto, StashDto, WorktreeDto};
    use chrome::PickTarget as T;
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace open")
    };
    let api = &daemons[d].api;
    let at = |tail: &str| format!("/v1/workspaces/{}/{tail}", ws.id);
    let title = target.title();

    let (items, values) = match target {
        // The GIT page names its own row, so these arrive already answered and
        // never reach a picker. Reported rather than silently listing nothing,
        // because a chooser that opens empty looks like the repository is.
        T::Checkout | T::Revert | T::CherryPick => {
            anyhow::bail!("{title} is chosen from the GIT page, not from a list")
        }
        T::DeleteBranch | T::Merge | T::Rebase => {
            let kind = ListKind::Pick(target);
            return branch_picker(daemons, hosts, view, kind, title).await;
        }
        T::StashPop | T::StashDrop => {
            let list: Vec<StashDto> = api.get_as(&at("git/stashes")).await?;
            anyhow::ensure!(!list.is_empty(), "nothing stashed");
            (
                list.iter()
                    .map(|s| format!("stash@{{{}}} {} {}", s.index, s.branch, s.message))
                    .collect(),
                list.iter().map(|s| s.index.to_string()).collect(),
            )
        }
        T::TagDelete => {
            let list: Vec<String> = api.get_as(&at("git/tags")).await?;
            anyhow::ensure!(!list.is_empty(), "no tags");
            (list.clone(), list)
        }
        T::RemoteRemove => {
            let list: Vec<RemoteDto> = api.get_as(&at("git/remotes")).await?;
            anyhow::ensure!(!list.is_empty(), "no remotes");
            (
                list.iter().map(|r| format!("{}  {}", r.name, r.url)).collect(),
                list.iter().map(|r| r.name.clone()).collect(),
            )
        }
        T::OpenWorktree | T::RemoveWorktree => {
            let list: Vec<WorktreeDto> = api.get_as(&at("git/worktrees")).await?;
            // The main worktree is the repository itself: it cannot be removed,
            // and it is already the workspace you are standing in.
            let list: Vec<WorktreeDto> = match target {
                T::RemoveWorktree => list.into_iter().filter(|w| !w.is_main).collect(),
                _ => list,
            };
            anyhow::ensure!(!list.is_empty(), "no other worktrees");
            (
                list.iter()
                    .map(|w| match &w.branch {
                        Some(b) => format!("{}  [{b}]", w.path),
                        None => format!("{}  (detached)", w.path),
                    })
                    .collect(),
                list.iter().map(|w| w.path.clone()).collect(),
            )
        }
    };
    Ok(Overlay::List(ListOverlay {
        title: title.to_string(),
        items,
        values: Some(values),
        sel: 0,
        kind: ListKind::Pick(target),
    }))
}

/// The call a chosen row turns into.
async fn run_pick_confirmed(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    target: chrome::PickTarget,
    value: &str,
) -> Result<()> {
    use chrome::PickTarget as T;
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace open")
    };
    let api = &daemons[d].api;
    let at = |tail: &str| format!("/v1/workspaces/{}/{tail}", ws.id);
    let enc = urlencode(value);
    match target {
        T::Checkout => {
            api.post(&at("checkout"), &serde_json::json!({ "branch": value })).await?;
        }
        T::Revert => {
            api.post(&at("git/revert"), &serde_json::json!({ "rev": value })).await?;
        }
        T::CherryPick => {
            api.post(&at("git/cherry-pick"), &serde_json::json!({ "rev": value })).await?;
        }
        T::DeleteBranch => {
            api.delete(&at(&format!("git/branch?name={enc}"))).await?;
        }
        T::Merge => {
            api.post(&at("git/merge"), &serde_json::json!({ "branch": value })).await?;
        }
        T::Rebase => {
            api.post(&at("git/rebase"), &serde_json::json!({ "onto": value })).await?;
        }
        T::StashPop => {
            let index: usize = value.parse().unwrap_or(0);
            api.post(&at("git/stash/apply"), &serde_json::json!({ "index": index, "pop": true }))
                .await?;
        }
        T::StashDrop => {
            api.delete(&at(&format!("git/stash?index={enc}"))).await?;
        }
        T::TagDelete => {
            api.delete(&at(&format!("git/tag?name={enc}"))).await?;
        }
        T::RemoteRemove => {
            api.delete(&at(&format!("git/remote?name={enc}"))).await?;
        }
        // A worktree is a directory with its own checkout, so opening it is
        // opening a workspace — no worktree-shaped route needed.
        T::OpenWorktree => {
            api.post("/v1/workspaces", &serde_json::json!({ "path": value })).await?;
        }
        T::RemoveWorktree => {
            api.delete(&at(&format!("git/worktree?path={enc}"))).await?;
        }
    }
    Ok(())
}

/// Carry out a git-menu row.
///
/// The one-call rows go straight out; the ones that need something chosen or
/// typed first open the picker or prompt that gets it. A row with neither says
/// so rather than doing nothing, because a menu entry that silently no-ops is
/// indistinguishable from a broken one.
async fn run_menu_action(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &mut View,
    action: crate::git_menu::GitAction,
) -> Result<()> {
    use crate::git_menu::GitAction as A;
    use chrome::PickTarget as T;
    // The destructive ones ask first. `needs_confirm` is the table's own
    // judgement, so the client cannot disagree with the daemon about which
    // rows are dangerous.
    if needs_asking(action, &mut view.confirmed_menu_action) {
        view.overlay = Some(Overlay::Confirm(chrome::ConfirmOverlay {
            title: "GIT".into(),
            header: format!("{} — this cannot be undone", label_of(action)),
            yes: false,
            kind: chrome::ConfirmKind::MenuAction,
        }));
        view.pending_menu_action = Some(action);
        return Ok(());
    }
    match action {
        A::Checkout => {
            view.overlay =
                Some(branch_picker(daemons, hosts, view, ListKind::Branch, "CHECK OUT").await?);
            return Ok(());
        }
        A::NewBranch => {
            view.overlay = Some(Overlay::Prompt(chrome::PromptOverlay {
                title: "NEW BRANCH".into(),
                text: String::new(),
                cursor: 0,
                kind: chrome::PromptKind::NewBranch,
                subtitle: Some("branches from where you are now".into()),
            }));
            return Ok(());
        }
        A::DeleteBranch => return open_pick(daemons, hosts, view, T::DeleteBranch).await,
        A::Merge => return open_pick(daemons, hosts, view, T::Merge).await,
        A::Rebase => return open_pick(daemons, hosts, view, T::Rebase).await,
        A::StashList => return open_pick(daemons, hosts, view, T::StashPop).await,
        A::StashDrop => return open_pick(daemons, hosts, view, T::StashDrop).await,
        A::TagDelete => return open_pick(daemons, hosts, view, T::TagDelete).await,
        A::RemoteRemove => return open_pick(daemons, hosts, view, T::RemoteRemove).await,
        A::WorktreeList => return open_pick(daemons, hosts, view, T::OpenWorktree).await,
        A::WorktreeRemove => return open_pick(daemons, hosts, view, T::RemoveWorktree).await,
        A::TagCreate => {
            view.overlay = Some(Overlay::Prompt(chrome::PromptOverlay {
                title: "NEW TAG".into(),
                text: String::new(),
                cursor: 0,
                kind: chrome::PromptKind::NewTag,
                subtitle: Some("tags the commit you are on".into()),
            }));
            return Ok(());
        }
        A::WorktreeAdd => {
            view.overlay = Some(Overlay::Prompt(chrome::PromptOverlay {
                title: "NEW WORKTREE".into(),
                text: String::new(),
                cursor: 0,
                kind: chrome::PromptKind::NewWorktree,
                subtitle: Some("a branch name; the checkout goes beside this one".into()),
            }));
            return Ok(());
        }
        _ => {}
    }
    let Some((route, body)) = menu_request(action) else {
        anyhow::bail!("{} is not wired yet", label_of(action))
    };
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace open")
    };
    daemons[d].api.post(&format!("/v1/workspaces/{}/{route}", ws.id), &body).await?;
    Ok(())
}

/// Whether a menu row still has to be asked about.
///
/// Destructive by the shared table's judgement, *and* not the one the confirm
/// box was just answered "yes" for. Consuming the answer is what stops the
/// second pass asking again and the third pass running unasked — which is why
/// it takes the slot rather than reading it.
fn needs_asking(
    action: crate::git_menu::GitAction,
    confirmed: &mut Option<crate::git_menu::GitAction>,
) -> bool {
    if !action.needs_confirm() {
        return false;
    }
    confirmed.take() != Some(action)
}

/// Open a chooser for `target`.
async fn open_pick(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &mut View,
    target: chrome::PickTarget,
) -> Result<()> {
    let overlay = pick_overlay(daemons, hosts, view, target).await?;
    view.overlay = Some(overlay);
    Ok(())
}

/// A menu action's label, for messages. Read out of the same table.
fn label_of(action: crate::git_menu::GitAction) -> &'static str {
    crate::git_menu::ITEMS.iter().find(|i| i.action == action).map(|i| i.label).unwrap_or("this")
}

/// The git menu, as a chooser: groups at the top level, rows inside one.
///
/// The table is `butai_core::git_menu`, unchanged — labels, mnemonics and the
/// rules about what is worth offering right now are a description of git, not
/// of a renderer, so both sides read the same one.
fn git_menu_overlay(
    group: Option<crate::git_menu::MenuGroup>,
    ws: Option<&WorkspaceDetail>,
) -> Overlay {
    use crate::git_menu::{groups_for, items_for, MenuContext};
    // Mid-sequence the menu shows the way out and nothing else, which is what
    // `MenuContext` is for.
    let in_sequence = ws
        .and_then(|w| w.changes.as_ref())
        .is_some_and(|c| !matches!(c.state, butai_protocol::api::RepoState::Clean));
    let cx = MenuContext { in_sequence };
    match group {
        None => {
            let items: Vec<String> =
                groups_for(&cx).into_iter().map(|g| format!("{}…", g.label())).collect();
            Overlay::List(ListOverlay {
                title: "GIT".into(),
                items,
                values: None,
                sel: 0,
                kind: ListKind::GitGroups,
            })
        }
        Some(g) => {
            let mut items = vec![chrome::BROWSE_UP.to_string()];
            items.extend(items_for(g, &cx).into_iter().map(|i| i.label.to_string()));
            Overlay::List(ListOverlay {
                title: format!("GIT · {}", g.label()),
                items,
                values: None,
                sel: 0,
                kind: ListKind::GitGroup(g),
            })
        }
    }
}

/// The action a chosen git-menu row names.
fn menu_action(
    group: crate::git_menu::MenuGroup,
    label: &str,
    ws: Option<&WorkspaceDetail>,
) -> Option<crate::git_menu::GitAction> {
    use crate::git_menu::{items_for, MenuContext};
    let in_sequence = ws
        .and_then(|w| w.changes.as_ref())
        .is_some_and(|c| !matches!(c.state, butai_protocol::api::RepoState::Clean));
    items_for(group, &MenuContext { in_sequence })
        .into_iter()
        .find(|i| i.label == label)
        .map(|i| i.action)
}

/// The route and body a menu action turns into, when it is one call.
///
/// `None` for the rows whose label ends in `…`: those need something chosen or
/// typed first, and the ones that are wired lead to their own picker or prompt.
fn menu_request(action: crate::git_menu::GitAction) -> Option<(&'static str, serde_json::Value)> {
    use crate::git_menu::GitAction as A;
    use serde_json::json;
    Some(match action {
        A::Fetch => ("git/fetch", json!({ "all": true, "prune": true })),
        A::Pull => ("git/pull", json!({})),
        A::PullRebase => ("git/pull", json!({ "rebase": true })),
        A::Push => ("git/push", json!({})),
        A::PushUpstream => ("git/push", json!({ "set_upstream": true })),
        A::PushForce => ("git/push", json!({ "force": true })),
        A::StashPush => ("git/stash", json!({})),
        A::StashPop => ("git/stash/apply", json!({ "pop": true })),
        A::SequenceContinue => ("git/sequence", json!({ "action": "continue" })),
        A::SequenceAbort => ("git/sequence", json!({ "action": "abort" })),
        A::SequenceSkip => ("git/sequence", json!({ "action": "skip" })),
        // No message: amending without one keeps the commit's own, which is
        // what "amend last commit" means when the staged changes are the edit.
        A::Amend => ("git/amend", json!({})),
        A::ResetSoft => ("git/reset", json!({ "rev": "HEAD~1", "mode": "soft" })),
        A::ResetHard => ("git/reset", json!({ "mode": "hard" })),
        A::WorktreePrune => ("git/worktree/prune", json!({})),
        _ => return None,
    })
}

/// What choosing a row in the folder browser means.
#[derive(Debug, PartialEq, Eq)]
enum BrowseStep {
    /// List this directory instead.
    Descend(String),
    /// Open a workspace here.
    OpenHere(String),
    /// Ask for a name, then make a folder here.
    NewHere(String),
}

/// Read a chosen row against the directory it was listed from.
///
/// Pure, so the path arithmetic — which is the part that silently opens the
/// wrong folder — is testable without a filesystem.
fn browse_step(dir: &str, choice: &str) -> BrowseStep {
    if choice == chrome::BROWSE_OPEN {
        return BrowseStep::OpenHere(dir.to_string());
    }
    if choice == chrome::BROWSE_NEW {
        return BrowseStep::NewHere(dir.to_string());
    }
    if choice == chrome::BROWSE_UP {
        // Up from the root is the root: there is nowhere above `/`, and
        // producing an empty path would ask the daemon to list the home
        // directory instead, which is not "up".
        let up = std::path::Path::new(dir).parent().map(|p| p.to_string_lossy().into_owned());
        return BrowseStep::Descend(
            up.filter(|p| !p.is_empty()).unwrap_or_else(|| dir.to_string()),
        );
    }
    let name = choice.strip_suffix('/').unwrap_or(choice);
    let joined = std::path::Path::new(dir).join(name);
    BrowseStep::Descend(joined.to_string_lossy().into_owned())
}

/// List `dir` and build the chooser for it.
///
/// `GET /v1/fs` browses the daemon's *host*, not a workspace — which is the
/// point: the folder a new workspace opens in does not exist as a workspace
/// yet, so no workspace-scoped route can reach it.
async fn open_browser(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    dir: &str,
) -> Result<Overlay> {
    use butai_protocol::api::BrowseDto;
    // The machine the browser is pointed at, which is not always the one the
    // tab bar is on: with more than one connected, opening a workspace asks
    // where first, and the answer lives on the view until the browser closes.
    let d = browse_daemon(daemons, hosts, view);
    let route = if dir.is_empty() {
        "/v1/fs".to_string()
    } else {
        format!("/v1/fs?path={}", urlencode(dir))
    };
    let dto: BrowseDto = daemons[d].api.get_as(&route).await?;
    Ok(browse_overlay(dto))
}

/// The picker for a listing that has already arrived.
///
/// Split out of [`open_browser`] because `POST /v1/fs/mkdir` answers with the
/// listing of the folder it just made — stepping into a new folder is this
/// function, and asking for a listing the daemon has already sent would be a
/// second round trip for an answer we are holding. The web client's picker
/// splits itself the same way, for the same reason.
fn browse_overlay(dto: butai_protocol::api::BrowseDto) -> Overlay {
    // The two verbs about *here* lead, then the way out, then what is in it.
    // `[new folder]` sits beside `[open this folder]` because the two are one
    // gesture — make a project and open it — and putting it after the
    // directories would bury it under a long listing on the one screen where
    // the folder you want is the one that is not there.
    let mut items = vec![chrome::BROWSE_OPEN.to_string(), chrome::BROWSE_NEW.to_string()];
    if dto.parent.is_some() {
        items.push(chrome::BROWSE_UP.to_string());
    }
    items.extend(dto.entries.iter().filter(|e| e.is_dir).map(|e| format!("{}/", e.name)));
    Overlay::List(ListOverlay {
        title: format!("OPEN IN {}", dto.path),
        items,
        values: None,
        sel: 0,
        kind: ListKind::Browse { dir: dto.path },
    })
}

/// Ask for the name of a folder to create in `dir`.
fn new_folder_prompt(dir: &str) -> Overlay {
    Overlay::Prompt(chrome::PromptOverlay {
        title: "NEW FOLDER".into(),
        text: String::new(),
        cursor: 0,
        kind: chrome::PromptKind::NewFolder { dir: dir.to_string() },
        // Where it will land. The picker is gone by now, so without this the
        // box asks for a name with nothing on screen saying where.
        subtitle: Some(format!("in {dir}")),
    })
}

/// Make one folder in the picker's current directory, and take its listing.
///
/// On the machine the *picker* is pointed at, which is not always the tab bar's:
/// the directory on screen was listed there, and creating the folder anywhere
/// else would make one nobody asked for on a machine nobody is looking at.
async fn make_folder(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    dir: &str,
    name: &str,
) -> Result<butai_protocol::api::BrowseDto> {
    let d = browse_daemon(daemons, hosts, view);
    // `path` is the parent and `name` is one component: the daemon refuses a
    // name with a separator in it, so the picker cannot write outside the
    // folder it is showing.
    let body = serde_json::json!({ "path": dir, "name": name });
    let bytes = daemons[d].api.post("/v1/fs/mkdir", &body).await?;
    crate::api::parse(&bytes)
}

/// Open a workspace on `path`.
async fn open_workspace(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    path: &str,
) -> Result<()> {
    // The same machine the browsing happened on, or the directory that was
    // picked would be opened somewhere it may not even exist.
    let d = browse_daemon(daemons, hosts, view);
    let body = serde_json::json!({ "path": path });
    daemons[d].api.post("/v1/workspaces", &body).await?;
    Ok(())
}

/// A click on the SETTINGS page: a group, a setting, or an option of the list
/// one of them has open.
///
/// Returns the same flows the keys do, so a clicked row and the Enter it stands
/// for are one code path — the rule the overlay hit-testing already follows.
fn settings_click(
    view: &mut View,
    st: &mut chrome::Settings,
    cols: u16,
    rows: u16,
    x: u16,
    y: u16,
) -> Option<Flow> {
    use chrome::settings::{Edit, Kind, RowId};

    let geom = chrome::page_geom(cols, rows, view);
    let area = chrome::settings::columns(geom.stage_box);
    let grps = chrome::settings::groups(st, view);
    if grps.is_empty() {
        return None;
    }

    if area.groups.contains(x, y) {
        let i = chrome::settings::group_at(area.groups, grps.len(), y)?;
        st.group = i;
        st.row = 0;
        st.open = None;
        return Some(Flow::SettingsEdit(Edit::Moved));
    }
    if !area.body.contains(x, y) {
        return None;
    }

    let grp = &grps[st.group.min(grps.len() - 1)];
    // Options first: while a list is open its rows sit where the settings under
    // it would otherwise be, and testing the settings first would answer with
    // whichever one the list is covering.
    if let Some(o) = chrome::settings::option_at(area.body, grp, st, y) {
        let row = grp.rows.get(st.row.min(grp.rows.len().saturating_sub(1)))?;
        let Kind::Choice(options) = &row.kind else { return None };
        let chosen = options.get(o)?.clone();
        st.open = None;
        return Some(Flow::SettingsEdit(match row.id {
            RowId::Theme => Edit::Theme(chosen),
            RowId::DefaultAgent if chosen == chrome::settings::ASK_EVERY_TIME => {
                Edit::DefaultAgent(None)
            }
            RowId::DefaultAgent => Edit::DefaultAgent(Some(chosen)),
            _ => Edit::Moved,
        }));
    }

    let i = chrome::settings::row_at(area.body, grp, st, y)?;
    let row = grp.rows.get(i)?;
    let (id, kind, value) = (row.id, row.kind.clone(), row.value.clone());
    st.row = i;
    // A click on a row does what Enter would: it opens a list, flips a toggle,
    // and leaves a fact alone.
    Some(match (&kind, id) {
        (Kind::Choice(options), _) => {
            st.open = Some(options.iter().position(|o| *o == value).unwrap_or(0));
            Flow::SettingsEdit(Edit::Moved)
        }
        (Kind::Toggle(on), RowId::AutoAttach) => Flow::SettingsEdit(Edit::AutoAttach(!on)),
        (Kind::Toggle(on), RowId::Links) => Flow::SettingsEdit(Edit::Links(!on)),
        (Kind::Toggle(on), RowId::UpdateCheck) => Flow::SettingsEdit(Edit::UpdateCheck(!on)),
        _ => {
            st.open = None;
            Flow::SettingsEdit(Edit::Moved)
        }
    })
}

/// A click on the tab bar or the footer while a page you *entered* is up —
/// SETTINGS or HELP.
///
/// **SETTINGS used to swallow both rows** along with the rest of the screen:
/// [`settings_click`] answered `None` for anything outside its two columns and
/// the press was dropped. So BOOTH, the space buttons, the workspace chips and
/// every footer button did nothing while the page was open, and the only ways
/// out were `esc` and the key. Reported as the page being stuck, which is
/// exactly what it looked like.
///
/// The bars belong to the workbench on every page, so the fix is to let them
/// mean what they mean everywhere else. `ret` is the page that was entered from
/// — see [`chrome::Settings::ret`] and [`chrome::Help::ret`]. Shared by both
/// rather than written twice, because "these two rows still work" is one rule
/// and a second copy of it is the copy that gets forgotten.
fn page_bar_click(
    target: hit::Target,
    view: &mut View,
    ret: Page,
    tab_count: usize,
    ws: Option<&WorkspaceDetail>,
) -> Flow {
    use hit::Target;
    // A target that names a page sets it itself, and `[settings]` has to stay a
    // toggle — putting the page back before either of those would send them
    // somewhere the click did not ask for. The rest mean "look at something
    // else", and none of them means anything on this page, so it gets out of
    // the way first.
    if matches!(
        target,
        Target::Tab(_)
            | Target::CloseTab
            | Target::NewWorkspace
            | Target::Machines
            | Target::Footer("[layout]")
            | Target::Footer("[detach]")
    ) {
        view.page = ret;
    }
    run_click(target, view, tab_count, ws)
}

/// Which palette the screen should be wearing right now.
///
/// The file's, unless the cursor is standing on an option in the theme row's
/// open list — then that one, so walking the list repaints the workbench as you
/// go. This is the whole reason settings is a page: the preview is the entire
/// screen, and a modal would be covering the thing you are trying to judge.
///
/// Derived rather than stored, so there is no state to forget to put back when
/// the list closes, the cursor leaves the row, or the page does.
fn settings_palette(st: &chrome::Settings, view: &View) -> String {
    let Some(opt) = st.open else { return st.saved_theme.clone() };
    let grps = chrome::settings::groups(st, view);
    let Some(grp) = grps.get(st.group) else { return st.saved_theme.clone() };
    let Some(row) = grp.rows.get(st.row) else { return st.saved_theme.clone() };
    if row.id != chrome::settings::RowId::Theme {
        return st.saved_theme.clone();
    }
    let chrome::settings::Kind::Choice(options) = &row.kind else {
        return st.saved_theme.clone();
    };
    options.get(opt).cloned().unwrap_or_else(|| st.saved_theme.clone())
}

/// How a `[[remote]]` block reads on the SETTINGS page.
///
/// The two ways in are named rather than collapsed: `ssh gpu-box` dials for
/// itself and `socket /tmp/fwd.sock` expects something else to have forwarded
/// it, and which of the two a machine is explains most of the ways it fails to
/// connect.
fn remote_label(r: &crate::config::RemoteDef) -> String {
    let how = match (&r.host, &r.socket) {
        (Some(host), _) => format!("ssh {host}"),
        (None, Some(sock)) => format!("socket {sock}"),
        // Neither key is a block that cannot be dialled at all. Saying so beats
        // drawing a blank row for it.
        (None, None) => "no host or socket".to_string(),
    };
    match &r.name {
        Some(name) => format!("{name} — {how}"),
        None => how,
    }
}

/// A chooser over the agent types the daemon has configured.
///
/// Opens on the pinned one rather than the first, so the row the cursor starts
/// on is the one the rail's `+` is already advertising.
async fn agent_picker(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    pinned: Option<&str>,
) -> Result<Overlay> {
    let d = active_daemon(daemons, hosts, view);
    let items: Vec<String> = daemons[d].api.get_as("/v1/agents").await?;
    anyhow::ensure!(!items.is_empty(), "no agents configured");
    let sel = pinned_row(&items, pinned);
    Ok(Overlay::List(ListOverlay {
        // The title carries `d`, because `?` was the only place that mentioned
        // it and a key you have to leave the picker to find out about is one
        // nobody presses. It goes in the title rather than a row: every line in
        // this box is a selectable agent, and a hint among them reads like one.
        title: AGENT_PICKER_TITLE.into(),
        items,
        values: None,
        sel,
        kind: ListKind::SpawnAgent,
    }))
}

/// The agent picker's title, naming the one key in it that is not Enter.
pub const AGENT_PICKER_TITLE: &str = "SPAWN AGENT — d pins as default";

/// Which row a freshly-opened agent picker starts on.
///
/// The pinned one, so the cursor is already on what the rail's `+` advertises —
/// which is the row you are most likely to want and the one you would otherwise
/// have to hunt for. A pin naming an agent this daemon does not have falls back
/// to the top rather than to nothing: the config is the client's and the agent
/// list is the daemon's, so the two can legitimately disagree once a second
/// machine is in the tab bar.
fn pinned_row(items: &[String], pinned: Option<&str>) -> usize {
    pinned.and_then(|p| items.iter().position(|i| i == p)).unwrap_or(0)
}

/// Pin (or unpin) the agent the rail's `a` spawns without asking.
///
/// The *client's* config, because the pin decides what a button on this screen
/// does. It also has to be: a daemon on another machine keeps its `config.toml`
/// over there, and pinning through it would write the wrong file on the wrong
/// host — the daemon's own version of this did exactly that for a relayed tab.
///
/// The name is still checked against the daemon, because it is the daemon that
/// has to be able to spawn it.
async fn pin_agent(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    name: Option<String>,
) -> Result<Option<String>> {
    if let Some(name) = &name {
        let d = active_daemon(daemons, hosts, view);
        let known: Vec<String> = daemons[d].api.get_as("/v1/agents").await?;
        anyhow::ensure!(
            known.iter().any(|k| k == name),
            "no agent named {name:?} — `:agent` lists what is configured"
        );
    }
    crate::config::Config::save_default_agent(name.as_deref())
        .with_context(|| "saving the pin to config.toml".to_string())?;
    Ok(name)
}

/// A chooser over the machines in `~/.ssh/config`.
///
/// The one overlay in the client that fetches nothing. Its rows come off the
/// local disk because they are about *this* client's reach — which machines to
/// put in its tab bar — and the daemon it happens to be attached to is not
/// party to that. It is also what makes the picker work at all now the relay is
/// going: the client dials the second daemon itself, so the first one never
/// needs to know the second exists.
/// A chooser over the machines already connected — where to open a workspace.
///
/// Only reached with more than one, so it never asks a question with one
/// answer. Rows carry the daemon's *index* rather than its name: two machines
/// are allowed to have none, and the local daemon has none by definition.
fn machine_picker(daemons: &[Daemon], hosts: &[Option<String>]) -> Overlay {
    let items: Vec<String> = (0..daemons.len())
        .map(|d| {
            let name = hosts.get(d).and_then(|h| h.as_deref()).unwrap_or("this machine");
            let n = daemons[d].state.tabs.len();
            format!("{name}  —  {n} workspace{}", if n == 1 { "" } else { "s" })
        })
        .collect();
    Overlay::List(ListOverlay {
        title: "OPEN ON WHICH MACHINE".into(),
        items,
        values: Some((0..daemons.len()).map(|d| d.to_string()).collect()),
        sel: 0,
        kind: ListKind::Machine,
    })
}

/// A machine already in the tab bar, as the picker needs to describe it.
struct Connected {
    /// The badge its tabs carry, which is also what disconnecting names.
    host: String,
    /// Whether *this* client opened the ssh behind it.
    ///
    /// False for a `[[remote]] socket` block: that machine is reached through a
    /// forward somebody else set up, so there is no child of ours to kill and
    /// the row must not offer to. Without this the box promised a disconnect
    /// that [`disconnect_daemon`] would then refuse.
    ours: bool,
}

/// The machines in the bar, paired with whether we can drop them.
///
/// The local daemon is `None` in `hosts` and never appears. A machine is ours
/// to drop when the socket it is reached on is one of our own forwards, which
/// is the same question [`disconnect_daemon`] asks before it removes anything —
/// asked here so the row and the action cannot disagree.
fn connected_machines(
    hosts: &[Option<String>],
    sockets: &[PathBuf],
    forwards: &[crate::ssh::Forward],
) -> Vec<Connected> {
    hosts
        .iter()
        .enumerate()
        .filter_map(|(d, h)| h.as_ref().map(|host| (d, host)))
        .map(|(d, host)| {
            let ours =
                sockets.get(d).is_some_and(|s| forwards.iter().any(|f| f.socket() == s.as_path()));
            Connected { host: host.clone(), ours }
        })
        .collect()
}

/// The spaces menu: every view of this workspace, with what each one is asking
/// for.
///
/// The cursor starts on the space you are already in, so the menu opens where
/// you are and one press of Enter is a no-op rather than a jump. Rows come from
/// [`chrome::spaces_menu_rows`], which is also what the badge on the button is
/// derived from, so the two cannot disagree about whether a space wants you.
fn space_picker(
    view: &View,
    ws: Option<&butai_protocol::api::WorkspaceDetail>,
    usage: Option<&chrome::usage::Usage>,
) -> Overlay {
    Overlay::List(ListOverlay {
        title: "VIEWS".into(),
        items: chrome::spaces_menu_rows(view, ws, usage),
        values: None,
        sel: Page::ORDER.iter().position(|p| *p == view.page).unwrap_or(0),
        kind: ListKind::Space,
    })
}

fn host_picker(
    hosts: &[Option<String>],
    sockets: &[PathBuf],
    forwards: &[crate::ssh::Forward],
    dialling: &HashSet<String>,
) -> Overlay {
    let connected = connected_machines(hosts, sockets, forwards);
    host_overlay(crate::ssh_config::hosts(), &connected, dialling)
}

/// The value of the picker row that asks for a destination instead of offering
/// one. Not an ssh target, so it cannot collide with an alias: `ssh` would read
/// a leading `:` as an empty host.
const TYPE_DESTINATION: &str = ":type";

/// Prefixes for the rows that are about a machine already here, rather than one
/// to bring in. The alias follows verbatim, so reading it back is a
/// `strip_prefix` rather than a parse.
///
/// The same trick [`TYPE_DESTINATION`] uses, for the same reason: a leading `:`
/// is not an ssh destination, and the space after the word cannot appear in a
/// `Host` pattern — `~/.ssh/config` splits those on whitespace.
const DISCONNECT: &str = ":disconnect ";
const CONNECTING: &str = ":connecting ";
/// A machine that is here but not ours to drop. Its row says why rather than
/// being unselectable: a row that swallows Enter reads as a broken box.
const KEEP: &str = ":keep ";

/// The picker's rows: the machines already here, then the ones to add.
///
/// **The connected ones are rows, not omissions.** They used to be filtered out
/// entirely, which left the client with no surface anywhere that answered "which
/// machines am I holding open?" — and the one way to drop one was a right-click
/// on a tab that machine happened to own, which is a menu you can only find if
/// you already know the link exists. They come first here, because that answer
/// is worth more than the offer, and choosing one drops it.
///
/// Split from the read so the rows can be tested without a config file on
/// disk — the interesting half is which rows appear, not the parse, which
/// `ssh_config` covers already.
fn host_overlay(
    hosts: Vec<crate::ssh_config::SshHost>,
    connected: &[Connected],
    dialling: &HashSet<String>,
) -> Overlay {
    let mut items: Vec<String> = Vec::new();
    let mut values: Vec<String> = Vec::new();

    // The marker column is `branch_picker`'s — a mark and two spaces, so every
    // alias starts in the same place whatever its state.
    //
    // A machine reached through somebody else's forward says only that it is
    // connected. There is no ssh of ours under it to kill, so offering to
    // disconnect it would be a row that answers with a refusal.
    for m in connected {
        match m.ours {
            true => {
                items.push(format!("* {}  —  connected, enter to disconnect", m.host));
                values.push(format!("{DISCONNECT}{}", m.host));
            }
            false => {
                items.push(format!("* {}  —  connected on a forward of its own", m.host));
                values.push(format!("{KEEP}{}", m.host));
            }
        }
    }
    // On its way. Without a row of its own an ssh still doing its key exchange
    // looked exactly like a machine nobody had asked for, so the obvious move
    // was to ask again — and the only thing that came back was a footer saying
    // it was already connecting.
    let mut waiting: Vec<&String> = dialling.iter().collect();
    // A `HashSet` has no order and this list is read every time it opens.
    waiting.sort();
    for host in waiting {
        items.push(format!("· {host}  —  connecting…"));
        values.push(format!("{CONNECTING}{host}"));
    }

    // A machine already in the bar, or already on its way, is not offered
    // again: connecting it twice would open a second ssh to the same daemon and
    // duplicate every one of its projects in the tab bar.
    let here: HashSet<&str> = connected.iter().map(|m| m.host.as_str()).collect();
    let had_config = !hosts.is_empty();
    for h in hosts.into_iter().filter(|h| !here.contains(h.alias.as_str())) {
        if dialling.contains(&h.alias) {
            continue;
        }
        items.push(match h.detail() {
            Some(detail) => format!("  {}  —  {detail}", h.alias),
            None => format!("  {}", h.alias),
        });
        // The alias, not the row: ssh resolves the alias itself, and everything
        // the detail column spells out is ssh's own business.
        values.push(h.alias);
    }

    // Last, because the aliases are the answer when there are any — but always
    // present, because `~/.ssh/config` is not where everyone keeps their
    // machines. Without it a user with no config file had a box that listed
    // nothing and did nothing on Enter: every machine they could reach by
    // typing `ssh user@host` was unreachable from here, and the box explained
    // that by naming a file they had chosen not to have.
    //
    // It names the file only when the file really is empty. Saying so with
    // every alias in it connected already would be a lie about the one thing
    // the row exists to explain.
    items.push(match had_config {
        false => "  type a destination  —  no Host entries in ~/.ssh/config".into(),
        true => "  type a destination…".into(),
    });
    values.push(TYPE_DESTINATION.into());

    Overlay::List(ListOverlay {
        title: "MACHINES".into(),
        items,
        values: Some(values),
        sel: 0,
        kind: ListKind::Host,
    })
}

/// Ask for an ssh destination, for the machines that are not in a config file.
///
/// Whatever is typed goes to `ssh` verbatim, so it is exactly as expressive as
/// the command line is — `user@host`, an alias, `host` on its own. Options
/// (`-p`, `-i`, `-J`) are deliberately not parsed out of it: they belong in
/// `~/.ssh/config`, which is the file ssh reads and the one the rows above come
/// from.
fn destination_prompt() -> Overlay {
    Overlay::Prompt(chrome::PromptOverlay {
        title: "CONNECT MACHINE".into(),
        text: String::new(),
        cursor: 0,
        kind: chrome::PromptKind::SshDestination,
        subtitle: Some("user@host, or a Host from ~/.ssh/config".into()),
    })
}

/// A chooser over the repository's local branches.
///
/// The checked-out one is marked in the row and *not* in the value, which is
/// what `values` is for: the marker is for the reader and the name is for git.
async fn branch_picker(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    kind: ListKind,
    title: &str,
) -> Result<Overlay> {
    use butai_protocol::api::BranchesDto;
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace open")
    };
    let dto: BranchesDto =
        daemons[d].api.get_as(&format!("/v1/workspaces/{}/branches", ws.id)).await?;
    anyhow::ensure!(!dto.branches.is_empty(), "no branches (is this a repository?)");
    let items = dto
        .branches
        .iter()
        .map(|b| if Some(b) == dto.current.as_ref() { format!("* {b}") } else { format!("  {b}") })
        .collect();
    Ok(Overlay::List(ListOverlay {
        title: title.to_string(),
        items,
        values: Some(dto.branches),
        sel: 0,
        kind,
    }))
}

/// One directory listing, as tree rows.
/// Read the USAGE page's roster from the daemon behind the active tab.
///
/// One daemon, not the fleet. An account limit is the same wherever you look at
/// it from, but the *panes on it* are per machine, and merging four machines'
/// rosters would need a rule for what to do when they disagree about a version
/// or an account. This is the page for the machine whose tab you are on, which
/// is also the machine whose agents `panes` counts.
///
/// A failure leaves the previous roster in place and flashes: the numbers on
/// screen are still the last true ones, and throwing them away to show an error
/// would lose the only thing the page exists to show.
async fn refresh_usage(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    usage: &mut chrome::usage::Usage,
) -> Option<String> {
    use butai_protocol::api::UsageDto;
    let d = active_daemon(daemons, hosts, view);
    match daemons[d].api.get_as::<UsageDto>("/v1/usage").await {
        Ok(dto) => {
            usage.dto = dto;
            usage.loaded = true;
            usage.move_sel(0); // keep the cursor inside a roster that shrank
            None
        }
        Err(e) => Some(format!("usage: {e:#}")),
    }
}

async fn fetch_dir(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    page: Page,
    dir: &str,
) -> Result<Vec<chrome::FileEntry>> {
    use butai_protocol::api::TreeDto;
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace open")
    };
    let path = urlencode(dir);
    // DOCS asks the daemon for the docs listing rather than filtering the
    // answer, because the `●` markers come back in the same reply and a marker
    // filtered by nobody promises rows this page then drops. See `TreeFilter`.
    let filter = if page == Page::Docs { "&filter=docs" } else { "" };
    let dto: TreeDto = daemons[d]
        .api
        .get_as(&format!("/v1/workspaces/{}/tree?path={path}{filter}", ws.id))
        .await?;
    Ok(dto
        .entries
        .into_iter()
        .map(|e| chrome::FileEntry {
            name: e.name,
            path: e.path,
            is_dir: e.is_dir,
            changed: e.changed,
        })
        .collect())
}

/// One file's text.
async fn fetch_file(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    path: &str,
) -> Result<Editor> {
    use butai_protocol::api::FileDto;
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace open")
    };
    let dto: FileDto = daemons[d]
        .api
        .get_as(&format!("/v1/workspaces/{}/file?path={}", ws.id, urlencode(path)))
        .await?;
    Ok(Editor::new(dto.path, &dto.text, dto.truncated))
}

/// Write the buffer back with `POST .../upload`.
///
/// The same route the web client's drag-and-drop uses: the body is the bytes
/// and `?path=` is where they go. There is no editor-shaped endpoint because
/// there does not need to be one — an editor is a client that reads a file and
/// writes it back.
async fn save_file(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    path: &str,
    contents: &str,
) -> Result<()> {
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace open")
    };
    let route = format!("/v1/workspaces/{}/upload?path={}", ws.id, urlencode(path));
    daemons[d].api.post_bytes(&route, contents.as_bytes().to_vec()).await?;
    Ok(())
}

/// Delete one workspace file. The confirm box has already been answered.
async fn delete_file(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    path: &str,
) -> Result<()> {
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace open")
    };
    let route = format!("/v1/workspaces/{}/file?path={}", ws.id, urlencode(path));
    daemons[d].api.delete(&route).await?;
    Ok(())
}

/// The unified diff text for what `kind` names.
///
/// Three shapes of the same question, and all three are routes a script or a
/// GUI would use: `/diff` for a file, `/diff` without a pathspec for a whole
/// section, `/show` for a commit.
async fn fetch_diff(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    kind: &DiffKind,
) -> Result<String> {
    use butai_protocol::api::DiffDto;
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace open")
    };
    let route = match kind {
        // An empty pathspec is the whole worktree — the route wants the
        // parameter present, and git reads "" as "everything".
        DiffKind::Unstaged { path } => format!(
            "/v1/workspaces/{}/diff?path={}",
            ws.id,
            urlencode(path.as_deref().unwrap_or(""))
        ),
        DiffKind::Staged { path } => format!(
            "/v1/workspaces/{}/diff?kind=staged&path={}",
            ws.id,
            urlencode(path.as_deref().unwrap_or(""))
        ),
        DiffKind::Commit { id, .. } => {
            format!("/v1/workspaces/{}/show?id={}", ws.id, urlencode(id))
        }
    };
    let dto: DiffDto = daemons[d].api.get_as(&route).await?;
    Ok(dto.patch)
}

/// Send a patch to `git/apply`.
///
/// The one place this page writes anything. Everything else — parsing, the
/// cursor, which lines are picked — happened here without the daemon hearing
/// about it, and what it finally receives is a patch and two booleans.
async fn apply_diff(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    patch: &str,
    target: ApplyTarget,
    reverse: bool,
) -> Result<()> {
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace open")
    };
    let body = serde_json::json!({ "patch": patch, "target": target, "reverse": reverse });
    daemons[d].api.post(&format!("/v1/workspaces/{}/git/apply", ws.id), &body).await?;
    Ok(())
}

/// Start a command as a process pane and hand back the pane it runs in.
///
/// `POST .../processes` answers 200 with no body, so the pane is found by
/// diffing the process list around the call rather than read out of the reply.
/// That is deliberate: teaching the route to return an id would change a
/// response Caliper already consumes, to save this client one round trip it
/// makes at human speed.
async fn run_process(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    name: &str,
    command: &str,
) -> Result<PaneId> {
    use butai_protocol::api::ProcessDto;
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace open")
    };
    let route = format!("/v1/workspaces/{}/processes", ws.id);
    let before: Vec<PaneId> =
        daemons[d].api.get_as::<Vec<ProcessDto>>(&route).await?.iter().map(|p| p.pane).collect();

    let body = serde_json::json!({ "name": name, "command": command });
    daemons[d].api.post(&route, &body).await?;

    // The spawn is synchronous in the daemon, but the process list is rebuilt
    // on its own tick, so the new row can be a moment behind the reply.
    for _ in 0..40 {
        let now: Vec<ProcessDto> = daemons[d].api.get_as(&route).await?;
        if let Some(p) = now.iter().find(|p| !before.contains(&p.pane)) {
            return Ok(p.pane);
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    anyhow::bail!("{name} did not appear in the process list")
}

/// How to reach the machine an announcement came from.
///
/// `hint` is what the far side derived from `$SSH_CONNECTION`; behind NAT it is
/// an address that means nothing here, so the ssh arguments the daemon
/// recovered from the pane's own process win when it has them.
fn announced_target(a: &butai_protocol::api::RemoteAnnounceDto) -> Result<&str> {
    let target = if a.ssh_target.is_empty() { a.hint.as_str() } else { a.ssh_target.as_str() };
    if target.is_empty() {
        anyhow::bail!("a machine announced itself but said nothing about how to reach it");
    }
    Ok(target)
}

/// Carry out a whole-file git action.
///
/// Six ordinary REST calls. The daemon holds no notion that a rail was
/// involved: it is told a path or a message, exactly as a script or the web
/// client would tell it.
async fn run_git(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    action: &GitAction,
) -> Result<()> {
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace open")
    };
    let api = &daemons[d].api;
    let at = |tail: &str| format!("/v1/workspaces/{}/{tail}", ws.id);
    match action {
        GitAction::Stage(path) => {
            api.post(&at("changes/stage"), &serde_json::json!({ "path": path })).await?;
        }
        GitAction::Unstage(path) => {
            api.post(&at("changes/unstage"), &serde_json::json!({ "path": path })).await?;
        }
        GitAction::Discard(path) => {
            api.post(&at("changes/discard"), &serde_json::json!({ "path": path })).await?;
        }
        GitAction::Commit { message, all } => {
            let route = if *all { "changes/commit-all" } else { "changes/commit" };
            api.post(&at(route), &serde_json::json!({ "message": message })).await?;
        }
        // Push is the one that can outlive an HTTP request. The route answers
        // 202 for that, and the outcome arrives on the `git_op` event either
        // way, so there is nothing to wait for here.
        GitAction::Push => {
            api.post(&at("git/push"), &serde_json::json!({})).await?;
        }
        GitAction::NewBranch(name) => {
            let body = serde_json::json!({ "branch": name, "create": true });
            api.post(&at("checkout"), &body).await?;
        }
        GitAction::NewTag(name) => {
            api.post(&at("git/tag"), &serde_json::json!({ "name": name })).await?;
        }
        // The checkout goes *beside* this one, named after the branch:
        // `/code/proj` on branch `spike` becomes `/code/proj-spike`. A worktree
        // inside the repository would be a directory git then has to ignore.
        GitAction::NewWorktree(branch) => {
            let path = worktree_path(&ws.cwd, branch);
            let body = serde_json::json!({
                "path": path, "branch": branch, "new_branch": true
            });
            api.post(&at("git/worktree"), &body).await?;
        }
        GitAction::Resolve { path, take } => {
            api.post(&at("git/resolve"), &serde_json::json!({ "path": path, "take": take }))
                .await?;
        }
    }
    Ok(())
}

/// Whether an announced machine should be dialled.
///
/// Already in the bar, or already on its way, means no. The second half matters
/// because an ssh takes seconds to come up: without it, three announcements a
/// second apart become three connections to one machine.
fn should_dial(target: &str, hosts: &[Option<String>], dialling: &HashSet<String>) -> bool {
    !hosts.iter().flatten().any(|h| h == target) && !dialling.contains(target)
}

/// Whether a `remote_announce` should bring its machine in on its own.
///
/// A named function rather than an `&&` in the loop's match guard so the answer
/// can be asserted: the guard is the only place `remote_auto_attach` is read,
/// and a setting read in exactly one unreachable place is how it came to be
/// ignored in the first place.
///
/// `auto` gates *this* path only. A host picked from `[+ host]` is a deliberate
/// act, and turning the setting off is documented as choosing to connect that
/// way instead — so the picker calls [`should_dial`] directly.
fn announcement_dials(
    target: &str,
    hosts: &[Option<String>],
    dialling: &HashSet<String>,
    auto: bool,
) -> bool {
    auto && should_dial(target, hosts, dialling)
}

/// Connect a machine the user named — a picker row, or one typed by hand.
///
/// Both are the deliberate act `remember: true` is about: choosing `[+ host]`
/// and naming a machine is what says "this one is mine", so both write the
/// `[[remote]]` block once it answers.
///
/// The two ways it can already be here are worth different sentences. A machine
/// in the bar is not a failure and not a wait — it is a row you can go to now —
/// and telling someone it "is already connecting" sends them to watch for a tab
/// that arrived before they asked.
fn connect_machine(
    target: String,
    hosts: &[Option<String>],
    view: &mut View,
    dialling: &mut HashSet<String>,
    dial_meta: &mut HashMap<String, DialMeta>,
    tx: &tokio::sync::mpsc::UnboundedSender<(String, Result<crate::ssh::Forward>)>,
) {
    if hosts.iter().flatten().any(|h| *h == target) {
        view.flash = Some(format!("{target} is already in the tab bar"));
        return;
    }
    if dialling.contains(&target) {
        view.flash = Some(format!("{target} is already connecting"));
        return;
    }
    view.flash = Some(format!("connecting to {target}…"));
    let label = target.clone();
    spawn_dial(
        target,
        // The picker dials with no extra ssh arguments and asks the far side
        // where its daemon is, which is why the block it writes is `host` alone.
        DialMeta { label, remember: true, reconnect: false, args: Vec::new(), socket_path: None },
        dialling,
        dial_meta,
        tx,
    );
}

/// Drop the link to a machine, named by the badge its tabs carry.
///
/// The picker's way in: it has an alias, not an index, because its rows are
/// about machines rather than about tabs.
fn disconnect_host(
    host: &str,
    daemons: &mut Vec<Daemon>,
    hosts: &mut Vec<Option<String>>,
    sockets: &mut Vec<PathBuf>,
    forwards: &mut Vec<crate::ssh::Forward>,
    view: &mut View,
) -> Result<String> {
    let d = hosts
        .iter()
        .position(|h| h.as_deref() == Some(host))
        .with_context(|| format!("{host} is not connected"))?;
    disconnect_daemon(d, daemons, hosts, sockets, forwards, view)
}

/// Stop a disconnected machine coming back on the next attach.
///
/// A disconnect is only half a disconnect while the `[[remote]]` block that
/// dialled the machine is still in the file: detaching and attaching again
/// re-dialled it, which read as the disconnect having quietly undone itself.
///
/// Called here rather than inside [`disconnect_daemon`] for the same reason
/// [`crate::config::Config::save_remote`] is called where a dial lands: the
/// write goes to the user's real config, so it belongs at the gesture, and the
/// removal itself stays testable against a path of the test's own choosing.
fn forget_machine(host: &str, view: &mut View) {
    match crate::config::Config::forget_remote(host) {
        Ok(true) => view.flash = Some(format!("{host} disconnected — forgotten")),
        Ok(false) => {}
        // It is disconnected either way, but it will be back tomorrow, and the
        // only place that can be said is here.
        Err(e) => {
            tracing::warn!("forget remote {host}: {e}");
            view.flash = Some(format!(
                "{host} disconnected, but its [[remote]] block could not be removed: {e}"
            ));
        }
    }
}

/// Drop the link to the machine at `d`: kill its ssh, and take it out of the
/// tab bar.
///
/// **Forgetting the `Daemon` is what makes this visible**, and it is the half
/// that was missing. Dropping the forward kills the ssh and removes the socket,
/// but the client went on holding that daemon's last known state: its
/// workspaces stayed in the tab bar looking live, `hosts` went on naming it —
/// so reconnecting was refused as "already in the tab bar" — and its
/// event-stream task retried a socket that no longer existed, with backoff,
/// until the client quit. Disconnecting looked like it had done nothing.
///
/// The far daemon is untouched. It keeps running with every pane it had, which
/// is what makes this a link being dropped rather than work being destroyed,
/// and why reconnecting costs one ssh and no confirmation box.
///
/// Two things keep this from removing the local daemon out from under the
/// client: `hosts[d]` must name a machine (the local one is `None` by
/// definition), and the socket must be one *we* forwarded. A daemon that came
/// in as an `Endpoint` has no forward, so it fails the second even if it
/// carries a name.
///
/// Returns the badge of the machine it dropped, which is what
/// [`forget_machine`] needs to find its `[[remote]]` block.
fn disconnect_daemon(
    d: usize,
    daemons: &mut Vec<Daemon>,
    hosts: &mut Vec<Option<String>>,
    sockets: &mut Vec<PathBuf>,
    forwards: &mut Vec<crate::ssh::Forward>,
    view: &mut View,
) -> Result<String> {
    let host = hosts.get(d).cloned().flatten().context("this machine is not a link to drop")?;
    let socket = sockets.get(d).cloned().context("no socket for that machine")?;
    let before = forwards.len();
    forwards.retain(|f| f.socket() != socket);
    anyhow::ensure!(forwards.len() < before, "{host} is not one we dialled");

    // Dropping the `Daemon` also drops the receiving end of its event stream,
    // which is what stops the task behind it: the next send fails and it
    // returns instead of retrying a socket that has been deleted.
    daemons.remove(d);
    hosts.remove(d);
    sockets.remove(d);

    // Every index into `daemons` held elsewhere has just moved under it.
    view.browse_daemon = index_after_removal(view.browse_daemon, d);
    // The staged pane belonged to a machine that may have just left, so the
    // stage is not re-pointed here — `view.staged` is cleared and the tab goes
    // back to the first, and the loop's own `current_stage` opens whatever that
    // is on the next repaint.
    reset_sel(view);
    view.tab = 0;
    view.flash = Some(format!("{host} disconnected"));
    Ok(host)
}

/// Where a held daemon index points once the one at `removed` has gone.
///
/// `Vec::remove` shifts everything after it down one, so an index kept across a
/// disconnect means a different machine unless it moves too — and the one this
/// guards, `view.browse_daemon`, decides which machine a new workspace opens
/// on. Its own name is the answer to the third case: the machine it pointed at
/// is the one that left, so there is nothing to point at.
fn index_after_removal(held: Option<usize>, removed: usize) -> Option<usize> {
    match held {
        Some(b) if b == removed => None,
        Some(b) if b > removed => Some(b - 1),
        keep => keep,
    }
}

/// Bring up a machine's forward off the loop, answering on `tx`.
///
/// Never awaited inline. An ssh connection is seconds of DNS, TCP and key
/// exchange, and doing it in the loop stops the screen repainting and the
/// keyboard responding for all of them — which is exactly what the first live
/// run of the announce path did.
///
/// `meta` carries both halves: how to reach the machine, and what to do with it
/// when it arrives — see [`DialMeta`]. The arguments are read back out of it
/// rather than passed alongside it, so what the dial *used* and what a later
/// reconnect *repeats* cannot be two different things.
fn spawn_dial(
    target: String,
    meta: DialMeta,
    dialling: &mut HashSet<String>,
    dial_meta: &mut HashMap<String, DialMeta>,
    tx: &tokio::sync::mpsc::UnboundedSender<(String, Result<crate::ssh::Forward>)>,
) {
    let args = meta.args.clone();
    let socket = meta.socket_path.clone();
    dialling.insert(target.clone());
    dial_meta.insert(target.clone(), meta);
    let tx = tx.clone();
    tokio::spawn(async move {
        let out = crate::ssh::dial(&target, &args, socket.as_deref())
            .await
            .with_context(|| target.to_string());
        tx.send((target, out)).ok();
    });
}

/// What a dial in flight should do when it lands.
#[derive(Debug, Clone)]
struct DialMeta {
    /// Tab badge. The ssh destination unless a `[[remote]] name` overrode it.
    label: String,
    /// Whether landing writes a `[[remote]]` block.
    ///
    /// True only for a machine picked from `[+ host]`. An announcement is
    /// adopted for the session and a machine already in the config is already
    /// written down, so both leave the file alone — see
    /// [`crate::config::Config::save_remote`].
    remember: bool,
    /// Whether this is rebuilding a link that dropped, rather than adding a
    /// machine. A reconnect lands in the tab the machine already has; an
    /// addition appends a new one. See the `adopt_rx` arm.
    reconnect: bool,
    /// Extra ssh arguments, as [`crate::ssh::dial`] takes them.
    args: Vec<String>,
    /// The far daemon's socket, when the far side already said where it is (a
    /// handoff, or a `[[remote]] socket_path`). `None` means ask — the case for
    /// a machine chosen from the picker, which has told us nothing.
    socket_path: Option<String>,
}

/// How a machine in the tab bar was reached, kept for as long as it is there.
///
/// **A reconnect that dropped these would be dialling somewhere else.**
/// `[[remote]] ssh_args` of `["-J","bastion"]` or `["-p","2222"]` is not
/// decoration, and an announced machine's arguments were recovered from the
/// pane's own `ssh` precisely so the way back matches the way there. Keyed by
/// tab badge, like [`Downed`], because that is what `hosts` holds.
///
/// Only machines *we* dialled get an entry, which makes it the ownership test
/// too: a `[[remote]] socket` is somebody else's forward with no ssh of ours
/// behind it, so it has no spec and is never re-dialled. That is the same
/// distinction [`disconnect_daemon`] draws before it drops a machine.
#[derive(Debug, Clone)]
struct DialSpec {
    /// ssh destination. Differs from the badge when `[[remote]] name` is set.
    target: String,
    args: Vec<String>,
    socket_path: Option<String>,
}

/// How long after a link drops before spending an ssh on rebuilding it.
///
/// Not immediate. The ordinary drop is the far daemon restarting or a stream
/// hiccup, and the stream task's own retry answers those in under a second for
/// nothing. A dial is not free: one can spend twenty seconds in `whoami` alone
/// waiting on a machine that is simply off.
const REDIAL_MIN: Duration = Duration::from_secs(5);

/// The ceiling on that wait. A laptop shut for the afternoon should cost a
/// handful of ssh attempts, not one every ten seconds until it opens.
const REDIAL_MAX: Duration = Duration::from_secs(300);

/// Consecutive stream losses that mean "gone" rather than "hiccuped", when the
/// ssh child itself still looks alive.
const LOSSES_BEFORE_REDIAL: u32 = 2;

/// A machine whose link has dropped, and what has been tried about it.
///
/// Keyed by tab badge rather than by `daemons` index: an `alt-h` disconnect
/// removes an entry and shifts every index after it, while a badge is stable
/// for as long as the machine is in the bar.
#[derive(Debug, Default, Clone)]
struct Downed {
    /// Stream losses since it was last connected.
    losses: u32,
    /// The wait after the next attempt. Doubles, capped at [`REDIAL_MAX`].
    backoff: Duration,
    /// When an attempt may next be made. `None` before the first one, which is
    /// what lets a conclusively dead ssh be answered without waiting.
    next_try: Option<Instant>,
}

/// Rebuild the link to the machine at `d`, if it is ours and it is time.
///
/// The old forward is dropped *before* the dial goes out, and the order is
/// load-bearing twice over. [`crate::ssh::local_socket_path`] is (target, our
/// pid), so a re-dial binds the **same path**, and `forward()` unlinks it
/// before binding — a stale `Forward` dropped afterwards would delete the
/// socket the new ssh had just created. And killing the old ssh is what
/// releases the ControlMaster it holds open: on a slept laptop that master is
/// half-open, and a dial that multiplexes onto it hangs rather than connecting.
///
/// The old `Daemon` stays in `daemons` meanwhile, so the tab keeps its place
/// and its last-known rails instead of blinking out and coming back.
#[allow(clippy::too_many_arguments)]
fn redial_lost(
    d: usize,
    hosts: &[Option<String>],
    sockets: &[PathBuf],
    forwards: &mut Vec<crate::ssh::Forward>,
    dialled: &HashMap<String, DialSpec>,
    downed: &mut HashMap<String, Downed>,
    dialling: &mut HashSet<String>,
    dial_meta: &mut HashMap<String, DialMeta>,
    view: &mut View,
    tx: &tokio::sync::mpsc::UnboundedSender<(String, Result<crate::ssh::Forward>)>,
) {
    let Some(host) = hosts.get(d).cloned().flatten() else { return };
    // No spec means not ours to rebuild: the local daemon, or a `[[remote]]
    // socket` somebody else forwarded and whose ssh we do not hold.
    let Some(spec) = dialled.get(&host).cloned() else { return };
    // One dial at a time. A dial can spend twenty seconds in `whoami`, which is
    // longer than the first backoff step, so without this a slow one would be
    // joined by a second and both would land.
    if dialling.contains(&spec.target) {
        return;
    }
    let Some(socket) = sockets.get(d).cloned() else { return };
    let alive = forwards.iter_mut().find(|f| f.socket() == socket).is_some_and(|f| f.is_alive());
    if !redial_due(downed, &host, alive, Instant::now()) {
        return;
    }
    forwards.retain(|f| f.socket() != socket);
    view.flash = Some(format!("{host} went away — reconnecting"));
    spawn_dial(
        spec.target,
        DialMeta {
            label: host,
            // The block is already in the file, or was deliberately never
            // written. Coming back must not change which.
            remember: false,
            reconnect: true,
            args: spec.args,
            socket_path: spec.socket_path,
        },
        dialling,
        dial_meta,
        tx,
    );
}

/// Whether to spend an ssh rebuilding this machine's forward now.
///
/// **Two ways in, because the two signals see different failures.** A child
/// that has exited is conclusive and acts at once: the ssh is gone, so the
/// forwarded socket is never coming back and the stream task retrying it is
/// retrying nothing. A child that is still running proves very little on a
/// slept laptop — the link is half-open and ssh has not given up on it yet —
/// so that path waits for [`LOSSES_BEFORE_REDIAL`] in a row, which is what
/// separates a machine that went away from a far daemon that is restarting.
///
/// Takes `now` rather than reading the clock so the backoff is testable
/// without sleeping through it.
fn redial_due(
    downed: &mut HashMap<String, Downed>,
    host: &str,
    forward_alive: bool,
    now: Instant,
) -> bool {
    let d = downed.entry(host.to_string()).or_default();
    d.losses += 1;
    if forward_alive && d.losses < LOSSES_BEFORE_REDIAL {
        return false;
    }
    if d.next_try.is_some_and(|t| now < t) {
        return false;
    }
    // Grown *before* the attempt, so the wait that follows a failure is
    // already the longer one and a machine that is off backs off even if its
    // dial never answers.
    d.backoff = if d.backoff.is_zero() { REDIAL_MIN } else { (d.backoff * 2).min(REDIAL_MAX) };
    d.next_try = Some(now + d.backoff);
    true
}

/// Restart the process the PROCESSES cursor is on — the `r` its hint row
/// advertises, and what clicking that row means.
///
/// The restart allocates a new pane id, so a client looking at the old one has
/// to let go of it; leaving `staged` pointing at a pane that no longer exists
/// would show an empty stage until the next tab switch.
async fn restart_process(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &mut View,
) -> Result<()> {
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace open")
    };
    let Some(p) = ws.processes.get(view.proc_sel) else { anyhow::bail!("no process selected") };
    let (id, pane) = (ws.id, p.pane);
    daemons[d]
        .api
        .post(&format!("/v1/workspaces/{id}/processes/{pane}/restart"), &serde_json::json!({}))
        .await?;
    if view.staged == Some(pane) {
        view.staged = None;
    }
    Ok(())
}

/// Stop a process pane in the active workspace.
async fn kill_process(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    pane: PaneId,
) -> Result<()> {
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace open")
    };
    kill_pane(daemons, Route { daemon: d, workspace: ws.id, pane }).await
}

/// A pane, said the way a route says one: which machine, which workspace on it,
/// which pane.
///
/// Everything on a rail is in the workspace you are looking at, so for years
/// "the pane" was the whole address and the other two were read from the view.
/// BOOTH's fleet broke that: its rows cross daemons, and a pane id is only
/// unique within one — so an act reached from there has to carry where it is
/// acting, or it lands on whatever holds that id on the machine you are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Route {
    /// Index into `daemons`, as [`chrome::AllAgentRow::daemon`] is.
    daemon: usize,
    workspace: butai_protocol::SessionId,
    pane: PaneId,
}

/// Stop a pane, wherever it lives.
async fn kill_pane(daemons: &[Daemon], at: Route) -> Result<()> {
    let d = daemons.get(at.daemon).context("that machine has gone")?;
    d.api.delete(&format!("/v1/workspaces/{}/processes/{}", at.workspace, at.pane)).await?;
    Ok(())
}

/// POST one git route on the active workspace.
///
/// The GIT page's fetch is the only verb here that is not already a
/// `PickTarget`, and a one-line helper beats threading another arm through
/// `run_git` for a route with no arguments worth naming.
async fn post_git(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    tail: &str,
    body: &serde_json::Value,
) -> Result<()> {
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        anyhow::bail!("no workspace open")
    };
    daemons[d].api.post(&format!("/v1/workspaces/{}/{tail}", ws.id), body).await?;
    Ok(())
}

/// Re-read the GIT page, but only when it is the page showing.
///
/// Every write to the repository goes through one of three arms — `Pick`,
/// `MenuAction`, `Git` — and all three have to land here. Leaving the page
/// showing the branch you just deleted reads as the verb having done nothing,
/// and the `g` menu, whose rows this page's own footer advertises, went through
/// the two arms that did not refresh.
fn refresh_git_if_showing(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    git: &mut chrome::Git,
    generation: &mut u64,
    tx: &tokio::sync::mpsc::UnboundedSender<GitLoad>,
) {
    if view.page == Page::Git {
        spawn_git_refresh(daemons, hosts, view, git, generation, tx);
    }
}

/// Rows the GIT page's body has for a diff, less its hint row.
///
/// The diff widget is told its height rather than measuring it, because the
/// paint takes `&self` — see `DiffView::view_rows`. On this page the box is the
/// third column, not the stage, so it needs its own measurement.
fn git_body_rows(cols: u16, rows: u16, view: &View) -> u16 {
    let band = chrome::page_geom(cols, rows, view).stage_box;
    chrome::git_columns(band).body_box.height.saturating_sub(3)
}

/// Load everything the GIT page draws, over `/v1/*`.
///
/// Six plain REST reads, exactly the ones any client would make — there is no
/// git message on the wire and there must not be one. The daemon renders a
/// screen only when a program's bytes are on a PTY, and none of this is a
/// program: it is JSON, and the client draws it.
///
/// A resource that fails is left empty rather than aborting the rest. A
/// repository with no remotes configured is not a broken page, and neither is
/// an older daemon that has never heard of `git/worktrees`; the sections
/// simply do not appear.
/// The six answers, as they came back.
///
/// Carried as DTOs rather than as a finished [`chrome::Git`] because the page
/// goes on being used while they are in flight: the cursors move, a commit gets
/// opened, the scope changes. Sending the whole struct back would drag all of
/// that to where it was when the read started.
struct GitLoad {
    /// Which request this answers. A page that asked again — a new tab, `r`, a
    /// commit — has a newer one, and this is how the older answer is known to be
    /// stale and dropped instead of drawn over it.
    generation: u64,
    log: Option<butai_protocol::api::LogDto>,
    branches: Option<butai_protocol::api::BranchesDto>,
    tags: Vec<String>,
    stashes: Vec<butai_protocol::api::StashDto>,
    remotes: Vec<butai_protocol::api::RemoteDto>,
    worktrees: Vec<butai_protocol::api::WorktreeDto>,
}

/// Start the GIT page's six reads on their own task.
///
/// **Awaited on the event loop, these stopped the client dead for as long as
/// they took** — measured at 260–310ms arriving on a 5,400-commit repository,
/// and they run after *every* git action as well as on arrival and on every tab
/// change, so a commit or a push froze the keyboard too. That is the same
/// failure [`spawn_dial`] was written to avoid, and this is the same answer:
/// the loop keeps drawing, and the result arrives as a [`GitLoad`].
fn spawn_git_refresh(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    git: &mut chrome::Git,
    generation: &mut u64,
    tx: &tokio::sync::mpsc::UnboundedSender<GitLoad>,
) {
    // Bumped even when there is no workspace to read, so an answer already in
    // flight for the old one cannot land on the empty page.
    *generation = generation.wrapping_add(1);
    let generation = *generation;
    let d = active_daemon(daemons, hosts, view);
    let Some(ws) = active_workspace(daemons, hosts, view) else {
        *git = chrome::Git { loaded: true, ..chrome::Git::default() };
        return;
    };
    // Says "loading…" rather than "not a git repository" while the answer is
    // out: the page is about to be redrawn without it, and the empty state is a
    // statement about the repository, not about the wait.
    git.loaded = false;
    // The daemon's own handle, not a fresh one off its path: this workspace can
    // be on another machine, and rebuilding the handle would rebuild it with
    // the local spawn policy.
    let api = daemons[d].api.clone();
    let base = format!("/v1/workspaces/{}", ws.id);
    let scope = git.scope.query();
    let tx = tx.clone();
    tokio::spawn(async move {
        let load = read_git(&api, &base, &scope, generation).await;
        tx.send(load).ok();
    });
}

/// The six reads themselves, off the loop.
///
/// Six plain REST reads, exactly the ones any client would make — there is no
/// git message on the wire and there must not be one. The daemon renders a
/// screen only when a program's bytes are on a PTY, and none of this is a
/// program: it is JSON, and the client draws it.
///
/// A resource that fails is left empty rather than aborting the rest. A
/// repository with no remotes configured is not a broken page, and neither is
/// an older daemon that has never heard of `git/worktrees`; the sections
/// simply do not appear.
async fn read_git(api: &crate::api::Api, base: &str, scope: &str, generation: u64) -> GitLoad {
    use butai_protocol::api::{BranchesDto, LogDto, RemoteDto, StashDto, WorktreeDto};
    let (log_url, branches_url) =
        (format!("{base}/git/log?limit=200&{scope}"), format!("{base}/branches"));
    let (tags_url, stashes_url) = (format!("{base}/git/tags"), format!("{base}/git/stashes"));
    let (remotes_url, worktrees_url) =
        (format!("{base}/git/remotes"), format!("{base}/git/worktrees"));
    let log = api.get_as::<LogDto>(&log_url);
    let branches = api.get_as::<BranchesDto>(&branches_url);
    let tags = api.get_as::<Vec<String>>(&tags_url);
    let stashes = api.get_as::<Vec<StashDto>>(&stashes_url);
    let remotes = api.get_as::<Vec<RemoteDto>>(&remotes_url);
    let worktrees = api.get_as::<Vec<WorktreeDto>>(&worktrees_url);
    // Concurrently: six sequential round trips over a Unix socket is not slow,
    // but it is six times the latency for one screen, and they do not depend on
    // each other.
    let (log, branches, tags, stashes, remotes, worktrees) =
        tokio::join!(log, branches, tags, stashes, remotes, worktrees);
    GitLoad {
        generation,
        log: log.ok(),
        branches: branches.ok(),
        tags: tags.unwrap_or_default(),
        stashes: stashes.unwrap_or_default(),
        remotes: remotes.unwrap_or_default(),
        worktrees: worktrees.unwrap_or_default(),
    }
}

/// Put a finished read on the page, against whatever the cursors are now.
fn apply_git_load(
    git: &mut chrome::Git,
    load: GitLoad,
    changes: Option<&butai_protocol::api::ChangesDto>,
    here: Option<butai_protocol::SessionId>,
) {
    match load.log {
        Some(l) => {
            git.log = l.commits;
            git.more = l.more;
        }
        None => git.log.clear(),
    }
    git.branches = load.branches;
    git.tags = load.tags;
    git.stashes = load.stashes;
    git.remotes = load.remotes;
    git.worktrees = load.worktrees;
    git.loaded = true;
    // Both lists just changed under their cursors; leaving one where it was is
    // how a cursor comes to name a row that is no longer there — an empty verb
    // footer over a selection you cannot see. The offsets follow the cursors,
    // so clamping these two is the whole of it.
    git.hist_sel = git.hist_sel.min(git.log.len().saturating_sub(1));
    let refs_len = chrome::ref_rows(git, changes, here).len();
    git.refs_sel = git.refs_sel.min(refs_len.saturating_sub(1));
}

/// Percent-encode a query value.
///
/// Paths routinely carry spaces, `#` and `+`, every one of which changes the
/// meaning of a query string. Small enough to do here rather than take a
/// dependency for one field.
pub(crate) fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The first event from any connected daemon, and which one it came from.
///
/// `select_all` over every stream, so a second machine costs one more future
/// rather than another loop.
///
/// **The index is the point.** `select_all` hands back which future finished,
/// and that is the position in `daemons`; without it a `Lost` says only that
/// *some* machine dropped, which is not enough to go and dial it again.
async fn next_any_event(daemons: &mut [Daemon]) -> Option<(usize, DaemonEvent)> {
    if daemons.is_empty() {
        return std::future::pending().await;
    }
    let futures: Vec<_> = daemons.iter_mut().map(|d| Box::pin(d.next_event())).collect();
    let (ev, d, _) = futures::future::select_all(futures).await;
    ev.map(|ev| (d, ev))
}

/// Await the stage connection, or never when there is none.
///
/// `select!` needs every branch to be a future; a pending future is how a
/// missing stage stops that branch from firing without restructuring the loop.
/// A lost stage is pending too, and that is load-bearing rather than tidy: once
/// the transport's sender is dropped `recv()` returns `None` immediately and
/// forever, so a branch that kept awaiting it would spin the select loop at the
/// speed of the CPU for as long as the machine stayed down.
async fn recv_stage(stage: Option<&mut Stage>) -> Option<ServerMsg> {
    match stage {
        Some(s) if s.lost.is_none() => s.transport.from_server.recv().await,
        _ => std::future::pending().await,
    }
}

#[derive(Debug)]
enum Flow {
    Continue,
    Detach,
    /// Take the offered update. Leaves the loop: the download and the swap
    /// need the terminal back first, so they happen in [`crate::run_client`]
    /// once the guard has restored it.
    Update,
    /// Turn the offered update down, for good, for that version. Writes
    /// `[update] declined_version`; see [`chrome::ConfirmKind::Update`].
    DeclineUpdate(String),
    /// `:update` — ask now, whatever the file says, and say what came back.
    CheckUpdate,
    /// Open the agent picker; the list comes from the daemon, so the loop
    /// fetches it rather than the key handler.
    PickAgent,
    /// Open the picker for the links on screen. The list comes from the last
    /// painted buffer, which the loop holds and a key handler does not.
    PickLinks,
    /// Put a URL on the *local* clipboard — what `y` does in the link picker,
    /// and the fallback when there is no browser on this machine to open it
    /// with. OSC 52 goes to the terminal emulator, which is on a desktop even
    /// when this client is not.
    CopyLink(String),
    /// Open the branch picker. Same reason: the list is a fetch.
    PickBranch,
    /// Re-read `GET /v1/usage`. A fetch, so the loop runs it rather than the
    /// key handler.
    RefreshUsage,
    /// Run a search and put its hits in the open search overlay.
    Search(String),
    /// Open the folder browser, starting at this directory.
    Browse(String),
    /// Make `name` in `dir`, then show the picker inside it.
    MakeFolder {
        dir: String,
        name: String,
    },
    /// Close a workspace, once its confirm box has been answered.
    CloseWorkspace(butai_protocol::SessionId),
    /// Carry out a git-menu row that has been confirmed.
    MenuAction(crate::git_menu::GitAction),
    /// Carry out a chosen row that has been confirmed.
    Pick {
        target: chrome::PickTarget,
        value: String,
    },
    /// Act on the chosen row of the open list, then close it.
    Choose,
    /// Go to the agent BOOTH's fleet cursor names — its workspace, on its
    /// machine, with its screen on the stage.
    OpenFleetAgent(usize),
    /// Re-read everything the GIT page shows.
    GitRefresh,
    /// Ask before a destructive pick, with the row named in the question — the
    /// only point at which it can say *what* is about to go.
    ConfirmPick {
        target: chrome::PickTarget,
        value: String,
    },
    /// Fetch one remote.
    GitFetch(String),
    /// Put the selected commit's full id on the clipboard.
    GitCopySha,
    /// Point the GIT page's history at another ref, then re-read it.
    GitScope(chrome::GitScope),
    /// Load the commit the GIT page's history cursor is on into its body.
    GitOpenCommit,
    /// Show a stash's diff in the GIT page's body. `stash@{n}` is a revision
    /// like any other, so this is the same `show` the commits use.
    GitShowRev {
        rev: String,
        title: String,
    },
    /// Load a working-tree diff into the GIT page's body.
    ///
    /// Distinct from [`Flow::OpenDiff`] only in where the answer lands: that one
    /// takes over the DIFF page, and doing so here would throw away the refs and
    /// the history you opened the file from. The diff itself is the same fetch
    /// and the same widget.
    GitOpenDiff {
        kind: DiffKind,
        /// Survive a refresh in place — after staging, the hunk you were on has
        /// gone and the next one has taken its number, which is where you want
        /// to be.
        keep_cursor: bool,
    },
    /// (Re)list a directory on the Files page.
    ListDir(String),
    /// Open the file the cursor is on.
    OpenFile(String),
    /// Open a file and scroll to a line — what a search hit resolves to.
    OpenFileAt {
        path: String,
        line: Option<u32>,
    },
    /// Fetch a diff and show it. `keep_cursor` survives a refresh in place —
    /// after staging, the hunk you were on has gone and the next one has taken
    /// its number, which is where you want to be.
    OpenDiff {
        kind: DiffKind,
        keep_cursor: bool,
    },
    /// Send what the diff cursor names to `git/apply`.
    ApplyDiff {
        discard: bool,
    },
    /// Write the open buffer back to the workspace.
    SaveFile,
    /// Delete a file off disk, once its confirm box has been answered.
    DeleteFile(String),
    /// A whole-file git action on the CHANGES rail.
    Git(GitAction),
    /// Run a command as a process pane.
    RunProcess {
        name: String,
        command: String,
        then: Spawned,
    },
    /// Spawn a named agent — the bound form, where the name is already known.
    SpawnAgent(String),
    /// Spawn the pinned agent, or open the picker when nothing is pinned. What
    /// the AGENTS rail's `a` does, as against `A`/`C-b a`, which always ask.
    NewAgent,
    /// Pin (or unpin) the agent `a` spawns without asking.
    PinAgent(Option<String>),
    /// The SETTINGS page changed something the key handler could not finish:
    /// a file to write, a palette to repaint in, or both.
    SettingsEdit(chrome::settings::Edit),
    /// Go to the SETTINGS page, remembering the one being left.
    OpenSettings,
    /// Leave it, for the page it was opened from.
    CloseSettings,
    /// Go to the HELP page, remembering the one being left. The same pair, for
    /// the same reason: it is entered and left rather than cycled to.
    OpenHelp,
    CloseHelp,
    /// Scroll the staged pane's scrollback, in pages.
    Scroll(i16),
    /// Open the host picker. A `Flow` rather than a direct call because the
    /// list is built from the machines already connected, which the loop holds.
    PickHost,
    /// Open the tab bar's spaces menu. A `Flow` for the same reason: every row
    /// carries the badge its space is showing, and those come from the open
    /// workspace and the usage roster, neither of which [`run_view`] has.
    PickSpace,
    /// Connect a machine named by hand rather than chosen from a row. Same
    /// destination `ssh` takes, and it lands in the same place the picker's
    /// rows do — including being written down as a `[[remote]]`.
    DialHost(String),
    /// Put the focused rail's selected row on the stage — what Enter does, and
    /// what a second click on an already-selected row means.
    StageSelected,
    /// Open the diff the CHANGES cursor names.
    OpenSelectedDiff,
    /// Restart the selected process — the `r` its hint row advertises.
    RestartProcess,
    /// Enter or leave LAYOUT mode. A `Flow` because leaving writes the rail
    /// geometry to the user's config, and the loop is where that happens.
    ToggleLayout,
    /// Finish a drag: copy what it covered. The loop has the painted screen,
    /// which is the only place the selected *text* exists.
    CopySelection,
    /// Put this machine's clipboard image in the workspace and paste its path.
    PasteImage,
    /// Open the git menu at its top level.
    GitMenu,
    /// Kill whatever is on the stage.
    CloseStagePane,
    /// Kill the row the focused left rail's cursor is on — the `x` both
    /// sections advertise, and the first row of the right-click menu.
    KillSelected,
    /// One framed command on a fresh control connection.
    ///
    /// The two that go this way (`kill-server`, `reload-config`) are about the
    /// daemon itself rather than a workspace, so a session-less control
    /// connection is exactly the right shape — it is what `butai kill-server`
    /// has always used.
    Control(butai_protocol::Command),
    /// Open the folder browser at the workspace you are already in.
    ///
    /// A `Flow` for the same reason as [`Flow::PickHost`]: the starting
    /// directory is the active workspace's, and which workspace that is comes
    /// from the daemon list the loop holds.
    BrowseHere,
    /// Ask before closing the active workspace. The confirm box names the
    /// workspace, so it too needs the list the loop holds — the answered form
    /// is [`Flow::CloseWorkspace`].
    AskCloseWorkspace,
    /// Move along the tab bar. Clamped against the number of tabs, which spans
    /// every connected daemon and so is the loop's to count.
    GoTab(TabMove),
}

/// Which tab a [`Flow::GoTab`] means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabMove {
    /// By number, counting from 1 as the bar labels them.
    To(usize),
    Next,
    Prev,
}

/// Where the pane a [`Flow::RunProcess`] makes should end up.
///
/// The distinction is between a command you asked for in order to *watch* — a
/// shell, `htop`, a build — and one whose effect is the point and whose pane is
/// incidental. `docker restart` is the second kind: it finishes in a second,
/// and staging it would throw the Docker page away to show a pane that has
/// already exited.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Spawned {
    /// On the stage, on the agents page, with the keyboard.
    Stage,
    /// In the PROCESSES rail, leaving the page you are on alone.
    Rail,
    /// In the Docker page's logs column, under this label.
    Follow(String),
}

/// Keys on the Files page. `None` means "not mine, carry on".
///
/// Navigation is local — moving the cursor down a directory listing is not the
/// daemon's business — but opening a row needs a fetch, so those return a flow
/// for the loop to act on.
fn handle_files_key(k: event::KeyEvent, view: &mut View, files: &mut Files) -> Option<Flow> {
    // While editing, the buffer has the keyboard: every printable character is
    // text, so nothing below may claim one. Only Esc and C-s get out.
    if let Some(open) = files.open.as_mut() {
        if open.mode == EditMode::Edit {
            return Some(handle_editor_key(k, open));
        }
    }
    match k.code {
        event::KeyCode::Down | event::KeyCode::Char('j') if view.focus != Focus::Stage => {
            files.move_sel(1);
            Some(Flow::Continue)
        }
        event::KeyCode::Up | event::KeyCode::Char('k') if view.focus != Focus::Stage => {
            files.move_sel(-1);
            Some(Flow::Continue)
        }
        event::KeyCode::Enter => {
            let e = files.selected()?;
            Some(if e.is_dir {
                Flow::ListDir(e.path.clone())
            } else {
                Flow::OpenFile(e.path.clone())
            })
        }
        // Backspace walks up; at the root it does nothing rather than escaping
        // the workspace.
        event::KeyCode::Backspace => files.parent().map(Flow::ListDir),
        // `x` is the destructive key everywhere else in this client — discard,
        // kill, drop, remove — so it is the one here too.
        //
        // Deliberately *not* guarded on focus the way `j`/`k` are. Focus starts
        // on the stage and arriving here does not move it, so a verb gated on
        // the tree having the keyboard would do nothing on the page as it opens
        // — the `d diff` failure a few arms down, repeated. It is safe to leave
        // ungated because it does not delete: it opens a box that names the
        // path, and the row the cursor is on is the row the box asks about.
        event::KeyCode::Char('x') => {
            let e = files.selected()?;
            // Directories are refused here rather than at the daemon, so the
            // answer is immediate and the box never opens on a question that
            // was always going to be a 400.
            if e.is_dir {
                return Some(Flow::Continue);
            }
            let path = e.path.clone();
            view.overlay = Some(Overlay::Confirm(chrome::ConfirmOverlay {
                title: "DELETE".into(),
                header: format!("delete {path} — this cannot be undone"),
                yes: false,
                kind: chrome::ConfirmKind::DeleteFile { path },
            }));
            Some(Flow::Continue)
        }
        event::KeyCode::Char('e' | 'i') if files.open.is_some() => {
            files.open.as_mut()?.edit();
            Some(Flow::Continue)
        }
        // Scrolling the open file needs the cursor off the tree; the `j`/`k`
        // above belong to the listing.
        event::KeyCode::Down | event::KeyCode::Char('j') => {
            files.open.as_mut()?.scroll_by(1);
            Some(Flow::Continue)
        }
        event::KeyCode::Up | event::KeyCode::Char('k') => {
            files.open.as_mut()?.scroll_by(-1);
            Some(Flow::Continue)
        }
        // Closes the page, not the session — see the note on the diff page's
        // handler. An unsaved buffer refuses once first.
        event::KeyCode::Esc | event::KeyCode::Char('q') => {
            if let Some(open) = files.open.as_mut() {
                if !open.may_close() {
                    return Some(Flow::Continue);
                }
            }
            view.page = Page::Agents;
            Some(Flow::Continue)
        }
        _ => None,
    }
}

/// Keys while the buffer has the keyboard.
///
/// Everything that is not Esc or C-s goes to the widget, which owns the cursor,
/// the selection and undo. Nothing here reaches the daemon until C-s.
fn handle_editor_key(k: event::KeyEvent, open: &mut Editor) -> Flow {
    let ctrl = k.modifiers.contains(event::KeyModifiers::CONTROL);
    if ctrl && k.code == event::KeyCode::Char('s') {
        return Flow::SaveFile;
    }
    if k.code == event::KeyCode::Esc {
        open.stop_editing();
        return Flow::Continue;
    }
    if open.area.input(k) {
        open.touch();
    }
    Flow::Continue
}

/// A git action the CHANGES rail can take on what the cursor names.
///
/// Whole-file work only. Hunks and lines are the diff page's, through
/// `git/apply`; these are the four routes that take a path and the two that
/// take a message.
#[derive(Debug, Clone, PartialEq, Eq)]
enum GitAction {
    Stage(String),
    Unstage(String),
    Discard(String),
    Commit {
        message: String,
        all: bool,
    },
    Push,
    Resolve {
        path: String,
        take: butai_protocol::api::ResolveSide,
    },
    /// Branch from here and switch to it — `checkout` with `create`.
    NewBranch(String),
    /// Tag the commit you are on.
    NewTag(String),
    /// A worktree on a new branch, in a directory named after it.
    NewWorktree(String),
}

/// Keys on the CHANGES rail. `None` means "not mine, carry on".
///
/// The keys are the daemon's verb table: `s` stage, `u` unstage, `x` discard,
/// `o`/`t`/`a` resolve, `c` commit, `C` stage all and commit, `p` push. What
/// each row offers depends on which section it is in, which is why the row
/// model the rail draws from is the same one this reads.
fn handle_changes_key(
    k: event::KeyEvent,
    view: &mut View,
    ws: Option<&WorkspaceDetail>,
) -> Option<Flow> {
    let changes = ws?.changes.as_ref()?;
    let row = chrome::change_rows(changes).get(view.changes_sel).copied();
    // The path under the cursor, and which side of the index it is on.
    let file = match row {
        Some(chrome::ChangeRow::File { change, staged }) => Some((change.path.clone(), staged)),
        _ => None,
    };
    let conflicted = match row {
        Some(chrome::ChangeRow::Conflicted { path }) => Some(path.to_string()),
        _ => None,
    };

    match k.code {
        event::KeyCode::Char('s') => {
            let (path, staged) = file?;
            // Staging something already staged is a no-op the rail should not
            // pretend to do; the row offers `u` instead.
            (!staged).then_some(Flow::Git(GitAction::Stage(path)))
        }
        event::KeyCode::Char('u') => {
            let (path, staged) = file?;
            staged.then_some(Flow::Git(GitAction::Unstage(path)))
        }
        // Discard throws away work on disk, so it asks first — and the box
        // opens with "no" selected.
        event::KeyCode::Char('x') => {
            let (path, staged) = file?;
            if staged {
                return None;
            }
            view.overlay = Some(Overlay::Confirm(chrome::ConfirmOverlay {
                title: "DISCARD".into(),
                header: format!("throw away changes to {path}"),
                yes: false,
                kind: chrome::ConfirmKind::Discard { path },
            }));
            Some(Flow::Continue)
        }
        // `d` was in the verb table from the day the table existed, and bound
        // nowhere: the footer advertised `d diff` on four kinds of row, `?`
        // listed it, clicking the word ran it through this function — and this
        // function had no arm for it, so all three did nothing. Enter opened the
        // diff and only Enter did. That is the exact failure the table was built
        // to make impossible, which is why it is worth fixing rather than
        // deleting the verb: `d` is what the rail has always said it was.
        event::KeyCode::Char('d') => diff_under_cursor(ws, view.changes_sel)
            .map(|kind| Flow::OpenDiff { kind, keep_cursor: false }),
        event::KeyCode::Char('o') | event::KeyCode::Char('t') | event::KeyCode::Char('a') => {
            use butai_protocol::api::ResolveSide;
            let path = conflicted?;
            let take = match k.code {
                event::KeyCode::Char('o') => ResolveSide::Ours,
                event::KeyCode::Char('t') => ResolveSide::Theirs,
                _ => ResolveSide::Resolved,
            };
            Some(Flow::Git(GitAction::Resolve { path, take }))
        }
        event::KeyCode::Char(c @ ('c' | 'C')) => {
            let all = c == 'C';
            let staged = changes.staged.len();
            let subtitle = if all {
                let n = changes.staged.len() + changes.unstaged.len();
                Some(format!("{n} file(s), staged and unstaged"))
            } else if staged == 0 {
                Some("nothing staged — this will fail".to_string())
            } else {
                Some(format!("{staged} staged file(s)"))
            };
            view.overlay = Some(Overlay::Prompt(chrome::PromptOverlay {
                title: if all { "COMMIT ALL".into() } else { "COMMIT".into() },
                text: String::new(),
                cursor: 0,
                kind: chrome::PromptKind::Commit { all },
                subtitle,
            }));
            Some(Flow::Continue)
        }
        // Only offered when there is something to push, the way the footer
        // offers it — a key that silently does nothing is worse than no key.
        event::KeyCode::Char('p') if changes.ahead > 0 => Some(Flow::Git(GitAction::Push)),
        _ => None,
    }
}

/// Keys on the AGENTS and PROCESSES sections. `None` means "not mine".
///
/// Dispatched from [`crate::verbs`]' tables rather than from a match on
/// characters, so the footer under each list and the keys that work there are
/// one thing. Both were previously hand-written strings advertising `x:kill`
/// and `r:restart` — neither of which was bound anywhere, which is how a rail
/// came to document two keys that did nothing.
///
/// Killing does not ask. The right-click menu's `Close agent` never did either,
/// and an agent is a process whose transcript is on disk — unlike the changes
/// rail's `x`, which throws away the only copy of an edit and therefore
/// confirms.
fn handle_rail_key(k: event::KeyEvent, focus: Focus, rows: usize, pinned: bool) -> Option<Flow> {
    let verbs = match focus {
        Focus::Agents => crate::verbs::agents_verbs(pinned),
        Focus::Processes => crate::verbs::procs_verbs(),
        _ => return None,
    };
    let event::KeyCode::Char(c) = k.code else { return None };
    let id = verbs.iter().find(|v| v.key == c)?.id;
    Some(match id {
        crate::verbs::VerbId::NewAgent => Flow::NewAgent,
        crate::verbs::VerbId::PickAgent => Flow::PickAgent,
        crate::verbs::VerbId::NewShell => {
            Flow::RunProcess { name: "shell".into(), command: String::new(), then: Spawned::Stage }
        }
        // The two that act on the row under the cursor say so by doing nothing
        // when there is no row, rather than reporting a failure from the daemon
        // about a pane that was never named.
        crate::verbs::VerbId::Restart if rows > 0 => Flow::RestartProcess,
        crate::verbs::VerbId::Kill if rows > 0 => Flow::KillSelected,
        // `m` is in the table so that it is bound, documented and out of the
        // other verbs' way — but the menu names the pane the cursor is on, and
        // that needs the workspace, which this function does not take. The loop
        // answers it; falling through is how it gets there.
        _ => return None,
    })
}

/// Keys on BOOTH's fleet. `None` means "not mine".
///
/// One verb, and it is the rails' verb: `x` ends the session the cursor is on.
/// The fleet was navigation-only — `j`, `k`, `enter`, `tab` — on the grounds
/// that a lettered verb here would be "a key for a thing that is not there".
/// That was true of the rails' *other* verbs and never of this one: every row
/// here is an agent, ending one is the same act on the same kind of thing, and
/// the page you go to when you want to know what your agents are doing was the
/// one page where you could not act on the answer. Reaching one meant `[open]`
/// to its project on its machine, `x` on the rail, then back.
///
/// It does not ask, for the same reason the rails' `x` does not: an agent is a
/// process whose transcript is on disk. The route it kills is the *row's* — see
/// [`selected_route`], which is where a fleet cursor and a rail cursor stop
/// meaning the same thing.
///
/// **The focus test is in here rather than at the call site** so that "only
/// while the fleet has the keyboard" is a fact about a function instead of a
/// condition in a loop no unit test can reach: `tab` hands BOOTH's middle
/// column the keyboard, and from there every key is the agent's.
fn handle_fleet_key(k: event::KeyEvent, view: &View, rows: usize) -> Option<Flow> {
    if view.focus != Focus::AllAgents {
        return None;
    }
    match k.code {
        // Nothing to kill is nothing to say: no row means no failure from the
        // daemon about a pane that was never named, which is how the rails
        // spell the same guard.
        event::KeyCode::Char('x') if rows > 0 => Some(Flow::KillSelected),
        _ => None,
    }
}

/// Keys on the GIT page. `None` means "not mine, carry on".
///
/// **Nothing here changes the repository.** `Enter` reads: on a ref it scopes
/// the graph, on a commit it loads a diff, on a worktree it goes there. The
/// verbs that write are lettered and arrive with the verb footer — the same
/// division the Docker page makes between `enter follow` and `r restart`.
fn handle_git_key(
    k: event::KeyEvent,
    view: &mut View,
    git: &mut chrome::Git,
    ws: Option<&WorkspaceDetail>,
    rows: u16,
) -> Option<Flow> {
    let changes = ws.and_then(|w| w.changes.as_ref());
    let here = ws.map(|w| w.id);

    // Tab first, and unconditionally: this page has three columns and no pane,
    // so nothing below may forward a key to one. Leaving it to the general
    // cycle sent Tab to a *background* pane the moment focus reached the body
    // — the diff scrolled nowhere and the keystrokes landed in a shell.
    if k.code == event::KeyCode::Tab {
        view.focus = match view.focus {
            Focus::Refs => Focus::History,
            // The body joins the cycle only once it holds a commit. Tabbing
            // onto an empty COMMIT box pointed the keyboard at a column with
            // nothing in it to walk, and every key after that did nothing —
            // the page read as frozen from the one press that reached it.
            Focus::History if git.body.is_some() => Focus::Stage,
            _ => Focus::Refs,
        };
        return Some(Flow::Continue);
    }

    // Closing a commit (or arriving with none) leaves this focus naming a
    // column that is not there. Hand the keyboard back to the list it came
    // from, so no route can strand it on a body that does not exist.
    if view.focus == Focus::Stage && git.body.is_none() {
        view.focus = Focus::History;
    }

    // The body is a diff, and `Focus::Stage` is what being in it means here.
    // It scrolls with the same keys the lists walk with, because it is the
    // same gesture pointed at a third column.
    if view.focus == Focus::Stage {
        // `None` for anything this column has no use for, never `Continue`: the
        // page is consulted before the global bindings, so swallowing the rest
        // left `q`, `?`, `/` and every alt key dead while the diff had focus.
        let body = git.body.as_mut()?;
        // A commit's diff is history and answers none of these; the widget
        // already refuses them, and the hint row under it never offers them.
        let mutable = body.kind.as_ref().is_some_and(chrome::DiffKind::mutable);
        match k.code {
            // In line-select `j`/`k` walk the picked lines rather than the view,
            // which is what makes `space space space` take a run of them — the
            // same split the DIFF page makes between its two modes.
            event::KeyCode::Char('j') | event::KeyCode::Down
                if body.mode == chrome::DiffMode::Lines =>
            {
                body.step_line(1)
            }
            event::KeyCode::Char('k') | event::KeyCode::Up
                if body.mode == chrome::DiffMode::Lines =>
            {
                body.step_line(-1)
            }
            event::KeyCode::Char('j') | event::KeyCode::Down => body.scroll_by(1),
            event::KeyCode::Char('k') | event::KeyCode::Up => body.scroll_by(-1),
            event::KeyCode::PageDown => body.scroll_by(rows.max(1) as isize),
            event::KeyCode::PageUp => body.scroll_by(-(rows.max(1) as isize)),
            // Hunk to hunk, across files. The keys the DIFF page uses, because
            // this is the DIFF page's widget in another box.
            event::KeyCode::Char(']') => body.step_hunk(1),
            event::KeyCode::Char('[') => body.step_hunk(-1),
            // Shut the file you are in, or every file at once. The reason this
            // exists: the working tree's diff is every changed file end to end,
            // and reading it without folds means scrolling past four files to
            // reach the fifth.
            event::KeyCode::Char('z') => body.toggle_fold(),
            event::KeyCode::Char('Z') => body.toggle_fold_all(),
            // Staging, from the diff rather than from the list — which is the
            // half of it a whole-file `s` on the row cannot do.
            event::KeyCode::Char(' ') if mutable && body.mode == chrome::DiffMode::Lines => {
                body.pick_line()
            }
            event::KeyCode::Char(' ') if mutable => {
                return Some(Flow::ApplyDiff { discard: false })
            }
            event::KeyCode::Enter if mutable && body.mode == chrome::DiffMode::Lines => {
                return Some(Flow::ApplyDiff { discard: false });
            }
            event::KeyCode::Char('v') if mutable && body.mode == chrome::DiffMode::Lines => {
                body.cancel_line_select()
            }
            event::KeyCode::Char('v') if mutable => body.line_select(),
            event::KeyCode::Char('x') if mutable => return Some(Flow::ApplyDiff { discard: true }),
            // Esc leaves line-select before it leaves the diff: one key, one
            // level, so a picked run is never thrown away by the press that was
            // meant to cancel it.
            event::KeyCode::Esc if body.mode == chrome::DiffMode::Lines => {
                body.cancel_line_select()
            }
            // Esc closes what you were reading and hands the cursor back to the
            // list you opened it from — which the diff itself says: only a
            // commit can have come from HISTORY.
            event::KeyCode::Esc => {
                let from_history = body.kind.as_ref().is_none_or(|k| !chrome::DiffKind::mutable(k));
                git.body = None;
                view.focus = if from_history { Focus::History } else { Focus::Refs };
            }
            _ => return None,
        }
        return Some(Flow::Continue);
    }

    // The row list is rebuilt rather than cached: it is the same call the
    // drawing makes, so the cursor cannot be indexing a different list than the
    // one on screen.
    let refs_len = chrome::ref_rows(git, changes, here).len();
    let on_refs = view.focus == Focus::Refs;
    let len = if on_refs { refs_len } else { git.log.len() };
    let sel = if on_refs { &mut git.refs_sel } else { &mut git.hist_sel };

    // A page of the list, so PgUp/PgDn move by what is on screen rather than by
    // a constant that is wrong at every other terminal size.
    let page = rows.max(1) as isize;
    let step = match k.code {
        event::KeyCode::Char('j') | event::KeyCode::Down => 1,
        event::KeyCode::Char('k') | event::KeyCode::Up => -1,
        event::KeyCode::PageDown => page,
        event::KeyCode::PageUp => -page,
        // Home/End only. `g` is the git menu here — the footer says so on every
        // row — and `g`-for-top made the advertised verb unreachable from the
        // keyboard while its footer word still worked. The CHANGES rail gave up
        // the same key for the same reason.
        event::KeyCode::Home => -(len as isize),
        event::KeyCode::End => len as isize,
        _ => 0,
    };
    if step != 0 {
        chrome::Git::move_in(sel, step, len);
        return Some(Flow::Continue);
    }

    // Back to everything, from wherever the scope was narrowed to. Before the
    // verb table because Esc is not a verb — it is the way out.
    if k.code == event::KeyCode::Esc && git.scope != chrome::GitScope::Everything {
        return Some(Flow::GitScope(chrome::GitScope::Everything));
    }

    // Everything else comes off the same table the footer draws, so a key that
    // works here is a key the footer showed and vice versa. A key not in the
    // table does not exist on this page.
    let rows = chrome::ref_rows(git, changes, here);
    let kind = if on_refs {
        chrome::ref_row_kind(&rows, git.refs_sel)
    } else if git.log.is_empty() {
        crate::verbs::GitRow::None
    } else {
        crate::verbs::GitRow::Commit
    };
    let verbs = crate::verbs::git_footer(kind);
    let pressed = match k.code {
        event::KeyCode::Enter => '\n',
        event::KeyCode::Char(c) => c,
        _ => return None,
    };
    let id = verbs.iter().find(|v| v.key == pressed)?.id;
    git_verb_flow(id, view, git, &rows)
}

/// Carry out one GIT-page verb on the row the cursor is on.
///
/// Split from the key handling so the footer's click path and the keyboard
/// reach the *same* code: a verb that works one way and not the other is the
/// failure the shared verb table exists to prevent.
fn git_verb_flow(
    id: crate::verbs::VerbId,
    view: &mut View,
    git: &chrome::Git,
    rows: &[chrome::RefRow<'_>],
) -> Option<Flow> {
    use crate::verbs::VerbId as V;
    use chrome::PickTarget as T;

    // What the REFS cursor names, in the shapes the verbs need.
    let row = rows.get(git.refs_sel);
    let branch = match row {
        Some(chrome::RefRow::Branch { entry, .. }) => Some(entry.name.clone()),
        _ => None,
    };
    // The changed file under the cursor, and which side of the index it is on.
    let changed = match row {
        Some(chrome::RefRow::Change(chrome::ChangeRow::File { change, staged })) => {
            Some((change.path.clone(), *staged))
        }
        _ => None,
    };
    let conflicted = match row {
        Some(chrome::RefRow::Change(chrome::ChangeRow::Conflicted { path })) => {
            Some((*path).to_string())
        }
        _ => None,
    };

    Some(match id {
        V::Refresh => Flow::GitRefresh,
        V::GitMenu => Flow::GitMenu,
        // The reference is a page now, not a modal, so this `?` goes through the
        // same verb `?` does anywhere else rather than putting up an overlay
        // this page would then be reading behind.
        V::Help => run_view(ViewVerb::Help, view),
        V::GoToChanges => {
            // The rail still owns the commit box and the sync buttons, so this
            // is not a leftover: it is the way to the half of the working tree
            // this page deliberately did not copy.
            view.page = Page::Agents;
            view.focus = Focus::Changes;
            Flow::Continue
        }
        // The file rows carry the CHANGES rail's own verbs, and they resolve to
        // the same `GitAction`s the rail sends — one implementation of "stage
        // this file", reached from two surfaces.
        V::Stage => {
            let (path, staged) = changed?;
            // Staging what is already staged is a no-op this must not pretend to
            // do; that row offers `u` instead.
            if staged {
                return None;
            }
            Flow::Git(GitAction::Stage(path))
        }
        V::Unstage => {
            let (path, staged) = changed?;
            staged.then_some(Flow::Git(GitAction::Unstage(path)))?
        }
        V::Discard => {
            let (path, staged) = changed?;
            if staged {
                return None;
            }
            // Throwing away work on disk asks first, and opens on "no" — the
            // same box the rail puts up, because it is the same loss.
            view.overlay = Some(chrome::Overlay::Confirm(chrome::ConfirmOverlay {
                title: "DISCARD".into(),
                header: format!("throw away changes to {path}"),
                yes: false,
                kind: chrome::ConfirmKind::Discard { path },
            }));
            Flow::Continue
        }
        V::ResolveOurs | V::ResolveTheirs | V::ResolveDone => {
            use butai_protocol::api::ResolveSide;
            let path = conflicted?;
            let take = match id {
                V::ResolveOurs => ResolveSide::Ours,
                V::ResolveTheirs => ResolveSide::Theirs,
                _ => ResolveSide::Resolved,
            };
            Flow::Git(GitAction::Resolve { path, take })
        }
        // `enter` on a file, or on the summary row above them all. Both land in
        // the body beside the list rather than on the DIFF page: the point of
        // putting the files here is that you can read one without losing the
        // branches and the history you were looking at.
        V::Diff => {
            let kind = match row? {
                chrome::RefRow::WorkingTree { .. } => DiffKind::Unstaged { path: None },
                chrome::RefRow::Change(chrome::ChangeRow::File { change, staged }) => {
                    let path = Some(change.path.clone());
                    if *staged {
                        DiffKind::Staged { path }
                    } else {
                        DiffKind::Unstaged { path }
                    }
                }
                // A conflicted file has no staged side to compare against; the
                // worktree copy is the one with the markers in it.
                chrome::RefRow::Change(chrome::ChangeRow::Conflicted { path }) => {
                    DiffKind::Unstaged { path: Some((*path).to_string()) }
                }
                _ => return None,
            };
            Flow::GitOpenDiff { kind, keep_cursor: false }
        }
        V::Scope => match row? {
            chrome::RefRow::Branch { entry, .. } => {
                Flow::GitScope(chrome::GitScope::Ref(entry.name.clone()))
            }
            chrome::RefRow::Tag(name) => Flow::GitScope(chrome::GitScope::Ref((*name).to_string())),
            _ => Flow::Continue,
        },
        V::Show => match row {
            // On REFS this is a stash; its `stash@{n}` is a revision like any
            // other, so it is the same `show` the commits use.
            Some(chrome::RefRow::Stash(dto)) if view.focus == Focus::Refs => Flow::GitShowRev {
                rev: format!("stash@{{{}}}", dto.index),
                title: dto.message.clone(),
            },
            _ => Flow::GitOpenCommit,
        },
        V::Checkout => Flow::Pick { target: T::Checkout, value: branch? },
        V::Merge => Flow::Pick { target: T::Merge, value: branch? },
        V::DeleteBranch => Flow::ConfirmPick { target: T::DeleteBranch, value: branch? },
        V::Fetch => match row? {
            chrome::RefRow::Remote { name, .. } => Flow::GitFetch((*name).to_string()),
            _ => Flow::Continue,
        },
        V::TagDelete => match row? {
            chrome::RefRow::Tag(name) => {
                Flow::ConfirmPick { target: T::TagDelete, value: (*name).to_string() }
            }
            _ => Flow::Continue,
        },
        V::StashPop => match row? {
            chrome::RefRow::Stash(dto) => {
                Flow::Pick { target: T::StashPop, value: dto.index.to_string() }
            }
            _ => Flow::Continue,
        },
        V::StashDrop => match row? {
            chrome::RefRow::Stash(dto) => {
                Flow::ConfirmPick { target: T::StashDrop, value: dto.index.to_string() }
            }
            _ => Flow::Continue,
        },
        V::OpenWorktree => match row? {
            chrome::RefRow::Worktree { dto, here: false } => {
                // Already open somewhere? Go there rather than opening a
                // second workspace on one directory — which is what
                // `WorktreeDto.workspace` is for.
                Flow::Pick { target: T::OpenWorktree, value: dto.path.clone() }
            }
            _ => Flow::Continue,
        },
        V::RemoveWorktree => match row? {
            chrome::RefRow::Worktree { dto, here: false } => {
                Flow::ConfirmPick { target: T::RemoveWorktree, value: dto.path.clone() }
            }
            // Removing the checkout you are standing in is not a thing git will
            // do, and offering it would be a row that only ever errors.
            _ => Flow::Continue,
        },
        V::CopySha => Flow::GitCopySha,
        V::Revert => Flow::Pick { target: T::Revert, value: git.commit()?.id.clone() },
        V::CherryPick => Flow::Pick { target: T::CherryPick, value: git.commit()?.id.clone() },
        _ => return None,
    })
}

/// Keys on the USAGE page. `None` means "not mine, carry on".
///
/// Deliberately three: move, move, re-read. The page reports a state of the
/// world it does not own — an account's standing lives with the provider — so
/// there is nothing on it to change from here, and a verb that pretended
/// otherwise would be the one lie on a page whose whole job is not to invent
/// numbers.
fn handle_usage_key(k: event::KeyEvent, usage: &mut chrome::usage::Usage) -> Option<Flow> {
    match k.code {
        event::KeyCode::Char('j') | event::KeyCode::Down => {
            usage.move_sel(1);
            Some(Flow::Continue)
        }
        event::KeyCode::Char('k') | event::KeyCode::Up => {
            usage.move_sel(-1);
            Some(Flow::Continue)
        }
        event::KeyCode::Char('r') => Some(Flow::RefreshUsage),
        _ => None,
    }
}

/// Keys on the SETTINGS page. `None` means "not mine, carry on".
///
/// The page has one cursor, not two: `j`/`k` walk the settings, `tab` walks the
/// groups, and inside an expanded list the same `j`/`k` walk the options. A
/// second cursor for the group list would be a second thing to keep track of on
/// a page whose entire job is to be obvious.
///
/// Every arm reports [`Edit::Moved`] at least, because the palette on screen is
/// a function of where the cursor is — an open theme list previews the row it
/// is on, and moving off it has to put the old one back.
fn handle_settings_key(
    k: event::KeyEvent,
    view: &mut View,
    st: &mut chrome::Settings,
    cols: u16,
    rows: u16,
) -> Option<Flow> {
    use chrome::settings::{Dim, Edit, Kind, RowId};
    use event::KeyCode as K;

    let grps = chrome::settings::groups(st, view);
    if grps.is_empty() {
        return None;
    }
    st.group = st.group.min(grps.len() - 1);
    let grp = &grps[st.group];
    st.row = st.row.min(grp.rows.len().saturating_sub(1));
    let row = grp.rows.get(st.row)?;
    let (id, kind, value) = (row.id, row.kind.clone(), row.value.clone());
    let last = grp.rows.len().saturating_sub(1);
    let moved = Some(Flow::SettingsEdit(Edit::Moved));

    match k.code {
        K::Down | K::Char('j') => {
            match (st.open, &kind) {
                (Some(o), Kind::Choice(opts)) => {
                    st.open = Some((o + 1).min(opts.len().saturating_sub(1)))
                }
                _ => st.row = (st.row + 1).min(last),
            }
            moved
        }
        K::Up | K::Char('k') => {
            match (st.open, &kind) {
                (Some(o), Kind::Choice(_)) => st.open = Some(o.saturating_sub(1)),
                _ => st.row = st.row.saturating_sub(1),
            }
            moved
        }
        // Groups only when nothing is expanded: inside a list, Tab would leave
        // a preview applied with no cursor left pointing at it.
        K::Tab if st.open.is_none() => {
            st.group = (st.group + 1) % grps.len();
            st.row = 0;
            moved
        }
        K::BackTab if st.open.is_none() => {
            st.group = (st.group + grps.len() - 1) % grps.len();
            st.row = 0;
            moved
        }
        K::Enter => match (st.open, &kind) {
            (Some(o), Kind::Choice(opts)) => {
                let chosen = opts.get(o)?.clone();
                st.open = None;
                Some(Flow::SettingsEdit(match id {
                    RowId::Theme => Edit::Theme(chosen),
                    // The first option is "no pin at all", and it writes an
                    // absent key rather than an agent by that name.
                    RowId::DefaultAgent if chosen == chrome::settings::ASK_EVERY_TIME => {
                        Edit::DefaultAgent(None)
                    }
                    RowId::DefaultAgent => Edit::DefaultAgent(Some(chosen)),
                    _ => Edit::Moved,
                }))
            }
            // Open on whatever is current, so the cursor starts at the value
            // rather than at the top of the list.
            (None, Kind::Choice(opts)) => {
                st.open = Some(opts.iter().position(|o| *o == value).unwrap_or(0));
                moved
            }
            _ => Some(Flow::Continue),
        },
        K::Char(' ') => match (&kind, id) {
            (Kind::Toggle(on), RowId::AutoAttach) => {
                Some(Flow::SettingsEdit(Edit::AutoAttach(!on)))
            }
            (Kind::Toggle(on), RowId::Links) => Some(Flow::SettingsEdit(Edit::Links(!on))),
            (Kind::Toggle(on), RowId::UpdateCheck) => {
                Some(Flow::SettingsEdit(Edit::UpdateCheck(!on)))
            }
            _ => Some(Flow::Continue),
        },
        // `-`/`+` rather than the arrows: left and right mean nothing else on
        // this page, but a size row is the only thing they would act on, and a
        // pair of keys that works on one row in six reads as broken on the
        // other five. `h`/`l` come along because they always do here.
        K::Char('-') | K::Char('h') | K::Left => nudge(view, &kind, cols, rows, -2),
        K::Char('+') | K::Char('=') | K::Char('l') | K::Right => nudge(view, &kind, cols, rows, 2),
        // Back to sizing itself to the terminal, which is a real state and the
        // one every band starts in — so there has to be a way back to it.
        K::Char('0') => match &kind {
            Kind::Size(Dim::Band(b)) => {
                chrome::set_band(&mut view.geom, rows, *b, None);
                Some(Flow::SettingsEdit(Edit::Geom))
            }
            _ => Some(Flow::Continue),
        },
        // Esc closes the list first and the page second. Abandoning a preview
        // must not also throw you off the page you were previewing on.
        K::Esc | K::Char('q') => {
            if st.open.is_some() {
                st.open = None;
                return moved;
            }
            view.page = st.ret;
            Some(Flow::Continue)
        }
        _ => None,
    }
}

/// How long the open topic is, and how many rows of it are on screen.
///
/// One function because three things need the same two numbers and must agree:
/// the key handler clamping a scroll, the wheel doing the same, and the page
/// itself deciding whether to say there is more below. They come from
/// [`chrome::help::read`], so what is counted is what is drawn — a topic laid
/// out for a narrow terminal is *longer*, and a clamp computed from the source
/// would strand the last lines of it off the bottom.
fn help_metrics(view: &View, st: &chrome::Help, cols: u16, rows: u16) -> (usize, u16) {
    let topics = crate::reference::TOPICS;
    let geom = chrome::page_geom(cols, rows, view);
    let area = chrome::help::columns(geom.stage_box);
    let topic = &topics[st.topic.min(topics.len() - 1)];
    let lines = chrome::help::read(topic, &view.prefix, chrome::help::text_width(area.body));
    (lines.len(), chrome::help::text_height(area.body))
}

/// Keys on the HELP page. `None` means "not mine, carry on".
///
/// A reading page, so the keys are a pager's: `j`/`k` and the arrows move a
/// line, `space` and the page keys move a screen, `home`/`end` are the ends.
/// `tab` walks the topics, because that is what `tab` does everywhere else in
/// this client — it moves between the things a page holds.
///
/// It is consulted before the stage forward, for the reason SETTINGS and GIT
/// are: this page has no pane, so a bare `j` that fell through would be typed
/// into whatever shell was running behind it.
fn handle_help_key(
    k: event::KeyEvent,
    view: &mut View,
    st: &mut chrome::Help,
    cols: u16,
    rows: u16,
) -> Option<Flow> {
    use event::KeyCode as K;
    let topics = crate::reference::TOPICS;
    let (lines, height) = help_metrics(view, st, cols, rows);
    let max = chrome::help::max_scroll(lines, height);
    // A screen, less two rows of overlap: the lines you were reading when you
    // pressed it stay on screen, which is what makes a paging key readable.
    let screen = (height as usize).saturating_sub(2).max(1);

    match k.code {
        K::Down | K::Char('j') => st.scroll = (st.scroll + 1).min(max),
        K::Up | K::Char('k') => st.scroll = st.scroll.saturating_sub(1),
        K::PageDown | K::Char(' ') | K::Char('f') => st.scroll = (st.scroll + screen).min(max),
        K::PageUp | K::Char('b') => st.scroll = st.scroll.saturating_sub(screen),
        K::Home | K::Char('g') => st.scroll = 0,
        K::End | K::Char('G') => st.scroll = max,
        // The topics, forwards and back. `n`/`p` come along because the pair is
        // what every reader binds them to, and neither means anything else here.
        K::Tab | K::Char('n') | K::Right | K::Char('l') => {
            st.go((st.topic + 1) % topics.len());
        }
        K::BackTab | K::Char('p') | K::Left | K::Char('h') => {
            st.go((st.topic + topics.len() - 1) % topics.len());
        }
        // Bound here rather than left to the global table, where `q` detaches
        // the client: leaving a page you opened to read something must never be
        // the same key as leaving the client.
        K::Esc | K::Char('q') => return Some(Flow::CloseHelp),
        _ => return None,
    }
    Some(Flow::Continue)
}

/// A click on the HELP page: a topic in the contents column, or nothing.
///
/// Returns the same flow the keys do, so a clicked row and the `tab` that would
/// have reached it are one code path.
fn help_click(
    view: &View,
    st: &mut chrome::Help,
    cols: u16,
    rows: u16,
    x: u16,
    y: u16,
) -> Option<Flow> {
    let geom = chrome::page_geom(cols, rows, view);
    let area = chrome::help::columns(geom.stage_box);
    if !area.list.contains(x, y) {
        return None;
    }
    st.go(chrome::help::topic_at(area.list, y)?);
    Some(Flow::Continue)
}

/// Grow or shrink whatever the size row under the cursor measures.
///
/// Rails go through `resize_rail` and bands through `nudge_band`, which are the
/// same two functions LAYOUT mode's drag calls — so a width you can type is a
/// width you could have dragged to, and neither gesture has its own floor.
fn nudge(
    view: &mut View,
    kind: &chrome::settings::Kind,
    cols: u16,
    rows: u16,
    delta: i16,
) -> Option<Flow> {
    use chrome::settings::{Dim, Edit, Kind};
    let Kind::Size(dim) = kind else { return Some(Flow::Continue) };
    let want = chrome::system_h_wanted(&view.gauges);
    match dim {
        Dim::LeftRail => chrome::resize_rail(&mut view.geom, cols, true, delta),
        Dim::RightRail => chrome::resize_rail(&mut view.geom, cols, false, delta),
        Dim::Band(b) => chrome::nudge_band(&mut view.geom, rows, *b, delta, want),
    }
    Some(Flow::SettingsEdit(Edit::Geom))
}

/// Keys on the Docker page. `None` means "not mine, carry on".
///
/// Every action is a command run in the workspace, which is `POST .../processes`
/// — the route the PROCESSES rail already uses. There is no docker verb on the
/// wire, and adding one would be a side channel for something the API can
/// already say.
fn handle_docker_key(
    k: event::KeyEvent,
    view: &mut View,
    docker: &mut Docker,
    sys: &butai_protocol::api::SysDto,
    ws: Option<&WorkspaceDetail>,
) -> Option<Flow> {
    let cwd = ws.map(|w| w.cwd.as_str()).unwrap_or("");
    let stacks = chrome::project_stacks(sys, cwd);
    let rows = chrome::docker_rows(&stacks);
    // What the cursor names: one container, or a whole stack.
    let target = rows.get(docker.sel.min(rows.len().saturating_sub(1))).map(|row| match row {
        DockerRow::Stack(i) => (stacks[*i], stacks[*i].dto.label.clone(), None),
        DockerRow::Container { stack, name, .. } => {
            (stacks[*stack], (*name).to_string(), Some((*name).to_string()))
        }
    });

    match k.code {
        event::KeyCode::Esc | event::KeyCode::Char('q') => {
            view.page = Page::Agents;
            Some(Flow::Continue)
        }
        event::KeyCode::Down | event::KeyCode::Char('j') => {
            docker.move_sel(1, rows.len());
            Some(Flow::Continue)
        }
        event::KeyCode::Up | event::KeyCode::Char('k') => {
            docker.move_sel(-1, rows.len());
            Some(Flow::Continue)
        }
        event::KeyCode::Enter => {
            let (stack, label, container) = target?;
            Some(Flow::RunProcess {
                name: format!("logs {label}"),
                command: docker_command(&stack, container.as_deref(), "logs -f --tail 200"),
                then: Spawned::Follow(label),
            })
        }
        // These two are the effect, not the pane: `docker restart` exits in a
        // second, so it stays in the rail and this page stays up. Staging it
        // would close the Docker page to show something already finished.
        event::KeyCode::Char(verb @ ('r' | 'x')) => {
            let (stack, label, container) = target?;
            let word = if verb == 'r' { "restart" } else { "stop" };
            Some(Flow::RunProcess {
                name: format!("{word} {label}"),
                command: docker_command(&stack, container.as_deref(), word),
                then: Spawned::Rail,
            })
        }
        // A shell is an interactive process, so it belongs on the stage with
        // the agents rather than in this page's logs column.
        event::KeyCode::Char('s') => {
            let (stack, label, container) = target?;
            // A one-container stack *is* its container, so the header row can
            // open a shell too; a multi-container project cannot — there is no
            // single thing to exec into.
            let name = container.or_else(|| {
                stack
                    .dto
                    .containers
                    .first()
                    .map(|c| c.name.clone())
                    .filter(|_| stack.dto.containers.len() == 1)
            })?;
            Some(Flow::RunProcess {
                name: label,
                command: format!(
                    "docker exec -it {} sh -lc 'exec ${{SHELL:-sh}}'",
                    shell_quote(&name)
                ),
                then: Spawned::Stage,
            })
        }
        _ => None,
    }
}

/// The docker command for a container, or for a whole stack.
///
/// The three shapes are the daemon's, unchanged: a compose project with a
/// working directory runs `docker compose` from it, one without runs
/// `docker compose -p`, and a standalone stack is just its containers named
/// directly. Getting the last one wrong is how `docker logs` ends up with no
/// argument at all.
fn docker_command(stack: &chrome::Stack<'_>, container: Option<&str>, verb: &str) -> String {
    if let Some(name) = container {
        return format!("docker {verb} {}", shell_quote(name));
    }
    let dto = stack.dto;
    if dto.project.is_empty() {
        let names: Vec<String> = dto.containers.iter().map(|c| shell_quote(&c.name)).collect();
        return format!("docker {verb} {}", names.join(" "));
    }
    // `--ansi always` keeps compose's per-service prefix colours even though it
    // runs off a pipe rather than a full TTY. It is a compose flag only.
    let sub = if verb.starts_with("logs") { format!("--ansi always {verb}") } else { verb.into() };
    if dto.workdir.is_empty() {
        format!("docker compose -p {} {sub}", shell_quote(&dto.project))
    } else {
        format!("cd {} && docker compose {sub}", shell_quote(&dto.workdir))
    }
}

/// Single-quote a string for `sh`, the way the daemon's own docker commands do.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Keys on the Diff page. `None` means "not mine, carry on".
///
/// The keys are the daemon's: `]`/`[` walk hunks, `Space` takes the one under
/// the cursor, `v` drops into line-select, `x` discards. Everything except the
/// two that write the repository resolves here without a round trip.
fn handle_diff_key(k: event::KeyEvent, view: &mut View, diff: &mut DiffView) -> Option<Flow> {
    let page = diff.page();
    match k.code {
        // In line-select, Esc backs out of the selection rather than the page —
        // the same thing `v` does, and what the daemon's pane bound it to.
        event::KeyCode::Esc if diff.mode == DiffMode::Lines => {
            diff.cancel_line_select();
            Some(Flow::Continue)
        }
        // `q` closes the view rather than the session. The daemon's diff pane
        // swallowed it — no verb is bound to it — and letting it fall through to
        // the workbench's detach would turn the universal "close this" into
        // "end my session", from a screen you opened to read something.
        event::KeyCode::Esc | event::KeyCode::Char('q') => {
            view.page = Page::Agents;
            Some(Flow::Continue)
        }
        // In line-select, j/k walk the hunk's changed lines instead of
        // scrolling: the cursor is the thing being moved, and letting the view
        // scroll out from under it was the obvious way to make "pick line 4"
        // mean the wrong line.
        event::KeyCode::Down | event::KeyCode::Char('j') if diff.mode == DiffMode::Lines => {
            diff.step_line(1);
            Some(Flow::Continue)
        }
        event::KeyCode::Up | event::KeyCode::Char('k') if diff.mode == DiffMode::Lines => {
            diff.step_line(-1);
            Some(Flow::Continue)
        }
        event::KeyCode::Down | event::KeyCode::Char('j') => {
            diff.scroll_by(1);
            Some(Flow::Continue)
        }
        event::KeyCode::Up | event::KeyCode::Char('k') => {
            diff.scroll_by(-1);
            Some(Flow::Continue)
        }
        event::KeyCode::PageDown => {
            diff.scroll_by(page as isize);
            Some(Flow::Continue)
        }
        event::KeyCode::PageUp => {
            diff.scroll_by(-(page as isize));
            Some(Flow::Continue)
        }
        event::KeyCode::Home | event::KeyCode::Char('g') => {
            diff.scroll = 0;
            Some(Flow::Continue)
        }
        event::KeyCode::End | event::KeyCode::Char('G') => {
            diff.scroll_to_end();
            Some(Flow::Continue)
        }
        event::KeyCode::Char(']') => {
            diff.step_hunk(1);
            Some(Flow::Continue)
        }
        event::KeyCode::Char('[') => {
            diff.step_hunk(-1);
            Some(Flow::Continue)
        }
        event::KeyCode::Char('v') => {
            match diff.mode {
                DiffMode::Read => diff.line_select(),
                DiffMode::Lines => diff.cancel_line_select(),
            }
            Some(Flow::Continue)
        }
        event::KeyCode::Char(' ') if diff.mode == DiffMode::Lines => {
            diff.pick_line();
            Some(Flow::Continue)
        }
        // Space takes the hunk; Enter takes the picked lines. Two keys because
        // they are two operations — and Enter does nothing in read mode rather
        // than quietly meaning "the whole hunk", which is the one mistake a
        // partial-staging tool must not make.
        event::KeyCode::Char(' ') => Some(Flow::ApplyDiff { discard: false }),
        event::KeyCode::Enter if diff.mode == DiffMode::Lines => {
            Some(Flow::ApplyDiff { discard: false })
        }
        event::KeyCode::Char('x') => Some(Flow::ApplyDiff { discard: true }),
        event::KeyCode::Char('r') => {
            diff.kind.clone().map(|kind| Flow::OpenDiff { kind, keep_cursor: true })
        }
        _ => None,
    }
}

/// The diff the CHANGES cursor names, if it names one.
fn diff_under_cursor(ws: Option<&WorkspaceDetail>, sel: usize) -> Option<DiffKind> {
    let c = ws?.changes.as_ref()?;
    match chrome::change_rows(c).get(sel)? {
        // A whole section: the same thing the daemon's rail does when the
        // cursor is on a heading.
        chrome::ChangeRow::Header("Unstaged") => Some(DiffKind::Unstaged { path: None }),
        chrome::ChangeRow::Header("Staged") => Some(DiffKind::Staged { path: None }),
        chrome::ChangeRow::Header(_) => None,
        chrome::ChangeRow::File { change, staged: false } => {
            Some(DiffKind::Unstaged { path: Some(change.path.clone()) })
        }
        chrome::ChangeRow::File { change, staged: true } => {
            Some(DiffKind::Staged { path: Some(change.path.clone()) })
        }
        // A conflicted file's worktree still holds the markers, so its unstaged
        // diff is the honest view of what has to be resolved.
        chrome::ChangeRow::Conflicted { path } => {
            Some(DiffKind::Unstaged { path: Some((*path).to_string()) })
        }
        chrome::ChangeRow::Commit { id, summary } => {
            Some(DiffKind::Commit { id: (*id).to_string(), summary: (*summary).to_string() })
        }
    }
}

/// Keys while a modal is up.
///
/// Deliberately small: an overlay answers one question, so it only needs a
/// cursor, a commit and a cancel.
fn handle_overlay_key(k: event::KeyEvent, view: &mut View) -> Flow {
    // A prompt is being typed into, so it claims every printable key — `q`
    // included, which the others use to dismiss. It gets its own handler for
    // that reason, and nothing below can steal a character out of a commit
    // message.
    if matches!(view.overlay, Some(Overlay::Prompt(_))) {
        return handle_prompt_key(k, view);
    }
    if matches!(view.overlay, Some(Overlay::Search(_))) {
        return handle_search_key(k, view);
    }
    let Some(overlay) = view.overlay.as_mut() else { return Flow::Continue };
    match (&mut *overlay, k.code) {
        (_, event::KeyCode::Esc) | (_, event::KeyCode::Char('q')) => view.overlay = None,
        (Overlay::List(list), event::KeyCode::Down | event::KeyCode::Char('j')) => list.move_sel(1),
        (Overlay::List(list), event::KeyCode::Up | event::KeyCode::Char('k')) => list.move_sel(-1),
        (Overlay::List(_), event::KeyCode::Enter) => return Flow::Choose,
        // `d` pins the highlighted agent as the default.
        //
        // The help screen has advertised this under DEFAULT AGENT since the pin
        // existed, and nothing bound it: the key fell into the catch-all below
        // and was swallowed, leaving `:agent-default` as the only route to a
        // pin that the rail's `[+ NAME]` button is built around. The flow, the
        // writer and the help text were all already here — only the keystroke
        // was missing.
        //
        // Only on the agent picker. The other lists are branches, hosts,
        // machines and menus, where "make this the default" means nothing, and
        // a `d` that did something different per list is a key you cannot
        // learn. The overlay closes because the pin is confirmed by a flash in
        // the footer, which an open modal would cover.
        (Overlay::List(list), event::KeyCode::Char('d'))
            if list.kind == ListKind::SpawnAgent && list.chosen().is_some() =>
        {
            let name = list.chosen().map(str::to_string);
            view.overlay = None;
            return Flow::PinAgent(name);
        }
        // `y` copies the highlighted link, which is `y` on a commit's sha in
        // the GIT page — the one letter this workbench already spells "take
        // this string with me". Only on the link picker, for the reason `d` is
        // only on the agent one: a key that means something different per list
        // is a key nobody can learn.
        (Overlay::List(list), event::KeyCode::Char('y'))
            if list.kind == ListKind::Links && list.chosen().is_some() =>
        {
            let url = list.chosen().unwrap_or_default().to_string();
            view.overlay = None;
            return Flow::CopyLink(url);
        }
        (
            Overlay::Confirm(c),
            event::KeyCode::Down
            | event::KeyCode::Up
            | event::KeyCode::Char('j')
            | event::KeyCode::Char('k')
            | event::KeyCode::Tab,
        ) => c.yes = !c.yes,
        // `y` answers directly, because a confirm box that needs two keystrokes
        // to say yes is one people learn to hammer.
        (Overlay::Confirm(c), event::KeyCode::Char('y')) => {
            c.yes = true;
            return confirm(view);
        }
        // `n` goes through the same answering path as `y` rather than just
        // dropping the box. For everything destructive that is the same thing —
        // [`confirm`] returns `Flow::Continue` when the answer was no — but an
        // update prompt's "no" *means* something, and has to be remembered.
        (Overlay::Confirm(c), event::KeyCode::Char('n')) => {
            c.yes = false;
            return confirm(view);
        }
        (Overlay::Confirm(_), event::KeyCode::Enter) => return confirm(view),
        _ => {}
    }
    Flow::Continue
}

/// The pointer while a modal is up.
///
/// The daemon had seven of these, one per modal, each with a hand-written width
/// to hit-test against. There is one here because there is one renderer: the
/// modal reports which of *its own drawn lines* is under the pointer, and this
/// turns that into the keystroke the line stands for — so a clicked row and the
/// Enter it advertises are the same code path.
fn overlay_mouse(m: &event::MouseEvent, view: &mut View, cols: u16, rows: u16) -> Flow {
    let key = |code| event::KeyEvent::new(code, event::KeyModifiers::NONE);
    match m.kind {
        // The wheel moves the cursor, which is what the arrows do — a modal
        // long enough to scroll is exactly the one you reach for the wheel on.
        event::MouseEventKind::ScrollUp => {
            return handle_overlay_key(key(event::KeyCode::Up), view)
        }
        event::MouseEventKind::ScrollDown => {
            return handle_overlay_key(key(event::KeyCode::Down), view)
        }
        event::MouseEventKind::Down(_) => {}
        _ => return Flow::Continue,
    }
    let Some(overlay) = view.overlay.as_mut() else { return Flow::Continue };
    let Some(row) = chrome::overlay_hit(cols, rows, overlay, m.column, m.row) else {
        // Outside the box: dismiss. The one thing a click past a question can
        // mean is "not this".
        view.overlay = None;
        return Flow::Continue;
    };
    match overlay {
        // The prompt's row is text being typed, so clicking it keeps the modal
        // rather than answering something the user did not ask.
        Overlay::Prompt(_) => Flow::Continue,
        Overlay::List(list) => {
            if row >= list.items.len() {
                return Flow::Continue;
            }
            list.sel = row;
            Flow::Choose
        }
        // Two rows above the hits: the query, then a blank.
        Overlay::Search(f) => {
            let Some(i) = row.checked_sub(2) else { return Flow::Continue };
            if i >= f.hits.len() {
                return Flow::Continue;
            }
            f.sel = i;
            handle_search_key(key(event::KeyCode::Enter), view)
        }
        // Row 2 is `no` and row 3 is `yes`; the header and the blank above them
        // answer nothing.
        Overlay::Confirm(c) => match row {
            2 => {
                c.yes = false;
                confirm(view)
            }
            3 => {
                c.yes = true;
                confirm(view)
            }
            _ => Flow::Continue,
        },
    }
}

/// Close a confirm box and do what it was asking about, if it was answered yes.
fn confirm(view: &mut View) -> Flow {
    let Some(Overlay::Confirm(c)) = view.overlay.take() else { return Flow::Continue };
    if !c.yes {
        // For every question about destroying something, no means "leave it
        // alone" and there is nothing left to do. An update prompt is the one
        // question where the answer is worth keeping: no means *this version*,
        // not "not now", and the file has to learn which one.
        //
        // `esc` never reaches here — it drops the overlay in
        // [`handle_overlay_key`] — which is what makes dismissing the box the
        // way to be asked again next launch.
        return match c.kind {
            chrome::ConfirmKind::Update { version } => Flow::DeclineUpdate(version),
            _ => Flow::Continue,
        };
    }
    match c.kind {
        chrome::ConfirmKind::Discard { path } => Flow::Git(GitAction::Discard(path)),
        chrome::ConfirmKind::DeleteFile { path } => Flow::DeleteFile(path),
        chrome::ConfirmKind::CloseWorkspace { id, .. } => Flow::CloseWorkspace(id),
        // Hand the action back to the same path that asked, with the answer
        // recorded so it goes through this time.
        chrome::ConfirmKind::Pick { target, value, .. } => Flow::Pick { target, value },
        chrome::ConfirmKind::Update { .. } => Flow::Update,
        chrome::ConfirmKind::MenuAction => match view.pending_menu_action.take() {
            Some(action) => {
                view.confirmed_menu_action = Some(action);
                Flow::MenuAction(action)
            }
            None => Flow::Continue,
        },
    }
}

/// Keys while the search box is up.
///
/// Typing narrows, arrows choose, Enter opens. Every printable key is part of
/// the query, so like the prompt this claims the keyboard — and unlike it, it
/// re-runs the search on each change rather than waiting for Enter.
fn handle_search_key(k: event::KeyEvent, view: &mut View) -> Flow {
    let ctrl = k.modifiers.contains(event::KeyModifiers::CONTROL);
    match k.code {
        event::KeyCode::Esc => {
            view.overlay = None;
            return Flow::Continue;
        }
        event::KeyCode::Enter => {
            let Some(Overlay::Search(f)) = view.overlay.take() else { return Flow::Continue };
            let Some(hit) = f.chosen().cloned() else { return Flow::Continue };
            // Open on the Files page, where the editor already lives; the line
            // number scrolls it to the match.
            view.page = Page::Files;
            return Flow::OpenFileAt { path: hit.path, line: hit.line };
        }
        event::KeyCode::Down => {
            if let Some(Overlay::Search(f)) = view.overlay.as_mut() {
                f.move_sel(1);
            }
            return Flow::Continue;
        }
        event::KeyCode::Up => {
            if let Some(Overlay::Search(f)) = view.overlay.as_mut() {
                f.move_sel(-1);
            }
            return Flow::Continue;
        }
        _ => {}
    }
    let Some(Overlay::Search(f)) = view.overlay.as_mut() else { return Flow::Continue };
    match k.code {
        event::KeyCode::Backspace => f.backspace(),
        event::KeyCode::Char(c) if !ctrl => f.insert(c),
        _ => return Flow::Continue,
    }
    f.searching = true;
    Flow::Search(f.query.clone())
}

/// Keys while a line of text is being typed.
///
/// Deliberately a line editor and not an editor: arrows, Home/End, Backspace,
/// Delete, and characters. `Ctrl` combinations are left alone so the terminal's
/// own bindings keep working.
fn handle_prompt_key(k: event::KeyEvent, view: &mut View) -> Flow {
    let ctrl = k.modifiers.contains(event::KeyModifiers::CONTROL);
    match k.code {
        event::KeyCode::Esc => {
            view.overlay = None;
            return Flow::Continue;
        }
        event::KeyCode::Enter => {
            let Some(Overlay::Prompt(p)) = view.overlay.take() else { return Flow::Continue };
            // An empty commit message is a `git commit` that opens an editor
            // nobody here can see, so it is refused now rather than there. The
            // wording is the prompt's, not this arm's: every kind refusing with
            // "a commit needs a message" is how an empty box comes to answer a
            // question nobody asked.
            if p.text.trim().is_empty() {
                view.flash = Some(match p.kind {
                    chrome::PromptKind::SshDestination => "a machine needs a destination".into(),
                    chrome::PromptKind::NewFolder { .. } => "a folder needs a name".into(),
                    _ => "a commit needs a message".to_string(),
                });
                // Every other prompt is entered from the workbench, so refusing
                // by closing puts you back where you started. This one is a step
                // *inside* the picker: closing it would throw away the machine
                // and the browsing that got here, and a stray second Enter would
                // be enough to do it. So the caret stays, and the same rule —
                // land where you were — is what keeps this box open.
                if matches!(p.kind, chrome::PromptKind::NewFolder { .. }) {
                    view.overlay = Some(Overlay::Prompt(p));
                }
                return Flow::Continue;
            }
            return match p.kind {
                chrome::PromptKind::Commit { all } => {
                    Flow::Git(GitAction::Commit { message: p.text, all })
                }
                chrome::PromptKind::NewBranch => Flow::Git(GitAction::NewBranch(p.text)),
                chrome::PromptKind::NewTag => Flow::Git(GitAction::NewTag(p.text)),
                chrome::PromptKind::NewWorktree => Flow::Git(GitAction::NewWorktree(p.text)),
                // Trimmed, because a destination pasted out of a terminal
                // brings its trailing space with it and `ssh " host"` is not a
                // host.
                chrome::PromptKind::SshDestination => Flow::DialHost(p.text.trim().to_string()),
                // Trimmed for the same reason, and because ` proj` is a folder
                // whose name begins with a space — legal, and never what was
                // meant. The daemon refuses the rest: separators, `.` and `..`.
                chrome::PromptKind::NewFolder { dir } => {
                    Flow::MakeFolder { dir, name: p.text.trim().to_string() }
                }
                // The `:` prompt speaks the same mini-language `[keys]` binds,
                // so a typed command and a bound one go the same way from here.
                chrome::PromptKind::Command => match crate::keymap::parse_action(&p.text) {
                    Ok(action) => run_bound(keys::bind(action), view),
                    Err(e) => {
                        view.flash = Some(format!("{e}"));
                        Flow::Continue
                    }
                },
            };
        }
        _ => {}
    }
    let Some(Overlay::Prompt(prompt)) = view.overlay.as_mut() else { return Flow::Continue };
    match k.code {
        event::KeyCode::Backspace => prompt.backspace(),
        event::KeyCode::Delete => prompt.delete(),
        event::KeyCode::Left => prompt.move_cursor(-1),
        event::KeyCode::Right => prompt.move_cursor(1),
        event::KeyCode::Home => prompt.to_start(),
        event::KeyCode::End => prompt.to_end(),
        event::KeyCode::Char(c) if !ctrl => prompt.insert(c),
        _ => {}
    }
    Flow::Continue
}

/// Carry out a click on BOOTH's fleet list.
///
/// The one list in the workbench where clicking a row cannot also open it: see
/// [`hit::FleetHit`] for why. A row moves the cursor and nothing else — which
/// re-points BOOTH's middle column at that agent's screen, because the preview
/// follows the cursor — and only `[open]` travels.
fn fleet_click(hit: hit::FleetHit, view: &mut View) -> Flow {
    let (row, flow) = match hit {
        hit::FleetHit::Open(row) => (row, Flow::OpenFleetAgent(row)),
        hit::FleetHit::Row(row) => (row, Flow::Continue),
    };
    view.all_agents_sel = row;
    view.focus = Focus::AllAgents;
    flow
}

/// Carry out a click, once [`hit::at`] has said what is under it.
///
/// Every arm goes the same way a key would: the pointer is a second way to
/// reach the workbench's vocabulary, not a vocabulary of its own. That is why
/// this returns a `Flow` rather than doing the work — the loop already knows
/// how to spawn an agent, and a click that learned it again would be a second
/// place to fix when it changes.
///
/// Clicking a row selects it; clicking the row already selected stages it,
/// which is what a second click means everywhere else in the interface too.
/// BOOTH's fleet is the exception, and has [`fleet_click`] to itself.
fn run_click(
    target: hit::Target,
    view: &mut View,
    tab_count: usize,
    ws: Option<&WorkspaceDetail>,
) -> Flow {
    use hit::Target;
    match target {
        Target::Tab(i) if i < tab_count => {
            select_tab(view, i);
            Flow::Continue
        }
        Target::Tab(_) => Flow::Continue,
        // The two-step close the daemon's `[x]` had, as the confirm box every
        // other destructive verb in this client already opens — one modal
        // rather than a flash message and an armed flag nothing on screen names.
        Target::CloseTab => {
            let Some(ws) = ws else { return Flow::Continue };
            view.overlay = Some(close_workspace_confirm(ws));
            Flow::Continue
        }
        // Clicking the space you are already on goes back to the agents page, so
        // the button is a toggle the way `alt-o` and `alt-c` are.
        // SETTINGS is entered and left rather than toggled to AGENTS like a
        // space: it remembers the page it was opened from, and pressing its
        // button again has to put you back there rather than somewhere you were
        // not. The flow is what carries that.
        Target::Space(Page::Settings) if view.page != Page::Settings => Flow::OpenSettings,
        Target::Space(Page::Settings) => Flow::CloseSettings,
        Target::Space(page) => {
            view.page = if view.page == page { Page::Agents } else { page };
            open_page(view)
        }
        Target::NewWorkspace => {
            // Each press asks again: which machine is part of the question, not
            // a setting, and remembering the last answer is how a workspace
            // silently lands on a machine you stopped thinking about.
            view.browse_daemon = None;
            Flow::Browse(String::new())
        }
        // The MACHINES picker. It opens with the machines already here at the
        // top of the list and the cursor on the first of them, each one saying
        // that Enter drops it — which is the whole reason a count is worth
        // pressing, and why the count and the `[+ host]` offer beside it are one
        // button rather than two. The picker needs the connected hosts, which
        // the loop has and this does not; `Flow::PickHost` is where it gets
        // them.
        Target::Machines => Flow::PickHost,
        Target::Spaces => Flow::PickSpace,
        Target::Footer("[layout]") => Flow::ToggleLayout,
        Target::Footer("[detach]") => Flow::Detach,
        Target::Footer("[help]") => run_view(ViewVerb::Help, view),
        // The same two flows the chip used to send, so the button and `alt-s`
        // are one gesture and SETTINGS is still a page you enter and leave.
        Target::Footer("[settings]") if view.page != Page::Settings => Flow::OpenSettings,
        Target::Footer("[settings]") => Flow::CloseSettings,
        Target::Footer(_) => Flow::Continue,
        // Through each rail's own key handler, with the key the button
        // advertises: a clicked verb and the keystroke it names are then the
        // same code path, and cannot come to mean different things.
        //
        // Clicking a verb focuses its section first, because every one of them
        // acts on that section's cursor — including the `[+ agent]` and
        // `[+ term]` buttons, which resolve here too. Without it, `x` under a
        // list you had not focused would kill a row you could not see selected.
        Target::AgentsVerb(key) | Target::ProcsVerb(key) => {
            let focus = match target {
                Target::AgentsVerb(_) => Focus::Agents,
                _ => Focus::Processes,
            };
            view.focus = focus;
            let rows = match (focus, ws) {
                (Focus::Agents, Some(w)) => w.agents.len(),
                (Focus::Processes, Some(w)) => w.processes.len(),
                _ => 0,
            };
            let ev = event::KeyEvent::new(event::KeyCode::Char(key), event::KeyModifiers::NONE);
            let pinned = view.pinned_agent.is_some();
            handle_rail_key(ev, focus, rows, pinned).unwrap_or(Flow::Continue)
        }
        Target::ChangesVerb(key) => {
            view.focus = Focus::Changes;
            let ev = event::KeyEvent::new(event::KeyCode::Char(key), event::KeyModifiers::NONE);
            handle_changes_key(ev, view, ws).unwrap_or(Flow::Continue)
        }
        Target::Rail(focus, row) => {
            let again = view.focus == focus && selection(view, focus) == row;
            view.focus = focus;
            set_selection(view, focus, row);
            if again {
                return stage_selected(focus, view.page, row);
            }
            Flow::Continue
        }
        // CPU and RAM are the first two gauges and open `htop`; anything below
        // is a GPU, and opens a GPU monitor. The same split the rail draws.
        //
        // Through the verb the keys use (`{prefix} S`, `{prefix} Y`,
        // `:monitor`), not a `Flow` of its own: this was the last click in the
        // workbench that had no keyboard spelling, and a second copy of what it
        // does is how the two would drift back apart.
        // NET and DSK open the system monitor, not one of their own: there is
        // no network or filesystem pane to open yet, and htop is still the
        // honest answer to "what is using this".
        Target::System(g) => {
            run_view(ViewVerb::Monitor { gpu: matches!(g, chrome::Gauge::Gpu(_)) }, view)
        }
        // The pane's own clicks are the daemon's business: only it knows
        // whether the program on the other end asked for the mouse.
        Target::Stage(..) => {
            view.focus = Focus::Stage;
            Flow::Continue
        }
        Target::Nothing => Flow::Continue,
    }
}

/// Carry out a context-menu row.
///
/// Every one of these is something a key already does; the menu is a second way
/// to reach them, not a second implementation, so each goes through the same
/// route the keyboard would use.
async fn run_menu_row(
    daemons: &mut Vec<Daemon>,
    hosts: &mut Vec<Option<String>>,
    view: &mut View,
    forwards: &mut Vec<crate::ssh::Forward>,
    sockets: &mut Vec<PathBuf>,
    target: chrome::MenuTarget,
    row: usize,
) -> Result<()> {
    use chrome::MenuTarget;
    let d = active_daemon(daemons, hosts, view);
    match (target, row) {
        (MenuTarget::Process(pane), 0) => {
            kill_process(daemons, hosts, view, pane).await?;
        }
        // The agent rows go to the workspace the *menu* names, not the one the
        // tab bar is on. On a rail those are the same workspace; on BOOTH's
        // fleet they are routinely not, and "close all agents" reading the
        // active tab would empty a project nobody had asked about.
        (MenuTarget::Agent { daemon, workspace, pane }, 0) => {
            kill_pane(daemons, Route { daemon, workspace, pane }).await?;
        }
        (MenuTarget::Agent { daemon, workspace, pane }, row) => {
            let keep = (row == 1).then_some(pane);
            let panes: Vec<PaneId> = daemons
                .get(daemon)
                .and_then(|d| d.state.workspace(workspace))
                .map(|w| w.agents.iter().map(|a| a.pane).filter(|p| Some(*p) != keep).collect())
                .unwrap_or_default();
            for pane in panes {
                kill_pane(daemons, Route { daemon, workspace, pane }).await?;
            }
        }
        (MenuTarget::Process(pane), _) => {
            let Some(ws) = active_workspace(daemons, hosts, view) else {
                anyhow::bail!("no workspace open")
            };
            let route = format!("/v1/workspaces/{}/processes/{pane}/restart", ws.id);
            daemons[d].api.post(&route, &serde_json::json!({})).await?;
            if view.staged == Some(pane) {
                view.staged = None;
            }
        }
        (MenuTarget::Tab(i), _) => {
            let (dd, t) = *tab_index(daemons, hosts).get(i).context("that tab has gone")?;
            let id = daemons[dd].state.tabs[t].id;
            daemons[dd].api.delete(&format!("/v1/workspaces/{id}")).await?;
        }
        // The same drop the picker's connected rows do, reached from the tab
        // that machine owns rather than from its name — so it resolves a tab
        // index to a daemon first, and [`disconnect_daemon`] does the rest.
        (MenuTarget::RemoteTab(i), _) => {
            let (dd, _) = *tab_index(daemons, hosts).get(i).context("that tab has gone")?;
            let host = disconnect_daemon(dd, daemons, hosts, sockets, forwards, view)?;
            forget_machine(&host, view);
        }
    }
    Ok(())
}

/// The context menu for whatever a right-click landed on, if it has one.
///
/// Built from what is under the pointer *now* and carrying the pane it names,
/// because the cursor can move between opening the menu and answering it — the
/// menu is about the row you right-clicked, not the row you end up on.
fn menu_for(
    target: hit::Target,
    ws: Option<&WorkspaceDetail>,
    daemons: &[Daemon],
    hosts: &[Option<String>],
    here: usize,
) -> Option<Overlay> {
    use chrome::MenuTarget;
    let ws = ws?;
    let kind = match target {
        hit::Target::Rail(Focus::Agents, row) => {
            MenuTarget::Agent { daemon: here, workspace: ws.id, pane: ws.agents.get(row)?.pane }
        }
        hit::Target::Rail(Focus::Processes, row) => {
            MenuTarget::Process(ws.processes.get(row)?.pane)
        }
        hit::Target::Tab(i) => {
            // A tab on another machine offers the host's action, not the
            // workspace's: closing someone else's project from here is not ours
            // to do, and dropping the link is.
            let (d, _) = *tab_index(daemons, hosts).get(i)?;
            match hosts.get(d).and_then(|h| h.as_ref()) {
                Some(_) => MenuTarget::RemoteTab(i),
                None => MenuTarget::Tab(i),
            }
        }
        _ => return None,
    };
    Some(menu_overlay(kind))
}

/// The context menu for a row of BOOTH's fleet.
///
/// Apart from [`menu_for`] because the fleet is not a `hit::Target` — its rows
/// come from the cross-daemon list rather than from the geometry, which is the
/// same reason [`hit::on_fleet`] is a second entry point. Both end in
/// [`menu_overlay`], so the right button, `m` and the rails cannot come to offer
/// different rows.
fn fleet_menu(fleet: &[chrome::AllAgentRow<'_>], sel: usize) -> Option<Overlay> {
    let at = fleet_route(fleet, sel)?;
    Some(menu_overlay(chrome::MenuTarget::Agent {
        daemon: at.daemon,
        workspace: at.workspace,
        pane: at.pane,
    }))
}

/// A menu target as the list overlay that shows it. The rows come from
/// [`chrome::MenuTarget::rows`], which the dispatch reads by index — one table,
/// so neither can grow a row the other does not know about.
fn menu_overlay(kind: chrome::MenuTarget) -> Overlay {
    let rows = kind.rows();
    Overlay::List(ListOverlay {
        title: kind.title().into(),
        items: rows.iter().map(|(label, _)| (*label).to_string()).collect(),
        values: None,
        sel: 0,
        kind: ListKind::Menu(kind),
    })
}

/// What `m` is pointing at.
///
/// The keyboard has no pointer, so the cursor stands in for one: on a rail it is
/// that row, and everywhere else it is the workspace itself — which is the menu
/// the tab chip carries, and where a remote tab's `Disconnect host` is reached
/// from the tab rather than from the machine's name (the picker's way).
///
/// A function of the view alone so it can be tested without a daemon, and so the
/// key and the right button are answered by the one [`menu_for`] rather than by
/// two builders that would drift.
fn menu_target(view: &View) -> hit::Target {
    match view.focus {
        Focus::Agents | Focus::Processes => {
            hit::Target::Rail(view.focus, selection(view, view.focus))
        }
        _ => hit::Target::Tab(view.tab),
    }
}

/// Carry out a click on a full-screen page's list.
///
/// The first click moves the cursor; a second on the same row opens it — the
/// same two-step the rails use, so a click means one thing across the whole
/// workbench rather than one thing per page.
fn page_click(
    view: &mut View,
    files: &mut Files,
    docker: &mut Docker,
    sys: &butai_protocol::api::SysDto,
    ws: Option<&WorkspaceDetail>,
    row: usize,
) -> Flow {
    // A repeat opens; the enter goes through the page's own key handler so a
    // click and the key it stands for cannot come to mean different things.
    let enter = event::KeyEvent::new(event::KeyCode::Enter, event::KeyModifiers::NONE);
    match view.page {
        Page::Files | Page::Docs => {
            if row >= files.entries.len() {
                return Flow::Continue;
            }
            let again = files.sel == row;
            files.sel = row;
            // Off the stage, or `j`/`k` would go on scrolling the open file
            // rather than walking the list just clicked in. `Agents` is what
            // "not the stage" is called here — it is the focus the page opens
            // with, so a click on the tree puts it back where it started.
            view.focus = Focus::Agents;
            if !again {
                return Flow::Continue;
            }
            handle_files_key(enter, view, files).unwrap_or(Flow::Continue)
        }
        _ => {
            let again = docker.sel == row;
            docker.sel = row;
            if !again {
                return Flow::Continue;
            }
            handle_docker_key(enter, view, docker, sys, ws).unwrap_or(Flow::Continue)
        }
    }
}

/// The wheel over a full-screen page. `None` means the page did not want it and
/// the rails (or the pane) should have a look.
///
/// The daemon forwarded a wheel event to whichever *pane* was under it, which
/// worked because every column of every page was a pane. Now only the docker
/// logs are, so each page's own scroll is a number this client already holds —
/// and moving that number is all a wheel over it can mean.
#[allow(clippy::too_many_arguments)]
fn page_wheel(
    view: &mut View,
    files: &mut Files,
    docs: &mut Files,
    diff: &mut DiffView,
    docker: &mut Docker,
    git: &mut chrome::Git,
    help: &mut chrome::Help,
    git_refs_len: usize,
    daemons: &[Daemon],
    hosts: &[Option<String>],
    machines_len: usize,
    delta: isize,
    cols: u16,
    rows: u16,
    x: u16,
    y: u16,
) -> Option<Flow> {
    // BOOTH has three columns and each takes the wheel differently: the fleet
    // moves its cursor, compute scrolls as a block, and the middle is a pane and
    // so is the daemon's scrollback like any other.
    //
    // Off `page_geom`, never `Chrome::compute`: BOOTH owns the whole band, and
    // the page-agnostic rectangles still have it sitting between two rails it
    // does not draw. Carving three columns out of that strip puts every one of
    // them somewhere the user is not pointing.
    if view.page == Page::Booth {
        let geom = chrome::page_geom(cols, rows, view);
        let c = chrome::booth_columns(chrome::booth_area(cols, &geom));
        if c.fleet_box.width > 0 && c.fleet_box.contains(x, y) {
            let was = view.focus;
            // Looking, not choosing — same as the rails.
            view.focus = chrome::Focus::AllAgents;
            move_sel(view, rail_counts(daemons, hosts, view), delta);
            view.focus = was;
            return Some(Flow::Continue);
        }
        if c.compute_box.width > 0 && c.compute_box.contains(x, y) {
            // Clamped to the last machine so the column cannot be scrolled into
            // empty space it can never scroll back from.
            let last = machines_len.saturating_sub(1);
            view.booth_compute_scroll =
                (view.booth_compute_scroll as isize + delta).clamp(0, last as isize) as usize;
            return Some(Flow::Continue);
        }
        return None;
    }
    // HELP is a page you read, so the wheel over it is the reading position —
    // the one thing on it that moves. Over the contents column it means the
    // same, because a list of eleven rows has nothing to scroll and scrolling
    // the text is unambiguously what was meant.
    if view.page == Page::Help {
        let (lines, height) = help_metrics(view, help, cols, rows);
        let max = chrome::help::max_scroll(lines, height);
        help.scroll = (help.scroll as isize + delta).clamp(0, max as isize) as usize;
        return Some(Flow::Continue);
    }
    // The GIT page has three scrollable columns, and the wheel belongs to
    // whichever one the pointer is over — not to whichever has focus.
    if view.page == Page::Git {
        let geom = chrome::page_geom(cols, rows, view);
        let c = chrome::git_columns(geom.stage_box);
        let step = |cur: usize, len: usize| {
            (cur as isize + delta).clamp(0, len.saturating_sub(1) as isize) as usize
        };
        // The wheel moves the *cursor*, which is what Files and Docker do. The
        // list offset follows the cursor here, so a second stored position is
        // one the selection could fall out of.
        if c.refs_rows.contains(x, y) {
            git.refs_sel = step(git.refs_sel, git_refs_len);
            return Some(Flow::Continue);
        }
        if c.hist_rows.contains(x, y) {
            git.hist_sel = step(git.hist_sel, git.log.len());
            return Some(Flow::Continue);
        }
        if c.body_box.contains(x, y) {
            if let Some(body) = git.body.as_mut() {
                body.scroll_by(delta);
            }
            return Some(Flow::Continue);
        }
        return None;
    }
    // The diff has no list column: it is one body, and the wheel scrolls it
    // wherever the pointer is inside the page.
    if view.page == Page::Diff {
        let geom = chrome::Chrome::compute(
            cols,
            rows,
            view.zen,
            view.geom,
            chrome::system_h_wanted(&view.gauges),
        );
        if !geom.stage_box.contains(x, y) {
            return None;
        }
        diff.scroll_by(delta);
        return Some(Flow::Continue);
    }
    let tree = page_tree(view.page, files, docs);
    let sel = if view.page.is_tree() { tree.sel } else { docker.sel };
    match hit::on_page(cols, rows, view, sel, x, y) {
        hit::PageTarget::Row(_) if view.page.is_tree() => {
            tree.move_sel(delta);
            Some(Flow::Continue)
        }
        hit::PageTarget::Row(_) => {
            docker.sel = (docker.sel as isize + delta).max(0) as usize;
            Some(Flow::Continue)
        }
        // Right of the list: the open file scrolls here; the docker logs are a
        // pane, so that one is still the daemon's scrollback.
        hit::PageTarget::Body if view.page.is_tree() => {
            if let Some(open) = tree.open.as_mut() {
                open.scroll_by(delta);
            }
            Some(Flow::Continue)
        }
        _ => None,
    }
}

/// Carry out a click on the GIT page.
///
/// A row click moves that list's cursor and focuses it; a *second* click on the
/// row already under the cursor runs its Enter verb, which is how the Docker
/// page's list behaves and what makes a single click harmless. A verb click
/// goes through [`git_verb_flow`] — the same function the keyboard uses, so a
/// button cannot do something its key does not.
fn git_click(
    target: hit::GitTarget,
    view: &mut View,
    git: &mut chrome::Git,
    changes: Option<&butai_protocol::api::ChangesDto>,
    here: Option<butai_protocol::SessionId>,
) -> Option<Flow> {
    match target {
        hit::GitTarget::RefRow(row) => {
            let again = view.focus == Focus::Refs && git.refs_sel == row;
            view.focus = Focus::Refs;
            git.refs_sel = row;
            if !again {
                return Some(Flow::Continue);
            }
            // Whatever this row's Enter means — scope, show, open — is what the
            // verb table says it is.
            let rows = chrome::ref_rows(git, changes, here);
            let kind = chrome::ref_row_kind(&rows, row);
            let id = crate::verbs::git_row_verbs(kind).iter().find(|v| v.key == '\n')?.id;
            git_verb_flow(id, view, git, &rows)
        }
        hit::GitTarget::HistRow(row) => {
            let again = view.focus == Focus::History && git.hist_sel == row;
            view.focus = Focus::History;
            git.hist_sel = row;
            if again {
                Some(Flow::GitOpenCommit)
            } else {
                Some(Flow::Continue)
            }
        }
        hit::GitTarget::RefVerb(key) => {
            view.focus = Focus::Refs;
            let rows = chrome::ref_rows(git, changes, here);
            let kind = chrome::ref_row_kind(&rows, git.refs_sel);
            let id = crate::verbs::git_footer(kind).iter().find(|v| v.key == key)?.id;
            git_verb_flow(id, view, git, &rows)
        }
        hit::GitTarget::HistVerb(key) => {
            view.focus = Focus::History;
            let rows = chrome::ref_rows(git, changes, here);
            let id = crate::verbs::git_footer(crate::verbs::GitRow::Commit)
                .iter()
                .find(|v| v.key == key)?
                .id;
            git_verb_flow(id, view, git, &rows)
        }
        hit::GitTarget::Body => {
            view.focus = Focus::Stage;
            Some(Flow::Continue)
        }
        hit::GitTarget::Nothing => None,
    }
}

/// Where the cursor is in one rail.
fn selection(view: &View, focus: Focus) -> usize {
    match focus {
        Focus::Agents => view.agent_sel,
        Focus::Processes => view.proc_sel,
        Focus::Changes => view.changes_sel,
        Focus::AllAgents => view.all_agents_sel,
        // The GIT page's two cursors live on the page's own state, the way the
        // Docker page's does — they are about a workspace, not about the rails.
        Focus::Refs | Focus::History | Focus::Stage => 0,
    }
}

fn set_selection(view: &mut View, focus: Focus, row: usize) {
    match focus {
        Focus::Agents => view.agent_sel = row,
        Focus::Processes => view.proc_sel = row,
        Focus::Changes => view.changes_sel = row,
        Focus::AllAgents => view.all_agents_sel = row,
        Focus::Refs | Focus::History | Focus::Stage => {}
    }
}

/// What Enter would do on the row the cursor is on — which on the rails is also
/// what a second click on it means.
///
/// Not on BOOTH: there the verb travels between machines, so it is the one row a
/// click may not carry out. Enter still does, because a keystroke aimed at the
/// cursor cannot be a slip of the pointer, and `[open]` is beside it either way.
fn stage_selected(focus: Focus, page: Page, sel: usize) -> Flow {
    match focus {
        // On BOOTH the cursor is in a cross-daemon list, so Enter is a
        // different verb: go to that agent's workspace on its machine. The
        // ALL AGENTS panel keeps `StageSelected`, because its rows are this
        // workspace's already.
        Focus::AllAgents if page == Page::Booth => Flow::OpenFleetAgent(sel),
        Focus::Agents | Focus::Processes | Focus::AllAgents => Flow::StageSelected,
        Focus::Changes => Flow::OpenSelectedDiff,
        // The GIT page answers Enter itself, in `handle_git_key`, before this
        // is reached — the same route the Docker page takes.
        Focus::Refs | Focus::History | Focus::Stage => Flow::Continue,
    }
}

/// Where a paste goes.
///
/// **Bracketed paste was switched on at startup and then dropped on the floor.**
/// [`crate::tui`] sends `EnableBracketedPaste`, so the terminal stops sending a
/// paste as keystrokes and sends the whole run as one `Event::Paste` instead —
/// which nothing here matched, so pasting into an agent did nothing at all. The
/// answer is not to turn the run back into keys: a program that asked for
/// bracketed paste wants the `ESC[200~` markers around it, which is why
/// [`InputEvent::Paste`] exists and the daemon encodes them per pane, against
/// that pane's modes.
///
/// Text goes wherever text is being typed, in the order the keyboard already
/// resolves: a modal first, then the file buffer when it has the keyboard, then
/// the program on the stage. The modals that take no text swallow it for the
/// same reason they swallow keys — they are a question, and a paste is not an
/// answer to it. BOOTH is the one page whose stage is somebody else's session on
/// show rather than one you are typing into, so a paste there says so instead of
/// appearing in an agent on another machine.
fn paste_text(
    text: String,
    view: &mut View,
    files: &mut Files,
    docs: &mut Files,
    stage: Option<&Stage>,
) -> Flow {
    if let Some(overlay) = view.overlay.as_mut() {
        match overlay {
            Overlay::Prompt(p) => {
                for c in as_one_line(&text).chars() {
                    p.insert(c);
                }
            }
            Overlay::Search(f) => {
                for c in as_one_line(&text).chars() {
                    f.insert(c);
                }
                f.searching = true;
                return Flow::Search(f.query.clone());
            }
            Overlay::List(_) | Overlay::Confirm(_) => {}
        }
        return Flow::Continue;
    }
    if view.page.is_tree() {
        if let Some(open) = page_tree(view.page, files, docs).open.as_mut() {
            if open.mode == EditMode::Edit {
                if open.area.insert_str(&text) {
                    open.touch();
                }
                return Flow::Continue;
            }
        }
    }
    // On BOOTH the keyboard is the fleet's until you hand it to the middle
    // column, so a paste with the cursor still in the list is aimed at nothing —
    // and dropping it silently is what "pasting doesn't work" looked like the
    // last time. Once the stage has the focus it is a pane like any other.
    if view.page == Page::Booth && view.focus != Focus::Stage {
        view.flash = Some("click the preview or Tab to it to type there".into());
        return Flow::Continue;
    }
    match stage {
        Some(s) => {
            s.transport.to_server.send(ClientMsg::Input(InputEvent::Paste(text))).ok();
        }
        None => view.flash = Some("nothing on the stage".into()),
    }
    Flow::Continue
}

/// How many cells of chrome are drawn down the left of the text on this page.
///
/// Line numbers over the open file on Files and Docs; the marker column and the
/// two number columns over a diff. Zero everywhere else, and zero on a buffer
/// being edited — the widget draws its own body with no gutter. A selection is
/// clipped to start after it, so a copied function comes back as code rather
/// than as code with a column of numbers welded to the front of every line.
///
/// The diff's answer depends on the patch *and* on the box, since the numbers
/// are the first thing a narrow body gives up — which is why this is asked of
/// the view rather than read off a constant.
fn text_gutter(
    view: &View,
    files: &Files,
    docs: &Files,
    diff: &DiffView,
    git: &chrome::Git,
    cols: u16,
    rows: u16,
) -> u16 {
    match view.page {
        Page::Diff => diff.gutter_w(chrome::stage_rect(cols, rows, view).width),
        Page::Git => {
            let body = chrome::git_columns(chrome::page_geom(cols, rows, view).stage_box).body_box;
            git.body
                .as_ref()
                .map(|d| d.gutter_w(body.width.saturating_sub(2)))
                .unwrap_or(chrome::DIFF_GUTTER_W)
        }
        page if page.is_tree() => {
            let tree = if view.page == Page::Docs { docs } else { files };
            tree.open.as_ref().map(chrome::editor_gutter_w).unwrap_or(0)
        }
        _ => 0,
    }
}

/// A pasted run flattened onto one line, for the places that hold one.
///
/// A prompt is a line editor, so the newlines cannot go in as they are. Dropping
/// them outright would run the last word of one line into the first of the next,
/// which is how a pasted branch name silently becomes a different branch name —
/// so a run of control characters becomes a single space, and a leading or
/// trailing one becomes nothing.
fn as_one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut gap = false;
    for c in text.chars() {
        if c.is_control() {
            gap = !out.is_empty();
        } else {
            if gap {
                out.push(' ');
                gap = false;
            }
            out.push(c);
        }
    }
    out
}

/// Pass a pane-local mouse event to the program on the stage.
///
/// Sent whether or not the program asked for the mouse: the daemon knows which
/// (it is parsing that program's output) and drops it when it did not, which is
/// the only place that can be decided.
fn forward_mouse(stage: Option<&Stage>, x: u16, y: u16, m: &event::MouseEvent) {
    let Some(s) = stage else { return };
    let alt = m.modifiers.contains(event::KeyModifiers::ALT);
    let ev = match m.kind {
        event::MouseEventKind::Down(event::MouseButton::Left) => {
            InputEvent::MouseDown { x, y, alt, button: butai_protocol::MouseButton::Left }
        }
        event::MouseEventKind::Down(event::MouseButton::Right) => {
            InputEvent::MouseDown { x, y, alt, button: butai_protocol::MouseButton::Right }
        }
        event::MouseEventKind::Drag(event::MouseButton::Left) => {
            InputEvent::MouseDrag { x, y, alt }
        }
        event::MouseEventKind::Up(event::MouseButton::Left) => InputEvent::MouseUp { x, y },
        // The wheel is a mouse event like the others, and goes the same way. It
        // used to be the exception — the TUI sent `ScrollPage`, which scrolls
        // the daemon's scrollback and nothing else — so a program that had
        // asked for the mouse never saw a notch, and one drawing on the
        // alternate screen has no scrollback for the fallback to move. Over
        // `claude`, `less` or `vim` the wheel did nothing at all.
        event::MouseEventKind::ScrollUp => InputEvent::ScrollUp { x, y },
        event::MouseEventKind::ScrollDown => InputEvent::ScrollDown { x, y },
        _ => return,
    };
    s.transport.to_server.send(ClientMsg::Input(ev)).ok();
}

/// Whichever GPU monitor is installed: `nvtop` first because it covers both
/// vendors, then each vendor's own tool, then a line saying why there is
/// nothing — an empty pane would read as a broken one.
fn gpu_monitor() -> String {
    "if command -v nvtop >/dev/null 2>&1; then exec nvtop; \
     elif command -v nvidia-smi >/dev/null 2>&1; then exec watch -n1 nvidia-smi; \
     elif command -v rocm-smi >/dev/null 2>&1; then exec watch -n1 rocm-smi; \
     elif command -v radeontop >/dev/null 2>&1; then exec radeontop; \
     else echo 'no GPU monitor (install nvtop / nvidia-smi / rocm-smi)'; sleep 5; fi"
        .to_string()
}

/// Carry out a resolved binding.
///
/// Split from the resolution ([`keys::bind`]) so the sorting is testable
/// without a running client, and so the two halves of what the daemon used to
/// do in one function stay apart: deciding *what* a key means, and doing it.
/// Carry out one view verb.
///
/// The single place a space, a rail or a tab is chosen, whichever layer asked:
/// the Alt keys resolve to these, the prefix table's bindings resolve to these,
/// and so does a `:` command. Before this existed the Alt layer was the only
/// one that could reach them, and the prefix table still named a free-pane
/// model the workbench had already dropped — which is how 23 of its 33 default
/// bindings came to do nothing.
fn run_view(verb: ViewVerb, view: &mut View) -> Flow {
    match verb {
        // Each space toggles back to work, so the key that took you there also
        // brings you home.
        ViewVerb::Space(page) => {
            view.page = if view.page == page { Page::Agents } else { page };
            open_page(view)
        }
        ViewVerb::SpaceNext => {
            view.page = view.page.next();
            open_page(view)
        }
        ViewVerb::SpacePrev => {
            view.page = view.page.prev();
            open_page(view)
        }
        // The fleet list is BOOTH's, so the key that focuses it goes there
        // first — otherwise it is a cursor on a page that draws no such list.
        ViewVerb::Focus(Focus::AllAgents) => {
            view.page = Page::Booth;
            view.focus = Focus::AllAgents;
            Flow::Continue
        }
        // BOOTH draws two of the seven sections — its fleet and its stage — so a
        // verb naming one of the workspace rails has nowhere to put the cursor
        // here and lands on the fleet instead.
        //
        // This is also the way *out* of BOOTH's stage, and it has to be one of
        // these rather than a bare key: once the middle column has the keyboard
        // every unmodified keystroke is the agent's, `esc` and `tab` included,
        // so taking one back for the chrome would break the program you just
        // asked to talk to. `alt-esc` and `alt-w` both arrive here.
        ViewVerb::Focus(section) if view.page == Page::Booth && section != Focus::Stage => {
            view.focus = Focus::AllAgents;
            Flow::Continue
        }
        ViewVerb::Focus(section) => {
            view.focus = section;
            Flow::Continue
        }
        ViewVerb::Tab(n) => Flow::GoTab(TabMove::To(n)),
        ViewVerb::TabNext => Flow::GoTab(TabMove::Next),
        ViewVerb::TabPrev => Flow::GoTab(TabMove::Prev),
        ViewVerb::NewWorkspace => Flow::BrowseHere,
        ViewVerb::CloseWorkspace => Flow::AskCloseWorkspace,
        ViewVerb::Host => Flow::PickHost,
        ViewVerb::Spaces => Flow::PickSpace,
        ViewVerb::Layout => Flow::ToggleLayout,
        // Always on the agents page, because that is where its pane appears —
        // which is also what `Spawned::Stage` asks for, so the key and the `t`
        // under the PROCESSES list make the one gesture.
        ViewVerb::NewTerminal => {
            view.page = Page::Agents;
            Flow::RunProcess { name: "shell".into(), command: String::new(), then: Spawned::Stage }
        }
        // The same pane the SYSTEM gauges put up, on the same page, for the same
        // reason `terminal` sets it: this is where its stage is.
        ViewVerb::Monitor { gpu } => {
            view.page = Page::Agents;
            Flow::RunProcess {
                name: if gpu { "gpu".into() } else { "top".into() },
                command: if gpu { gpu_monitor() } else { "htop".into() },
                then: Spawned::Stage,
            }
        }
        ViewVerb::Search => {
            view.overlay = Some(Overlay::Search(chrome::SearchOverlay::default()));
            Flow::Search(String::new())
        }
        ViewVerb::Branch => Flow::PickBranch,
        ViewVerb::Update => Flow::CheckUpdate,
        // The list is built by the loop, which is where the painted screen is.
        ViewVerb::Links => Flow::PickLinks,
        // The reference is a page of its own — not a modal, which covered the
        // thing you opened it to ask about, and not DOCS, which answered a press
        // on help with a project's file tree. Pressing it again is the way back
        // out, exactly as `[settings]` is: both are entered and left rather than
        // cycled through, so neither can leave you somewhere you never were.
        ViewVerb::Help if view.page == Page::Help => Flow::CloseHelp,
        ViewVerb::Help => {
            view.overlay = None;
            Flow::OpenHelp
        }
    }
}

fn run_bound(bound: keys::Bound, view: &mut View) -> Flow {
    use keys::{Ask, Bound, Local};
    match bound {
        Bound::Detach => Flow::Detach,
        Bound::Prompt => {
            view.overlay = Some(Overlay::Prompt(chrome::PromptOverlay {
                title: "COMMAND".into(),
                text: String::new(),
                cursor: 0,
                kind: chrome::PromptKind::Command,
                subtitle: Some(
                    "space · workspace · focus · agent · process · terminal · detach".into(),
                ),
            }));
            Flow::Continue
        }
        Bound::Local(Local::Zen) => {
            view.zen = !view.zen;
            Flow::Continue
        }
        Bound::Local(Local::GitMenu) => Flow::GitMenu,
        Bound::Local(Local::PickAgent) => Flow::PickAgent,
        Bound::Local(Local::OpenFile(path)) => {
            view.page = Page::Files;
            if path.is_empty() {
                Flow::ListDir(String::new())
            } else {
                Flow::OpenFile(path)
            }
        }
        Bound::Local(Local::Theme(name)) => {
            // Palettes are the client's since phase 3 — the daemon does not draw
            // the chrome any more, so it has no say in its colours.
            view.flash = Some(match name {
                Some(name) => format!("themes are client-side now; set `theme = \"{name}\"`"),
                None => "themes are client-side now; see [ui] theme in config.toml".into(),
            });
            Flow::Continue
        }
        // The pane is the daemon's, but the connection streaming it is already
        // open — so this is a message on that, not a route of its own.
        Bound::Local(Local::Scroll(pages)) => Flow::Scroll(pages),
        Bound::Local(Local::PinAgent(name)) => Flow::PinAgent(name),
        Bound::Local(Local::PasteImage) => Flow::PasteImage,
        Bound::Local(Local::View(verb)) => run_view(verb, view),
        Bound::Ask(Ask::SpawnAgent(name)) => Flow::SpawnAgent(name),
        Bound::Ask(Ask::NewProcess { name, command }) => {
            Flow::RunProcess { name, command, then: Spawned::Stage }
        }
        Bound::Ask(Ask::ClosePane) => Flow::CloseStagePane,
        Bound::Ask(Ask::KillServer { clear }) => Flow::Control(if clear {
            butai_protocol::Command::KillServerClear
        } else {
            butai_protocol::Command::KillServer
        }),
        Bound::Ask(Ask::ReloadConfig) => Flow::Control(butai_protocol::Command::ReloadConfig),
        other => {
            view.flash = other.why();
            Flow::Continue
        }
    }
}

/// Keys while LAYOUT mode is on.
///
/// The rails are workbench-wide, so this moves them for every tab at once —
/// which is why the HUD says so. Nothing here reaches a pane: the whole point
/// of the mode is that the arrows mean something else for as long as it is on,
/// and the footer says that too.
fn handle_layout_key(k: event::KeyEvent, view: &mut View, cols: u16, rows: u16) -> Flow {
    let section = chrome::Section::of(view.focus);
    let left = !section.on_right_rail();
    match k.code {
        event::KeyCode::Left | event::KeyCode::Char('h') => {
            chrome::resize_rail(&mut view.geom, cols, left, -2)
        }
        event::KeyCode::Right | event::KeyCode::Char('l') => {
            chrome::resize_rail(&mut view.geom, cols, left, 2)
        }
        event::KeyCode::Up | event::KeyCode::Char('k') => {
            let want = chrome::system_h_wanted(&view.gauges);
            chrome::resize_section(&mut view.geom, rows, section, 2, want)
        }
        event::KeyCode::Down | event::KeyCode::Char('j') => {
            let want = chrome::system_h_wanted(&view.gauges);
            chrome::resize_section(&mut view.geom, rows, section, -2, want)
        }
        event::KeyCode::Esc | event::KeyCode::Enter => return Flow::ToggleLayout,
        _ => {}
    }
    Flow::Continue
}

/// The prefix key and the keystroke after it. `None` means "not mine".
///
/// Two states, and the second consumes whatever follows: that is what makes the
/// table configurable without every binding having to avoid colliding with the
/// rest of the interface. Pressing the prefix twice sends it through to the
/// pane, which is how you type a literal `C-b` in a program that wants one.
fn handle_prefix(
    k: event::KeyEvent,
    view: &mut View,
    keymap: &Keymap,
    stage: Option<&Stage>,
) -> Option<Flow> {
    let key = KeyEvent::from_crossterm(&k)?;
    if !std::mem::take(&mut view.prefix_armed) {
        if key != keymap.prefix {
            return None;
        }
        view.prefix_armed = true;
        return Some(Flow::Continue);
    }
    if key == keymap.prefix {
        if let Some(s) = stage {
            s.transport.to_server.send(ClientMsg::Input(InputEvent::Key(key))).ok();
        }
        return Some(Flow::Continue);
    }
    Some(match keymap.resolve(&key) {
        Some(action) => run_bound(keys::bind(action.clone()), view),
        // Silent in the daemon, which logged it where nobody looks. An unbound
        // key after a prefix is a typo, and saying so is how you find out the
        // binding you thought you had is not there.
        None => {
            view.flash = Some(format!("{} is not bound", keys::key_label(&key)));
            Flow::Continue
        }
    })
}

/// Route one terminal event.
///
/// Navigation resolves here and never reaches the daemon — that is the whole
/// point of the client owning its selection, and it is why an arrow key costs
/// nothing over ssh. Only keys aimed at the staged pane are forwarded.
#[allow(clippy::too_many_arguments)]
fn handle_input(
    ev: event::Event,
    view: &mut View,
    daemons: &[Daemon],
    hosts: &[Option<String>],
    stage: Option<&Stage>,
    files: &mut Files,
    docs: &mut Files,
    diff: &mut DiffView,
    docker: &mut Docker,
    git: &mut chrome::Git,
    settings: &mut chrome::Settings,
    help: &mut chrome::Help,
    usage: &mut chrome::usage::Usage,
    keymap: &Keymap,
    drag: &mut Drag,
    option_as_alt: bool,
    pane_wants_mouse: bool,
    cols: &mut u16,
    rows: &mut u16,
) -> Flow {
    let counts = rail_counts(daemons, hosts, view);
    let tab_count = tab_index(daemons, hosts).len();
    match ev {
        event::Event::Resize(c, r) => {
            *cols = c;
            *rows = r;
        }
        // A whole pasted run in one event, because bracketed paste is on. It is
        // text, not keys, and it goes where text goes.
        event::Event::Paste(text) => return paste_text(text, view, files, docs, stage),
        // Shift+mouse is left to the terminal for its own selection, which is
        // how you copy out of butai with the tools you already have.
        event::Event::Mouse(m) if m.modifiers.contains(event::KeyModifiers::SHIFT) => {}
        event::Event::Mouse(m) => {
            // A modal has the pointer for the same reason it has the keyboard:
            // it is a question, and clicking past it answers something else. A
            // click *on* a row picks it, exactly as Enter on that row would;
            // anywhere outside dismisses.
            if view.overlay.is_some() {
                drag.clear();
                return overlay_mouse(&m, view, *cols, *rows);
            }
            let refs = tabs_of(daemons, hosts);
            let ws = active_workspace(daemons, hosts, view);
            // BOOTH's fleet spans daemons, so the hit test needs the same list
            // the drawing was given or a click lands on the wrong agent.
            let fleet = all_agent_rows(daemons, hosts);
            let fleet_machines = machine_rows(daemons, hosts, &fleet);
            // How wide the open file's line numbers are, so a selection can
            // start after them. Only the buffer knows, and only here is it in
            // scope — hence a value passed down rather than a lookup.
            let gutter = text_gutter(view, files, docs, diff, git, *cols, *rows);
            match m.kind {
                // The right button only ever opens the context menu, and only
                // over something that has one — never over blank chrome.
                event::MouseEventKind::Down(event::MouseButton::Right) => {
                    // BOOTH's fleet first, for the reason the left button
                    // resolves it first: its rows are not in the geometry, and
                    // `hit::at` answers `Nothing` for the whole column.
                    //
                    // The menu carries the row it was opened on and the cursor
                    // does not move, which is what the rails do — right-clicking
                    // a row to end it should not also re-point the preview at a
                    // machine you were not watching.
                    if let Some(fleet_hit) =
                        hit::on_fleet(*cols, *rows, view, &fleet, &fleet_machines, m.column, m.row)
                    {
                        let (hit::FleetHit::Row(sel) | hit::FleetHit::Open(sel)) = fleet_hit;
                        view.overlay = fleet_menu(&fleet, sel);
                        return Flow::Continue;
                    }
                    let target =
                        hit::at(*cols, *rows, view, &refs, daemons.len(), ws, m.column, m.row);
                    let here = active_daemon(daemons, hosts, view);
                    if let Some(menu) = menu_for(target, ws, daemons, hosts, here) {
                        view.overlay = Some(menu);
                    }
                }
                event::MouseEventKind::Down(event::MouseButton::Left) => {
                    // BOOTH's fleet first: it is the only region whose rows come
                    // from a cross-daemon list, so it is resolved against that
                    // list rather than against the geometry alone.
                    if let Some(fleet_hit) =
                        hit::on_fleet(*cols, *rows, view, &fleet, &fleet_machines, m.column, m.row)
                    {
                        // Armed, not dropped: every other list in the workbench
                        // can be dragged over and copied out of, and a column of
                        // agent titles and the machines they are on is one of
                        // the more useful ones to be able to quote.
                        drag.press(view, *cols, *rows, m.column, m.row, gutter);
                        return fleet_click(fleet_hit, view);
                    }
                    // SETTINGS resolves its own clicks, because doing it in
                    // `hit` would mean handing that module the page's cursor
                    // and its expanded list — state it deliberately does not
                    // take for any other page.
                    if view.page == Page::Settings {
                        drag.clear();
                        let geom = chrome::page_geom(*cols, *rows, view);
                        // The two bars are the workbench's on every page, this
                        // one included.
                        if m.row == geom.tabbar.y || m.row == geom.footer.y {
                            let target = hit::at(
                                *cols,
                                *rows,
                                view,
                                &refs,
                                daemons.len(),
                                ws,
                                m.column,
                                m.row,
                            );
                            return page_bar_click(target, view, settings.ret, tab_count, ws);
                        }
                        if let Some(flow) =
                            settings_click(view, settings, *cols, *rows, m.column, m.row)
                        {
                            return flow;
                        }
                        return Flow::Continue;
                    }
                    // HELP, on the same terms: its contents column is a list
                    // only this page knows the rows of, and the two bars stay
                    // the workbench's — which is what makes clicking a tab, a
                    // space or `[help]` again the way out of it.
                    if view.page == Page::Help {
                        drag.clear();
                        let geom = chrome::page_geom(*cols, *rows, view);
                        if m.row == geom.tabbar.y || m.row == geom.footer.y {
                            let target = hit::at(
                                *cols,
                                *rows,
                                view,
                                &refs,
                                daemons.len(),
                                ws,
                                m.column,
                                m.row,
                            );
                            return page_bar_click(target, view, help.ret, tab_count, ws);
                        }
                        if let Some(flow) = help_click(view, help, *cols, *rows, m.column, m.row) {
                            return flow;
                        }
                        return Flow::Continue;
                    }
                    // A full-screen page owns the middle of the screen, so it
                    // gets the click before the rails' geometry is consulted.
                    if view.page == Page::Git {
                        // But it owns the *middle*, and only that. The tab bar
                        // and the footer are the workbench's on every page, this
                        // one included — `page_geom` widens the band and leaves
                        // both exactly where they were — and they have to be
                        // resolved before the band below, whose "the page owns
                        // everything else" answer would otherwise swallow them.
                        //
                        // It did: every control that was not one of the three
                        // git columns went dead to the mouse, the spaces
                        // control included, and that control is how you leave.
                        // Reported as the whole client freezing on arrival at
                        // GIT, which is what a page you cannot click your way
                        // out of looks like — the keyboard was answering the
                        // whole time.
                        //
                        // There used to be a third region here: the view rail's
                        // columns down the left edge. The spaces live on the tab
                        // bar now, so the row test covers them.
                        //
                        // Asking `hit::at` is safe here in a way it is not
                        // lower down: it resolves both rows before it ever
                        // reaches a rail rectangle, so neither can come back as
                        // the `Rail(Agents, n)` the note below guards against.
                        // SETTINGS does the same for its two bars, and for the
                        // same reason.
                        let geom = chrome::page_geom(*cols, *rows, view);
                        if m.row == geom.tabbar.y || m.row == geom.footer.y {
                            drag.clear();
                            let target = hit::at(
                                *cols,
                                *rows,
                                view,
                                &refs,
                                daemons.len(),
                                ws,
                                m.column,
                                m.row,
                            );
                            return run_click(target, view, tab_count, ws);
                        }
                        let owned = active_workspace(daemons, hosts, view).cloned();
                        let ch = owned.as_ref().and_then(|w| w.changes.clone());
                        let here = owned.as_ref().map(|w| w.id);
                        let target = hit::on_git_page(
                            *cols,
                            *rows,
                            view,
                            git,
                            ch.as_ref(),
                            here,
                            m.column,
                            m.row,
                        );
                        drag.press(view, *cols, *rows, m.column, m.row, gutter);
                        if let Some(flow) = git_click(target, view, git, ch.as_ref(), here) {
                            return flow;
                        }
                        // The page owns its whole band, so a press that hit none
                        // of its three columns is nothing — not a fall through to
                        // `hit::at`, whose rail rectangles are still populated on
                        // a full-width page and would answer `Rail(Agents, n)`,
                        // moving focus into a list that is not drawn. BOOTH guards
                        // the identical trap; this is the same one.
                        return Flow::Continue;
                    }
                    let tree = page_tree(view.page, files, docs);
                    let sel = if view.page.is_tree() { tree.sel } else { docker.sel };
                    match hit::on_page(*cols, *rows, view, sel, m.column, m.row) {
                        hit::PageTarget::Row(row) => {
                            drag.press(view, *cols, *rows, m.column, m.row, gutter);
                            let sys = &daemons[active_daemon(daemons, hosts, view)].state.system;
                            return page_click(view, tree, docker, sys, ws, row);
                        }
                        hit::PageTarget::Body => {
                            drag.press(view, *cols, *rows, m.column, m.row, gutter);
                            view.focus = Focus::Stage;
                            return Flow::Continue;
                        }
                        // `[find]` opens the search the `/` key opens.
                        hit::PageTarget::Find => {
                            drag.clear();
                            view.overlay = Some(Overlay::Search(chrome::SearchOverlay::default()));
                            return Flow::Search(String::new());
                        }
                        hit::PageTarget::Nothing => {}
                    }
                    let target =
                        hit::at(*cols, *rows, view, &refs, daemons.len(), ws, m.column, m.row);
                    // A press arms a possible drag and drops any previous one.
                    // Before the click is carried out, since acting on it can
                    // change the page under the pointer.
                    drag.press(view, *cols, *rows, m.column, m.row, gutter);
                    // A click into the pane goes to the pane as well as moving
                    // focus: a program that asked for the mouse gets it, and
                    // one that did not is unaffected.
                    if let hit::Target::Stage(px, py) = target {
                        forward_mouse(stage, px, py, &m);
                    }
                    return run_click(target, view, tab_count, ws);
                }
                event::MouseEventKind::Drag(event::MouseButton::Left) => {
                    // A pane that grabbed the mouse gets the drag, unless Alt
                    // is held — which is how you select over `vim` or `less`.
                    let target =
                        hit::at(*cols, *rows, view, &refs, daemons.len(), ws, m.column, m.row);
                    let alt = m.modifiers.contains(event::KeyModifiers::ALT);
                    match target {
                        hit::Target::Stage(px, py) if !alt && pane_wants_mouse => {
                            forward_mouse(stage, px, py, &m)
                        }
                        _ => drag.to(m.column, m.row),
                    }
                }
                event::MouseEventKind::Up(event::MouseButton::Left) => {
                    if let hit::Target::Stage(px, py) =
                        hit::at(*cols, *rows, view, &refs, daemons.len(), ws, m.column, m.row)
                    {
                        forward_mouse(stage, px, py, &m);
                    }
                    return Flow::CopySelection;
                }
                event::MouseEventKind::ScrollUp | event::MouseEventKind::ScrollDown => {
                    let down = matches!(m.kind, event::MouseEventKind::ScrollDown);
                    let delta = if down { 1 } else { -1 };
                    // A full-screen page owns the wheel over its own columns,
                    // before the rails are consulted — same precedence a click
                    // gets, for the same reason.
                    let git_refs_len = {
                        let ws = active_workspace(daemons, hosts, view);
                        let ch = ws.and_then(|w| w.changes.as_ref());
                        chrome::ref_rows(git, ch, ws.map(|w| w.id)).len()
                    };
                    if let Some(flow) = page_wheel(
                        view,
                        files,
                        docs,
                        diff,
                        docker,
                        git,
                        help,
                        git_refs_len,
                        daemons,
                        hosts,
                        daemons.len(),
                        delta,
                        *cols,
                        *rows,
                        m.column,
                        m.row,
                    ) {
                        return flow;
                    }
                    match hit::at(*cols, *rows, view, &refs, daemons.len(), None, m.column, m.row) {
                        // Over a rail the wheel moves that rail's cursor,
                        // without taking focus — you are looking, not choosing.
                        hit::Target::Rail(focus, _) => {
                            let was = view.focus;
                            view.focus = focus;
                            move_sel(view, rail_counts(daemons, hosts, view), delta);
                            view.focus = was;
                        }
                        // Over the pane the wheel is the pane's, and goes to it
                        // as the press and the drag do — pane-local, over the
                        // connection already streaming it, for the daemon to
                        // decide between the program and the scrollback. It is
                        // the only end that can decide: whether the program
                        // asked for the mouse is something only the side
                        // parsing its output knows.
                        hit::Target::Stage(px, py) => forward_mouse(stage, px, py, &m),
                        // Everywhere else — a box border, a bar, the gap
                        // between two rails — still reaches the staged pane's
                        // scrollback, which is where the wheel has always gone
                        // when it landed on no control.
                        _ => return Flow::Scroll(delta as i16),
                    }
                }
                _ => {}
            }
        }
        event::Event::Key(k) => {
            // Before anything reads the key: on a Mac, Option composed it into
            // a character and no Alt was ever reported. Undo that here, once,
            // so every layer below sees the key the user pressed rather than
            // each having to know about it.
            let k = if option_as_alt { mac_option(k) } else { k };
            let alt = k.modifiers.contains(event::KeyModifiers::ALT);
            // A modal is a question the interface is asking; nothing else gets
            // the keyboard until it is answered or dismissed.
            if view.overlay.is_some() {
                return handle_overlay_key(k, view);
            }
            // The prefix, before anything else — including the stage, which
            // otherwise swallows every key. That is the point of a prefix: it
            // is how you reach the multiplexer from inside a program that is
            // using the whole keyboard.
            if let Some(flow) = handle_prefix(k, view, keymap, stage) {
                return flow;
            }
            // Layout mode: the arrows resize the focused rail (←/→) or the
            // section within it (↑/↓) rather than reaching a pane. Alt-focus
            // keys still work, so you can retarget without leaving.
            if view.layout.is_some() && !alt {
                return handle_layout_key(k, view, *cols, *rows);
            }
            if view.page.is_tree() && !alt {
                let tree = page_tree(view.page, files, docs);
                if let Some(flow) = handle_files_key(k, view, tree) {
                    return flow;
                }
            }
            // Each rail's own verbs, when the cursor is in it. One `if` per
            // rail rather than one dispatcher, because each reads a different
            // shape of state — but all three go through their own verb table,
            // so the footer under a list is the list of keys that work there.
            if view.page == Page::Agents && !alt {
                let ws = active_workspace(daemons, hosts, view);
                if view.focus == Focus::Changes {
                    if let Some(flow) = handle_changes_key(k, view, ws) {
                        return flow;
                    }
                }
                let (agents, procs, ..) = counts;
                let rows = if view.focus == Focus::Agents { agents } else { procs };
                let pinned = view.pinned_agent.is_some();
                if let Some(flow) = handle_rail_key(k, view.focus, rows, pinned) {
                    return flow;
                }
            }
            // BOOTH's fleet, which has one lettered verb. Before the stage
            // forward below, and gated on the fleet having the keyboard: once
            // `tab` has handed it to the middle column every key is the agent's,
            // `x` included.
            if view.page == Page::Booth && !alt {
                let (.., fleet) = counts;
                if let Some(flow) = handle_fleet_key(k, view, fleet) {
                    return flow;
                }
            }
            // Before the stage forward below: this page has no pane, and a
            // bare `j` on it would otherwise be typed into whichever shell was
            // running behind it. The GIT page learned the same lesson.
            if view.page == Page::Settings && !alt {
                if let Some(flow) = handle_settings_key(k, view, settings, *cols, *rows) {
                    return flow;
                }
            }
            // Before the stage forward, for the reason SETTINGS is: these
            // pages have no pane, so a bare `j` would otherwise be typed into
            // whichever shell was running behind them.
            if view.page == Page::Help && !alt {
                if let Some(flow) = handle_help_key(k, view, help, *cols, *rows) {
                    return flow;
                }
            }
            if view.page == Page::Usage && !alt {
                if let Some(flow) = handle_usage_key(k, usage) {
                    return flow;
                }
            }
            if view.page == Page::Docker && !alt {
                let ws = active_workspace(daemons, hosts, view);
                let sys = &daemons[active_daemon(daemons, hosts, view)].state.system;
                if let Some(flow) = handle_docker_key(k, view, docker, sys, ws) {
                    return flow;
                }
            }
            if view.page == Page::Git && !alt {
                let ws = active_workspace(daemons, hosts, view).cloned();
                let band = chrome::git_columns(chrome::page_geom(*cols, *rows, view).stage_box);
                let list_h = if view.focus == Focus::Refs {
                    band.refs_rows.height
                } else {
                    band.hist_rows.height
                };
                if let Some(d) = git.body.as_mut() {
                    d.set_view_rows(band.body_box.height.saturating_sub(3));
                }
                if let Some(flow) = handle_git_key(k, view, git, ws.as_ref(), list_h) {
                    return flow;
                }
            }
            if view.page == Page::Diff && !alt {
                // The body's height decides what a page is and how far the
                // cursor has to scroll, and only here are the terminal's
                // dimensions known.
                diff.set_view_rows(chrome::diff_body_rows(*cols, *rows, view));
                if let Some(flow) = handle_diff_key(k, view, diff) {
                    return flow;
                }
            }
            // The Alt layer belongs to the chrome, wherever the cursor is.
            //
            // The daemon said exactly that and dispatched it before a pane saw
            // anything. This client had carved out only Alt-Esc and Alt-d,
            // which went unnoticed while it opened focused on a rail — now that
            // it opens on the stage, every other Alt binding was being typed
            // into the shell instead. An Alt key the chrome does not bind still
            // falls through, so Alt-b and Alt-f keep reaching readline.
            if alt {
                if let Some(flow) = alt_binding(k.code, view) {
                    return flow;
                }
            }
            // Only onto a pane the page actually shows. `Focus::Stage` means
            // "the stage has the keyboard" on WORK and "the body column has it"
            // everywhere else, and the two are not interchangeable: on the GIT
            // page this forwarded every key the page did not consume into a
            // shell nobody could see, so one Tab left the page dead to the
            // keyboard while it typed `r`, `g` and Enter at a prompt. Falling
            // through instead reaches the global bindings below, which is what
            // `handle_git_key` returning `None` was always meant to mean.
            if view.focus == Focus::Stage && view.page.draws_stage() {
                if let (Some(s), Some(key)) = (stage, KeyEvent::from_crossterm(&k)) {
                    s.transport.to_server.send(ClientMsg::Input(InputEvent::Key(key))).ok();
                }
                return Flow::Continue;
            }
            // The arms below read a bare letter, and a Ctrl-modified one is not
            // that. Without this, `C-b` on a client whose prefix is `C-a` opened
            // the branch picker — the tuple matched on the code alone and
            // dropped the modifier on the floor.
            if k.modifiers.contains(event::KeyModifiers::CONTROL) {
                return Flow::Continue;
            }
            match (k.code, alt) {
                (event::KeyCode::Char('q'), false) => return Flow::Detach,
                (event::KeyCode::Char('?'), false) => return run_view(ViewVerb::Help, view),
                // `a` spawns what the rail's `+` advertises — the pinned agent
                // when there is one. `A` is the deliberate "let me choose",
                // which is also what `:agent` and `C-b a` do.
                (event::KeyCode::Char('a'), false) => return Flow::NewAgent,
                (event::KeyCode::Char('A'), false) => return Flow::PickAgent,
                (event::KeyCode::Char('b'), false) => return Flow::PickBranch,
                (event::KeyCode::Char('/'), false) => {
                    view.overlay = Some(Overlay::Search(chrome::SearchOverlay::default()));
                    return Flow::Search(String::new());
                }
                // `f` beside `/`, and bare for the same reason it is: the two
                // are what you reach for while reading, and a rail with the
                // cursor is where reading happens.
                (event::KeyCode::Char('f'), false) => return Flow::PickLinks,
                (event::KeyCode::Char('g'), false) => return Flow::GitMenu,
                // The menu the right button opens, from the keyboard.
                //
                // It was the pointer's alone, and it is the only place "close
                // others" and "close all agents" live. A mouseless client could
                // not reach either — not by another key, not from the `g` menu,
                // not from `:`. The same builder answers both, so the two
                // cannot come to offer different rows.
                //
                // On a rail it is that row's menu; anywhere else it is the
                // workspace's own, which is what the tab chip's menu is and how
                // a remote tab reaches `disconnect` — `alt-h` reaches the same
                // act by naming the machine instead.
                (event::KeyCode::Char('m'), false) => {
                    // On BOOTH the cursor is in a list of every machine's
                    // agents, so the row it is on is the menu — the workspace's
                    // own would be the tab bar's, which is not what you are
                    // looking at.
                    if view.page == Page::Booth && view.focus == Focus::AllAgents {
                        let fleet = all_agent_rows(daemons, hosts);
                        view.overlay = fleet_menu(&fleet, view.all_agents_sel);
                        return Flow::Continue;
                    }
                    let ws = active_workspace(daemons, hosts, view);
                    let here = active_daemon(daemons, hosts, view);
                    if let Some(menu) = menu_for(menu_target(view), ws, daemons, hosts, here) {
                        view.overlay = Some(menu);
                    }
                }
                // Start where you already are: opening a sibling project is far
                // more common than opening one from the filesystem root.
                (event::KeyCode::Char('n'), false) => {
                    let here = active_workspace(daemons, hosts, view)
                        .map(|w| w.cwd.clone())
                        .unwrap_or_default();
                    return Flow::Browse(here);
                }
                // Shifted because it kills everything running in the workspace
                // and the unshifted key is a file verb on the rail. `alt-x` is
                // the daemon's spelling of the same thing.
                (event::KeyCode::Char('X'), false) => {
                    if let Some(ws) = active_workspace(daemons, hosts, view) {
                        view.overlay = Some(close_workspace_confirm(ws));
                    }
                }
                (event::KeyCode::Tab, false) => {
                    // The GIT page answers Tab itself, in `handle_git_key`;
                    // this is every other page's cycle.
                    view.focus = match view.focus {
                        // Those two exist only on GIT — reaching one from here
                        // would be a cursor in a list this page does not draw.
                        Focus::Refs | Focus::History => Focus::Stage,
                        Focus::Agents => Focus::Processes,
                        Focus::Processes => Focus::Changes,
                        // The fleet list is BOOTH's own column, not a stop on
                        // the workbench's cycle.
                        Focus::Changes | Focus::AllAgents => Focus::Stage,
                        Focus::Stage => Focus::Agents,
                    }
                }
                // Enter on the CHANGES rail opens what the cursor names, the
                // way it always has; anywhere else it puts the cursor on the
                // stage so keys go to the pane.
                (event::KeyCode::Enter, false) if view.focus == Focus::Changes => {
                    let ws = active_workspace(daemons, hosts, view);
                    if let Some(kind) = diff_under_cursor(ws, view.changes_sel) {
                        return Flow::OpenDiff { kind, keep_cursor: false };
                    }
                }
                // Enter on a rail puts *that row* on the stage, the way it
                // always has; from anywhere else it just moves the cursor onto
                // the stage so keys reach the pane.
                //
                // Through the same resolver a second click uses, so the one row
                // where the two differ — BOOTH's fleet, which travels rather than
                // stages — cannot answer one of them and forget the other. It
                // did: Enter here staged a pane belonging to a workspace the tab
                // bar said you were not in, and on a second machine it did not
                // resolve at all.
                (event::KeyCode::Enter, false)
                    if matches!(
                        view.focus,
                        Focus::Agents | Focus::Processes | Focus::AllAgents
                    ) =>
                {
                    return stage_selected(view.focus, view.page, selection(view, view.focus))
                }
                (event::KeyCode::Enter, false) => view.focus = Focus::Stage,
                (event::KeyCode::Down | event::KeyCode::Char('j'), false) => {
                    move_sel(view, counts, 1)
                }
                (event::KeyCode::Up | event::KeyCode::Char('k'), false) => {
                    move_sel(view, counts, -1)
                }
                _ => {}
            }
        }
        _ => {}
    }
    Flow::Continue
}

/// The Alt layer: what one Alt-modified key means to the chrome.
///
/// `None` means the chrome does not bind it, and the key belongs to whatever
/// has focus — which is how `alt-b` and `alt-f` still reach readline in a
/// focused shell. Everything here works from anywhere, a pane with the keyboard
/// included, because that is what an Alt binding is *for*: the daemon's
/// dispatcher said "the Alt layer always belongs to the chrome" and ran its
/// equivalent of this before any pane saw the key.
/// Read a macOS Option-composed character back as the Alt key that made it.
///
/// Only a bare character qualifies. A terminal that already reports Alt needs
/// no help, and one reporting Ctrl is saying something else entirely — so an
/// event carrying either is passed through untouched, which also means turning
/// this on can never break a terminal that was working.
///
/// Shift is cleared along with it: `¯` is Option-Shift-`,` and the key it
/// stands for is `<`, whose own shift is already in the character. That is the
/// same rule [`crate::keymap::normalize`] applies to every other capital.
fn mac_option(k: event::KeyEvent) -> event::KeyEvent {
    use event::KeyModifiers as M;
    if k.modifiers.intersects(M::ALT | M::CONTROL) {
        return k;
    }
    let event::KeyCode::Char(c) = k.code else { return k };
    match keys::option_char(c) {
        Some(plain) => event::KeyEvent::new(event::KeyCode::Char(plain), M::ALT),
        None => k,
    }
}

/// Most of the layer is a view verb, and those go through [`run_view`] — the
/// same function the prefix table's bindings and the `:` prompt reach, so
/// `alt-o` and `C-b o` cannot come to mean different things.
fn alt_binding(code: event::KeyCode, view: &mut View) -> Option<Flow> {
    use event::KeyCode as K;
    if let Some(verb) = alt_verb(code) {
        return Some(run_view(verb, view));
    }
    match code {
        K::Char('d') => Some(Flow::Detach),
        K::Char('z') => {
            view.zen = !view.zen;
            Some(Flow::Continue)
        }
        // Alt-e was the daemon's "put the editor up", which in this client is
        // the Files page: the editor is not a pane any more, it is what that
        // page's right-hand column is. Unlike `alt-o` it does not toggle back —
        // it names the page rather than the space.
        K::Char('e') => {
            view.page = Page::Files;
            Some(open_page(view))
        }
        // Alt-0: BOOTH, beside the numbered workspaces because that is where its
        // chip is. The spaces menu does not carry BOOTH — it is a peer of the
        // workspaces, not a view of one — so this and the chip are how you reach
        // it, and `alt-,` / `alt-.` walk the views without passing through it.
        K::Char('0') => {
            view.page = Page::Booth;
            reset_sel(view);
            // Through `open_page`, like every other route onto a page, so the
            // keyboard lands on the fleet. It did not: this arm set the page and
            // stopped, while the chip and `alt-,`/`alt-.` went the long way and
            // focused the list — so BOOTH's *documented* key was the one route
            // that arrived with the middle column still holding the keyboard,
            // and `j` walked nothing while typing into somebody's agent.
            Some(open_page(view))
        }
        // Alt-s: this client's own configuration. Not a space like the toggles
        // in [`alt_verb`], because it is not one of them: it is a peer of the
        // workspace chips, so it is entered and left rather than cycled
        // through, and the page it remembers on the way in is where `esc` puts
        // you back. Swallowed rather than declined when it is already showing,
        // so a second press cannot fall through to a pane behind it.
        K::Char('s') => {
            Some(if view.page == Page::Settings { Flow::Continue } else { Flow::OpenSettings })
        }
        // Alt-v: the image on *your* clipboard, written beside the workspace
        // and its path pasted in. Works over ssh, because the read happens on
        // this machine.
        K::Char('v') => Some(Flow::PasteImage),
        // Alt-Enter always asks which agent, where `a` spawns the pin without
        // asking. After the refactor the picker had no key at all unless you
        // already knew about `A`.
        K::Enter => Some(Flow::PickAgent),
        // Not ours: let the pane have it.
        _ => None,
    }
}

/// The Alt keys that are a view verb, and which one.
///
/// Split from [`alt_binding`] so the mapping is a table rather than a body: the
/// point of it is that every one of these is *also* reachable by name, from
/// `[keys]` and from `:`.
fn alt_verb(code: event::KeyCode) -> Option<ViewVerb> {
    use event::KeyCode as K;
    Some(match code {
        K::Esc => ViewVerb::Focus(Focus::Agents),
        K::Char('x') => ViewVerb::CloseWorkspace,
        // The spaces. Alt-c is containers: `alt-d` is detach, so that page
        // cannot have the letter its own name starts with. Alt-m is markdown.
        K::Char('o') => ViewVerb::Space(Page::Files),
        K::Char('c') => ViewVerb::Space(Page::Docker),
        K::Char('m') => ViewVerb::Space(Page::Docs),
        // Alt-r: the repository. `alt-g` already puts the cursor on the CHANGES
        // rail — which is the *other* git surface, so taking its letter for this
        // page would have swapped the two things most easily confused.
        K::Char('r') => ViewVerb::Space(Page::Git),
        // Alt-u: usage. The one free letter that is also the word — `alt-a`
        // is the AGENTS rail and `alt-l` is layout, so neither half of
        // "account limits" was available.
        K::Char('u') => ViewVerb::Space(Page::Usage),
        // Alt-,/. cycle the spaces in the order the menu lists them — the
        // pairing the tab bar advertises by naming the one you are on.
        K::Char(',') => ViewVerb::SpacePrev,
        K::Char('.') => ViewVerb::SpaceNext,
        // Alt-space: all of them at once, which is what the button on the bar
        // opens. Space rather than a letter because every letter that names this
        // control is taken and none of the free ones names it at all: `v` is
        // paste-image, `s` is settings, `m` is markdown. "The spaces" is the one
        // key left that still says what it does.
        K::Char(' ') => ViewVerb::Spaces,
        // Shifted, the same pair walks the whole tab bar, which spans every
        // connected daemon — the machine a project lives on is a badge, not a
        // separate place to navigate to.
        K::Char('<') => ViewVerb::TabPrev,
        K::Char('>') => ViewVerb::TabNext,
        // A tab by number, as the daemon's bar always had.
        K::Char(c @ '1'..='9') => ViewVerb::Tab(c as usize - '0' as usize),
        // The section keys: one Alt letter per rail, so you can get from a
        // focused pane to the list you want without walking Tab round the whole
        // cycle.
        K::Char('a') => ViewVerb::Focus(Focus::Agents),
        K::Char('p') => ViewVerb::Focus(Focus::Processes),
        K::Char('g') => ViewVerb::Focus(Focus::Changes),
        K::Char('w') => ViewVerb::Focus(Focus::AllAgents),
        // Alt-h: another machine, whose projects join this tab bar. The client
        // dials it directly — there is no daemon in the middle relaying a
        // second daemon's screen any more.
        K::Char('h') => ViewVerb::Host,
        // Alt-l: resize the rails. Also the `[layout]` button, which is where
        // the mode is discoverable from.
        K::Char('l') => ViewVerb::Layout,
        // Alt-t: a new shell, always on the work page, because that is where
        // its pane will appear.
        K::Char('t') => ViewVerb::NewTerminal,
        // Alt-n and Alt-/ are the daemon's spellings of `n` and `/`, and unlike
        // the bare letters they work from a focused pane.
        K::Char('n') => ViewVerb::NewWorkspace,
        K::Char('/') => ViewVerb::Search,
        _ => return None,
    })
}

/// How many rows each rail has, so a cursor cannot walk off the end.
fn rail_counts(daemons: &[Daemon], hosts: &[Option<String>], view: &View) -> Counts {
    let all = all_agent_rows(daemons, hosts).len();
    let Some(ws) = active_workspace(daemons, hosts, view) else { return (0, 0, 0, all) };
    // Counted from the rows the rail actually draws, headings included: the
    // cursor and Enter index the same list, so anything that can be selected
    // has to be counted here or the two disagree at the bottom of the rail.
    let changes = ws.changes.as_ref().map(|c| chrome::change_rows(c).len()).unwrap_or(0);
    (ws.agents.len(), ws.processes.len(), changes, all)
}

/// Row counts for each rail: agents, processes, changes, all-agents.
type Counts = (usize, usize, usize, usize);

/// Every agent on every connected daemon, in tab order.
///
/// Rebuilt per paint rather than cached: it is bounded by the number of agents a
/// person actually has open, and a cache would be one more thing to invalidate
/// on every push.
fn all_agent_rows<'a>(
    daemons: &'a [Daemon],
    hosts: &'a [Option<String>],
) -> Vec<chrome::AllAgentRow<'a>> {
    let show_host = daemons.len() > 1;
    let mut out = Vec::new();
    for (d, daemon) in daemons.iter().enumerate() {
        for tab in &daemon.state.tabs {
            let Some(ws) = daemon.state.workspace(tab.id) else { continue };
            for agent in &ws.agents {
                out.push(chrome::AllAgentRow {
                    workspace: &ws.name,
                    workspace_id: ws.id,
                    agent,
                    host: if show_host { hosts[d].as_deref() } else { None },
                    daemon: d,
                });
            }
        }
    }
    out
}

/// Every connected daemon and its telemetry, for the BOOTH page's compute
/// column.
///
/// One entry per daemon in connection order, so `AllAgentRow::daemon` indexes
/// straight into it. `SysDto` is per machine and stays per machine — this is the
/// only place the client asks *every* daemon for its own, since every other page
/// is about one workspace and takes `active_daemon`'s.
fn machine_rows<'a>(
    daemons: &'a [Daemon],
    hosts: &'a [Option<String>],
    all: &[chrome::AllAgentRow<'a>],
) -> Vec<chrome::MachineRow<'a>> {
    daemons
        .iter()
        .enumerate()
        .map(|(d, daemon)| chrome::MachineRow {
            // The local daemon has no host badge — there is nothing to qualify
            // it against — but the compute column is a list of machines and an
            // unnamed row in it reads as a bug.
            label: hosts.get(d).and_then(|h| h.as_deref()).unwrap_or("local"),
            sys: &daemon.state.system,
            agents: all.iter().filter(|r| r.daemon == d).count(),
            // `State::connected` has been maintained since the event stream
            // grew a reconnect loop and read by nothing at all. This is the
            // first thing to draw it: a machine that is away goes on reporting
            // whatever it last measured, and gauges that keep moving on a
            // machine that is not there are the reason a dead link looked like
            // a working one.
            live: daemon.state.connected,
        })
        .collect()
}

/// Put a pane that has just been spawned on the stage.
///
/// Without this, `[+ term]`, `t`, a spawned agent and a click on the CPU gauge
/// all appeared to do nothing at all for anyone who had staged something by
/// hand: [`chrome::staged_pane`] prefers this client's own choice, and the
/// daemon's `stage` — which does follow the newest pane — is only the fallback
/// for when the client has made none. So the first spawn of a session worked
/// and every one after it silently did not.
///
/// On the agents page, because that is the only page with a stage to put it on:
/// reached from the Files tree or a diff, a new shell would otherwise start
/// behind whatever full-screen page you were reading.
fn stage_new_pane(view: &mut View, pane: PaneId) {
    view.staged = Some(pane);
    view.focus = Focus::Stage;
    view.page = Page::Agents;
}

/// Go to a workspace because the user asked for that workspace.
///
/// Every page but BOOTH is a *view of* the active workspace, so switching tab
/// under one is coherent — you asked for the same view of a different project,
/// and `aec1e21` is what re-points the trees when you do. BOOTH is the exception:
/// it is not about a workspace at all, it is the surface that spans daemons. So
/// "which workspace is active" is not a question BOOTH is answering, and choosing
/// a chip while it is up can only mean "take me to that project".
///
/// Without this, clicking a workspace chip on BOOTH changed the active tab behind
/// an unchanged screen and looked like it did nothing — the chip you pressed did
/// not even take the bracket, because BOOTH had it. Clicking `booth` a second time
/// was the only way out, which is how it was reported.
///
/// Only the three paths where a tab was *chosen* come here: a click, `alt-1`..
/// `alt-9`, and `alt-<`/`alt->`. A tab closing under you or a machine dropping
/// off also move `view.tab`, and being thrown off BOOTH by one of those would be
/// the screen moving on its own.
/// Carry out a [`Flow::GoTab`].
///
/// A function rather than two lines in the loop, and it goes through
/// [`select_tab`]: choosing a tab while BOOTH is showing has to leave BOOTH, and
/// that rule lives in there. Open-coding `view.tab` and [`reset_sel`] is exactly
/// how the keyboard route came to disagree with the click route about it — the
/// click kept the rule and every Alt key lost it.
fn go_tab(view: &mut View, m: TabMove, count: usize) {
    if let Some(to) = tab_target(m, view.tab, count) {
        select_tab(view, to);
    }
}

/// Which tab a [`TabMove`] lands on, or `None` when it names one that is not
/// there.
///
/// A number past the end of the bar is a key that does nothing, not a wrap onto
/// some other project — `alt-7` on a bar of three is a mistake, and moving the
/// screen somewhere arbitrary is a worse answer than staying put.
fn tab_target(m: TabMove, tab: usize, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match m {
        TabMove::To(n) if n <= count => n - 1,
        TabMove::To(_) => return None,
        TabMove::Next => (tab + 1) % count,
        TabMove::Prev => (tab + count - 1) % count,
    })
}

fn select_tab(view: &mut View, tab: usize) {
    view.tab = tab;
    reset_sel(view);
    if view.page == Page::Booth {
        view.page = Page::Agents;
    }
}

/// A new tab starts at the top of its rails rather than inheriting a cursor
/// that pointed at some other project's third agent.
fn reset_sel(view: &mut View) {
    view.agent_sel = 0;
    view.proc_sel = 0;
    view.changes_sel = 0;
    // A pane chosen on one tab means nothing on another, and following it there
    // would show a project's stage the pane it does not contain.
    view.staged = None;
}

/// The pane behind the focused rail's selected row, if it has one.
///
/// The CHANGES rail has no panes — its rows are files — so this is `None`
/// there, and a second click opens the diff instead.
/// Which daemon a browse-and-open goes to.
///
/// `view.browse_daemon` when the machine picker has answered, and the active
/// tab's otherwise — which is every single-machine client, where asking would
/// be a question with one answer.
fn browse_daemon(daemons: &[Daemon], hosts: &[Option<String>], view: &View) -> usize {
    view.browse_daemon
        .filter(|d| *d < daemons.len())
        .unwrap_or_else(|| active_daemon(daemons, hosts, view))
}

fn selected_pane(daemons: &[Daemon], hosts: &[Option<String>], view: &View) -> Option<PaneId> {
    let ws = active_workspace(daemons, hosts, view)?;
    match view.focus {
        Focus::Agents => ws.agents.get(view.agent_sel).map(|a| a.pane),
        Focus::Processes => ws.processes.get(view.proc_sel).map(|p| p.pane),
        Focus::AllAgents => {
            all_agent_rows(daemons, hosts).get(view.all_agents_sel).map(|r| r.agent.pane)
        }
        _ => None,
    }
}

/// The same row [`selected_pane`] names, with the machine and workspace it is
/// on — what anything that *calls* on it needs.
///
/// The two differ only on BOOTH. Staging asks the first question ("which pane
/// do I draw") and the answer is a number this client keeps to itself; ending a
/// session asks the second ("where do I send the DELETE"), and taking the
/// active tab's answer to that is how `x` on a `gpu-box` row killed a pane at
/// home. Every other cursor is in the workspace you are looking at, so there the
/// two agree by construction.
fn selected_route(daemons: &[Daemon], hosts: &[Option<String>], view: &View) -> Option<Route> {
    if view.focus == Focus::AllAgents {
        return fleet_route(&all_agent_rows(daemons, hosts), view.all_agents_sel);
    }
    let d = active_daemon(daemons, hosts, view);
    let ws = active_workspace(daemons, hosts, view)?;
    let pane = selected_pane(daemons, hosts, view)?;
    Some(Route { daemon: d, workspace: ws.id, pane })
}

/// Where the fleet's `sel`th agent lives. `None` once it has exited — the list
/// is live, and a row can go between the frame you pressed and the press
/// arriving.
///
/// Takes the assembled rows rather than the daemons so that it is a function of
/// the list, which is the only way to state the property that matters — a row on
/// `gpu-box` routes to `gpu-box` — without a daemon on the other end of a
/// socket.
fn fleet_route(fleet: &[chrome::AllAgentRow<'_>], sel: usize) -> Option<Route> {
    let row = fleet.get(sel)?;
    Some(Route { daemon: row.daemon, workspace: row.workspace_id, pane: row.agent.pane })
}

/// Go to the agent BOOTH's fleet cursor names, wherever it lives.
///
/// The fleet spans daemons, so this is not "stage a pane" — it is *go to that
/// workspace, on that machine, and stage it*. Staging alone pointed the middle
/// column at a pane belonging to a workspace the tab bar said you were not in,
/// and on a second daemon it did not resolve at all: `Stage::watch` re-points
/// only within the connection it already has, so a pane on another daemon needs
/// the tab to move first and the connection to be reopened, which the loop does
/// when it sees the tab's daemon change.
///
/// Returns false when the row has gone — the fleet is live, and an agent can
/// exit between the frame you clicked and the click arriving.
fn open_fleet_agent(
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &mut View,
    sel: usize,
) -> bool {
    let rows = all_agent_rows(daemons, hosts);
    let Some(row) = rows.get(sel) else { return false };
    let pane = row.agent.pane;
    let Some(tab) = tab_index(daemons, hosts)
        .iter()
        .position(|(d, t)| *d == row.daemon && daemons[*d].state.tabs[*t].name == row.workspace)
    else {
        return false;
    };
    view.all_agents_sel = sel;
    view.tab = tab;
    view.staged = Some(pane);
    view.page = Page::Agents;
    view.focus = Focus::Stage;
    true
}

fn move_sel(view: &mut View, (agents, procs, changes, all_agents): Counts, delta: isize) {
    let step = |sel: &mut usize, len: usize| {
        if len == 0 {
            *sel = 0;
            return;
        }
        let next = *sel as isize + delta;
        *sel = next.clamp(0, len as isize - 1) as usize;
    };
    match view.focus {
        Focus::Agents => step(&mut view.agent_sel, agents),
        Focus::Processes => step(&mut view.proc_sel, procs),
        Focus::Changes => step(&mut view.changes_sel, changes),
        Focus::AllAgents => step(&mut view.all_agents_sel, all_agents),
        // Both GIT cursors live on the page's own state; `handle_git_key`
        // walks them.
        Focus::Refs | Focus::History | Focus::Stage => {}
    }
}

/// Chrome, then the streamed pane, then any modal — in that order.
///
/// The order is the whole content of this function, and getting it wrong is not
/// subtle: blitting the pane after an overlay paints the pane over the modal,
/// which looked like "the overlay never opened" until it was seen on a real
/// screen.
fn compose(
    screen: &mut Buffer,
    cols: u16,
    rows: u16,
    scene: &chrome::Scene<'_>,
    view: &View,
    theme: &Theme,
    stage: Option<&Buffer>,
) {
    chrome::draw(screen, cols, rows, scene, view, theme);
    if let Some(pane) = stage {
        chrome::blit(screen, pane, chrome::stage_rect(cols, rows, view));
    }
    // After the blit for the same reason the overlay is after both: the cells it
    // dims are the pane's, and a notice explaining that the screen is a
    // photograph is no use underneath the photograph.
    if let Some(down) = scene.stage_down.as_ref() {
        if stage.is_some() {
            chrome::draw_stage_down(
                screen,
                chrome::stage_rect(cols, rows, view),
                down,
                theme,
                view.tick,
            );
        }
    }
    chrome::draw_overlay_layer(screen, cols, rows, view, theme);
}

/// The tab bar's chips, in the order they are drawn.
///
/// Built once and used by both the painting and the hit-testing: a click has to
/// resolve against the chips that are actually on screen, and a second
/// construction of the list is a second thing to keep in step.
fn tabs_of<'a>(daemons: &'a [Daemon], hosts: &'a [Option<String>]) -> Vec<chrome::Tab<'a>> {
    // Badge tabs by host only when more than one daemon is connected; with one,
    // every tab would carry the same word.
    let show_host = daemons.len() > 1;
    tab_index(daemons, hosts)
        .into_iter()
        .map(|(d, t)| chrome::Tab {
            summary: &daemons[d].state.tabs[t],
            host: if show_host { hosts[d].as_deref() } else { None },
            live: daemons[d].state.connected,
        })
        .collect()
}

/// Keep [`View::gauges`] in step with the machine on screen.
///
/// The SYSTEM section is as tall as the hardware needs, so this is what makes
/// plugging in a GPU — or connecting to a machine that has one — move the
/// boundary above it. Switching tabs to another daemon does the same, which is
/// why it is refreshed per frame rather than only when telemetry arrives.
fn sync_gauges(view: &mut View, daemons: &[Daemon], hosts: &[Option<String>]) {
    let (d, _) = *tab_index(daemons, hosts).get(view.tab).unwrap_or(&(0, 0));
    let Some(daemon) = daemons.get(d).or(daemons.first()) else { return };
    view.gauges = chrome::system_gauges(&daemon.state.system, &view.net, &view.disks);
}

/// Compose the screen and write only what changed.
#[allow(clippy::too_many_arguments)]
fn paint(
    painted: &mut Buffer,
    cols: u16,
    rows: u16,
    daemons: &[Daemon],
    hosts: &[Option<String>],
    view: &View,
    theme: &Theme,
    stage: Option<&Stage>,
    files: Option<&Files>,
    docs: Option<&Files>,
    diff: Option<&chrome::DiffView>,
    docker: Option<&Docker>,
    git: Option<&chrome::Git>,
    settings: Option<&chrome::Settings>,
    help: Option<&chrome::Help>,
    usage: Option<&chrome::usage::Usage>,
    drag: &Drag,
) -> Result<links::ScreenLinks> {
    let area = Rect::new(0, 0, cols, rows);
    let mut screen = Buffer::empty(area);
    let tabs = tabs_of(daemons, hosts);
    let ws = active_workspace(daemons, hosts, view);
    let (d, _) = *tab_index(daemons, hosts).get(view.tab).unwrap_or(&(0, 0));
    let sys = &daemons.get(d).unwrap_or(&daemons[0]).state.system;
    let all_agents = all_agent_rows(daemons, hosts);
    // Only BOOTH reads these, but they cost one pass over a list bounded by the
    // number of machines you are connected to, so they are not worth gating.
    let machines = machine_rows(daemons, hosts, &all_agents);
    // The staged pane's machine, when it has stopped answering. `hosts` is
    // indexed by daemon and holds `None` for the local one, which is exactly
    // the distinction the notice draws.
    let stage_down = stage.and_then(|s| {
        let since = s.lost?;
        Some(chrome::StageDown {
            host: hosts.get(s.daemon).and_then(Option::as_deref),
            secs: since.elapsed().as_secs(),
            has_frame: s.has_frame(),
        })
    });
    let scene = chrome::Scene {
        tabs: &tabs,
        daemons: daemons.len(),
        workspace: ws,
        system: sys,
        all_agents: &all_agents,
        machines: &machines,
        files,
        docs,
        diff,
        docker,
        git,
        settings,
        help,
        usage,
        stage_down,
    };
    // The Files and Diff pages own the middle of the screen, so there is no
    // pane to blit under them. The Docker page has one — its logs column — and
    // `stage_rect` has already measured the pane to fit exactly there.
    let pane = match view.page {
        Page::Booth | Page::Agents | Page::Docker => stage.map(|s| &s.buf),
        // GIT draws its own three columns and has no pane: its body is a
        // diff, which is text the client already holds.
        // SETTINGS, HELP and USAGE draw their own columns and have no pane
        // under them, the same as GIT: everything on all three is this
        // client's own — its configuration, its reference (compiled in), and
        // a list of numbers it already holds.
        Page::Files
        | Page::Docs
        | Page::Diff
        | Page::Git
        | Page::Settings
        | Page::Help
        | Page::Usage => None,
    };
    compose(&mut screen, cols, rows, &scene, view, theme, pane);
    // Over the composed screen, so a drag can cover the rails, a diff and the
    // pane alike — and so what is highlighted is exactly what a copy takes,
    // because both read this same buffer.
    if let Some((a, b)) = drag.span {
        selection::highlight(&mut screen, a, b, drag.clip);
    }
    // The URLs on the screen that is about to be painted — over the composed
    // buffer, so a link is found wherever it ended up being drawn, and computed
    // whatever `[ui] links` says: the toggle governs what is written to the
    // terminal, and the picker reads this map either way.
    //
    // Only a page that blits a pane passes a rect: that is the one region whose
    // rows are a program's own wrapping rather than this client's layout, and
    // therefore the one place a URL may continue on the row below.
    let links = links::ScreenLinks::of(&screen, pane.map(|_| chrome::stage_rect(cols, rows, view)));
    // Where this terminal's own cursor belongs, measured against the same rect
    // `compose` just blitted the pane into. Resolved before anything is
    // written, because the write below hides the cursor for the duration of the
    // paint and this is what says whether — and where — to put it back.
    let caret = stage_caret(view, stage, pane, cols, rows);

    if painted.area != area {
        *painted = Buffer::empty(area);
        // A resized terminal has nothing reliable to diff against.
        let mut out = io::stdout().lock();
        queue!(out, terminal::Clear(terminal::ClearType::All))?;
        out.flush()?;
    }
    let diff = painted.diff(&screen);
    let mut out = io::stdout().lock();
    // Hidden for the paint itself. Every cell written parks the cursor after
    // it, so a visible one would skitter across the screen following the diff.
    // It costs nothing: this is queued with the cells and flushed once, so the
    // terminal sees hide, draw and show as a single write.
    queue!(out, cursor::Hide)?;
    write_cells(&mut out, &diff, &links, view.links)?;
    // Put it back, or leave it hidden — a pane with no cursor of its own must
    // not leave one of this terminal's parked at the end of the last cell the
    // diff happened to touch.
    if let Some((x, y)) = caret {
        let shape = if view.focus == Focus::Stage {
            // The terminal's own shape and blink: the one the user already
            // reads as "what I type goes here". Not the frame's `cursor_shape`
            // — the daemon has no way to know it (vt100 does not track
            // DECSCUSR), so honouring that field would mean forcing a block on
            // everyone in the name of a value nothing measured.
            cursor::SetCursorStyle::DefaultUserShape
        } else {
            // Shown anyway, because where the pane's cursor sits is worth
            // knowing while the keyboard is on a rail — but steady and
            // underlined, so it does not claim keystrokes it would not
            // receive. The terminal's nearest thing to the hollow cursor the
            // web client draws for exactly this state.
            cursor::SetCursorStyle::SteadyUnderScore
        };
        queue!(out, shape, cursor::MoveTo(x, y), cursor::Show)?;
    }
    out.flush()?;
    *painted = screen;
    Ok(links)
}

/// Ends the hyperlink a cell run was written into: OSC 8 with no id and no
/// target. `ESC \` rather than `BEL` to terminate it, which is the form the
/// specification gives and the one every terminal that implements it takes.
const CLOSE_LINK: &[u8] = b"\x1b]8;;\x1b\\";

/// Where this terminal's cursor goes: the staged pane's, in screen
/// coordinates, or `None` when nothing on screen has one.
///
/// Only the daemon knows where a program's cursor is — it holds the PTY and the
/// emulator, and the escape sequences that move a cursor are consumed there and
/// never reach this terminal. So it rides on every frame, and placing it is the
/// client's half of the same split that has the client drawing the cells.
///
/// That half went missing when the client took over the drawing: the daemon
/// used to compose the whole screen and its painter placed the one cursor that
/// came with it, and the frame handler that replaced it kept the cells and
/// dropped the position. A terminal pane had no caret at all — no block to type
/// against in a shell, and no way to see that an agent was sitting on a prompt.
fn stage_caret(
    view: &View,
    stage: Option<&Stage>,
    pane: Option<&Buffer>,
    cols: u16,
    rows: u16,
) -> Option<(u16, u16)> {
    // A modal owns the keyboard and draws its own caret into the buffer. The
    // pane's would be a second cursor on a screen that already shows one, and
    // the real one would be the one in the wrong place.
    if view.overlay.is_some() {
        return None;
    }
    // `None` on every page that blits no pane. The stage keeps streaming while
    // GIT or FILES is up, so its cursor says nothing about what is on screen.
    let pane = pane?;
    let (cx, cy) = stage?.cursor?;
    let at = chrome::stage_rect(cols, rows, view);
    // Exactly [`chrome::blit`]'s bound: it copies the smaller of the two
    // rectangles, so a cursor past that edge is on a cell nobody drew. Reachable
    // in the ordinary way — between a resize and the daemon's first frame at the
    // new size the pane buffer is one size and the rect is another.
    if cx >= at.width.min(pane.area.width) || cy >= at.height.min(pane.area.height) {
        return None;
    }
    Some((at.x + cx, at.y + cy))
}

/// Everything about a cell that is not its symbol.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Style {
    fg: Color,
    bg: Color,
    modifier: Modifier,
}

/// Put the terminal into `style`.
///
/// **Every modifier, not only bold.** The painter emitted `Bold`/`NormalIntensity`
/// and dropped the other five, which cost two visible things: a drag-selection
/// was invisible — [`selection::highlight`] marks it `REVERSED` and nothing ever
/// wrote SGR 7, so people dragged over the screen and saw no selection at all —
/// and every pane lost the italic, underline, dim, reverse and strike-through
/// the daemon had faithfully carried across the wire in [`apply_frame`]. The
/// painter this replaced, back when the daemon composed the frames, wrote all of
/// them; the loss came in with client-side drawing.
///
/// `Reset` first because attributes are sticky and there is no "not italic"
/// being emitted — and the colours after it, because `Reset` clears those too.
/// Write the cells a diff says changed: position, style, hyperlink, glyph.
///
/// `marked_up` is `[ui] links` — whether a URL is handed to the terminal as an
/// OSC 8 hyperlink, so its own pointer can follow it.
fn write_cells(
    out: &mut impl Write,
    diff: &[(u16, u16, &ratatui::buffer::Cell)],
    links: &links::ScreenLinks,
    marked_up: bool,
) -> Result<()> {
    // The style the terminal is currently in, so a run of cells that share one
    // costs a single SGR. Restoring it is not an optimisation here — writing
    // every cell's style would now mean a `Reset` per cell, which is more bytes
    // than this client ever sent before.
    let mut current: Option<Style> = None;
    // Which hyperlink the terminal is currently writing cells into, by id. Kept
    // like the style beside it and for the same reason: OSC 8 is a mode, not a
    // wrapper, so a run of cells inside one link costs one sequence rather than
    // one each. Moving the cursor does not end it, which is what makes this
    // survive a diff that skips the cells in between — they are already
    // hyperlinked from the frame that drew them.
    let mut link_open: Option<u64> = None;
    for (x, y, cell) in diff {
        queue!(out, cursor::MoveTo(*x, *y))?;
        let style = Style { fg: cell.fg, bg: cell.bg, modifier: cell.modifier };
        if current != Some(style) {
            apply_style(out, style)?;
            current = Some(style);
        }
        let link = if marked_up { links.at(*x, *y) } else { None };
        if link.map(|(id, _)| id) != link_open {
            match link {
                Some((id, url)) => out.write_all(open_link(id, url).as_bytes())?,
                None => out.write_all(CLOSE_LINK)?,
            }
            link_open = link.map(|(id, _)| id);
        }
        // A cell whose symbol was set to the empty string is one column of
        // nothing; the terminal needs a byte for it or everything after it on
        // the row shifts left.
        match cell.symbol() {
            "" => out.write_all(b" ")?,
            s => out.write_all(s.as_bytes())?,
        }
    }
    // Never leave one open: the next thing written to this terminal is the
    // cursor, and after that whatever the shell prints when butai exits.
    if link_open.is_some() {
        out.write_all(CLOSE_LINK)?;
    }
    Ok(())
}

/// Begin a hyperlink: `OSC 8 ; params ; target ST`.
///
/// The `id` is what tells the terminal that the cells written next are all one
/// link — which is what a wrapped URL needs, since its halves are separated by
/// a cursor move to the next row. See [`links::ScreenLinks::at`] for where it
/// comes from.
fn open_link(id: u64, url: &str) -> String {
    format!("\x1b]8;id={id:x};{url}\x1b\\")
}

fn apply_style(out: &mut impl Write, style: Style) -> Result<()> {
    queue!(out, SetAttribute(Attribute::Reset))?;
    queue!(out, SetForegroundColor(rat_to_ct(style.fg)))?;
    queue!(out, SetBackgroundColor(rat_to_ct(style.bg)))?;
    for (flag, attr) in [
        (Modifier::BOLD, Attribute::Bold),
        (Modifier::DIM, Attribute::Dim),
        (Modifier::ITALIC, Attribute::Italic),
        (Modifier::UNDERLINED, Attribute::Underlined),
        (Modifier::REVERSED, Attribute::Reverse),
        (Modifier::CROSSED_OUT, Attribute::CrossedOut),
    ] {
        if style.modifier.contains(flag) {
            queue!(out, SetAttribute(attr))?;
        }
    }
    Ok(())
}

/// Apply a damage diff from the daemon into the stage buffer.
fn apply_frame(buf: &mut Buffer, update: &FrameUpdate) {
    if update.full {
        buf.reset();
    }
    let area = buf.area;
    for run in &update.cells {
        let mut x = run.x;
        for cell in &run.cells {
            if x >= area.width || run.y >= area.height {
                break;
            }
            if let Some(dst) = buf.cell_mut((area.x + x, area.y + run.y)) {
                dst.set_symbol(&cell.ch);
                dst.set_fg(conv_color(cell.fg));
                dst.set_bg(conv_color(cell.bg));
                let mut m = Modifier::empty();
                if cell.mods.bold {
                    m |= Modifier::BOLD;
                }
                if cell.mods.italic {
                    m |= Modifier::ITALIC;
                }
                if cell.mods.underline {
                    m |= Modifier::UNDERLINED;
                }
                if cell.mods.reverse {
                    m |= Modifier::REVERSED;
                }
                if cell.mods.dim {
                    m |= Modifier::DIM;
                }
                if cell.mods.crossed_out {
                    m |= Modifier::CROSSED_OUT;
                }
                dst.modifier = m;
            }
            // Advance by display width, not one per cell: a run carries no
            // filler for the second column of a wide grapheme.
            x += unicode_width::UnicodeWidthStr::width(cell.ch.as_str()).max(1) as u16;
        }
    }
}

fn conv_color(c: PColor) -> Color {
    match c {
        PColor::Default => Color::Reset,
        PColor::Indexed(n) => Color::Indexed(n),
        PColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn rat_to_ct(c: Color) -> crossterm::style::Color {
    use crossterm::style::Color as Ct;
    match c {
        Color::Reset => Ct::Reset,
        Color::Black => Ct::Black,
        Color::Red => Ct::DarkRed,
        Color::Green => Ct::DarkGreen,
        Color::Yellow => Ct::DarkYellow,
        Color::Blue => Ct::DarkBlue,
        Color::Magenta => Ct::DarkMagenta,
        Color::Cyan => Ct::DarkCyan,
        Color::Gray => Ct::Grey,
        Color::DarkGray => Ct::DarkGrey,
        Color::LightRed => Ct::Red,
        Color::LightGreen => Ct::Green,
        Color::LightYellow => Ct::Yellow,
        Color::LightBlue => Ct::Blue,
        Color::LightMagenta => Ct::Magenta,
        Color::LightCyan => Ct::Cyan,
        Color::White => Ct::White,
        Color::Indexed(n) => Ct::AnsiValue(n),
        Color::Rgb(r, g, b) => Ct::Rgb { r, g, b },
    }
}

/// Terminal events, read on a blocking thread.
///
/// Raw crossterm events rather than `ClientMsg`s, because this client decides
/// what a key means before anything reaches the wire.
fn spawn_raw_input() -> UnboundedReceiver<event::Event> {
    let (tx, rx) = unbounded_channel();
    std::thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            while let Ok(ev) = event::read() {
                if tx.send(ev).is_err() {
                    break;
                }
            }
        })
        .ok();
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use butai_protocol::{Cell as PCell, CellRun, Mods};

    fn entry(name: &str, is_dir: bool) -> chrome::FileEntry {
        chrome::FileEntry { name: name.into(), path: name.into(), is_dir, changed: false }
    }

    /// Descending into a folder has to be reversible on screen, not only via a
    /// key nothing advertises.
    #[test]
    fn a_subdirectory_listing_offers_a_way_back_up() {
        let rows = tree_rows(Page::Files, vec![entry("a.rs", false)], "src");
        assert_eq!(rows[0].name, "..", "the first row must be the way back");
        assert!(rows[0].is_dir, "`..` has to be a directory or Enter would try to open it");
        assert_eq!(rows[0].path, "", "one level above `src` is the root");
    }

    /// At the root there is nowhere above to go, and offering one would walk out
    /// of the workspace.
    #[test]
    fn the_root_listing_has_no_way_out_of_the_workspace() {
        let rows = tree_rows(Page::Files, vec![entry("a.rs", false)], "");
        assert!(rows.iter().all(|e| e.name != ".."), "the root must not offer `..`");
    }

    /// **The Docs filter is not here any more, and must not come back.**
    ///
    /// It ran in this function, over a listing whose `changed` markers had
    /// already been decided across the whole change set — so a directory kept a
    /// `●` earned by a file this function then dropped, and following one down
    /// the rail landed on an empty listing every time. `fetch_dir` asks for
    /// `?filter=docs` and the rows arrive filtered by the same rule that
    /// decided their markers.
    ///
    /// A `.rs` therefore has to pass straight through. If a filter creeps back
    /// in here the two rules can disagree again, and this is what says so.
    #[test]
    fn the_docs_page_does_not_filter_the_listing_a_second_time() {
        let entries = vec![entry("code.rs", false), entry("NOTES.md", false)];
        let rows = tree_rows(Page::Docs, entries, "sub");
        let names: Vec<&str> = rows.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["..", "code.rs", "NOTES.md"], "the daemon already filtered");
        assert_eq!(rows[0].name, "..", "and the way back is still on top");
    }

    /// A tree page lists a project's own files, and only those.
    ///
    /// **The DOCS root used to carry butai's own reference too**, as a
    /// `butai://reference` folder above the project's writing, because `[help]`
    /// landed here and something had to hold the topics. Help is its own page
    /// now, so this rail answers one question again — and the thing being
    /// pinned is that nothing in it is invented by the client: every row came
    /// from the listing, or is the `..` that walks out of it.
    #[test]
    fn a_tree_page_lists_the_project_and_nothing_of_butais_own() {
        for (page, dir) in
            [(Page::Docs, ""), (Page::Docs, "docs"), (Page::Files, ""), (Page::Files, "src")]
        {
            let given = vec![entry("README.md", false), entry("sub", true)];
            let rows = tree_rows(page, given.clone(), dir);
            for row in &rows {
                assert!(
                    row.name == ".." || given.iter().any(|e| e.name == row.name),
                    "`{}` is in the {page:?} rail without being in the listing",
                    row.name
                );
                assert!(
                    !row.path.contains("://"),
                    "`{}` carries a sentinel path rather than a file's",
                    row.name
                );
            }
        }
    }

    fn frame(full: bool, runs: Vec<CellRun>) -> FrameUpdate {
        FrameUpdate {
            full,
            cells: runs,
            cursor: None,
            cursor_shape: Default::default(),
            wants_mouse: false,
        }
    }

    /// The ordinary case must stay silent: a matching pair is every normal run,
    /// and a footer that always says something says nothing.
    #[test]
    fn a_matching_daemon_says_nothing() {
        assert_eq!(skew_notice(Some(env!("CARGO_PKG_VERSION"))), None);
    }

    /// A daemon too old to send the field is the case this was written for — the
    /// one that spent a session looking like five unrelated bugs.
    #[test]
    fn a_daemon_that_cannot_name_itself_is_reported_as_old() {
        let notice = skew_notice(None).expect("a silent old daemon is the bug, not the fix");
        assert!(
            notice.contains("predates this client"),
            "{notice:?} must compare against the client, not claim the daemon is \
             older than a version it may itself be — an unreleased fix does not \
             bump CARGO_PKG_VERSION, so both sides usually say the same number"
        );
        assert!(
            notice.contains("kill-server"),
            "{notice:?} must name the way out, not just the diagnosis"
        );
    }

    #[test]
    fn a_daemon_of_a_different_build_names_both_versions() {
        let notice = skew_notice(Some("0.0.1-ancient")).expect("a mismatch must be reported");
        assert!(notice.contains("0.0.1-ancient"), "{notice:?} should name the daemon's version");
        assert!(notice.contains(env!("CARGO_PKG_VERSION")), "{notice:?} should name the client's");
    }

    fn run(x: u16, y: u16, text: &str) -> CellRun {
        CellRun {
            x,
            y,
            cells: text
                .chars()
                .map(|ch| PCell {
                    ch: ch.to_string(),
                    fg: PColor::Default,
                    bg: PColor::Default,
                    mods: Mods::default(),
                })
                .collect(),
        }
    }

    fn text_of(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width)
            .filter_map(|x| buf.cell((x, y)).map(|c| c.symbol().to_string()))
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn a_full_frame_clears_what_the_previous_pane_left() {
        // The property `Watch` depends on: a full frame must not leave cells
        // from the pane we stopped showing.
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 2));
        apply_frame(&mut buf, &frame(true, vec![run(0, 0, "OLD PANE OUTPUT")]));
        assert_eq!(text_of(&buf, 0), "OLD PANE OUTPUT");
        apply_frame(&mut buf, &frame(true, vec![run(0, 0, "new")]));
        assert_eq!(text_of(&buf, 0), "new", "a full frame should have cleared the rest");
    }

    #[test]
    fn a_partial_frame_leaves_the_rest_alone() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 2));
        apply_frame(&mut buf, &frame(true, vec![run(0, 0, "abcdef")]));
        apply_frame(&mut buf, &frame(false, vec![run(2, 0, "XY")]));
        assert_eq!(text_of(&buf, 0), "abXYef");
    }

    #[test]
    fn a_wide_grapheme_advances_two_columns() {
        // A run carries no filler cell for the second column, so a reader that
        // advances one per cell shifts everything after it.
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        apply_frame(&mut buf, &frame(true, vec![run(0, 0, "日本x")]));
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "日");
        assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "本");
        assert_eq!(buf.cell((4, 0)).unwrap().symbol(), "x");
    }

    /// The bug this pins: the streamed pane used to be blitted after the
    /// chrome, so it painted straight over any modal. It read as "the overlay
    /// never opened", and no test of `chrome::draw` alone could see it —
    /// blitting is what does the damage, and it happens outside that function.
    #[test]
    fn a_modal_survives_the_pane_being_blitted_over_the_stage() {
        use butai_protocol::api::SysDto;
        use chrome::{Overlay, Scene, Theme};

        let (cols, rows) = (100u16, 30u16);
        let view = View {
            overlay: Some(Overlay::Confirm(chrome::ConfirmOverlay {
                title: "CLOSE".into(),
                header: "close this workspace?".into(),
                yes: false,
                kind: chrome::ConfirmKind::CloseWorkspace {
                    id: butai_protocol::SessionId(1),
                    name: "proj".into(),
                },
            })),
            ..Default::default()
        };
        // A pane full of solid text, so anything painting over the modal shows.
        let rect = chrome::stage_rect(cols, rows, &view);
        let mut pane = Buffer::empty(rect);
        for y in 0..rect.height {
            for x in 0..rect.width {
                if let Some(c) = pane.cell_mut((rect.x + x, rect.y + y)) {
                    c.set_char('#');
                }
            }
        }

        let mut screen = Buffer::empty(Rect::new(0, 0, cols, rows));
        let sys = SysDto::default();
        let scene = Scene::new(&[], &sys);
        compose(&mut screen, cols, rows, &scene, &view, &Theme::default(), Some(&pane));

        let text: String = (0..rows)
            .map(|y| {
                (0..cols)
                    .filter_map(|x| screen.cell((x, y)).map(|c| c.symbol().to_string()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("CLOSE"), "the modal was painted over:\n{text}");
        assert!(text.contains("close this workspace?"), "the modal lost its content:\n{text}");
        assert!(text.contains('#'), "the pane should still be on the stage around it");
    }

    #[test]
    fn query_values_are_encoded() {
        // Paths routinely carry characters that change what a query string
        // means; `/` is the one that must survive, since it is the path.
        assert_eq!(urlencode("src/main.rs"), "src/main.rs");
        assert_eq!(urlencode("a b.txt"), "a%20b.txt");
        assert_eq!(urlencode("c#1+2&x=y"), "c%231%2B2%26x%3Dy");
        assert_eq!(urlencode(""), "");
    }

    #[test]
    fn selection_stops_at_the_ends_of_a_rail() {
        let mut view = View { focus: Focus::Agents, ..Default::default() };
        move_sel(&mut view, (3, 0, 0, 0), -1);
        assert_eq!(view.agent_sel, 0, "should not walk off the top");
        for _ in 0..10 {
            move_sel(&mut view, (3, 0, 0, 0), 1);
        }
        assert_eq!(view.agent_sel, 2, "should not walk off the end");
    }

    #[test]
    fn an_empty_rail_keeps_its_cursor_at_zero() {
        let mut view = View { focus: Focus::Processes, ..Default::default() };
        move_sel(&mut view, (0, 0, 0, 0), 1);
        assert_eq!(view.proc_sel, 0);
    }

    /// Every row of the CHANGES rail, and what Enter opens on it.
    ///
    /// The mapping is by *index into the drawn rows*, so this is the assertion
    /// that catches the two lists drifting apart: shift the headings and every
    /// row below opens the wrong file.
    #[test]
    fn enter_on_the_changes_rail_opens_what_the_cursor_names() {
        use butai_protocol::api::{ChangesDto, CommitDto, FileChange, RepoState};
        use butai_protocol::SessionId;

        let file = |path: &str, code: &str| FileChange {
            path: path.into(),
            code: code.into(),
            added: 1,
            deleted: 0,
        };
        let ws = WorkspaceDetail {
            id: SessionId(1),
            name: "proj".into(),
            cwd: "/tmp/proj".into(),
            agents: vec![],
            processes: vec![],
            stage: None,
            changes: Some(ChangesDto {
                branch: "main".into(),
                staged: vec![file("s.rs", "A")],
                unstaged: vec![file("u.rs", "M")],
                recent_commits: vec![CommitDto {
                    id: "abcdef1234".into(),
                    summary: "first".into(),
                }],
                conflicted: vec![],
                upstream: None,
                ahead: 0,
                behind: 0,
                state: RepoState::Clean,
                detached: false,
            }),
        };
        let at = |i| diff_under_cursor(Some(&ws), i);
        // Row 0 is the "Unstaged" heading, and a heading diffs its section.
        assert_eq!(at(0), Some(DiffKind::Unstaged { path: None }));
        assert_eq!(at(1), Some(DiffKind::Unstaged { path: Some("u.rs".into()) }));
        assert_eq!(at(2), Some(DiffKind::Staged { path: None }));
        assert_eq!(at(3), Some(DiffKind::Staged { path: Some("s.rs".into()) }));
        // "Commits" is a heading over history; there is no section to diff.
        assert_eq!(at(4), None);
        assert_eq!(
            at(5),
            Some(DiffKind::Commit { id: "abcdef1234".into(), summary: "first".into() })
        );
        assert_eq!(at(6), None, "past the end of the rail");
        assert_eq!(diff_under_cursor(None, 0), None, "no workspace, nothing to open");
    }

    fn announcement(hint: &str, ssh_target: &str) -> butai_protocol::api::RemoteAnnounceDto {
        butai_protocol::api::RemoteAnnounceDto {
            pane: PaneId(1),
            hint: hint.into(),
            socket: "/run/user/1000/butai/butai.sock".into(),
            ssh_target: ssh_target.into(),
            ssh_args: vec!["-p".into(), "2222".into()],
        }
    }

    /// Which way back the client takes.
    ///
    /// The ssh arguments the daemon recovered from the pane's own process reach
    /// the same host the same way. `$SSH_CONNECTION` is a fallback because
    /// behind NAT it is an address that means nothing on this side.
    #[test]
    fn the_way_back_prefers_the_pane_over_the_far_sides_guess() {
        assert_eq!(announced_target(&announcement("10.8.0.4", "build-box")).unwrap(), "build-box");
        assert_eq!(announced_target(&announcement("10.8.0.4", "")).unwrap(), "10.8.0.4");
        assert!(
            announced_target(&announcement("", "")).is_err(),
            "an announcement with no way back is an error, not a silent no-op"
        );
    }

    /// A pane can announce more than once — a reconnect, a second `butai` in the
    /// same session — and each one must not open a second connection.
    ///
    /// The loop's guard is `already in the bar || already being dialled`, and
    /// the second half matters because an ssh takes seconds: without it, three
    /// announcements a second apart become three connections.
    #[test]
    fn a_machine_already_here_or_on_its_way_is_not_dialled_again() {
        let a = announcement("10.8.0.4", "build-box");
        let target = announced_target(&a).unwrap();
        let none: HashSet<String> = Default::default();

        assert!(should_dial(target, &[None], &none), "an unknown machine should be dialled");
        assert!(
            !should_dial(target, &[None, Some("build-box".into())], &none),
            "it is already in the bar"
        );
        assert!(
            !should_dial(target, &[None], &["build-box".to_string()].into()),
            "it is already being dialled"
        );
        // The local daemon has no badge, and `None` must not match anything.
        assert!(should_dial("build-box", &[None, None], &none));
    }

    /// `[general] remote_auto_attach = false` means "I would rather connect
    /// hosts deliberately with `[+ host]`" — so it must stop an announcement
    /// from dialling and must *not* stop the picker.
    ///
    /// The daemon owned this until it stopped dialling and started reporting.
    /// The setting kept parsing after that and nothing read it, so an
    /// announcement connected regardless of what the user had asked for.
    #[test]
    fn auto_attach_off_stops_an_announcement_but_not_the_picker() {
        let a = announcement("10.8.0.4", "build-box");
        let target = announced_target(&a).unwrap();
        let none: HashSet<String> = Default::default();

        assert!(announcement_dials(target, &[None], &none, true), "on: it should dial");
        assert!(!announcement_dials(target, &[None], &none, false), "off: it must not dial");
        // ...and off must not disable the dedup either way round.
        assert!(!announcement_dials(target, &[None], &none.clone(), false));
        assert!(!announcement_dials(target, &[None, Some("build-box".into())], &none, true));
        // The picker does not consult it at all.
        assert!(should_dial(target, &[None], &none), "the picker must still dial");
    }

    /// A far daemon that restarts drops the stream for a moment and comes
    /// straight back. Spending an ssh on that would mean a dial every time a
    /// remote machine's daemon was upgraded, so a live forward has to lose the
    /// stream twice before anything happens.
    #[test]
    fn a_hiccup_on_a_live_forward_does_not_cost_an_ssh() {
        let mut downed = HashMap::new();
        let now = Instant::now();
        assert!(!redial_due(&mut downed, "gpu-box", true, now), "one loss is a hiccup");
        assert!(redial_due(&mut downed, "gpu-box", true, now), "two in a row is gone");
    }

    /// The other half: an ssh that has exited is conclusive. The socket went
    /// with it and the stream task is retrying a path that cannot come back, so
    /// there is nothing to wait a second loss for.
    #[test]
    fn a_dead_ssh_is_redialled_at_once() {
        let mut downed = HashMap::new();
        assert!(redial_due(&mut downed, "gpu-box", false, Instant::now()));
    }

    /// A machine that is off must not cost an ssh every time its stream task
    /// gives up — that is one dial every ten seconds, each able to sit in
    /// `whoami` for twenty, for as long as the laptop is shut.
    #[test]
    fn a_machine_that_stays_down_backs_off() {
        let mut downed = HashMap::new();
        let t0 = Instant::now();
        assert!(redial_due(&mut downed, "gpu-box", false, t0), "the first is free");
        assert!(!redial_due(&mut downed, "gpu-box", false, t0), "not twice in the same instant");
        assert!(
            !redial_due(&mut downed, "gpu-box", false, t0 + REDIAL_MIN - Duration::from_millis(1)),
            "not before the wait is up"
        );

        // Each attempt doubles the next wait, so an hour of being off costs a
        // handful of dials rather than the ~360 that answering every stream
        // retry would. The stream task gives up every 10s; this is that clock.
        let hour = Duration::from_secs(3600);
        let mut at = t0;
        let mut fired = Vec::new();
        while at < t0 + hour {
            at += Duration::from_secs(10);
            if redial_due(&mut downed, "gpu-box", false, at) {
                fired.push(at - t0);
            }
        }
        assert!(fired.len() < 20, "{} dials in an hour of being off", fired.len());
        assert_eq!(downed["gpu-box"].backoff, REDIAL_MAX, "the wait must stop growing");
        // Once capped it is one every five minutes, not one every ten seconds.
        let last_two = &fired[fired.len() - 2..];
        assert_eq!(last_two[1] - last_two[0], REDIAL_MAX);
    }

    /// Coming back is what resets it: the next drop starts from the short wait
    /// again rather than inheriting five minutes from an outage last morning.
    #[test]
    fn coming_back_clears_the_backoff() {
        let mut downed = HashMap::new();
        let t0 = Instant::now();
        // Far enough apart that each one is actually due, so the backoff grows.
        let mut at = t0;
        for _ in 0..4 {
            assert!(redial_due(&mut downed, "gpu-box", false, at));
            at += REDIAL_MAX;
        }
        assert!(downed["gpu-box"].backoff > REDIAL_MIN);

        // What the `Connected` arm does.
        downed.remove("gpu-box");

        assert!(redial_due(&mut downed, "gpu-box", false, t0), "a fresh drop tries at once");
        assert_eq!(downed["gpu-box"].backoff, REDIAL_MIN);
    }

    /// Two machines back off independently. Sharing a schedule would mean one
    /// asleep laptop delaying the reconnect of a build box that is right there.
    #[test]
    fn machines_back_off_independently() {
        let mut downed = HashMap::new();
        let now = Instant::now();
        assert!(redial_due(&mut downed, "gpu-box", false, now));
        assert!(redial_due(&mut downed, "build-box", false, now));
        assert!(!redial_due(&mut downed, "gpu-box", false, now));
    }

    fn ssh_host(alias: &str, hostname: Option<&str>) -> crate::ssh_config::SshHost {
        crate::ssh_config::SshHost {
            alias: alias.into(),
            hostname: hostname.map(str::to_string),
            port: None,
            user: None,
        }
    }

    fn rows_of(overlay: &Overlay) -> (Vec<String>, Vec<String>) {
        let Overlay::List(list) = overlay else { panic!("the host picker is not a list") };
        (list.items.clone(), list.values.clone().expect("a host row's value is its alias"))
    }

    /// A machine we dialled ourselves, and so can drop.
    fn ours(host: &str) -> Connected {
        Connected { host: host.into(), ours: true }
    }

    /// A machine already in the bar is not offered a second time — it is shown
    /// as connected instead.
    ///
    /// Connecting it twice would open a second ssh to the same daemon and
    /// duplicate every one of its projects in the tab bar, so it never appears
    /// among the offers — matched by *alias*, which is what ssh resolves and
    /// what the badge carries, not by the hostname behind it. What it does get
    /// is a row of its own, at the top, that drops the link.
    #[test]
    fn the_host_picker_shows_a_connected_machine_instead_of_offering_it() {
        let hosts = vec![
            ssh_host("gpu-box", Some("10.0.0.5")),
            ssh_host("build-box", None),
            ssh_host("laptop", None),
        ];
        let connected = [ours("build-box")];
        let (items, values) = rows_of(&host_overlay(hosts, &connected, &Default::default()));
        assert_eq!(
            values,
            vec![
                format!("{DISCONNECT}build-box"),
                "gpu-box".into(),
                "laptop".into(),
                TYPE_DESTINATION.into()
            ],
            "build-box is here already, so its row disconnects rather than dials"
        );
        // It says which it is, and what Enter will do to it — this box is the
        // only place either is written down.
        assert!(items[0].contains("build-box"), "{:?}", items[0]);
        assert!(items[0].contains("connected"), "{:?}", items[0]);
        assert!(items[0].contains("disconnect"), "{:?}", items[0]);
        // The offers read as the alias plus where it goes; the value is the
        // bare alias, because that is what `ssh` is handed.
        assert!(items[1].contains("gpu-box"), "{:?}", items[1]);
        assert!(items[1].contains("10.0.0.5"), "the detail column is missing: {:?}", items[1]);
        assert_eq!(items[2].trim(), "laptop", "an alias that adds nothing gets no detail column");
    }

    /// The local daemon is not a link, so it gets no row.
    ///
    /// It is `None` in `hosts` by definition, and a "disconnect this machine"
    /// row would offer to cut the client off from the daemon it is running
    /// against — which is what `q` is for. [`connected_machines`] is what drops
    /// it, so this asserts on that rather than on a hand-built list.
    #[test]
    fn the_host_picker_offers_no_way_to_disconnect_this_machine() {
        let machines = connected_machines(&[None], &[PathBuf::from("/run/local.sock")], &[]);
        assert!(machines.is_empty(), "this machine must not be a row");
        let (_, values) = rows_of(&host_overlay(vec![], &machines, &Default::default()));
        assert_eq!(values, vec![TYPE_DESTINATION]);
    }

    /// A machine reached through a forward we did not open says so, and does
    /// not offer to close it.
    ///
    /// `[[remote]] socket` names a socket somebody else forwarded — the user's
    /// own `ssh -N -L`, or another tool's. There is no child of ours under it,
    /// so [`disconnect_daemon`] refuses; the row has to refuse first, or the box
    /// advertises an action that answers with an error.
    #[test]
    fn a_machine_on_someone_elses_forward_is_shown_but_not_offered_up() {
        // No forwards of ours at all, and a named machine in the bar: exactly
        // what an `[[remote]] socket` block produces.
        let machines = connected_machines(
            &[None, Some("gpu-box".into())],
            &[PathBuf::from("/run/local.sock"), PathBuf::from("/run/gpu.sock")],
            &[],
        );
        assert_eq!(machines.len(), 1, "the named one, and not this machine");
        assert!(!machines[0].ours, "we did not dial it");

        let (items, values) = rows_of(&host_overlay(vec![], &machines, &Default::default()));
        assert_eq!(values, vec![format!("{KEEP}gpu-box"), TYPE_DESTINATION.into()]);
        assert!(items[0].contains("gpu-box"), "{:?}", items[0]);
        assert!(items[0].contains("connected"), "it is still here: {:?}", items[0]);
        assert!(
            !items[0].contains("enter to disconnect"),
            "it must not promise what it cannot do: {:?}",
            items[0]
        );
    }

    /// Disconnecting refuses everything that is not a link we opened.
    ///
    /// All three guards run before anything is removed, which is what lets this
    /// be asserted without a live daemon on the other end — and the property
    /// they protect is that the client cannot drop the daemon it is running
    /// against and leave itself with nothing to draw.
    #[test]
    fn only_a_machine_we_dialled_can_be_disconnected() {
        // An alias nobody is connected to.
        let mut view = View::default();
        let (mut daemons, mut forwards) = (Vec::new(), Vec::new());
        let mut hosts = vec![None];
        let mut sockets = vec![PathBuf::from("/run/local.sock")];
        let err = disconnect_host(
            "gpu-box",
            &mut daemons,
            &mut hosts,
            &mut sockets,
            &mut forwards,
            &mut view,
        )
        .expect_err("nothing by that name is connected");
        assert!(format!("{err:#}").contains("not connected"), "{err:#}");

        // This machine, by index. It is `None` in `hosts`, which is what says
        // "not a link" — there is no ssh under it to drop.
        let err =
            disconnect_daemon(0, &mut daemons, &mut hosts, &mut sockets, &mut forwards, &mut view)
                .expect_err("the local daemon is not a link");
        assert!(format!("{err:#}").contains("not a link"), "{err:#}");

        // A named machine that arrived as an `Endpoint` rather than a dial: it
        // has a badge but no forward of ours, so dropping one would kill
        // nothing and remove a daemon that is still perfectly reachable.
        hosts.push(Some("gpu-box".into()));
        sockets.push(PathBuf::from("/run/gpu.sock"));
        let err =
            disconnect_daemon(1, &mut daemons, &mut hosts, &mut sockets, &mut forwards, &mut view)
                .expect_err("we did not dial it");
        assert!(format!("{err:#}").contains("not one we dialled"), "{err:#}");

        // Nothing was removed on the way through any of them.
        assert_eq!(hosts.len(), 2, "a refused disconnect must not shorten the bar");
        assert_eq!(sockets.len(), 2);
    }

    /// Indices held across a disconnect follow the machine they named.
    ///
    /// `browse_daemon` is the one that matters: it decides which machine `alt-n`
    /// opens a project on, so an index left pointing one slot too high opens the
    /// new workspace on somebody else's machine.
    #[test]
    fn a_held_daemon_index_moves_with_the_removal() {
        // Below the removal: unmoved.
        assert_eq!(index_after_removal(Some(0), 1), Some(0));
        // Above it: shifted down, because `Vec::remove` closed the gap.
        assert_eq!(index_after_removal(Some(2), 1), Some(1));
        assert_eq!(index_after_removal(Some(3), 1), Some(2));
        // The machine it pointed at is the one that left.
        assert_eq!(index_after_removal(Some(1), 1), None);
        // Nothing held stays nothing held.
        assert_eq!(index_after_removal(None, 1), None);
    }

    /// A machine whose ssh is still coming up says so.
    ///
    /// It is in neither list — not connected, and filtered out of the offers so
    /// a second Enter cannot start a second ssh — so without a row of its own a
    /// slow host simply vanished from the box between asking for it and its
    /// tabs arriving.
    #[test]
    fn the_host_picker_names_a_machine_still_dialling() {
        let dialling: HashSet<String> = ["gpu-box".to_string()].into();
        let hosts = vec![ssh_host("gpu-box", None), ssh_host("laptop", None)];
        let (items, values) = rows_of(&host_overlay(hosts, &[], &dialling));
        assert_eq!(
            values,
            vec![format!("{CONNECTING}gpu-box"), "laptop".into(), TYPE_DESTINATION.into()],
            "the one being dialled is not also offered"
        );
        assert!(items[0].contains("connecting"), "{:?}", items[0]);
    }

    /// No ssh config is not a machine you cannot reach.
    ///
    /// It was: the box listed one row that named `~/.ssh/config`, carried no
    /// value and did nothing on Enter, so a user who keeps no config file — and
    /// reaches every one of their machines by typing `ssh user@host` — had a
    /// picker that could not connect to any of them. The row that asks is
    /// always the last one, so the aliases stay the answer when there are any.
    #[test]
    fn a_machine_can_be_typed_when_the_config_offers_none() {
        let (items, values) = rows_of(&host_overlay(vec![], &[], &Default::default()));
        assert_eq!(values, vec![TYPE_DESTINATION], "the only way out has to be a way out");
        assert!(items[0].trim().starts_with("type a destination"), "{:?}", items[0]);
        // Still says why the list is empty — that part was right.
        assert!(items[0].contains("~/.ssh/config"), "{:?}", items[0]);

        // Every host being connected already is *not* the same situation, and
        // saying so would be a lie about the one thing that row explains: the
        // file has an entry, it is just already in the bar.
        let all_here =
            host_overlay(vec![ssh_host("gpu-box", None)], &[ours("gpu-box")], &Default::default());
        let (items, values) = rows_of(&all_here);
        assert_eq!(values, vec![format!("{DISCONNECT}gpu-box"), TYPE_DESTINATION.into()]);
        assert!(
            !items[1].contains("~/.ssh/config"),
            "the file is not empty — it is all connected: {:?}",
            items[1]
        );
    }

    /// What is typed goes to `ssh` as it was typed, minus the whitespace a
    /// paste brings with it. Enter on an empty box asks again rather than
    /// dialling the empty string — which `ssh` reads as a host called nothing.
    #[test]
    fn a_typed_destination_is_dialled_verbatim() {
        let mut view = View { overlay: Some(destination_prompt()), ..View::default() };
        for c in " paul@10.0.0.5 ".chars() {
            handle_prompt_key(key(event::KeyCode::Char(c)), &mut view);
        }
        let flow = handle_prompt_key(key(event::KeyCode::Enter), &mut view);
        assert!(
            matches!(&flow, Flow::DialHost(t) if t == "paul@10.0.0.5"),
            "{flow:?} should have dialled the trimmed destination"
        );

        let mut empty = View { overlay: Some(destination_prompt()), ..View::default() };
        let flow = handle_prompt_key(key(event::KeyCode::Enter), &mut empty);
        assert!(matches!(flow, Flow::Continue), "an empty box must not dial: {flow:?}");
        assert_eq!(
            empty.flash.as_deref(),
            Some("a machine needs a destination"),
            "and it must say what it wanted, not ask for a commit message"
        );
    }

    /// The three shapes a docker command takes.
    ///
    /// The standalone case is here because live testing caught it: a one-
    /// container stack has no compose project, and treating it as one produced
    /// `docker logs -f --tail 200` with no container — which the box duly
    /// showed as docker's usage message.
    #[test]
    fn a_docker_command_names_what_it_acts_on() {
        use butai_protocol::api::{ContainerDto, StackDto};
        let dto = |project: &str, workdir: &str, names: &[&str]| StackDto {
            label: "l".into(),
            project: project.into(),
            workdir: workdir.into(),
            running: names.len(),
            total: names.len(),
            containers: names
                .iter()
                .map(|n| ContainerDto { name: (*n).into(), state: "running".into() })
                .collect(),
        };

        // One container, picked out of a stack.
        let s = dto("app", "/proj", &["app-web-1", "app-db-1"]);
        let stack = chrome::Stack { dto: &s, mine: true };
        assert_eq!(
            docker_command(&stack, Some("app-web-1"), "logs -f"),
            "docker logs -f 'app-web-1'"
        );
        // The whole compose project, from its working directory.
        assert_eq!(
            docker_command(&stack, None, "logs -f"),
            "cd '/proj' && docker compose --ansi always logs -f"
        );
        // A project with no working directory is addressed by name.
        let s = dto("app", "", &["app-web-1"]);
        let stack = chrome::Stack { dto: &s, mine: true };
        assert_eq!(docker_command(&stack, None, "stop"), "docker compose -p 'app' stop");
        // A standalone stack is its containers, named directly — never
        // `docker compose`, and never with no argument at all.
        let s = dto("", "", &["lonely"]);
        let stack = chrome::Stack { dto: &s, mine: false };
        assert_eq!(docker_command(&stack, None, "logs -f"), "docker logs -f 'lonely'");
        // And a name that would otherwise reach the shell is quoted.
        let s = dto("", "", &["a'; rm -rf /"]);
        let stack = chrome::Stack { dto: &s, mine: false };
        assert_eq!(docker_command(&stack, None, "stop"), r"docker stop 'a'\''; rm -rf /'");
    }

    fn key(code: event::KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(code, event::KeyModifiers::NONE)
    }

    /// One agent, as the daemon describes it.
    fn agent_dto(
        pane: u64,
        title: &str,
        state: butai_protocol::api::AgentState,
    ) -> butai_protocol::api::AgentDto {
        butai_protocol::api::AgentDto {
            pane: PaneId(pane),
            title: title.into(),
            state,
            exited: None,
            question: false,
            started_ms: 0,
            working_since_ms: None,
            unread: false,
        }
    }

    /// A workspace with two agents, so a menu can name the second one and be
    /// wrong in a visible way if it names the first.
    fn ws_with_agents() -> WorkspaceDetail {
        use butai_protocol::api::AgentState;
        let agent = |pane: u64, title: &str| agent_dto(pane, title, AgentState::Idle);
        WorkspaceDetail {
            id: butai_protocol::SessionId(1),
            name: "proj".into(),
            cwd: "/tmp/proj".into(),
            agents: vec![agent(10, "claude"), agent(11, "codex")],
            processes: vec![],
            changes: None,
            stage: None,
        }
    }

    fn ws_with_changes(changes: butai_protocol::api::ChangesDto) -> WorkspaceDetail {
        use butai_protocol::SessionId;
        WorkspaceDetail {
            id: SessionId(1),
            name: "proj".into(),
            cwd: "/tmp/proj".into(),
            agents: vec![],
            processes: vec![],
            stage: None,
            changes: Some(changes),
        }
    }

    fn rail_changes(ahead: usize) -> butai_protocol::api::ChangesDto {
        use butai_protocol::api::{ConflictFile, FileChange, RepoState};
        let f = |path: &str, code: &str| FileChange {
            path: path.into(),
            code: code.into(),
            added: 1,
            deleted: 0,
        };
        butai_protocol::api::ChangesDto {
            branch: "main".into(),
            staged: vec![f("s.rs", "M")],
            unstaged: vec![f("u.rs", "M")],
            recent_commits: vec![],
            conflicted: vec![ConflictFile {
                path: "c.rs".into(),
                base: true,
                ours: true,
                theirs: true,
            }],
            upstream: None,
            ahead,
            behind: 0,
            state: RepoState::Clean,
            detached: false,
        }
    }

    /// Which verb each kind of row offers.
    ///
    /// The daemon's table is per-row-kind for a reason: `s` on something already
    /// staged and `x` on something that only exists in the index are both
    /// no-ops, and a key that silently does nothing is worse than no key. The
    /// rows come out of `change_rows`, so this also catches the drawn list and
    /// the acted-on list drifting apart.
    #[test]
    fn each_kind_of_changes_row_offers_only_its_own_verbs() {
        let ws = ws_with_changes(rail_changes(0));
        // Rows: 0 Conflicted heading, 1 c.rs, 2 Unstaged heading, 3 u.rs,
        //       4 Staged heading, 5 s.rs.
        let at = |sel: usize, code: char| {
            let mut view = View { changes_sel: sel, ..Default::default() };
            handle_changes_key(key(event::KeyCode::Char(code)), &mut view, Some(&ws))
        };

        assert!(matches!(at(3, 's'), Some(Flow::Git(GitAction::Stage(p))) if p == "u.rs"));
        assert!(at(3, 'u').is_none(), "an unstaged file has nothing to unstage");
        assert!(matches!(at(5, 'u'), Some(Flow::Git(GitAction::Unstage(p))) if p == "s.rs"));
        assert!(at(5, 's').is_none(), "a staged file is already staged");
        assert!(at(5, 'x').is_none(), "discard is a worktree verb, not an index one");

        // A conflicted file offers the three ways out and nothing that stages.
        use butai_protocol::api::ResolveSide;
        assert!(matches!(
            at(1, 'o'),
            Some(Flow::Git(GitAction::Resolve { take: ResolveSide::Ours, .. }))
        ));
        assert!(matches!(
            at(1, 't'),
            Some(Flow::Git(GitAction::Resolve { take: ResolveSide::Theirs, .. }))
        ));
        assert!(at(1, 's').is_none(), "staging a conflict would commit the markers");

        // Headings are not files.
        assert!(at(0, 's').is_none());
        assert!(at(2, 'x').is_none());
        assert!(at(9, 's').is_none(), "past the end of the rail");
    }

    /// Push is offered only when there is something to push, the way the footer
    /// offers it.
    #[test]
    fn push_is_bound_only_when_the_branch_is_ahead() {
        let mut view = View { changes_sel: 3, ..Default::default() };
        let behind = ws_with_changes(rail_changes(0));
        assert!(
            handle_changes_key(key(event::KeyCode::Char('p')), &mut view, Some(&behind)).is_none()
        );
        let ahead = ws_with_changes(rail_changes(2));
        assert!(matches!(
            handle_changes_key(key(event::KeyCode::Char('p')), &mut view, Some(&ahead)),
            Some(Flow::Git(GitAction::Push))
        ));
    }

    /// Discarding asks first, and the box it opens is answered "no".
    #[test]
    fn discard_asks_before_it_throws_work_away() {
        let ws = ws_with_changes(rail_changes(0));
        let mut view = View { changes_sel: 3, ..Default::default() };
        let flow = handle_changes_key(key(event::KeyCode::Char('x')), &mut view, Some(&ws));
        assert!(matches!(flow, Some(Flow::Continue)), "x must not discard on its own");
        let Some(Overlay::Confirm(c)) = &view.overlay else { panic!("no confirm box") };
        assert!(!c.yes);
        assert!(c.header.contains("u.rs"), "{}", c.header);

        // Answering no closes it and does nothing.
        let flow = handle_overlay_key(key(event::KeyCode::Char('n')), &mut view);
        assert!(matches!(flow, Flow::Continue));
        assert!(view.overlay.is_none());

        // So does pressing Enter on the "no" the box opened with — the path
        // someone takes by reflex, and the one that must not discard.
        handle_changes_key(key(event::KeyCode::Char('x')), &mut view, Some(&ws));
        let flow = handle_overlay_key(key(event::KeyCode::Enter), &mut view);
        assert!(matches!(flow, Flow::Continue), "enter on 'no' must not discard: {flow:?}");
        assert!(view.overlay.is_none(), "but it should still have closed the box");

        // Answering yes is what discards.
        handle_changes_key(key(event::KeyCode::Char('x')), &mut view, Some(&ws));
        let flow = handle_overlay_key(key(event::KeyCode::Char('y')), &mut view);
        assert!(matches!(&flow, Flow::Git(GitAction::Discard(p)) if p == "u.rs"), "{flow:?}");
        assert!(view.overlay.is_none(), "the box should have closed");
    }

    /// The update prompt's two answers mean different things from every other
    /// confirm box's, and the difference is the whole feature: `no` is about
    /// *that version* and has to be remembered, `esc` is "not now".
    #[test]
    fn declining_an_update_is_an_answer_and_not_a_dismissal() {
        // As every confirm box opens: on "no", so the update is something you
        // agree to rather than something you land on.
        let mut view = View { overlay: Some(update_overlay("1.1.0")), ..Default::default() };
        let Some(Overlay::Confirm(c)) = &view.overlay else { panic!("no confirm box") };
        assert!(!c.yes);
        assert!(c.header.contains("1.1.0"), "{}", c.header);
        assert!(c.header.contains(crate::update::CURRENT), "{}", c.header);

        // `n` carries the version out, so the loop can write it down. This is
        // what the other kinds do *not* do — theirs is a bare `Continue`.
        let flow = handle_overlay_key(key(event::KeyCode::Char('n')), &mut view);
        assert!(matches!(&flow, Flow::DeclineUpdate(v) if v == "1.1.0"), "{flow:?}");
        assert!(view.overlay.is_none());

        // Enter on the preselected "no" is the same answer by a different key.
        view.overlay = Some(update_overlay("1.1.0"));
        let flow = handle_overlay_key(key(event::KeyCode::Enter), &mut view);
        assert!(matches!(&flow, Flow::DeclineUpdate(v) if v == "1.1.0"), "{flow:?}");

        // `esc` is not an answer. Nothing is recorded, so the next launch asks
        // again — which is the only way back to the question without `:update`.
        view.overlay = Some(update_overlay("1.1.0"));
        let flow = handle_overlay_key(key(event::KeyCode::Esc), &mut view);
        assert!(matches!(flow, Flow::Continue), "esc must record nothing: {flow:?}");
        assert!(view.overlay.is_none());

        // `q` dismisses like `esc`, for the same reason.
        view.overlay = Some(update_overlay("1.1.0"));
        let flow = handle_overlay_key(key(event::KeyCode::Char('q')), &mut view);
        assert!(matches!(flow, Flow::Continue), "{flow:?}");

        // And yes is what updates.
        view.overlay = Some(update_overlay("1.1.0"));
        let flow = handle_overlay_key(key(event::KeyCode::Char('y')), &mut view);
        assert!(matches!(flow, Flow::Update), "{flow:?}");
        assert!(view.overlay.is_none());
    }

    /// Routing `n` through [`confirm`] changed the answering path for every
    /// confirm box, not just the new one. The others must still do nothing on
    /// no — this is the regression that change could cause.
    #[test]
    fn every_other_confirm_still_does_nothing_when_answered_no() {
        for kind in [
            chrome::ConfirmKind::Discard { path: "u.rs".into() },
            chrome::ConfirmKind::DeleteFile { path: "u.rs".into() },
            chrome::ConfirmKind::CloseWorkspace {
                id: butai_protocol::SessionId(1),
                name: "proj".into(),
            },
            chrome::ConfirmKind::MenuAction,
            chrome::ConfirmKind::Pick {
                target: chrome::PickTarget::DeleteBranch,
                value: "main".into(),
                label: "main".into(),
            },
        ] {
            let mut view = View {
                overlay: Some(Overlay::Confirm(chrome::ConfirmOverlay {
                    title: "X".into(),
                    header: "x".into(),
                    yes: false,
                    kind,
                })),
                ..Default::default()
            };
            let flow = handle_overlay_key(key(event::KeyCode::Char('n')), &mut view);
            assert!(matches!(flow, Flow::Continue), "no must stay inert: {flow:?}");
            assert!(view.overlay.is_none(), "and must still close the box");
        }
    }

    /// The box is drawn with "no" on row 2 and "yes" on row 3, and the pointer
    /// has to mean what the keyboard means — including carrying the declined
    /// version out, which is the part `view.overlay = None` used to swallow.
    #[test]
    fn clicking_no_on_an_update_declines_it() {
        const COLS: u16 = 80;
        const ROWS: u16 = 24;

        // Where the box actually lands is [`chrome::overlay_layout`]'s business,
        // so ask it rather than hard-coding a row that centring would move.
        let find_row = |want: usize| -> (u16, u16) {
            let overlay = update_overlay("1.1.0");
            for y in 0..ROWS {
                for x in 0..COLS {
                    if chrome::overlay_hit(COLS, ROWS, &overlay, x, y) == Some(want) {
                        return (x, y);
                    }
                }
            }
            panic!("row {want} of the update box was never drawn");
        };
        let click_at = |x, y| event::MouseEvent {
            kind: event::MouseEventKind::Down(event::MouseButton::Left),
            column: x,
            row: y,
            modifiers: event::KeyModifiers::NONE,
        };

        // Row 2 is `no`, and it declines rather than merely closing.
        let mut view = View { overlay: Some(update_overlay("1.1.0")), ..Default::default() };
        let (x, y) = find_row(2);
        let flow = overlay_mouse(&click_at(x, y), &mut view, COLS, ROWS);
        assert!(matches!(&flow, Flow::DeclineUpdate(v) if v == "1.1.0"), "{flow:?}");
        assert!(view.overlay.is_none());

        // Row 3 is `yes`.
        view.overlay = Some(update_overlay("1.1.0"));
        let (x, y) = find_row(3);
        let flow = overlay_mouse(&click_at(x, y), &mut view, COLS, ROWS);
        assert!(matches!(flow, Flow::Update), "{flow:?}");
        assert!(view.overlay.is_none());
    }

    /// `:update` is the way back to a prompt that was dismissed or turned down.
    #[test]
    fn the_update_verb_parses_and_dispatches() {
        assert_eq!(
            crate::keymap::parse_action("update"),
            Ok(crate::keymap::Action::View(ViewVerb::Update))
        );
        let mut view = View::default();
        assert!(matches!(run_view(ViewVerb::Update, &mut view), Flow::CheckUpdate));
    }

    /// A commit message is typed, and an empty one is refused before it reaches
    /// git — where it would open an editor nobody here can see.
    #[test]
    fn a_commit_needs_a_message() {
        let ws = ws_with_changes(rail_changes(0));
        let mut view = View { changes_sel: 5, ..Default::default() };
        handle_changes_key(key(event::KeyCode::Char('c')), &mut view, Some(&ws));
        assert!(matches!(view.overlay, Some(Overlay::Prompt(_))), "no prompt opened");

        let flow = handle_overlay_key(key(event::KeyCode::Enter), &mut view);
        assert!(matches!(flow, Flow::Continue), "an empty message must not commit");
        assert!(view.flash.as_deref().is_some_and(|f| f.contains("message")), "{:?}", view.flash);

        handle_changes_key(key(event::KeyCode::Char('c')), &mut view, Some(&ws));
        for c in "fix it".chars() {
            handle_overlay_key(key(event::KeyCode::Char(c)), &mut view);
        }
        let flow = handle_overlay_key(key(event::KeyCode::Enter), &mut view);
        assert!(
            matches!(&flow, Flow::Git(GitAction::Commit { message, all: false }) if message == "fix it"),
            "{flow:?}"
        );
    }

    /// Tab does not park the keyboard on a COMMIT box with no commit in it.
    ///
    /// It did, and the page then answered no key at all: `handle_git_key`
    /// returns `None` for everything once `Focus::Stage` has no body to scroll,
    /// and every one of those keys was forwarded to a pane the GIT page does
    /// not draw. One Tab was the whole reproduction — the page looked frozen
    /// while `enter`, `r` and `g` ran as commands in a shell nobody could see.
    #[test]
    fn tab_skips_the_commit_box_until_a_commit_is_in_it() {
        let mut view = View { page: Page::Git, focus: Focus::History, ..View::default() };
        let mut git = chrome::Git::default();

        handle_git_key(key(event::KeyCode::Tab), &mut view, &mut git, None, 10);
        assert_eq!(view.focus, Focus::Refs, "an empty body is not a column to walk into");
        handle_git_key(key(event::KeyCode::Tab), &mut view, &mut git, None, 10);
        assert_eq!(view.focus, Focus::History, "the two lists cycle between themselves");

        // With a commit open the body is a real third column and keeps its turn.
        git.body = Some(chrome::DiffView::new(
            chrome::DiffKind::Commit { id: "abc1234".into(), summary: "a commit".into() },
            "diff --git a/a b/a\n@@ -1 +1 @@\n-old\n+new\n",
        ));
        handle_git_key(key(event::KeyCode::Tab), &mut view, &mut git, None, 10);
        assert_eq!(view.focus, Focus::Stage, "an open commit is the third column");
    }

    /// Every letter the CHANGES rail's footer advertises actually does
    /// something.
    ///
    /// `d diff` did not, for as long as the verb table has existed: it was in
    /// the table, drawn in the footer, printed by `?`, and clicking the word ran
    /// it through `handle_changes_key` — which had no arm for it. Enter opened
    /// the diff and only Enter did. The table exists to make exactly this
    /// impossible, so the check belongs beside it rather than in a changelog.
    #[test]
    fn every_letter_the_changes_rail_advertises_is_bound() {
        use crate::verbs::ChangesRow;
        use butai_protocol::api::{ChangesDto, ConflictFile, FileChange, RepoState};
        let changes = ChangesDto {
            branch: "main".into(),
            staged: vec![FileChange {
                path: "staged.rs".into(),
                code: "M".into(),
                added: 1,
                deleted: 0,
            }],
            unstaged: vec![FileChange {
                path: "dirty.rs".into(),
                code: "M".into(),
                added: 2,
                deleted: 1,
            }],
            recent_commits: vec![butai_protocol::api::CommitDto {
                id: "abc1234".into(),
                summary: "a commit".into(),
            }],
            conflicted: vec![ConflictFile {
                path: "clash.rs".into(),
                base: true,
                ours: true,
                theirs: true,
            }],
            upstream: None,
            ahead: 1,
            behind: 0,
            state: RepoState::Clean,
            detached: false,
        };
        let ws = WorkspaceDetail {
            id: butai_protocol::SessionId(1),
            name: "proj".into(),
            cwd: "/tmp/proj".into(),
            agents: vec![],
            processes: vec![],
            changes: Some(changes.clone()),
            stage: None,
        };
        let rows = chrome::change_rows(&changes);
        // Walk every row, and for each one every verb its footer would draw.
        for (sel, row) in rows.iter().enumerate() {
            let kind = chrome::changes_row_kind(&changes, sel);
            if kind == ChangesRow::None {
                continue;
            }
            for verb in crate::verbs::changes_row_verbs(kind) {
                let mut view = View { focus: Focus::Changes, changes_sel: sel, ..View::default() };
                let flow =
                    handle_changes_key(key(event::KeyCode::Char(verb.key)), &mut view, Some(&ws));
                // Either it produced a flow, or it put an overlay up. Doing
                // neither is a footer word that lies.
                assert!(
                    flow.is_some() || view.overlay.is_some(),
                    "`{}` ({}) is drawn on a {row:?} row and bound to nothing",
                    verb.key,
                    verb.label,
                );
            }
        }
    }

    /// A working-tree diff in the GIT page's body stages from the body, with the
    /// keys the DIFF page uses — and a commit's diff answers none of them,
    /// because history has no index side to move anything to.
    #[test]
    fn the_git_pages_body_stages_a_hunk_but_never_a_commits() {
        const PATCH: &str = "diff --git a/a.txt b/a.txt\nindex 1..2 100644\n--- a/a.txt\n\
                             +++ b/a.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+TWO\n";
        let mut view = View { page: Page::Git, focus: Focus::Stage, ..View::default() };
        let mut git = chrome::Git {
            body: Some(chrome::DiffView::new(
                chrome::DiffKind::Unstaged { path: Some("a.txt".into()) },
                PATCH,
            )),
            ..Default::default()
        };

        let flow = handle_git_key(key(event::KeyCode::Char(' ')), &mut view, &mut git, None, 10);
        assert!(matches!(flow, Some(Flow::ApplyDiff { discard: false })), "{flow:?}");
        let flow = handle_git_key(key(event::KeyCode::Char('x')), &mut view, &mut git, None, 10);
        assert!(matches!(flow, Some(Flow::ApplyDiff { discard: true })), "{flow:?}");

        // `v` drops into line-select, where space picks rather than applies and
        // enter is what sends the picked run.
        handle_git_key(key(event::KeyCode::Char('v')), &mut view, &mut git, None, 10);
        assert_eq!(git.body.as_ref().unwrap().mode, chrome::DiffMode::Lines);
        let flow = handle_git_key(key(event::KeyCode::Char(' ')), &mut view, &mut git, None, 10);
        assert!(matches!(flow, Some(Flow::Continue)), "space should pick, not apply: {flow:?}");
        let flow = handle_git_key(key(event::KeyCode::Enter), &mut view, &mut git, None, 10);
        assert!(matches!(flow, Some(Flow::ApplyDiff { discard: false })), "{flow:?}");
        // And Esc leaves the mode before it leaves the diff, so a picked run is
        // never thrown away by the press meant to cancel it.
        handle_git_key(key(event::KeyCode::Esc), &mut view, &mut git, None, 10);
        assert_eq!(git.body.as_ref().unwrap().mode, chrome::DiffMode::Read);
        assert!(git.body.is_some(), "the first Esc closed the diff as well as the mode");

        // A commit is history. None of the four keys may reach `git/apply`.
        git.body = Some(chrome::DiffView::new(
            chrome::DiffKind::Commit { id: "abc1234".into(), summary: "a commit".into() },
            PATCH,
        ));
        for k in [' ', 'x', 'v'] {
            let flow = handle_git_key(key(event::KeyCode::Char(k)), &mut view, &mut git, None, 10);
            assert!(
                !matches!(flow, Some(Flow::ApplyDiff { .. })),
                "{k:?} tried to stage a commit: {flow:?}"
            );
        }
    }

    /// `enter` on a changed file opens it in the body beside the lists, not on
    /// the DIFF page — which would throw away the refs and history you were
    /// reading it from — and the side of the index it is on decides which diff.
    #[test]
    fn enter_on_a_changed_file_opens_it_beside_the_lists() {
        use butai_protocol::api::{ChangesDto, FileChange, RepoState};
        let changes = ChangesDto {
            branch: "main".into(),
            staged: vec![FileChange {
                path: "staged.rs".into(),
                code: "M".into(),
                added: 1,
                deleted: 0,
            }],
            unstaged: vec![FileChange {
                path: "dirty.rs".into(),
                code: "M".into(),
                added: 2,
                deleted: 1,
            }],
            recent_commits: vec![],
            conflicted: vec![],
            upstream: None,
            ahead: 0,
            behind: 0,
            state: RepoState::Clean,
            detached: false,
        };
        let git = chrome::Git::default();
        let rows = chrome::ref_rows(&git, Some(&changes), None);
        let mut view = View { page: Page::Git, focus: Focus::Refs, ..View::default() };

        // Row 0 is the summary, 1 the `Unstaged` heading, 2 the dirty file,
        // 3 the `Staged` heading, 4 the staged one.
        let open = |sel: usize, view: &mut View| {
            let g = chrome::Git { refs_sel: sel, ..Default::default() };
            let kind = chrome::ref_row_kind(&rows, sel);
            let id = crate::verbs::git_row_verbs(kind).iter().find(|v| v.key == '\n')?.id;
            git_verb_flow(id, view, &g, &rows)
        };
        assert!(
            matches!(
                open(0, &mut view),
                Some(Flow::GitOpenDiff { kind: DiffKind::Unstaged { path: None }, .. })
            ),
            "the summary row diffs the whole worktree"
        );
        assert!(
            matches!(open(2, &mut view), Some(Flow::GitOpenDiff { kind: DiffKind::Unstaged { path: Some(p) }, .. }) if p == "dirty.rs"),
            "an unstaged row diffs the worktree against the index"
        );
        assert!(
            matches!(open(4, &mut view), Some(Flow::GitOpenDiff { kind: DiffKind::Staged { path: Some(p) }, .. }) if p == "staged.rs"),
            "a staged row diffs the index against HEAD"
        );
    }

    /// The rail's letters do the rail's thing here. `s` on a staged row and `u`
    /// on an unstaged one are no-ops rather than requests the daemon will
    /// refuse — the footer does not offer them, so neither may the keyboard.
    #[test]
    fn the_file_rows_stage_and_unstage_with_the_rails_own_keys() {
        use butai_protocol::api::{ChangesDto, FileChange, RepoState};
        let changes = ChangesDto {
            branch: "main".into(),
            staged: vec![FileChange {
                path: "staged.rs".into(),
                code: "M".into(),
                added: 1,
                deleted: 0,
            }],
            unstaged: vec![FileChange {
                path: "dirty.rs".into(),
                code: "M".into(),
                added: 2,
                deleted: 1,
            }],
            recent_commits: vec![],
            conflicted: vec![],
            upstream: None,
            ahead: 0,
            behind: 0,
            state: RepoState::Clean,
            detached: false,
        };
        let ws = WorkspaceDetail {
            id: butai_protocol::SessionId(1),
            name: "proj".into(),
            cwd: "/tmp/proj".into(),
            agents: vec![],
            processes: vec![],
            changes: Some(changes),
            stage: None,
        };
        let press = |sel: usize, c: char| {
            let mut view = View { page: Page::Git, focus: Focus::Refs, ..View::default() };
            let mut git = chrome::Git { refs_sel: sel, ..Default::default() };
            handle_git_key(key(event::KeyCode::Char(c)), &mut view, &mut git, Some(&ws), 10)
        };
        assert!(
            matches!(press(2, 's'), Some(Flow::Git(GitAction::Stage(p))) if p == "dirty.rs"),
            "s on an unstaged row should stage it"
        );
        assert!(
            matches!(press(4, 'u'), Some(Flow::Git(GitAction::Unstage(p))) if p == "staged.rs"),
            "u on a staged row should unstage it"
        );
        assert!(press(4, 's').is_none(), "staging what is already staged is not a thing to do");
        assert!(press(2, 'u').is_none(), "there is nothing to unstage on the worktree side");
        // Discard asks first: it throws away the only copy of an edit.
        let mut view = View { page: Page::Git, focus: Focus::Refs, ..View::default() };
        let mut git = chrome::Git { refs_sel: 2, ..Default::default() };
        handle_git_key(key(event::KeyCode::Char('x')), &mut view, &mut git, Some(&ws), 10);
        assert!(
            matches!(&view.overlay, Some(Overlay::Confirm(c)) if !c.yes && c.header.contains("dirty.rs")),
            "{:?}",
            view.overlay
        );
    }

    /// A page that replaced the stage is not a page you can type into.
    ///
    /// The two predicates have to agree: a page that drew over the stage and
    /// put nothing terminal back has no pane left to forward a keystroke to.
    /// Reaching one anyway is what typed the GIT page's own verbs into a shell.
    ///
    /// BOOTH is the one page that took the band *and* gave a column back, so it
    /// is checked the other way round — against `stage_rect`, which is the
    /// measurement the daemon sizes the pane to. A page with a pane that size
    /// and no way to type into it is the bug this pairing exists to catch.
    #[test]
    fn a_page_that_covers_the_stage_has_no_pane_to_type_into() {
        // DOCKER is in the list although it does stream a pane: it is `docker
        // logs -f`, which reads and never listens, so there is nothing there a
        // keystroke could mean.
        for page in [Page::Files, Page::Docs, Page::Docker, Page::Git, Page::Settings, Page::Diff] {
            assert!(!page.draws_stage(), "{page:?} draws over the stage but claims to own a pane");
        }
        assert!(Page::Agents.draws_stage(), "WORK is the page the stage is on");

        // BOOTH's middle column is a live pane, so the keyboard has to be able
        // to reach it — and the pane is narrower than the band, which is how
        // you can tell the page gave a column back rather than kept the stage.
        assert!(Page::Booth.draws_stage(), "BOOTH's middle column is a pane you can type into");
        let booth = View { page: Page::Booth, ..Default::default() };
        let rect = chrome::stage_rect(160, 48, &booth);
        assert!(rect.width > 0 && rect.height > 0, "BOOTH claims a pane with no room to draw it");
        assert!(rect.width < 160 - 2, "BOOTH's stage is the middle column, not the whole band");
    }

    /// The wheel on BOOTH lands in the column that is drawn under the pointer.
    ///
    /// Reported as "I can't scroll the agent preview". The routing was reading
    /// its three columns off `Chrome::compute` — the page-agnostic rectangles,
    /// where the stage is still the narrow strip between two rails BOOTH does
    /// not draw — while everything that draws or clicks reads `page_geom`, the
    /// widened band. So the fleet and compute boxes the wheel tested against
    /// were offset into the middle of the *drawn* preview, and all three
    /// columns took the wheel meant for a neighbour: over the left of the
    /// preview it moved the fleet cursor, which on BOOTH swaps the very pane you
    /// were trying to read, and over the real fleet list it scrolled the pane
    /// instead of the list.
    #[test]
    fn booths_wheel_goes_to_the_column_under_the_pointer() {
        // Wide enough that all three columns are drawn; the bug needs the rails
        // `Chrome::compute` reserves to be affordable, which is the ordinary case.
        let (cols, rows) = (180u16, 40u16);
        let probe = View { page: Page::Booth, ..Default::default() };
        let geom = chrome::page_geom(cols, rows, &probe);
        let c = chrome::booth_columns(chrome::booth_area(cols, &geom));
        assert!(
            c.fleet_box.width > 0 && c.compute_box.width > 0,
            "BOOTH drew one column, not three"
        );

        // True when the notch reached the pane. Which is now the *pane's own*
        // wheel event rather than a `ScrollPage` command — see
        // [`the_wheel_over_a_pane_goes_to_the_pane`] — so the answer is what
        // arrived on the stage connection, not what came back as a `Flow`.
        let wheel = |x: u16, y: u16| {
            let (mut cols, mut rows) = (cols, rows);
            let (stage, mut sent) = fake_stage();
            let mut view = View { page: Page::Booth, focus: Focus::Stage, ..Default::default() };
            handle_input(
                event::Event::Mouse(event::MouseEvent {
                    kind: event::MouseEventKind::ScrollUp,
                    column: x,
                    row: y,
                    modifiers: event::KeyModifiers::NONE,
                }),
                &mut view,
                &[],
                &[],
                Some(&stage),
                &mut Files::default(),
                &mut Files::default(),
                &mut DiffView::default(),
                &mut Docker::default(),
                &mut chrome::Git::default(),
                &mut chrome::Settings::default(),
                &mut chrome::Help::default(),
                &mut chrome::usage::Usage::default(),
                &Keymap::default(),
                &mut Drag::default(),
                false,
                false,
                &mut cols,
                &mut rows,
            );
            matches!(sent.try_recv(), Ok(ClientMsg::Input(InputEvent::ScrollUp { .. })))
        };

        // Every column of the preview is the pane's — the whole width of it,
        // not the middle third that happened to fall outside the misplaced
        // boxes.
        let inner = c.stage_inner;
        for x in inner.x..inner.right() {
            let y = inner.y + inner.height / 2;
            assert!(wheel(x, y), "column {x} of the preview did not scroll the pane");
        }
        // And the columns beside it are not: their own lists take the wheel, so
        // a scroll there must not reach the pane.
        let y = c.fleet_rows.y + 1;
        assert!(!wheel(c.fleet_box.x + 1, y), "the fleet list gave its wheel to the pane");
        assert!(!wheel(c.compute_box.x + 1, y), "the compute column gave its wheel to the pane");
    }

    /// The wheel over a pane is the pane's, and travels as a mouse event.
    ///
    /// Reported as "scrolling the stage does nothing". It did nothing over
    /// exactly the panes people spend the day in. The wheel was the one mouse
    /// gesture the TUI did not forward: presses and drags went to the pane as
    /// `InputEvent`s, but a notch became `Command::ScrollPage`, which only ever
    /// moves the daemon's scrollback. Two consequences, and both are the
    /// complaint:
    ///
    ///   * a program that asked for mouse reporting — `claude`, `less --mouse`,
    ///     `vim` — never saw the notch, so it could not scroll itself;
    ///   * a program on the alternate screen has no scrollback for the fallback
    ///     to move, so nothing happened there either.
    ///
    /// Every other client already had this right: the browser's `<butai-screen>`
    /// sends `scroll_up`/`scroll_down` and the daemon's `pane_wheel` decides
    /// between the program and the scrollback. Deciding it here would be the
    /// TUI-shaped side channel the boundary refactor exists to delete — and it
    /// cannot be decided here anyway, because whether the program wants the
    /// mouse is known only where its output is parsed.
    ///
    /// Both halves are asserted. The pane must get the notch, *and* the answer
    /// must not still be `Flow::Scroll` — a build that did both would send the
    /// wheel twice and scroll two different things with one gesture.
    #[test]
    fn the_wheel_over_a_pane_goes_to_the_pane() {
        let (cols, rows) = (120u16, 40u16);
        let probe = View { focus: Focus::Stage, ..Default::default() };
        let inner = chrome::page_geom(cols, rows, &probe).stage_inner;
        let (x, y) = (inner.x + inner.width / 2, inner.y + inner.height / 2);

        let (mut cols, mut rows) = (cols, rows);
        let (stage, mut sent) = fake_stage();
        let mut view = View { focus: Focus::Stage, ..Default::default() };
        let flow = handle_input(
            event::Event::Mouse(event::MouseEvent {
                kind: event::MouseEventKind::ScrollDown,
                column: x,
                row: y,
                modifiers: event::KeyModifiers::NONE,
            }),
            &mut view,
            &[],
            &[],
            Some(&stage),
            &mut Files::default(),
            &mut Files::default(),
            &mut DiffView::default(),
            &mut Docker::default(),
            &mut chrome::Git::default(),
            &mut chrome::Settings::default(),
            &mut chrome::Help::default(),
            &mut chrome::usage::Usage::default(),
            &Keymap::default(),
            &mut Drag::default(),
            false,
            false,
            &mut cols,
            &mut rows,
        );
        // Pane-local, like the press and the drag beside it: the daemon has no
        // chrome to subtract, so a screen coordinate would land the notch a rail
        // to the right of where it was aimed.
        match sent.try_recv() {
            Ok(ClientMsg::Input(InputEvent::ScrollDown { x: px, y: py })) => {
                assert_eq!((px, py), (x - inner.x, y - inner.y), "the notch arrived off-pane");
            }
            other => panic!("the wheel over the pane sent {other:?}, not the pane's own scroll"),
        }
        assert!(
            !matches!(flow, Flow::Scroll(_)),
            "the wheel both forwarded and scrolled: one gesture, two scrolls"
        );
    }

    /// The GIT page does not eat the clicks that leave it.
    ///
    /// Reported as the whole client freezing on arrival at GIT: nothing could
    /// be clicked any more. Nothing was frozen — the page claimed the entire
    /// screen for its three columns and answered `Flow::Continue` to every
    /// press that missed them, which is every control that is not the page.
    /// The spaces button was among them, and it is the way out, so the way out
    /// was the thing that stopped responding.
    #[test]
    fn the_git_page_leaves_the_spaces_button_clickable() {
        let (mut cols, mut rows) = (180u16, 40u16);
        let probe = View { page: Page::Git, ..Default::default() };
        let geom = chrome::page_geom(cols, rows, &probe);
        let (bx, _) = chrome::spaces_button_span(&geom.tabbar, &probe, 1)
            .expect("the bar carries it at 180 columns");

        let mut view = View { page: Page::Git, ..Default::default() };
        let flow = handle_input(
            event::Event::Mouse(event::MouseEvent {
                kind: event::MouseEventKind::Down(event::MouseButton::Left),
                column: bx,
                row: geom.tabbar.y,
                modifiers: event::KeyModifiers::NONE,
            }),
            &mut view,
            &[],
            &[],
            None,
            &mut Files::default(),
            &mut Files::default(),
            &mut DiffView::default(),
            &mut Docker::default(),
            &mut chrome::Git::default(),
            &mut chrome::Settings::default(),
            &mut chrome::Help::default(),
            &mut chrome::usage::Usage::default(),
            &Keymap::default(),
            &mut Drag::default(),
            false,
            false,
            &mut cols,
            &mut rows,
        );
        // The flow asks the loop to build the menu, which is where the badges
        // are; what this pins is that the press was *heard* rather than eaten.
        assert!(
            matches!(flow, Flow::PickSpace),
            "a click on the spaces button must reach the menu from GIT, got {flow:?}"
        );
    }

    /// Every row of the git menu either goes somewhere or does something.
    ///
    /// The table is shared with the daemon, so a row added there shows up here
    /// automatically — and would show up as a row that silently does nothing if
    /// nobody wired it. This is the assertion that turns that into a failure.
    #[test]
    fn every_git_menu_row_leads_somewhere() {
        use crate::git_menu::{GitAction as A, ITEMS};
        // The rows that open a picker or a prompt instead of calling a route.
        const INTERACTIVE: &[A] = &[
            A::Checkout,
            A::NewBranch,
            A::DeleteBranch,
            A::StashList,
            A::StashDrop,
            A::TagCreate,
            A::TagDelete,
            A::RemoteRemove,
            A::WorktreeList,
            A::WorktreeAdd,
            A::WorktreeRemove,
            A::Merge,
            A::Rebase,
        ];
        let mut unwired = Vec::new();
        for item in ITEMS {
            if INTERACTIVE.contains(&item.action) {
                continue;
            }
            if menu_request(item.action).is_none() {
                unwired.push(item.label);
            }
        }
        assert!(unwired.is_empty(), "menu rows with no route: {unwired:?}");

        // And the interactive ones are exactly the labels that say so, so the
        // two lists cannot drift apart without the ellipsis drifting too.
        for item in ITEMS {
            let promises_more = item.label.ends_with('…');
            let is_interactive = INTERACTIVE.contains(&item.action);
            assert_eq!(
                promises_more,
                is_interactive,
                "{:?} says {:?} but is {}",
                item.action,
                item.label,
                if is_interactive { "interactive" } else { "direct" }
            );
        }
    }

    /// A row's value is what git is told; its label is for the reader. Mixing
    /// them sends `* main` to `checkout` or a whole `stash@{0}: …` line to
    /// `stash drop`.
    #[test]
    fn a_rows_value_is_what_acts_and_its_label_is_what_reads() {
        let list = ListOverlay {
            title: "CHECK OUT".into(),
            items: vec!["* main".into(), "  spike".into()],
            values: Some(vec!["main".into(), "spike".into()]),
            sel: 0,
            kind: ListKind::Branch,
        };
        assert_eq!(list.chosen(), Some("main"), "the marker must not reach git");
        assert_eq!(list.chosen_label(), Some("* main"), "but the reader keeps it");

        // Without values, a row means what it says.
        let plain = ListOverlay {
            title: "SPAWN AGENT".into(),
            items: vec!["claude".into()],
            values: None,
            sel: 0,
            kind: ListKind::SpawnAgent,
        };
        assert_eq!(plain.chosen(), Some("claude"));
        assert_eq!(plain.chosen_label(), Some("claude"));
    }

    /// A settings page with two themes and two agents to choose between.
    fn settings_state() -> chrome::Settings {
        chrome::Settings {
            themes: vec!["blueprint-dark".into(), "tokyonight".into(), "terminal".into()],
            agents: vec!["claude".into(), "codex".into()],
            saved_theme: "blueprint-dark".into(),
            loaded: true,
            ret: Page::Files,
            ..Default::default()
        }
    }

    /// Walking the theme list repaints the workbench, and leaving it puts the
    /// file's own palette back.
    ///
    /// This is the whole reason SETTINGS is a page rather than a modal: the
    /// preview is the entire screen. It is also the part with somewhere to
    /// leak — the palette is derived from the cursor, so every way out of the
    /// list has to lead back to the saved one.
    #[test]
    fn walking_the_theme_list_previews_and_leaving_it_restores() {
        let mut view = View { page: Page::Settings, ..Default::default() };
        let mut st = settings_state();

        // Closed, the screen wears what the file names.
        assert_eq!(settings_palette(&st, &view), "blueprint-dark");

        // Enter opens the list on the current value rather than at the top.
        let flow = handle_settings_key(key(event::KeyCode::Enter), &mut view, &mut st, 150, 40);
        assert!(matches!(flow, Some(Flow::SettingsEdit(_))), "{flow:?}");
        assert_eq!(st.open, Some(0), "the list opens where the value already is");
        assert_eq!(settings_palette(&st, &view), "blueprint-dark", "and previews it unchanged");

        // Moving down previews the row under the cursor, without writing.
        handle_settings_key(key(event::KeyCode::Char('j')), &mut view, &mut st, 150, 40);
        assert_eq!(settings_palette(&st, &view), "tokyonight", "the screen follows the cursor");
        assert_eq!(st.saved_theme, "blueprint-dark", "but nothing has been chosen yet");

        // Esc abandons the preview and stays on the page.
        let flow = handle_settings_key(key(event::KeyCode::Esc), &mut view, &mut st, 150, 40);
        assert!(matches!(flow, Some(Flow::SettingsEdit(_))), "{flow:?}");
        assert_eq!(st.open, None);
        assert_eq!(view.page, Page::Settings, "esc closed the list, not the page");
        assert_eq!(settings_palette(&st, &view), "blueprint-dark", "the old palette is back");

        // A second esc leaves, for the page it was opened from.
        handle_settings_key(key(event::KeyCode::Esc), &mut view, &mut st, 150, 40);
        assert_eq!(view.page, Page::Files, "and it remembers where you came from");
    }

    /// Enter on a highlighted theme asks for it to be kept — the one path that
    /// writes `[theme] name`.
    #[test]
    fn enter_on_a_theme_is_what_writes_it() {
        use chrome::settings::Edit;
        let mut view = View { page: Page::Settings, ..Default::default() };
        let mut st = settings_state();

        handle_settings_key(key(event::KeyCode::Enter), &mut view, &mut st, 150, 40);
        handle_settings_key(key(event::KeyCode::Char('j')), &mut view, &mut st, 150, 40);
        let flow = handle_settings_key(key(event::KeyCode::Enter), &mut view, &mut st, 150, 40);
        assert!(
            matches!(&flow, Some(Flow::SettingsEdit(Edit::Theme(n))) if n == "tokyonight"),
            "{flow:?}"
        );
        assert_eq!(st.open, None, "and the list closes behind it");
    }

    /// The default agent is set from the page, and "ask every time" clears the
    /// pin rather than pinning an agent by that name.
    #[test]
    fn the_default_agent_row_pins_and_unpins() {
        use chrome::settings::Edit;
        let mut view =
            View { page: Page::Settings, pinned_agent: Some("codex".into()), ..Default::default() };
        let mut st = chrome::Settings { group: 1, ..settings_state() };

        // Open the list; it starts on the agent that is pinned.
        handle_settings_key(key(event::KeyCode::Enter), &mut view, &mut st, 150, 40);
        assert_eq!(st.open, Some(2), "ask-every-time, claude, codex");
        let flow = handle_settings_key(key(event::KeyCode::Enter), &mut view, &mut st, 150, 40);
        assert!(
            matches!(&flow, Some(Flow::SettingsEdit(Edit::DefaultAgent(Some(n)))) if n == "codex"),
            "{flow:?}"
        );

        // The first option is the way back to being asked, and it writes None.
        handle_settings_key(key(event::KeyCode::Enter), &mut view, &mut st, 150, 40);
        for _ in 0..5 {
            handle_settings_key(key(event::KeyCode::Char('k')), &mut view, &mut st, 150, 40);
        }
        let flow = handle_settings_key(key(event::KeyCode::Enter), &mut view, &mut st, 150, 40);
        assert!(matches!(flow, Some(Flow::SettingsEdit(Edit::DefaultAgent(None)))), "{flow:?}");
    }

    /// A size row is nudged through the same clamp LAYOUT mode's drag uses, so
    /// a width you can type is one you could have dragged to.
    #[test]
    fn a_size_row_cannot_be_typed_past_the_floor_a_drag_stops_at() {
        let mut view = View { page: Page::Settings, ..Default::default() };
        // WORKBENCH, first row: the left rail.
        let mut st = chrome::Settings { group: 2, ..settings_state() };

        for _ in 0..40 {
            handle_settings_key(key(event::KeyCode::Char('-')), &mut view, &mut st, 150, 40);
        }
        assert_eq!(view.geom.left_w, chrome::RAIL_MIN_W, "hammering `-` stops at the floor");

        for _ in 0..80 {
            handle_settings_key(key(event::KeyCode::Char('+')), &mut view, &mut st, 150, 40);
        }
        assert!(view.geom.left_w <= chrome::RAIL_MAX_W, "and `+` at the cap");
        // And the stage still has room to exist, which is the cap that actually
        // bites on a narrow terminal.
        assert!(view.geom.left_w + view.geom.right_w + chrome::MIN_STAGE_W <= 150);
    }

    /// `0` puts a band back to sizing itself, which is where every one of them
    /// starts and therefore has to be reachable again.
    #[test]
    fn zero_puts_a_band_back_to_auto() {
        use chrome::settings::Edit;
        let mut view = View { page: Page::Settings, ..Default::default() };
        // WORKBENCH's third row is `procs_height`.
        let mut st = chrome::Settings { group: 2, row: 2, ..settings_state() };

        handle_settings_key(key(event::KeyCode::Char('+')), &mut view, &mut st, 150, 40);
        assert!(view.geom.procs_h.is_some(), "a nudge pins it");
        let flow = handle_settings_key(key(event::KeyCode::Char('0')), &mut view, &mut st, 150, 40);
        assert!(matches!(flow, Some(Flow::SettingsEdit(Edit::Geom))), "{flow:?}");
        assert_eq!(view.geom.procs_h, None, "and `0` lets go of it again");
    }

    /// `d` in the agent picker pins the highlighted agent.
    ///
    /// The help screen has advertised this key for as long as the pin existed,
    /// and for just as long `handle_overlay_key` had no arm for it: `d` fell
    /// into the catch-all and was swallowed, leaving `:agent-default` as the
    /// only way to set the thing the rail's `[+ NAME]` button is built around.
    ///
    /// Pressing the key is the whole point of this test. The one that was here
    /// asserts the *help text* mentions `d pins`, and it passed happily for the
    /// entire time the key did nothing — which is what a test that reads the
    /// documentation instead of the behaviour buys you.
    #[test]
    fn d_in_the_agent_picker_pins_the_highlighted_agent() {
        let picker = |sel| {
            Overlay::List(ListOverlay {
                title: AGENT_PICKER_TITLE.into(),
                items: vec!["claude".into(), "codex".into(), "aider".into()],
                values: None,
                sel,
                kind: ListKind::SpawnAgent,
            })
        };

        // The row under the cursor, not the first one.
        let mut view = View { overlay: Some(picker(1)), ..Default::default() };
        let flow = handle_overlay_key(key(event::KeyCode::Char('d')), &mut view);
        assert!(matches!(&flow, Flow::PinAgent(Some(n)) if n == "codex"), "{flow:?}");
        assert!(view.overlay.is_none(), "a flash confirms the pin, and a modal would cover it");

        // Enter still spawns. `d` must not have stolen the key the picker is
        // mostly opened to press.
        let mut view = View { overlay: Some(picker(1)), ..Default::default() };
        let flow = handle_overlay_key(key(event::KeyCode::Enter), &mut view);
        assert!(matches!(flow, Flow::Choose), "{flow:?}");

        // And `d` means nothing on the lists that have no default to set —
        // otherwise it is a key whose meaning you have to guess per modal.
        let mut view = View {
            overlay: Some(Overlay::List(ListOverlay {
                title: "CHECK OUT".into(),
                items: vec!["main".into()],
                values: None,
                sel: 0,
                kind: ListKind::Branch,
            })),
            ..Default::default()
        };
        let flow = handle_overlay_key(key(event::KeyCode::Char('d')), &mut view);
        assert!(matches!(flow, Flow::Continue), "{flow:?}");
        assert!(view.overlay.is_some(), "and it must not close the box either");
    }

    /// Which picks ask again after the row is chosen.
    ///
    /// These confirm *after* their picker, not before, because until a row is
    /// chosen there is nothing to name in the question. The menu's own
    /// destructive rows are the other way round for the same reason.
    #[test]
    fn the_picks_that_destroy_something_ask_with_it_named() {
        use chrome::PickTarget as T;
        for t in [T::DeleteBranch, T::StashDrop, T::TagDelete, T::RemoteRemove, T::RemoveWorktree] {
            assert!(t.destroys(), "{t:?} should ask");
        }
        for t in [T::Merge, T::Rebase, T::StashPop, T::OpenWorktree] {
            assert!(!t.destroys(), "{t:?} should not ask");
        }
    }

    /// Typing runs a search per keystroke, so several are in flight at once and
    /// they can land out of order. A late answer to an old query must not
    /// replace the answer to what is on screen.
    #[test]
    fn a_stale_search_answer_does_not_overwrite_a_newer_one() {
        let hits = |p: &str| {
            vec![chrome::SearchHit { path: p.into(), line: None, preview: String::new() }]
        };
        let mut view = View {
            overlay: Some(Overlay::Search(chrome::SearchOverlay {
                query: "needle".into(),
                cursor: 6,
                hits: hits("current.rs"),
                sel: 0,
                searching: true,
            })),
            ..Default::default()
        };

        apply_search(&mut view, "need", hits("stale.rs"));
        let Some(Overlay::Search(f)) = &view.overlay else { panic!() };
        assert_eq!(f.hits, hits("current.rs"), "a stale answer overwrote the live one");
        assert!(f.searching, "and it should still be waiting for its own");

        apply_search(&mut view, "needle", hits("fresh.rs"));
        let Some(Overlay::Search(f)) = &view.overlay else { panic!() };
        assert_eq!(f.hits, hits("fresh.rs"));
        assert!(!f.searching);
    }

    /// How a hit reads, and that a filename match is not dressed up as a line.
    #[test]
    fn a_search_hit_says_where_it_is() {
        use chrome::{SearchHit, SearchOverlay};
        let content =
            SearchHit { path: "src/a.rs".into(), line: Some(12), preview: "let x = 1;".into() };
        assert_eq!(SearchOverlay::label(&content), "src/a.rs:12  let x = 1;");
        let name = SearchHit { path: "src/a.rs".into(), line: None, preview: String::new() };
        assert_eq!(SearchOverlay::label(&name), "src/a.rs", "a filename match has no line");
    }

    /// Where a new worktree's checkout goes.
    #[test]
    fn a_worktree_goes_beside_the_one_it_came_from() {
        assert_eq!(worktree_path("/code/proj", "spike"), "/code/proj-spike");
        // A branch with slashes would otherwise put the checkout two
        // directories down, inside a path that does not exist.
        assert_eq!(worktree_path("/code/proj", "feature/x"), "/code/proj-feature-x");
        // And never *inside* the repository, which git would then have to
        // ignore.
        assert!(!worktree_path("/code/proj", "spike").starts_with("/code/proj/"));
    }

    /// A destructive row asks before it runs, and the answer is what lets it
    /// through — `needs_confirm` is the shared table's judgement, so the client
    /// cannot disagree with the daemon about which rows are dangerous.
    #[test]
    fn the_destructive_menu_rows_are_the_tables_own_list() {
        use crate::git_menu::GitAction as A;
        for action in [A::PushForce, A::ResetHard, A::SequenceAbort] {
            assert!(action.needs_confirm(), "{action:?} should ask first");
        }
        for action in [A::Fetch, A::Pull, A::Push, A::StashPush, A::Amend] {
            assert!(!action.needs_confirm(), "{action:?} should not ask");
        }
    }

    /// A destructive row is asked about once, runs once, and is asked about
    /// again next time.
    #[test]
    fn a_destructive_row_is_asked_about_exactly_once_per_run() {
        use crate::git_menu::GitAction as A;
        let mut confirmed = None;

        assert!(needs_asking(A::ResetHard, &mut confirmed), "it should ask the first time");
        // The confirm box was answered yes, which records the action.
        confirmed = Some(A::ResetHard);
        assert!(!needs_asking(A::ResetHard, &mut confirmed), "the answer should let it through");
        assert!(needs_asking(A::ResetHard, &mut confirmed), "and be spent, so the next run asks");

        // A yes for one row must not let a different one through.
        confirmed = Some(A::ResetHard);
        assert!(needs_asking(A::PushForce, &mut confirmed), "a yes is not transferable");

        // The harmless rows never ask.
        let mut confirmed = None;
        assert!(!needs_asking(A::Fetch, &mut confirmed));
        assert!(!needs_asking(A::StashPush, &mut confirmed));
    }

    /// Where each row of the folder browser leads.
    ///
    /// Path arithmetic is the part of a file browser that fails silently — it
    /// opens the wrong folder rather than erroring — so it is a pure function
    /// with a test rather than three `format!`s in the event loop.
    #[test]
    fn browsing_a_folder_goes_where_the_row_says() {
        assert_eq!(
            browse_step("/home/me/code", "proj/"),
            BrowseStep::Descend("/home/me/code/proj".into())
        );
        assert_eq!(
            browse_step("/home/me/code", chrome::BROWSE_UP),
            BrowseStep::Descend("/home/me".into())
        );
        assert_eq!(
            browse_step("/home/me/code", chrome::BROWSE_OPEN),
            BrowseStep::OpenHere("/home/me/code".into())
        );
        assert_eq!(
            browse_step("/home/me/code", chrome::BROWSE_NEW),
            BrowseStep::NewHere("/home/me/code".into()),
            "a new folder is made in the folder on screen, not in its parent"
        );
        // Up from the root is the root. An empty path would ask the daemon for
        // the home directory, which is not "up".
        assert_eq!(browse_step("/", chrome::BROWSE_UP), BrowseStep::Descend("/".into()));
        // A directory whose name is `..` or contains a slash cannot be
        // constructed by the daemon's listing, but the join must still not
        // escape by concatenation.
        assert_eq!(
            browse_step("/a/b", "c d/"),
            BrowseStep::Descend("/a/b/c d".into()),
            "a name with a space is one component"
        );
    }

    /// The picker offers a folder that does not exist yet, and offers it where
    /// it can be seen.
    ///
    /// A listing is the one screen where the folder you are after is the one
    /// that is not on it, so the row that answers that cannot be underneath the
    /// listing. Files are left out of the rows for the same reason they always
    /// were: a workspace opens on a directory.
    #[test]
    fn the_picker_offers_to_make_a_folder_where_you_are() {
        let entry = |name: &str, is_dir| butai_protocol::api::BrowseEntry {
            name: name.into(),
            path: format!("/home/me/code/{name}"),
            is_dir,
        };
        let dto = butai_protocol::api::BrowseDto {
            path: "/home/me/code".into(),
            parent: Some("/home/me".into()),
            entries: vec![entry("proj", true), entry("notes.md", false)],
        };
        let Overlay::List(list) = browse_overlay(dto) else { panic!("a picker is a list") };
        assert_eq!(
            list.items,
            vec![chrome::BROWSE_OPEN, chrome::BROWSE_NEW, chrome::BROWSE_UP, "proj/"],
            "the two verbs about here lead, then the way out, then the folders"
        );
        // The row and the directory the list carries are the whole contract
        // between this function and the dispatch, so read one against the other.
        let ListKind::Browse { dir } = &list.kind else { panic!("a picker browses") };
        assert_eq!(browse_step(dir, &list.items[1]), BrowseStep::NewHere("/home/me/code".into()));

        // The filesystem root has nowhere above it and is still a place to
        // start a project: only `..` goes away with the parent.
        let root =
            butai_protocol::api::BrowseDto { path: "/".into(), parent: None, entries: Vec::new() };
        let Overlay::List(list) = browse_overlay(root) else { panic!("a picker is a list") };
        assert_eq!(list.items, vec![chrome::BROWSE_OPEN, chrome::BROWSE_NEW]);
    }

    /// A named folder is made in the directory the picker was showing.
    ///
    /// The prompt *replaces* the list, so the directory rides on the prompt
    /// itself. Getting that wrong makes the folder somewhere the user never
    /// browsed to — the one failure a folder-maker must not have, and one that
    /// leaves a real directory behind when it happens.
    #[test]
    fn a_named_folder_is_made_where_the_picker_was() {
        let mut view =
            View { overlay: Some(new_folder_prompt("/home/me/code")), ..View::default() };
        for c in " new-proj ".chars() {
            handle_overlay_key(key(event::KeyCode::Char(c)), &mut view);
        }
        let flow = handle_overlay_key(key(event::KeyCode::Enter), &mut view);
        // Trimmed: ` new-proj ` is a legal directory name and never the one
        // that was meant.
        assert!(
            matches!(&flow, Flow::MakeFolder { dir, name }
                if dir == "/home/me/code" && name == "new-proj"),
            "got {flow:?}"
        );
        assert!(view.overlay.is_none(), "the box closes once it has been answered");
    }

    /// An empty name does not throw the picker away.
    ///
    /// Every other prompt is opened from the workbench, so refusing by closing
    /// puts you back where you started. This one is a step inside the picker,
    /// where the same keystroke that opened it — Enter — arriving once more is
    /// enough to discard the machine and the browsing that got here.
    #[test]
    fn an_empty_folder_name_keeps_the_box_open() {
        let mut view =
            View { overlay: Some(new_folder_prompt("/home/me/code")), ..View::default() };
        let flow = handle_overlay_key(key(event::KeyCode::Enter), &mut view);
        assert!(matches!(flow, Flow::Continue), "an unanswered box does nothing");
        let Some(Overlay::Prompt(p)) = &view.overlay else {
            panic!("the picker's step was dropped")
        };
        assert!(
            matches!(&p.kind, chrome::PromptKind::NewFolder { dir } if dir == "/home/me/code"),
            "and it still knows where it was going to make it"
        );
        assert_eq!(view.flash.as_deref(), Some("a folder needs a name"));
    }

    /// A prompt owns the keyboard: `q` is a character in a commit message, not
    /// the dismiss key it is in every other modal.
    #[test]
    fn a_prompt_does_not_lose_characters_to_the_other_modals_keys() {
        let ws = ws_with_changes(rail_changes(0));
        let mut view = View { changes_sel: 5, ..Default::default() };
        handle_changes_key(key(event::KeyCode::Char('c')), &mut view, Some(&ws));
        for c in "quick jk fix".chars() {
            handle_overlay_key(key(event::KeyCode::Char(c)), &mut view);
        }
        let Some(Overlay::Prompt(p)) = &view.overlay else { panic!("the prompt was dismissed") };
        assert_eq!(p.text, "quick jk fix");
    }

    /// Detach has to work from wherever you are, including with the keyboard
    /// on the stage — where every other key belongs to the pane.
    ///
    /// Found live: following a container's logs moves focus to the stage, and
    /// Alt-d was going to `docker logs`, which has no use for it. There was
    /// then no way off the page short of killing the client.
    #[test]
    fn alt_d_detaches_even_with_the_keyboard_on_the_stage() {
        let mut view = View { focus: Focus::Stage, ..Default::default() };
        let (mut files, mut diff, mut docker) =
            (Files::default(), DiffView::default(), Docker::default());
        let alt_d = event::KeyEvent::new(event::KeyCode::Char('d'), event::KeyModifiers::ALT);
        let flow = handle_input(
            event::Event::Key(alt_d),
            &mut view,
            &[],
            &[],
            None,
            &mut files,
            &mut Files::default(),
            &mut diff,
            &mut docker,
            &mut chrome::Git::default(),
            &mut chrome::Settings::default(),
            &mut chrome::Help::default(),
            &mut chrome::usage::Usage::default(),
            &Keymap::default(),
            &mut Drag::default(),
            false,
            false,
            &mut 120,
            &mut 40,
        );
        assert!(matches!(flow, Flow::Detach), "alt-d was swallowed by the stage");

        // A plain `d` is still the pane's: it is a character, not a command.
        let flow = handle_input(
            event::Event::Key(key(event::KeyCode::Char('d'))),
            &mut view,
            &[],
            &[],
            None,
            &mut files,
            &mut Files::default(),
            &mut diff,
            &mut docker,
            &mut chrome::Git::default(),
            &mut chrome::Settings::default(),
            &mut chrome::Help::default(),
            &mut chrome::usage::Usage::default(),
            &Keymap::default(),
            &mut Drag::default(),
            false,
            false,
            &mut 120,
            &mut 40,
        );
        assert!(matches!(flow, Flow::Continue), "a plain key should reach the pane");
    }

    fn ctrl(c: char) -> event::KeyEvent {
        event::KeyEvent::new(event::KeyCode::Char(c), event::KeyModifiers::CONTROL)
    }

    fn press(view: &mut View, k: event::KeyEvent, keymap: &Keymap) -> Flow {
        press_with(view, k, keymap, false)
    }

    /// `press`, with the macOS Option-as-Alt reading turned on.
    fn press_mac(view: &mut View, k: event::KeyEvent, keymap: &Keymap) -> Flow {
        press_with(view, k, keymap, true)
    }

    fn press_with(view: &mut View, k: event::KeyEvent, keymap: &Keymap, mac: bool) -> Flow {
        let (mut files, mut diff, mut docker) =
            (Files::default(), DiffView::default(), Docker::default());
        handle_input(
            event::Event::Key(k),
            view,
            &[],
            &[],
            None,
            &mut files,
            &mut Files::default(),
            &mut diff,
            &mut docker,
            &mut chrome::Git::default(),
            &mut chrome::Settings::default(),
            &mut chrome::Help::default(),
            &mut chrome::usage::Usage::default(),
            keymap,
            &mut Drag::default(),
            mac,
            false,
            &mut 120,
            &mut 40,
        )
    }

    /// The prefix reaches the workbench from inside a pane.
    ///
    /// That is the entire point of a prefix key, and it is the one thing the
    /// stage's "everything belongs to the pane" rule has to yield to — a client
    /// where `C-b d` only worked with the cursor on a rail would be one where
    /// the prefix table is unreachable exactly when you need it.
    #[test]
    fn the_prefix_gets_through_a_focused_stage() {
        let keymap = Keymap::default();
        let mut view = View { focus: Focus::Stage, ..Default::default() };
        assert!(matches!(press(&mut view, ctrl('b'), &keymap), Flow::Continue));
        assert!(view.prefix_armed, "the prefix did not arm from the stage");
        // And the key after it is the binding, not the pane's.
        let flow = press(&mut view, key(event::KeyCode::Char('d')), &keymap);
        assert!(matches!(flow, Flow::Detach), "C-b d did not detach");
        assert!(!view.prefix_armed, "the prefix stayed armed");
    }

    /// A stage whose pane connection is a pair of channels, so a test can read
    /// what the client sent to it.
    fn fake_stage() -> (Stage, UnboundedReceiver<ClientMsg>) {
        let (to_server, sent) = unbounded_channel();
        let (_never, from_server) = unbounded_channel();
        let stage = Stage {
            transport: crate::conn::Transport { to_server, from_server },
            wants_mouse: false,
            cursor: None,
            daemon: 0,
            pane: PaneId(1),
            buf: Buffer::empty(Rect::new(0, 0, 10, 3)),
            lost: None,
            retry_at: None,
        };
        (stage, sent)
    }

    /// A stage sized to the rect the page actually blits it into, with its
    /// cursor where the last frame put it.
    fn staged_at(view: &View, cols: u16, rows: u16, cursor: (u16, u16)) -> Stage {
        // The receiver goes; nothing here sends, and the caret is a question
        // about state rather than about the wire.
        let (mut stage, _sent) = fake_stage();
        stage.buf = Buffer::empty(chrome::stage_rect(cols, rows, view));
        stage.cursor = Some(cursor);
        stage
    }

    /// The frame's cursor is pane-relative and the pane is not at the origin,
    /// so placing it is an offset by wherever [`compose`] blitted it. Without
    /// that the caret lands in the tab bar, on top of the rails, or anywhere
    /// but the cell the program is writing at.
    #[test]
    fn the_caret_lands_where_the_pane_was_blitted() {
        let (cols, rows) = (120u16, 40u16);
        let view = View { page: Page::Agents, focus: Focus::Stage, ..Default::default() };
        let at = chrome::stage_rect(cols, rows, &view);
        assert!(at.x > 0 && at.y > 0, "a stage at the origin would prove nothing");
        let stage = staged_at(&view, cols, rows, (3, 2));
        assert_eq!(
            stage_caret(&view, Some(&stage), Some(&stage.buf), cols, rows),
            Some((at.x + 3, at.y + 2))
        );
    }

    /// A program that hid its cursor, a pane scrolled back, a command that
    /// exited: the daemon sends `None` and the client must show nothing rather
    /// than leave one parked wherever the last frame put it.
    #[test]
    fn a_hidden_cursor_puts_no_caret_on_screen() {
        let (cols, rows) = (120u16, 40u16);
        let view = View { page: Page::Agents, focus: Focus::Stage, ..Default::default() };
        let mut stage = staged_at(&view, cols, rows, (3, 2));
        stage.cursor = None;
        assert_eq!(stage_caret(&view, Some(&stage), Some(&stage.buf), cols, rows), None);
    }

    /// A modal draws its own caret into the buffer, and the keyboard is its.
    /// Leaving the real cursor on the pane underneath puts two carets on one
    /// screen, with the blinking one in the place that is not listening.
    #[test]
    fn a_modal_takes_the_caret_off_the_pane() {
        let (cols, rows) = (120u16, 40u16);
        let mut view = View { page: Page::Agents, focus: Focus::Stage, ..Default::default() };
        let stage = staged_at(&view, cols, rows, (3, 2));
        assert!(stage_caret(&view, Some(&stage), Some(&stage.buf), cols, rows).is_some());
        view.overlay = Some(Overlay::Search(Default::default()));
        assert_eq!(stage_caret(&view, Some(&stage), Some(&stage.buf), cols, rows), None);
    }

    /// The stage keeps streaming while a page that draws no pane is up, so its
    /// cursor is about a screen nobody is looking at. GIT is one: its body is a
    /// diff the client holds, and a caret over it would point at nothing.
    #[test]
    fn a_page_with_no_pane_has_no_caret() {
        let (cols, rows) = (120u16, 40u16);
        let view = View { page: Page::Git, ..Default::default() };
        let stage = staged_at(&view, cols, rows, (3, 2));
        assert_eq!(stage_caret(&view, Some(&stage), None, cols, rows), None);
    }

    /// Between a resize and the daemon's first frame at the new size, the
    /// cursor can name a cell outside the rectangle that was drawn — and the
    /// caret has to be dropped for that frame rather than pointing at a cell
    /// belonging to the chrome around the pane.
    #[test]
    fn a_cursor_past_the_pane_is_not_drawn() {
        let (cols, rows) = (120u16, 40u16);
        let view = View { page: Page::Agents, focus: Focus::Stage, ..Default::default() };
        let at = chrome::stage_rect(cols, rows, &view);
        let past_right = staged_at(&view, cols, rows, (at.width, 0));
        assert_eq!(stage_caret(&view, Some(&past_right), Some(&past_right.buf), cols, rows), None);
        let past_bottom = staged_at(&view, cols, rows, (0, at.height));
        assert_eq!(
            stage_caret(&view, Some(&past_bottom), Some(&past_bottom.buf), cols, rows),
            None
        );
    }

    /// Pressing the prefix twice types it. Without this there is no way to send
    /// a literal `C-b` to a program that wants one — an inner multiplexer, or
    /// readline's own back-a-character.
    #[test]
    fn the_prefix_twice_is_how_you_type_it() {
        let keymap = Keymap::default();
        let (stage, mut sent) = fake_stage();
        let (mut files, mut diff, mut docker) =
            (Files::default(), DiffView::default(), Docker::default());
        let mut view = View { focus: Focus::Stage, ..Default::default() };
        let mut go = |view: &mut View, k| {
            handle_input(
                event::Event::Key(k),
                view,
                &[],
                &[],
                Some(&stage),
                &mut files,
                &mut Files::default(),
                &mut diff,
                &mut docker,
                &mut chrome::Git::default(),
                &mut chrome::Settings::default(),
                &mut chrome::Help::default(),
                &mut chrome::usage::Usage::default(),
                &keymap,
                &mut Drag::default(),
                false,
                false,
                &mut 120,
                &mut 40,
            )
        };
        go(&mut view, ctrl('b'));
        assert!(sent.try_recv().is_err(), "arming the prefix must not reach the pane");
        let flow = go(&mut view, ctrl('b'));

        assert!(matches!(flow, Flow::Continue));
        assert!(!view.prefix_armed, "a doubled prefix must disarm, not re-arm");
        let msg = sent.try_recv().expect("the doubled prefix never reached the pane");
        let ClientMsg::Input(InputEvent::Key(k)) = msg else { panic!("{msg:?} is not a key") };
        assert_eq!(k, keymap.prefix, "the pane got {k:?}, not the prefix");
        // And it is a keystroke, not a complaint: reporting "^B is not bound"
        // here would mean the doubled prefix fell through to the table.
        assert_eq!(view.flash, None, "{:?}", view.flash);
    }

    /// A click on BOOTH's fleet stays on BOOTH.
    ///
    /// Reported: clicking an agent to look at it and then clicking it again
    /// threw the whole workbench onto that agent's workspace — on another
    /// machine, when it lived there. The rails' two-step is right for them
    /// because a second click there costs you a pane; here it costs the project
    /// you were in, and `[open]` sits on the row for exactly this reason.
    #[test]
    fn clicking_a_fleet_row_twice_does_not_leave_the_booth() {
        let mut view = View { page: Page::Booth, focus: Focus::AllAgents, ..Default::default() };
        assert!(matches!(fleet_click(hit::FleetHit::Row(2), &mut view), Flow::Continue));
        assert_eq!(view.all_agents_sel, 2, "the click must still move the cursor and the preview");
        // The second one on the same row is still a look, not a jump — which is
        // also what a double-click is, since a terminal has no such event.
        assert!(matches!(fleet_click(hit::FleetHit::Row(2), &mut view), Flow::Continue));
        assert_eq!(view.page, Page::Booth, "a click on a row left BOOTH");
        // And the button is the one thing that travels.
        assert!(matches!(fleet_click(hit::FleetHit::Open(2), &mut view), Flow::OpenFleetAgent(2)));
    }

    /// A fleet of two machines, for the tests about *where* an act on a row
    /// lands. The workspace ids collide on purpose: they are per daemon, so
    /// `SessionId(1)` on `gpu-box` is a different project from `SessionId(1)`
    /// here, and code that carries the id without the machine cannot tell.
    fn two_machine_fleet(agents: &[butai_protocol::api::AgentDto]) -> Vec<chrome::AllAgentRow<'_>> {
        use butai_protocol::SessionId;
        vec![
            chrome::AllAgentRow {
                workspace: "proj",
                workspace_id: SessionId(1),
                agent: &agents[0],
                host: None,
                daemon: 0,
            },
            chrome::AllAgentRow {
                workspace: "infra",
                workspace_id: SessionId(1),
                agent: &agents[1],
                host: Some("gpu-box"),
                daemon: 1,
            },
        ]
    }

    /// `x` on BOOTH ends the session the fleet cursor is on — on *its* machine,
    /// in *its* workspace.
    ///
    /// The whole difficulty of this page in one assertion. Every other cursor in
    /// the workbench is inside the workspace the tab bar names, so a pane id was
    /// address enough; the fleet's rows cross daemons, and a pane id is only
    /// unique within one. Routing this through the active tab would send the
    /// DELETE to the machine you are sitting at, where that id is somebody
    /// else's agent — a kill that reports success and ends the wrong thing.
    ///
    /// Mutation check: make [`fleet_route`] read the active workspace instead of
    /// the row and the second half fails.
    #[test]
    fn ending_a_fleet_session_names_the_machine_the_row_is_on() {
        use butai_protocol::{api::AgentState, SessionId};
        let agents =
            [agent_dto(10, "claude", AgentState::Idle), agent_dto(7, "codex", AgentState::Waiting)];
        let fleet = two_machine_fleet(&agents);

        let here = fleet_route(&fleet, 0).expect("row 0 is an agent");
        assert_eq!(here, Route { daemon: 0, workspace: SessionId(1), pane: PaneId(10) });
        let away = fleet_route(&fleet, 1).expect("row 1 is an agent");
        assert_eq!(
            away,
            Route { daemon: 1, workspace: SessionId(1), pane: PaneId(7) },
            "an agent on gpu-box must be ended on gpu-box"
        );
        // A row that has gone between the frame and the press is not an error to
        // report, it is nothing to do.
        assert_eq!(fleet_route(&fleet, 2), None);
    }

    /// `x` is the fleet's one lettered verb, and it only answers while the fleet
    /// has the keyboard.
    ///
    /// The second half is the part worth pinning: `tab` hands the keyboard to
    /// BOOTH's middle column, which is a live pane, and from there every key is
    /// the agent's — an `x` typed into a shell must stay an `x`.
    #[test]
    fn x_ends_the_fleet_row_and_only_while_the_fleet_has_the_keyboard() {
        let x = key(event::KeyCode::Char('x'));
        let fleet = View { page: Page::Booth, focus: Focus::AllAgents, ..Default::default() };
        assert!(matches!(handle_fleet_key(x, &fleet, 3), Some(Flow::KillSelected)));
        // Nothing to end is nothing to say — not a failure from the daemon about
        // a pane that was never named.
        assert!(handle_fleet_key(x, &fleet, 0).is_none());
        // And it is the only one: the rest of the rails' table is about lists
        // this page does not draw.
        for c in ['r', 'a', 'A', 't', 'X'] {
            assert!(
                handle_fleet_key(key(event::KeyCode::Char(c)), &fleet, 3).is_none(),
                "{c} is not a fleet verb"
            );
        }

        // The gate. Once `tab` has handed BOOTH's middle column the keyboard the
        // pane is a live agent, and an `x` typed at it has to stay an `x` —
        // this returning `None` is what lets it fall through to the forward.
        let stage = View { page: Page::Booth, focus: Focus::Stage, ..Default::default() };
        assert!(handle_fleet_key(x, &stage, 3).is_none(), "x on the preview must reach the agent");
    }

    /// The fleet's context menu names the row it was opened on, wherever that
    /// row lives — and it is the same menu the rails open, so `Close others`
    /// means the other agents in *that* project.
    #[test]
    fn the_fleet_menu_names_the_row_and_its_machine() {
        use butai_protocol::{api::AgentState, SessionId};
        let agents =
            [agent_dto(10, "claude", AgentState::Idle), agent_dto(7, "codex", AgentState::Waiting)];
        let fleet = two_machine_fleet(&agents);
        let Some(Overlay::List(list)) = fleet_menu(&fleet, 1) else { panic!("no menu") };
        assert_eq!(
            list.kind,
            ListKind::Menu(chrome::MenuTarget::Agent {
                daemon: 1,
                workspace: SessionId(1),
                pane: PaneId(7),
            })
        );
        // The same three rows the rails offer — one builder, so the pointer and
        // `m` cannot come to mean different things on different pages.
        assert_eq!(list.items, vec!["Close agent", "Close others", "Close all agents"]);
        assert!(fleet_menu(&fleet, 9).is_none(), "a row that has gone has no menu");
    }

    /// BOOTH's preview is a pane, so a key typed at it reaches the agent.
    ///
    /// Reported as "the agent is right there and I can't talk to it". It was
    /// worse than nothing happening: `draws_stage` said BOOTH had no pane, so
    /// every keystroke fell past the forward and into the global table, where
    /// `q` is detach and `a` spawns an agent. Clicking the preview to read it
    /// and pressing `q` to stop reading closed the client.
    ///
    /// Both halves are here on purpose — the key has to reach the pane *and*
    /// not reach `Flow::Detach` — because the first assertion alone passes on a
    /// build that does both.
    /// A bare key on USAGE is the page's, not the pane's.
    ///
    /// USAGE has no pane under it, so a `j` that fell through to the stage
    /// forward would be typed into whatever shell was running behind it. The
    /// dispatch that stops that is one `if` in `handle_input`, and deleting it
    /// left every other test passing — only clippy's unused-variable warning
    /// noticed, which is a thin thing to rest a page's keyboard on.
    #[test]
    fn a_key_on_usage_moves_its_cursor_instead_of_reaching_the_pane() {
        let (stage, mut sent) = fake_stage();
        let mut view = View { page: Page::Usage, focus: Focus::Stage, ..Default::default() };
        let mut usage = chrome::usage::Usage::default();
        let flow = handle_input(
            event::Event::Key(key(event::KeyCode::Char('j'))),
            &mut view,
            &[],
            &[],
            Some(&stage),
            &mut Files::default(),
            &mut Files::default(),
            &mut DiffView::default(),
            &mut Docker::default(),
            &mut chrome::Git::default(),
            &mut chrome::Settings::default(),
            &mut chrome::Help::default(),
            &mut usage,
            &Keymap::default(),
            &mut Drag::default(),
            false,
            false,
            &mut 120,
            &mut 40,
        );
        assert!(matches!(flow, Flow::Continue), "`j` on USAGE did {flow:?}");
        assert!(sent.try_recv().is_err(), "`j` on USAGE was typed into the pane behind it");

        // And `r` is the page's reload rather than a character.
        let flow = handle_input(
            event::Event::Key(key(event::KeyCode::Char('r'))),
            &mut view,
            &[],
            &[],
            Some(&stage),
            &mut Files::default(),
            &mut Files::default(),
            &mut DiffView::default(),
            &mut Docker::default(),
            &mut chrome::Git::default(),
            &mut chrome::Settings::default(),
            &mut chrome::Help::default(),
            &mut usage,
            &Keymap::default(),
            &mut Drag::default(),
            false,
            false,
            &mut 120,
            &mut 40,
        );
        assert!(matches!(flow, Flow::RefreshUsage), "`r` on USAGE did {flow:?}");
        assert!(sent.try_recv().is_err(), "`r` on USAGE reached the pane");
    }

    #[test]
    fn a_key_on_booths_preview_reaches_the_agent() {
        let keymap = Keymap::default();
        let (stage, mut sent) = fake_stage();
        let (mut files, mut diff, mut docker) =
            (Files::default(), DiffView::default(), Docker::default());
        // Where a click on the middle column leaves you.
        let mut view = View { page: Page::Booth, focus: Focus::Stage, ..Default::default() };
        let flow = handle_input(
            event::Event::Key(key(event::KeyCode::Char('q'))),
            &mut view,
            &[],
            &[],
            Some(&stage),
            &mut files,
            &mut Files::default(),
            &mut diff,
            &mut docker,
            &mut chrome::Git::default(),
            &mut chrome::Settings::default(),
            &mut chrome::Help::default(),
            &mut chrome::usage::Usage::default(),
            &keymap,
            &mut Drag::default(),
            false,
            false,
            &mut 120,
            &mut 40,
        );
        assert!(
            matches!(flow, Flow::Continue),
            "`q` over BOOTH's preview did {flow:?}, not typing"
        );
        let msg = sent.try_recv().expect("`q` never reached the agent");
        let ClientMsg::Input(InputEvent::Key(k)) = msg else { panic!("{msg:?} is not a key") };
        assert_eq!(k, KeyEvent::char('q'), "the pane got {k:?}");

        // And `alt-w` is the way back to the fleet, because every unmodified key
        // now belongs to the agent — `esc` and `tab` included.
        let back = run_view(ViewVerb::Focus(Focus::AllAgents), &mut view);
        assert!(matches!(back, Flow::Continue));
        assert_eq!(view.focus, Focus::AllAgents, "alt-w must hand the keyboard back to the fleet");
        assert_eq!(view.page, Page::Booth, "and must not leave the page to do it");

        // `alt-esc` names the AGENTS rail, which BOOTH does not draw. It lands on
        // the fleet rather than on a cursor in a list that is not on screen.
        view.focus = Focus::Stage;
        run_view(ViewVerb::Focus(Focus::Agents), &mut view);
        assert_eq!(
            view.focus,
            Focus::AllAgents,
            "a rail BOOTH does not draw is not somewhere to go"
        );
    }

    /// Enter on BOOTH goes to the agent; Enter on a rail stages a pane.
    ///
    /// Not the same verb, because the fleet crosses machines: staging alone
    /// pointed the middle column at a pane belonging to a workspace the tab bar
    /// said you were not in, and on a second daemon it did not resolve at all.
    #[test]
    fn enter_on_booth_travels_where_enter_on_a_rail_stages() {
        let keymap = Keymap::default();
        let enter = key(event::KeyCode::Enter);
        let mut view = View {
            page: Page::Booth,
            focus: Focus::AllAgents,
            all_agents_sel: 3,
            ..Default::default()
        };
        assert!(matches!(press(&mut view, enter, &keymap), Flow::OpenFleetAgent(3)));
        // A rail cursor stages the pane it names, as it always has.
        let mut view = View { focus: Focus::Agents, agent_sel: 3, ..Default::default() };
        assert!(matches!(press(&mut view, enter, &keymap), Flow::StageSelected));
    }

    /// A paste reaches the pane, as a paste.
    ///
    /// [`crate::tui`] turns bracketed paste on, so the terminal stops sending a
    /// paste as keystrokes and sends the run as one `Event::Paste`. Nothing
    /// matched it, so it went nowhere: reported as "pasting doesn't work", and
    /// it did not.
    #[test]
    fn a_paste_reaches_the_pane_whole() {
        let (stage, mut sent) = fake_stage();
        let (mut files, mut diff, mut docker) =
            (Files::default(), DiffView::default(), Docker::default());
        let mut view = View { focus: Focus::Stage, ..Default::default() };
        let flow = handle_input(
            event::Event::Paste("cargo test\nsecond line".into()),
            &mut view,
            &[],
            &[],
            Some(&stage),
            &mut files,
            &mut Files::default(),
            &mut diff,
            &mut docker,
            &mut chrome::Git::default(),
            &mut chrome::Settings::default(),
            &mut chrome::Help::default(),
            &mut chrome::usage::Usage::default(),
            &Keymap::default(),
            &mut Drag::default(),
            false,
            false,
            &mut 120,
            &mut 40,
        );
        assert!(matches!(flow, Flow::Continue));
        let msg = sent.try_recv().expect("the paste never reached the pane");
        // A paste and not keys: the `ESC[200~` markers belong to the daemon,
        // which is the only side that knows whether that pane asked for them.
        let ClientMsg::Input(InputEvent::Paste(text)) = msg else {
            panic!("{msg:?} is not a paste")
        };
        assert_eq!(text, "cargo test\nsecond line", "the run must arrive whole, newline and all");
    }

    /// A paste into a line editor is one line.
    ///
    /// The prompt holds a commit message or a branch name, so the newlines
    /// cannot go in as they are — and dropping them would run `fix` and `the`
    /// into `fixthe`, which for a branch name is a different branch.
    #[test]
    fn a_paste_into_a_prompt_is_flattened_not_dropped() {
        let ws = ws_with_changes(rail_changes(0));
        let mut view = View { changes_sel: 5, ..Default::default() };
        handle_changes_key(key(event::KeyCode::Char('c')), &mut view, Some(&ws));
        let flow = paste_text(
            "fix\nthe thing\n".into(),
            &mut view,
            &mut Files::default(),
            &mut Files::default(),
            None,
        );
        assert!(matches!(flow, Flow::Continue));
        let Some(Overlay::Prompt(p)) = &view.overlay else { panic!("the prompt was dismissed") };
        assert_eq!(p.text, "fix the thing");
    }

    /// BOOTH's stage is somebody else's session on show, so a paste there says so
    /// rather than typing into an agent on another machine.
    #[test]
    fn a_paste_on_booth_does_not_type_into_the_fleet() {
        let (stage, mut sent) = fake_stage();
        let mut view = View { page: Page::Booth, focus: Focus::AllAgents, ..Default::default() };
        paste_text(
            "rm -rf /\n".into(),
            &mut view,
            &mut Files::default(),
            &mut Files::default(),
            Some(&stage),
        );
        assert!(sent.try_recv().is_err(), "a paste on BOOTH reached the previewed agent");
        assert!(view.flash.is_some(), "and it did so silently");
    }

    /// Every route onto BOOTH lands the keyboard on the fleet.
    ///
    /// `alt-0` did not: it set the page and stopped, while the chip and the
    /// space-cycle went through `open_page`, which focuses the list. So the key
    /// the page is *documented* under was the one arrival where the middle
    /// column kept the keyboard — `j` walked nothing, and every bare letter was
    /// typed into whichever agent the preview happened to be showing.
    ///
    /// Found by driving the real client under a pty: `alt-0`, then `x`, put an
    /// `x` on an agent's command line instead of ending it.
    #[test]
    fn every_way_onto_booth_puts_the_keyboard_on_the_fleet() {
        let keymap = Keymap::default();
        let alt = |c| event::KeyEvent::new(event::KeyCode::Char(c), event::KeyModifiers::ALT);
        // `alt-0`, from the stage of the page you were on.
        let mut view = View { focus: Focus::Stage, ..Default::default() };
        press(&mut view, alt('0'), &keymap);
        assert_eq!(view.page, Page::Booth);
        assert_eq!(view.focus, Focus::AllAgents, "alt-0 left the keyboard on the stage");

        // The chip, which goes the long way round through `run_click`.
        let mut view = View { focus: Focus::Stage, ..Default::default() };
        run_click(hit::Target::Space(Page::Booth), &mut view, 1, None);
        assert_eq!((view.page, view.focus), (Page::Booth, Focus::AllAgents));

        // And leaving hands it back, or the cursor would be in a list the next
        // page does not draw.
        press(&mut view, alt('o'), &keymap);
        assert_eq!(view.focus, Focus::Stage, "{:?} kept BOOTH's cursor", view.page);
    }

    #[test]
    fn a_pasted_run_flattens_to_one_line() {
        assert_eq!(as_one_line("fix\nthe thing"), "fix the thing");
        // Leading and trailing runs are nothing, not a space — a pasted line
        // with its newline still on must not commit a message ending in one.
        assert_eq!(as_one_line("\r\n hi \r\n\r\n"), " hi ");
        assert_eq!(as_one_line("a\r\n\tb"), "a b", "a run of them is one space");
    }

    /// An unbound key after the prefix says so. The daemon logged it where
    /// nobody looks, which made a binding that was not there indistinguishable
    /// from one that was and did nothing.
    #[test]
    fn an_unbound_key_after_the_prefix_says_so() {
        let keymap = Keymap::default();
        let mut view = View::default();
        press(&mut view, ctrl('b'), &keymap);
        press(&mut view, key(event::KeyCode::Char('Q')), &keymap);
        let flash = view.flash.expect("nothing was said about an unbound key");
        assert!(flash.contains('Q') && flash.contains("not bound"), "{flash}");
    }

    /// `a` and `A` are not the same key.
    ///
    /// `a` spawns what the rail's `+` advertises — the pinned agent, when there
    /// is one — and `A` is the deliberate "let me choose". The daemon's own
    /// split, and the reason the pin is worth having at all: without it, `a`
    /// would be a key that sometimes opens a list and sometimes does not, with
    /// no way to ask for the list.
    #[test]
    fn a_spawns_and_shift_a_asks() {
        let keymap = Keymap::default();
        // Off the stage: a bare letter with the cursor *on* it belongs to the
        // pane, which is what `the_workbench_opens_on_the_stage` is about.
        let mut view = View { focus: Focus::Agents, ..Default::default() };
        assert!(matches!(
            press(&mut view, key(event::KeyCode::Char('a')), &keymap),
            Flow::NewAgent
        ));
        assert!(matches!(
            press(&mut view, key(event::KeyCode::Char('A')), &keymap),
            Flow::PickAgent
        ));
        // `C-b a` is the same deliberate ask, through the table rather than the
        // rail — so it must not be the pin's shortcut either.
        press(&mut view, ctrl('b'), &keymap);
        assert!(matches!(
            press(&mut view, key(event::KeyCode::Char('a')), &keymap),
            Flow::PickAgent
        ));
    }

    /// A fresh client has the keyboard on the stage, not on a rail.
    ///
    /// The rails answer to bare letters — `a` spawns an agent, `b` picks a
    /// branch, `n` opens a project — so a workbench that opens focused on one
    /// turns the first command you type into a handful of them. Typing
    /// `echo $PATH` into a fresh TUI opened the agent picker on the `a`. The
    /// daemon opened on the stage; this pins that it still does.
    #[test]
    fn the_workbench_opens_on_the_stage() {
        let keymap = Keymap::default();
        let mut view = View::default();
        assert_eq!(view.focus, Focus::Stage, "a fresh client should be typing into the pane");
        // With no pane attached the key is simply consumed — the point is that
        // it is *not* read as a workbench command.
        for c in ['a', 'A', 'b', 'n', 'g', 'q'] {
            let flow = press(&mut view, key(event::KeyCode::Char(c)), &keymap);
            assert!(matches!(flow, Flow::Continue), "`{c}` was read as a command, not typed");
        }
        // And Alt-Esc is the way out, onto the rail the daemon left you on.
        let esc = event::KeyEvent::new(event::KeyCode::Esc, event::KeyModifiers::ALT);
        press(&mut view, esc, &keymap);
        assert_eq!(view.focus, Focus::Agents);
        assert!(matches!(
            press(&mut view, key(event::KeyCode::Char('a')), &keymap),
            Flow::NewAgent
        ));
    }

    /// The Alt layer reaches the chrome from a focused pane.
    ///
    /// It has to, or the bindings are dead exactly when they are needed: the
    /// client opens on the stage, so a shell has the keyboard from the first
    /// keystroke. Only Alt-Esc and Alt-d were getting through, which left the
    /// spaces, the sections, the tabs and the pickers unreachable without first
    /// leaving the pane — and nothing on screen said you had to.
    /// A Mac's Option-composed characters drive the Alt layer.
    ///
    /// Option is a compose key on macOS, not a modifier: Option-o types `ø` and
    /// no Alt is ever reported, so the whole Alt layer was dead out of the box
    /// for most Mac users — silently, because a key that does nothing looks the
    /// same as a key that is not bound.
    #[test]
    fn a_mac_option_character_reaches_the_binding_it_stands_for() {
        let keymap = Keymap::default();
        let ch = |c| event::KeyEvent::new(event::KeyCode::Char(c), event::KeyModifiers::NONE);

        // `ø` is Option-o, and Alt-o is the files space.
        let mut view = View { focus: Focus::Stage, ..Default::default() };
        press_mac(&mut view, ch('ø'), &keymap);
        assert_eq!(view.page, Page::Files, "Option-o never reached alt-o");

        // A rail key, and the detach, which is the one people find first.
        let mut view = View { focus: Focus::Stage, ..Default::default() };
        press_mac(&mut view, ch('π'), &keymap);
        assert_eq!(view.focus, Focus::Processes, "Option-p never reached alt-p");
        assert!(matches!(press_mac(&mut view, ch('∂'), &keymap), Flow::Detach));
    }

    /// Turned off, the same character is just a character.
    ///
    /// It has to be: the reading is what makes `ø` untypeable, so anyone who
    /// writes a language that needs it must be able to get it back.
    #[test]
    fn without_the_mac_reading_an_option_character_is_left_alone() {
        let keymap = Keymap::default();
        let ch = |c| event::KeyEvent::new(event::KeyCode::Char(c), event::KeyModifiers::NONE);
        let mut view = View { focus: Focus::Stage, ..Default::default() };
        press(&mut view, ch('ø'), &keymap);
        assert_eq!(view.page, Page::Agents, "`ø` moved the workbench with the reading off");
    }

    /// The whole of the terminal half of the feature, in bytes.
    ///
    /// A hyperlink is invisible in a screen dump — it is state the terminal
    /// carries beside the cells — so the only place it can be caught wrong is
    /// here, and a typo in an escape sequence is a feature that silently does
    /// nothing.
    #[test]
    fn a_url_is_written_as_one_hyperlink_and_closed_after_it() {
        let mut before = Buffer::empty(Rect::new(0, 0, 24, 1));
        let mut after = Buffer::empty(Rect::new(0, 0, 24, 1));
        after.set_string(0, 0, "see https://example.com", ratatui::style::Style::default());
        let map = links::ScreenLinks::of(&after, None);
        let diff = before.diff(&after);

        let mut out: Vec<u8> = Vec::new();
        write_cells(&mut out, &diff, &map, true).expect("write");
        let bytes = String::from_utf8(out).expect("utf-8");
        let id = format!("{:x}", links::id_of("https://example.com"));

        // Opened once, at the `h` — not at the `s` of "see", and not once per
        // cell: the id makes the whole run one link, and per-cell sequences
        // would be twenty times the bytes for the same screen.
        assert_eq!(bytes.matches("\x1b]8;id=").count(), 1, "{bytes:?}");
        assert!(bytes.contains(&format!("\x1b]8;id={id};https://example.com\x1b\\h")), "{bytes:?}");
        // And closed, so what is drawn after it is not part of the link.
        assert!(bytes.ends_with("\x1b]8;;\x1b\\"), "{bytes:?}");

        // `[ui] links = false` writes the same cells and no sequences at all.
        before = Buffer::empty(Rect::new(0, 0, 24, 1));
        let mut plain: Vec<u8> = Vec::new();
        write_cells(&mut plain, &before.diff(&after), &map, false).expect("write");
        let plain = String::from_utf8(plain).expect("utf-8");
        assert!(!plain.contains("\x1b]8"), "{plain:?}");
        assert!(plain.contains("https://example.com".chars().next().unwrap()), "{plain:?}");
    }

    /// `f` is the picker, and it is a bare key — so it must not be one the
    /// stage swallows silently. Off the stage it opens; the loop is what turns
    /// the flow into a list, because that is where the painted screen is.
    #[test]
    fn f_asks_for_the_links_on_screen() {
        let keymap = Keymap::default();
        let ch = |c| event::KeyEvent::new(event::KeyCode::Char(c), event::KeyModifiers::NONE);
        let mut view = View { focus: Focus::Agents, ..Default::default() };
        assert!(matches!(press(&mut view, ch('f'), &keymap), Flow::PickLinks));
        // And through the prefix, which is how it is reached from a pane that
        // has the keyboard.
        let mut view = View { focus: Focus::Stage, ..Default::default() };
        press(
            &mut view,
            event::KeyEvent::new(event::KeyCode::Char('b'), event::KeyModifiers::CONTROL),
            &keymap,
        );
        assert!(matches!(press(&mut view, ch('f'), &keymap), Flow::PickLinks));
    }

    /// The picker's two verbs. `enter` is answered by the loop (it opens a
    /// browser, which no test should); `y` is answered here, and is the one
    /// that matters over ssh.
    #[test]
    fn y_copies_the_highlighted_link_and_closes_the_picker() {
        let mut view = View {
            overlay: Some(Overlay::List(chrome::ListOverlay {
                title: "LINKS".into(),
                items: vec!["https://a/1".into(), "https://b/2".into()],
                values: None,
                sel: 1,
                kind: ListKind::Links,
            })),
            ..Default::default()
        };
        let y = event::KeyEvent::new(event::KeyCode::Char('y'), event::KeyModifiers::NONE);
        let flow = handle_overlay_key(y, &mut view);
        assert!(matches!(&flow, Flow::CopyLink(url) if url == "https://b/2"), "{flow:?}");
        assert!(view.overlay.is_none(), "the picker stayed open");

        // `y` on any other list is not a copy — it is nothing, as it was.
        view.overlay = Some(Overlay::List(chrome::ListOverlay {
            title: "BRANCH".into(),
            items: vec!["main".into()],
            values: None,
            sel: 0,
            kind: ListKind::Branch,
        }));
        assert!(matches!(handle_overlay_key(y, &mut view), Flow::Continue));
        assert!(view.overlay.is_some(), "`y` dismissed a branch list");
    }

    /// The table is only the keys the Alt layer binds, so everything else stays
    /// typeable even with the reading on. `∫` is Option-b, and nothing binds
    /// Alt-b — it is a character, and it must survive as one.
    ///
    /// The list is a fixture of *currently* unbound keys, so a new binding is
    /// entitled to take one: `®` was here until `alt-r` became the GIT space.
    #[test]
    fn an_unbound_option_character_is_still_a_character() {
        let keymap = Keymap::default();
        let ch = |c| event::KeyEvent::new(event::KeyCode::Char(c), event::KeyModifiers::NONE);
        for c in ['∫', 'ƒ', '∆', '¥', 'œ'] {
            assert!(keys::option_char(c).is_none(), "`{c}` was taken away needlessly");
        }
        let mut view = View { focus: Focus::Stage, ..Default::default() };
        assert!(matches!(press_mac(&mut view, ch('∫'), &keymap), Flow::Continue));
        assert_eq!(view.page, Page::Agents);
    }

    /// A terminal that already reports Alt is untouched, so turning the reading
    /// on can never break a setup that was working.
    #[test]
    fn a_real_alt_key_is_never_reinterpreted() {
        let keymap = Keymap::default();
        let alt = |c| event::KeyEvent::new(event::KeyCode::Char(c), event::KeyModifiers::ALT);
        let mut view = View { focus: Focus::Stage, ..Default::default() };
        press_mac(&mut view, alt('o'), &keymap);
        assert_eq!(view.page, Page::Files, "a genuine alt-o stopped working");

        // And a Ctrl-modified key is saying something else entirely.
        let ctrl_o = event::KeyEvent::new(event::KeyCode::Char('ø'), event::KeyModifiers::CONTROL);
        assert_eq!(mac_option(ctrl_o), ctrl_o, "ctrl was swallowed");
    }

    /// Every key the Alt layer binds is either recoverable from a composed
    /// character or reachable another way. The five that are not — Option-e,
    /// Option-n and Option-u are dead keys, Option-Esc and Option-Enter are not
    /// composed at all — are why the prefix layer covering the same set
    /// matters.
    #[test]
    fn the_mac_table_covers_the_alt_layer_or_says_why_not() {
        // Dead keys: Option-e (´), Option-n (˜) and Option-u (¨) emit nothing
        // until the next keystroke, so there is no character to read back. The
        // USAGE space is reached with `<prefix> u` on a Mac for this reason.
        let dead = ['e', 'n', 'u'];
        let bound: Vec<char> = ('a'..='z')
            .chain('0'..='9')
            .chain([',', '.', '<', '>', '/'])
            .filter(|c| alt_binding(event::KeyCode::Char(*c), &mut View::default()).is_some())
            .collect();
        assert!(bound.len() > 20, "the Alt layer should bind more than {}", bound.len());
        for c in bound {
            if dead.contains(&c) {
                continue;
            }
            assert!(
                keys::option_char_for(c).is_some(),
                "alt-{c} is bound but no Option character reaches it"
            );
        }
    }

    /// The prefix layer and the Alt layer are the same interface.
    ///
    /// Both resolve to a [`ViewVerb`] and both hand it to [`run_view`], so a
    /// key that works on one works on the other. Before, the prefix table still
    /// spoke of splits and windows while the Alt layer had the real verbs, and
    /// the two could not be compared at all — which is how 23 dead bindings sat
    /// in the shipped table without anything noticing.
    #[test]
    fn the_prefix_and_the_alt_layer_agree_about_the_workbench() {
        let keymap = Keymap::default();
        let alt = |c| event::KeyEvent::new(event::KeyCode::Char(c), event::KeyModifiers::ALT);

        // Same letter, same space, whichever layer asked for it.
        for (c, page) in [('o', Page::Files), ('m', Page::Docs), ('c', Page::Docker)] {
            let mut by_alt = View { focus: Focus::Stage, ..Default::default() };
            press(&mut by_alt, alt(c), &keymap);

            let mut by_prefix = View { focus: Focus::Stage, ..Default::default() };
            press(&mut by_prefix, ctrl('b'), &keymap);
            press(&mut by_prefix, key(event::KeyCode::Char(c)), &keymap);

            assert_eq!(by_alt.page, page, "alt-{c} did not open {page:?}");
            assert_eq!(by_prefix.page, by_alt.page, "C-b {c} and alt-{c} disagree");
        }

        // And a rail key, which the prefix spells with the capital because the
        // lower-case letter is the agent verb.
        let mut by_alt = View { focus: Focus::Stage, ..Default::default() };
        press(&mut by_alt, alt('p'), &keymap);
        let mut by_prefix = View { focus: Focus::Stage, ..Default::default() };
        press(&mut by_prefix, ctrl('b'), &keymap);
        press(&mut by_prefix, key(event::KeyCode::Char('P')), &keymap);
        assert_eq!(by_alt.focus, Focus::Processes);
        assert_eq!(by_prefix.focus, by_alt.focus, "C-b P and alt-p disagree");
    }

    #[test]
    fn the_alt_layer_belongs_to_the_chrome_even_on_the_stage() {
        let keymap = Keymap::default();
        let alt = |c| event::KeyEvent::new(event::KeyCode::Char(c), event::KeyModifiers::ALT);

        // The spaces cycle, and land on the page the button row would show.
        let mut view = View { focus: Focus::Stage, ..Default::default() };
        press(&mut view, alt('.'), &keymap);
        assert_eq!(view.page, Page::Agents.next(), "alt-. was swallowed by the pane");
        press(&mut view, alt(','), &keymap);
        assert_eq!(view.page, Page::Agents);

        // A section key, from the stage.
        press(&mut view, alt('p'), &keymap);
        assert_eq!(view.focus, Focus::Processes, "alt-p was swallowed by the pane");

        // And the pickers, which are the ones a fresh client most needs.
        let mut view = View { focus: Focus::Stage, ..Default::default() };
        let enter = event::KeyEvent::new(event::KeyCode::Enter, event::KeyModifiers::ALT);
        assert!(matches!(press(&mut view, enter, &keymap), Flow::PickAgent));
        assert!(matches!(press(&mut view, alt('v'), &keymap), Flow::PasteImage));
        assert!(matches!(press(&mut view, alt('l'), &keymap), Flow::ToggleLayout));

        // What the chrome does *not* bind still belongs to the pane, so Alt-b
        // and Alt-f keep working in readline.
        let mut view = View { focus: Focus::Stage, ..Default::default() };
        let before = view.page;
        for c in ['b', 'f', 'y'] {
            assert!(matches!(press(&mut view, alt(c), &keymap), Flow::Continue));
        }
        assert_eq!(view.page, before, "an unbound alt key changed the chrome");
        assert_eq!(view.focus, Focus::Stage, "an unbound alt key moved the cursor");
    }

    /// Layout mode claims the arrows, and only the arrows.
    ///
    /// While it is on, `h`/`l` resize rather than reaching a pane — which is
    /// the whole point of a mode, and why the footer says so for as long as it
    /// lasts. `[layout]` used to toggle zen instead, which is a different
    /// feature that already has its own key.
    #[test]
    fn layout_mode_resizes_the_rail_the_cursor_is_in() {
        let keymap = Keymap::default();
        let mut view = View::default();
        assert!(matches!(
            run_click(hit::Target::Footer("[layout]"), &mut view, 1, None),
            Flow::ToggleLayout
        ));
        assert!(!view.zen, "[layout] is not the zen key");

        // Entering is the loop's job; do it the way the loop does.
        view.layout = Some(view.geom);
        let before = view.geom.left_w;
        press(&mut view, key(event::KeyCode::Right), &keymap);
        assert!(view.geom.left_w > before, "the left rail did not widen");
        press(&mut view, key(event::KeyCode::Left), &keymap);
        assert_eq!(view.geom.left_w, before, "and back again");

        // The cursor decides *which* rail: CHANGES is on the right.
        view.focus = Focus::Changes;
        let right = view.geom.right_w;
        press(&mut view, key(event::KeyCode::Right), &keymap);
        assert!(view.geom.right_w > right, "the right rail did not widen");
        assert_eq!(view.geom.left_w, before, "the left rail moved with it");

        // Esc leaves.
        assert!(matches!(press(&mut view, key(event::KeyCode::Esc), &keymap), Flow::ToggleLayout));
    }

    /// Help is a page of its own, and it leaves the file screen alone.
    ///
    /// **This is the whole point of the change.** `?` and `[help]` used to set
    /// `Page::Docs`, point its tree at `butai://reference` and open a topic in
    /// the file viewer — so pressing help rearranged the file screen: a
    /// different directory, a different selection, a buffer you never opened.
    /// Both routes are pinned here, and so is the absence of that damage.
    #[test]
    fn help_is_its_own_page_and_does_not_touch_the_files() {
        for (what, target) in [("?", None), ("[help]", Some(hit::Target::Footer("[help]")))] {
            let mut view = View { page: Page::Files, ..Default::default() };
            let mut files = Files { dir: "src".into(), sel: 3, ..Default::default() };
            let mut docs = Files { dir: "docs".into(), sel: 2, ..Default::default() };
            let flow = match target {
                Some(t) => run_click(t, &mut view, 1, None),
                None => handle_input(
                    event::Event::Key(key(event::KeyCode::Char('?'))),
                    &mut view,
                    &[],
                    &[],
                    None,
                    &mut files,
                    &mut docs,
                    &mut DiffView::default(),
                    &mut Docker::default(),
                    &mut chrome::Git::default(),
                    &mut chrome::Settings::default(),
                    &mut chrome::Help::default(),
                    &mut chrome::usage::Usage::default(),
                    &Keymap::default(),
                    &mut Drag::default(),
                    false,
                    false,
                    &mut 120,
                    &mut 40,
                ),
            };
            assert!(matches!(flow, Flow::OpenHelp), "{what} did not open HELP: {flow:?}");
            // Neither tree moved, and neither opened anything. The old route
            // failed every one of these.
            assert_eq!((docs.dir.as_str(), docs.sel), ("docs", 2), "{what} rebuilt the DOCS tree");
            assert_eq!((files.dir.as_str(), files.sel), ("src", 3), "{what} moved the file tree");
            assert!(docs.open.is_none() && files.open.is_none(), "{what} opened a buffer");
        }
    }

    /// It is entered and left, like SETTINGS: the second press goes back where
    /// you came from rather than leaving you on a page you never chose.
    #[test]
    fn help_goes_back_where_it_was_opened_from() {
        let mut help = chrome::Help::default();
        let mut view = View { page: Page::Docker, ..Default::default() };
        assert!(matches!(
            run_click(hit::Target::Footer("[help]"), &mut view, 1, None),
            Flow::OpenHelp
        ));
        // The loop is what carries the page across; do it the way the loop does.
        help.ret = view.page;
        view.page = Page::Help;

        // Both ways out: the button again, and `esc` on the page.
        assert!(matches!(
            run_click(hit::Target::Footer("[help]"), &mut view, 1, None),
            Flow::CloseHelp
        ));
        let flow = handle_help_key(key(event::KeyCode::Esc), &mut view, &mut help, 120, 40);
        assert!(matches!(flow, Some(Flow::CloseHelp)), "esc did not leave HELP: {flow:?}");
        // And the key that opened it, which is the third way out — a `?` that
        // opened the page it is already on would pin it open.
        assert!(
            matches!(run_view(ViewVerb::Help, &mut view), Flow::CloseHelp),
            "`?` re-opened HELP"
        );
        assert_eq!(help.ret, Page::Docker, "and it goes back where it was opened from");
    }

    /// The keys that read it: a line, a screen, the ends, and the topics.
    ///
    /// The reading position is clamped to the page it is on, which is what the
    /// modal never did — it scrolled past its own end and showed nothing.
    #[test]
    fn the_help_page_scrolls_and_stops() {
        let (cols, rows) = (120u16, 40u16);
        let mut view = View { page: Page::Help, ..Default::default() };
        let mut help = chrome::Help::default();
        let press = |view: &mut View, help: &mut chrome::Help, code| {
            handle_help_key(key(code), view, help, cols, rows)
        };

        assert!(press(&mut view, &mut help, event::KeyCode::Char('k')).is_some());
        assert_eq!(help.scroll, 0, "scrolling up from the top went somewhere");
        press(&mut view, &mut help, event::KeyCode::Char('j'));
        assert_eq!(help.scroll, 1);
        press(&mut view, &mut help, event::KeyCode::End);
        let end = help.scroll;
        assert!(end > 1, "the keys topic is longer than a screen; End went nowhere");
        for _ in 0..20 {
            press(&mut view, &mut help, event::KeyCode::PageDown);
        }
        assert_eq!(help.scroll, end, "paging ran past the end of the page");
        press(&mut view, &mut help, event::KeyCode::Home);
        assert_eq!(help.scroll, 0);

        // Tab walks the topics, from the top of each.
        press(&mut view, &mut help, event::KeyCode::Char('j'));
        let was = help.topic;
        press(&mut view, &mut help, event::KeyCode::Tab);
        assert_ne!(help.topic, was, "tab stayed on the same topic");
        assert_eq!(help.scroll, 0, "a new topic opened halfway down");
        press(&mut view, &mut help, event::KeyCode::BackTab);
        assert_eq!(help.topic, was, "shift-tab did not come back");
    }

    /// `[settings]` in the footer is the route the tab-bar chip used to be, and
    /// it still enters and leaves rather than toggling to AGENTS like a space.
    #[test]
    fn the_footer_settings_button_enters_and_leaves() {
        let mut view = View { page: Page::Docker, ..Default::default() };
        assert!(matches!(
            run_click(hit::Target::Footer("[settings]"), &mut view, 1, None),
            Flow::OpenSettings
        ));
        // The loop is what carries the page across; do it the way the loop does.
        let ret = view.page;
        view.page = Page::Settings;
        assert!(matches!(
            run_click(hit::Target::Footer("[settings]"), &mut view, 1, None),
            Flow::CloseSettings
        ));
        assert_eq!(ret, Page::Docker, "and it goes back where it was opened from");
    }

    /// SETTINGS used to swallow the tab bar and the footer, so BOOTH, the spaces
    /// and every button on either row did nothing while it was up.
    #[test]
    fn a_click_on_the_bars_gets_out_of_settings() {
        let ret = Page::Docker;
        // A space names where it is going, and goes there rather than to `ret`.
        let mut view = View { page: Page::Settings, ..Default::default() };
        page_bar_click(hit::Target::Space(Page::Booth), &mut view, ret, 1, None);
        assert_eq!(view.page, Page::Booth, "clicking BOOTH left SETTINGS up");

        let mut view = View { page: Page::Settings, ..Default::default() };
        page_bar_click(hit::Target::Space(Page::Files), &mut view, ret, 1, None);
        assert_eq!(view.page, Page::Files);

        // The space that *is* `ret` still goes there, rather than reading as a
        // press on the page you are already on and toggling to AGENTS.
        let mut view = View { page: Page::Settings, ..Default::default() };
        page_bar_click(hit::Target::Space(ret), &mut view, ret, 1, None);
        assert_eq!(view.page, ret, "the space you came from toggled instead");

        // A chip names no page, so it puts back the one SETTINGS was over.
        let mut view = View { page: Page::Settings, tab: 0, ..Default::default() };
        page_bar_click(hit::Target::Tab(1), &mut view, ret, 2, None);
        assert_eq!(view.page, ret, "a workspace chip left SETTINGS up over it");
        assert_eq!(view.tab, 1, "and it still selects the workspace");

        // `[help]` names a page of its own, and it is the loop that carries the
        // page across — so what this pins is the flow, not `view.page`.
        let mut view = View { page: Page::Settings, ..Default::default() };
        let flow = page_bar_click(hit::Target::Footer("[help]"), &mut view, ret, 1, None);
        assert!(matches!(flow, Flow::OpenHelp), "help from SETTINGS went nowhere");

        // And its own button stays a toggle: putting the page back first would
        // turn the press into a fresh `OpenSettings` and pin the page open.
        let mut view = View { page: Page::Settings, ..Default::default() };
        let flow = page_bar_click(hit::Target::Footer("[settings]"), &mut view, ret, 1, None);
        assert!(matches!(flow, Flow::CloseSettings), "[settings] stopped closing");
    }

    /// A rail cannot grow past the stage's minimum.
    ///
    /// Without the cap, widening on a narrow terminal trips the drawing's own
    /// fallback and collapses *both* rails — so the key would appear to do the
    /// opposite of what it says.
    #[test]
    fn a_rail_stops_before_it_squeezes_the_stage_out() {
        use crate::chrome::{MIN_STAGE_W, RAIL_MIN_W};
        let mut geom = chrome::View::default().geom;
        let cols = 80;
        for _ in 0..60 {
            chrome::resize_rail(&mut geom, cols, true, 2);
        }
        assert!(
            geom.left_w + geom.right_w + MIN_STAGE_W <= cols,
            "left={} right={} leaves no stage in {cols} columns",
            geom.left_w,
            geom.right_w
        );
        // And it cannot be shrunk out of existence either.
        for _ in 0..60 {
            chrome::resize_rail(&mut geom, cols, true, -2);
        }
        assert_eq!(geom.left_w, RAIL_MIN_W);
    }

    /// A right-click offers what it landed on, and nothing where there is
    /// nothing to offer.
    ///
    /// It carries the *pane*, not the cursor: the cursor can move between
    /// opening the menu and answering it, and "Close agent" has to mean the one
    /// you right-clicked.
    #[test]
    fn a_right_click_menu_names_the_row_it_was_opened_on() {
        use chrome::MenuTarget;
        let ws = ws_with_agents();
        let second = ws.agents[1].pane;
        let menu = menu_for(hit::Target::Rail(Focus::Agents, 1), Some(&ws), &[], &[], 0)
            .expect("an agent row has a menu");
        let Overlay::List(list) = &menu else { panic!("not a list") };
        let ListKind::Menu(MenuTarget::Agent { workspace, pane, .. }) = list.kind else {
            panic!("{:?}", list.kind)
        };
        assert_eq!(pane, second, "the menu named a different agent");
        // And it named the workspace the row is in, not just the pane: that is
        // what "close others" acts on, and what a fleet row can differ on.
        assert_eq!(workspace, ws.id);
        assert_eq!(list.items, vec!["Close agent", "Close others", "Close all agents"]);

        // A row past the end of the rail has nothing to offer, and neither does
        // empty chrome — a menu there would be a menu about nothing.
        assert!(menu_for(hit::Target::Rail(Focus::Agents, 9), Some(&ws), &[], &[], 0).is_none());
        assert!(menu_for(hit::Target::Nothing, Some(&ws), &[], &[], 0).is_none());
        assert!(menu_for(hit::Target::Footer("[help]"), Some(&ws), &[], &[], 0).is_none());
        // The CHANGES rail's rows are files, not panes.
        assert!(menu_for(hit::Target::Rail(Focus::Changes, 0), Some(&ws), &[], &[], 0).is_none());
    }

    /// `m` opens the same menu the right button does, on the same row.
    ///
    /// The menu was the pointer's alone, and it holds the only route to "close
    /// others", "close all agents" and "disconnect host" — three actions a
    /// mouseless client could not reach at all. The key resolves to a
    /// [`hit::Target`] and hands it to the same [`menu_for`], so the two cannot
    /// come to offer different rows.
    #[test]
    fn the_menu_key_points_where_the_cursor_is() {
        use chrome::MenuTarget;
        let ws = ws_with_agents();

        // On a rail: the row under the cursor, not the first one.
        let view = View { focus: Focus::Agents, agent_sel: 1, ..Default::default() };
        assert_eq!(menu_target(&view), hit::Target::Rail(Focus::Agents, 1));
        let menu = menu_for(menu_target(&view), Some(&ws), &[], &[], 0).expect("an agent menu");
        let Overlay::List(list) = &menu else { panic!("not a list") };
        assert_eq!(
            list.kind,
            ListKind::Menu(MenuTarget::Agent {
                daemon: 0,
                workspace: ws.id,
                pane: ws.agents[1].pane
            })
        );
        // The rows that exist nowhere else in the interface.
        assert!(list.items.iter().any(|i| i == "Close others"));
        assert!(list.items.iter().any(|i| i == "Close all agents"));

        let view = View { focus: Focus::Processes, proc_sel: 2, ..Default::default() };
        assert_eq!(menu_target(&view), hit::Target::Rail(Focus::Processes, 2));

        // Off a rail there is no row to point at, so it is the workspace's own
        // menu — the tab chip's, which is where `Disconnect host` lives.
        //
        // `AllAgents` is in the list because this is a function of the view and
        // that focus is reachable off BOOTH for exactly one frame before the
        // loop clears it. On BOOTH itself the key never gets here: the fleet's
        // rows come from the cross-daemon list rather than the geometry, so
        // there is no `hit::Target` for one and `fleet_menu` answers instead.
        for focus in [Focus::Changes, Focus::Stage, Focus::AllAgents, Focus::Refs, Focus::History] {
            let view = View { focus, tab: 3, ..Default::default() };
            assert_eq!(menu_target(&view), hit::Target::Tab(3), "{focus:?}");
        }
    }

    /// Every click in the workbench has a key that reaches the same thing.
    ///
    /// The rule the interface is built on, made mechanical: written as a `match`
    /// over [`hit::Target`] with no catch-all, so a new click target does not
    /// compile until someone has answered "and what is its key?". That is the
    /// only part of this a reviewer cannot forget to run.
    ///
    /// The arms assert against the real tables — [`alt_verb`], the shipped
    /// prefix keymap and [`crate::verbs`] — rather than restating a list of
    /// letters, so a key that is moved is a failure here rather than a comment
    /// that has quietly gone stale. Two targets have no key and say why.
    #[test]
    fn every_click_target_has_a_key() {
        use crate::keymap::{parse_key, Action, Keymap};
        let alt = |c: char| alt_verb(event::KeyCode::Char(c));
        let prefix = |k: &str| Keymap::default().resolve(&parse_key(k).unwrap()).cloned();
        // A bare letter only reaches the workbench with the cursor off the
        // stage: on it every key is the program's, which is what makes it a
        // terminal, and is why the Alt layer exists at all.
        let bare = |c: char| {
            let mut view = View { focus: Focus::Agents, ..Default::default() };
            press(&mut view, key(event::KeyCode::Char(c)), &Keymap::default())
        };

        for target in [
            hit::Target::Tab(0),
            hit::Target::CloseTab,
            hit::Target::Space(Page::Files),
            hit::Target::Spaces,
            hit::Target::NewWorkspace,
            hit::Target::Machines,
            hit::Target::Footer("[layout]"),
            hit::Target::Rail(Focus::Agents, 0),
            hit::Target::AgentsVerb('a'),
            hit::Target::ProcsVerb('t'),
            hit::Target::ChangesVerb('s'),
            hit::Target::System(chrome::Gauge::Cpu),
            hit::Target::Stage(0, 0),
            hit::Target::Nothing,
        ] {
            match target {
                // The tab bar: by number, and by walking it.
                hit::Target::Tab(_) => {
                    assert_eq!(alt('1'), Some(ViewVerb::Tab(1)));
                    assert_eq!(alt('>'), Some(ViewVerb::TabNext));
                    assert_eq!(prefix("2"), Some(Action::View(ViewVerb::Tab(2))));
                }
                hit::Target::CloseTab => {
                    assert_eq!(alt('x'), Some(ViewVerb::CloseWorkspace));
                    assert_eq!(prefix("X"), Some(Action::View(ViewVerb::CloseWorkspace)));
                }
                // The control that opens the menu answers to a key of its
                // own, so the list of spaces is reachable without a pointer —
                // otherwise a keyboard user would have to already know the
                // letters to find out the letters exist.
                hit::Target::Spaces => {
                    assert_eq!(alt(' '), Some(ViewVerb::Spaces));
                    assert_eq!(prefix(" "), Some(Action::View(ViewVerb::Spaces)));
                    // And it opens the same list the button does.
                    let mut view = View::default();
                    assert!(matches!(
                        run_click(hit::Target::Spaces, &mut view, 1, None),
                        Flow::PickSpace
                    ));
                }
                // Every space, not just the one in the array — walked from
                // [`Page::ORDER`], which is the list the menu is built from, so
                // a space added without a letter fails here rather than
                // shipping as a row you can only click. The GIT space was
                // exactly that: `alt-r` reached it, the prefix layer had
                // nothing, and `space git` was not a phrase this language knew.
                hit::Target::Space(_) => {
                    let printable = || (0x20u8..=0x7e).map(char::from);
                    for page in Page::ORDER {
                        // `work` is where every other space toggles back to, so
                        // it needs no Alt letter of its own.
                        assert!(
                            page == Page::Agents
                                || printable().any(|c| alt(c) == Some(ViewVerb::Space(page))),
                            "{page:?} has no key on the Alt layer"
                        );
                        assert!(
                            printable().any(|c| {
                                prefix(&c.to_string()) == Some(Action::View(ViewVerb::Space(page)))
                            }),
                            "{page:?} has no key on the prefix layer"
                        );
                    }
                    // BOOTH and SETTINGS are peers of the workspaces rather than
                    // spaces, so they are keyed rather than cycled.
                    let mut view = View::default();
                    assert!(alt_binding(event::KeyCode::Char('0'), &mut view).is_some());
                    assert!(matches!(
                        alt_binding(event::KeyCode::Char('s'), &mut view),
                        Some(Flow::OpenSettings)
                    ));
                }
                hit::Target::NewWorkspace => {
                    assert_eq!(alt('n'), Some(ViewVerb::NewWorkspace));
                    assert!(matches!(bare('n'), Flow::Browse(_)));
                }
                // The machine picker answers to the one key that opens it, and
                // sends the loop the same flow the button does.
                hit::Target::Machines => {
                    assert_eq!(alt('h'), Some(ViewVerb::Host));
                    let mut view = View::default();
                    assert!(matches!(run_click(target, &mut view, 1, None), Flow::PickHost));
                }
                // All four footer buttons, by the label the footer draws.
                hit::Target::Footer(_) => {
                    let mut view = View::default();
                    for (label, code) in [
                        ("[layout]", event::KeyCode::Char('l')),
                        ("[detach]", event::KeyCode::Char('d')),
                        ("[settings]", event::KeyCode::Char('s')),
                    ] {
                        assert!(
                            alt_binding(code, &mut view).is_some(),
                            "{label} has no key on the Alt layer"
                        );
                    }
                    assert!(matches!(bare('?'), Flow::OpenHelp));
                }
                // A row: the cursor walks to it and Enter stages it, which is
                // what a second click on it does.
                hit::Target::Rail(..) => {
                    let mut view = View { focus: Focus::Agents, ..Default::default() };
                    press(&mut view, key(event::KeyCode::Char('j')), &Keymap::default());
                    let flow = press(&mut view, key(event::KeyCode::Enter), &Keymap::default());
                    assert!(matches!(flow, Flow::StageSelected), "{flow:?}");
                    assert!(alt('a').is_some() && alt('p').is_some() && alt('g').is_some());
                }
                // A verb *is* a key — the click resolves to the letter the
                // footer drew, and goes through the same dispatch it does.
                hit::Target::AgentsVerb(_) => {
                    for pinned in [true, false] {
                        assert!(crate::verbs::agents_verbs(pinned).iter().any(|v| v.key == 'a'));
                    }
                }
                hit::Target::ProcsVerb(_) => {
                    assert!(crate::verbs::procs_verbs().iter().any(|v| v.key == 't'));
                }
                hit::Target::ChangesVerb(_) => {
                    assert!(crate::verbs::changes_help_verbs().iter().any(|v| v.key == 's'));
                }
                // The gauges are not a list the cursor can reach, so this is the
                // one target whose key had to be invented rather than found.
                hit::Target::System(_) => {
                    assert_eq!(
                        prefix("S"),
                        Some(Action::View(ViewVerb::Monitor { gpu: false })),
                        "the SYSTEM gauges are clickable and must not be click-only"
                    );
                    assert_eq!(prefix("Y"), Some(Action::View(ViewVerb::Monitor { gpu: true })));
                }
                // Clicking the stage puts the keyboard in it. Enter does the
                // same — `Flow::StageSelected` is what the loop turns into
                // `focus = Stage` — and `alt-esc` is the way back out.
                hit::Target::Stage(..) => {
                    let mut view = View { focus: Focus::Refs, ..Default::default() };
                    press(&mut view, key(event::KeyCode::Enter), &Keymap::default());
                    assert_eq!(view.focus, Focus::Stage, "Enter did not reach the stage");
                    assert_eq!(alt_verb(event::KeyCode::Esc), Some(ViewVerb::Focus(Focus::Agents)));
                }
                // Blank chrome. There is nothing to reach, which is the one
                // honest answer this test accepts.
                hit::Target::Nothing => {}
            }
        }
    }

    /// Every row the menu draws has somewhere to go.
    ///
    /// Dispatch matches on the row *index*, so a row added to the table without
    /// an arm would fall into the catch-all and quietly do the wrong thing.
    /// This walks the table instead of restating it.
    #[test]
    fn every_menu_row_is_within_its_targets_table() {
        use chrome::MenuTarget;
        for target in [
            MenuTarget::Agent {
                daemon: 0,
                workspace: butai_protocol::SessionId(1),
                pane: PaneId(1),
            },
            MenuTarget::Process(PaneId(2)),
            MenuTarget::Tab(0),
            MenuTarget::RemoteTab(0),
        ] {
            let rows = target.rows();
            assert!(!rows.is_empty(), "{target:?} offers nothing");
            assert!(!target.title().is_empty());
            // The destructive ones are the ones that take something away and
            // cannot be got back by pressing the same key again.
            let destructive = rows.iter().filter(|(_, d)| *d).count();
            match target {
                MenuTarget::Process(_) => assert_eq!(destructive, 0, "{target:?}"),
                _ => assert!(destructive >= 1, "{target:?} marks nothing destructive"),
            }
        }
    }

    /// A click selects; the same click again stages.
    ///
    /// One gesture with two meanings, and the second is the one that costs
    /// something — so it must not fire on the first click, which is how you
    /// *look* at a rail without leaving what is on the stage.
    #[test]
    fn clicking_a_row_selects_it_and_clicking_it_again_stages_it() {
        let mut view = View::default();
        assert!(matches!(
            run_click(hit::Target::Rail(Focus::Agents, 2), &mut view, 1, None),
            Flow::Continue
        ));
        assert_eq!((view.focus, view.agent_sel), (Focus::Agents, 2));

        // A different row is still only a selection.
        assert!(matches!(
            run_click(hit::Target::Rail(Focus::Agents, 3), &mut view, 1, None),
            Flow::Continue
        ));
        assert_eq!(view.agent_sel, 3);

        // The row already under the cursor is the one that stages.
        assert!(matches!(
            run_click(hit::Target::Rail(Focus::Agents, 3), &mut view, 1, None),
            Flow::StageSelected
        ));

        // On CHANGES the second click opens a diff instead: its rows are files,
        // not panes, so there is nothing to stage.
        view.focus = Focus::Changes;
        view.changes_sel = 1;
        assert!(matches!(
            run_click(hit::Target::Rail(Focus::Changes, 1), &mut view, 1, None),
            Flow::OpenSelectedDiff
        ));
    }

    /// The words under the AGENTS and PROCESSES lists do what they say.
    ///
    /// Both rails advertised `x:kill` — AGENTS since it was written, PROCESSES
    /// with `r:restart` beside it — and neither key was bound anywhere. Worse,
    /// the whole hint line was one hit box: clicking the word `x:kill` spawned
    /// an agent, because the *first* verb on the line was what the click meant.
    /// This walks the table both the footer and the hit-test now read.
    #[test]
    fn every_left_rail_verb_does_what_the_word_under_the_list_says() {
        let ws = ws_with_agents();
        let mut view = View::default();
        let run = |view: &mut View, t| run_click(t, view, 1, Some(&ws));

        assert!(matches!(run(&mut view, hit::Target::AgentsVerb('x')), Flow::KillSelected));
        assert_eq!(view.focus, Focus::Agents, "a verb acts on its own section's cursor");
        assert!(matches!(run(&mut view, hit::Target::AgentsVerb('a')), Flow::NewAgent));
        assert!(matches!(run(&mut view, hit::Target::AgentsVerb('A')), Flow::PickAgent));

        assert!(matches!(
            run(&mut view, hit::Target::ProcsVerb('t')),
            Flow::RunProcess { then: Spawned::Stage, .. }
        ));
        assert_eq!(view.focus, Focus::Processes);
        // This workspace has agents and no processes, so the two verbs that act
        // on a row have no row here — and say so by doing nothing rather than
        // asking the daemon to kill a pane nobody named.
        assert!(matches!(run(&mut view, hit::Target::ProcsVerb('x')), Flow::Continue));
        assert!(matches!(run(&mut view, hit::Target::ProcsVerb('r')), Flow::Continue));

        // The keyboard is the same table, so `x` on a focused rail kills the
        // row the cursor is on — and on an empty rail it is not an error.
        let kill =
            |focus, rows| handle_rail_key(key(event::KeyCode::Char('x')), focus, rows, false);
        assert!(matches!(kill(Focus::Agents, 2), Some(Flow::KillSelected)));
        assert!(matches!(kill(Focus::Processes, 1), Some(Flow::KillSelected)));
        assert!(kill(Focus::Agents, 0).is_none());
        // And a key no section lists is not the rail's, so it falls through to
        // the workbench-wide bindings rather than being swallowed here.
        assert!(handle_rail_key(key(event::KeyCode::Char('q')), Focus::Agents, 2, false).is_none());
        assert!(handle_rail_key(key(event::KeyCode::Char('x')), Focus::Changes, 2, false).is_none());
    }

    /// The Docker page's own verbs stay on the Docker page.
    ///
    /// `Spawned` is the reason there is a distinction at all: once a spawned
    /// pane goes to the stage, `r` and `x` here would close the page you are
    /// working on to show a `docker restart` that has already exited, while
    /// `s` — a shell you asked for in order to type in it — must still go.
    #[test]
    fn restarting_a_container_does_not_throw_away_the_page_you_are_on() {
        use butai_protocol::api::{ContainerDto, StackDto, SysDto};
        let sys = SysDto {
            stacks: vec![StackDto {
                label: "web".into(),
                project: "web".into(),
                workdir: "/tmp/proj".into(),
                running: 1,
                total: 1,
                containers: vec![ContainerDto { name: "web-1".into(), state: "running".into() }],
            }],
            ..Default::default()
        };
        let ws = WorkspaceDetail { cwd: "/tmp/proj".into(), ..ws_with_agents() };
        let mut docker = chrome::Docker::default();
        let mut view = View { page: Page::Docker, ..Default::default() };
        let press = |view: &mut View, docker: &mut chrome::Docker, c: char| {
            handle_docker_key(key(event::KeyCode::Char(c)), view, docker, &sys, Some(&ws))
        };

        for verb in ['r', 'x'] {
            let flow = press(&mut view, &mut docker, verb).expect("the row offers {verb}");
            assert!(
                matches!(flow, Flow::RunProcess { then: Spawned::Rail, .. }),
                "`{verb}` should leave the page alone, got {flow:?}"
            );
            assert_eq!(view.page, Page::Docker);
        }
        // Enter follows the logs in this page's own column, which is neither.
        let flow = press(&mut view, &mut docker, '\0');
        assert!(flow.is_none(), "a nul is not a docker verb");
        let enter =
            handle_docker_key(key(event::KeyCode::Enter), &mut view, &mut docker, &sys, Some(&ws));
        assert!(matches!(enter, Some(Flow::RunProcess { then: Spawned::Follow(_), .. })));
        // And the one that is a shell goes to the stage like every other shell.
        let flow = press(&mut view, &mut docker, 's').expect("a one-container stack offers s");
        assert!(matches!(flow, Flow::RunProcess { then: Spawned::Stage, .. }), "{flow:?}");
    }

    /// A pane that was just made is the pane you are looking at.
    ///
    /// [`chrome::staged_pane`] prefers this client's own choice over the
    /// daemon's, so once anything had been staged by hand every later spawn
    /// went into the rail behind an unchanged stage — `[+ term]`, a click on
    /// the CPU gauge and a spawned agent all appeared to do nothing at all.
    #[test]
    fn a_newly_spawned_pane_goes_on_the_stage() {
        let mut view = View { staged: Some(PaneId(10)), page: Page::Files, ..Default::default() };
        stage_new_pane(&mut view, PaneId(42));
        assert_eq!(view.staged, Some(PaneId(42)));
        assert_eq!(view.focus, Focus::Stage, "a new shell is one you can type in");
        // And on the agents page, or it would be behind whatever full-screen page
        // the spawn was reached from.
        assert_eq!(view.page, Page::Agents);
    }

    /// Choosing a workspace on BOOTH goes to that workspace.
    ///
    /// BOOTH is the one page that is not a view *of* the active workspace, so it
    /// is the one page where switching tab under it says nothing on screen. It
    /// was reported as exactly that: "if I click on workspace back again, it
    /// doesn't exit home, only if I click home again" — the chip took the tab
    /// but not the bracket, because BOOTH still had it.
    ///
    /// Every other page keeps its own behaviour: the same view, re-pointed at
    /// the project you just chose, which is what `aec1e21` is for.
    #[test]
    fn choosing_a_workspace_on_booth_leaves_it() {
        let mut view = View { page: Page::Booth, tab: 0, ..Default::default() };
        assert!(matches!(run_click(hit::Target::Tab(1), &mut view, 3, None), Flow::Continue));
        assert_eq!(view.tab, 1, "the chip should still select its workspace");
        assert_eq!(view.page, Page::Agents, "BOOTH kept the screen after a workspace was chosen");

        // A tree page is a view of a workspace, so it stays up and re-points.
        for page in [Page::Files, Page::Docs, Page::Docker] {
            let mut view = View { page, tab: 0, ..Default::default() };
            run_click(hit::Target::Tab(2), &mut view, 3, None);
            assert_eq!(view.tab, 2);
            assert_eq!(view.page, page, "{page:?} should survive a tab change");
        }

        // The keyboard routes go the same way — `alt-1..9` and `alt-<`/`alt->`
        // are the same choice made with different fingers.
        for code in
            [event::KeyCode::Char('2'), event::KeyCode::Char('>'), event::KeyCode::Char('<')]
        {
            let mut view = View { page: Page::Booth, tab: 0, ..Default::default() };
            // Both halves, because the bug was in the seam between them: the
            // key produced the right move, and the arm that carried it out
            // went round `select_tab`. `go_tab` is that arm.
            let Some(Flow::GoTab(m)) = alt_binding(code, &mut view) else {
                panic!("alt-{code:?} should ask for a tab");
            };
            go_tab(&mut view, m, 3);
            assert_eq!(view.tab, if code == event::KeyCode::Char('<') { 2 } else { 1 });
            assert_eq!(view.page, Page::Agents, "alt-{code:?} left the screen on BOOTH");
        }

        // But being *thrown* off a tab is not a choice: `select_tab` is only on
        // the paths where the user asked, so a tab closing under you leaves the
        // page alone.
        let mut view = View { page: Page::Booth, tab: 2, ..Default::default() };
        view.tab = view.tab.saturating_sub(1);
        reset_sel(&mut view);
        assert_eq!(view.page, Page::Booth, "a tab closing should not move the screen");
    }

    /// A click on a tab that is not there does nothing.
    ///
    /// The chips are drawn from a list that can shrink between the paint and
    /// the click — a workspace closing on another client is enough — and
    /// `view.tab` indexes that list everywhere else.
    #[test]
    fn a_click_on_a_tab_that_has_gone_is_ignored() {
        let mut view = View { tab: 1, ..Default::default() };
        run_click(hit::Target::Tab(5), &mut view, 2, None);
        assert_eq!(view.tab, 1, "the cursor followed a tab that does not exist");
        run_click(hit::Target::Tab(0), &mut view, 2, None);
        assert_eq!(view.tab, 0);
    }

    /// Switching tabs drops the chosen pane.
    ///
    /// The stage is this client's viewport, and a pane chosen on one project
    /// is not in another — following it across would leave the stage empty
    /// with nothing on screen to explain why.
    #[test]
    fn changing_tab_lets_go_of_the_pane_this_client_chose() {
        let mut view = View { staged: Some(PaneId(7)), ..Default::default() };
        reset_sel(&mut view);
        assert_eq!(view.staged, None);
    }

    /// The picker opens on the pinned agent.
    ///
    /// A pin the daemon has never heard of is not an error: the config is the
    /// client's and the list is the daemon's, so the two disagree the moment a
    /// second machine joins the tab bar — and a picker that opened on nothing
    /// would be worse than one that opened on the top.
    #[test]
    fn the_agent_picker_starts_on_the_pinned_row() {
        let items: Vec<String> =
            ["claude", "codex", "gemini"].iter().map(|s| s.to_string()).collect();
        assert_eq!(pinned_row(&items, Some("gemini")), 2);
        assert_eq!(pinned_row(&items, None), 0);
        assert_eq!(pinned_row(&items, Some("not-here")), 0, "a stale pin is not an error");
        assert_eq!(pinned_row(&[], Some("claude")), 0);
    }

    /// A Ctrl-modified letter is not the bare letter.
    ///
    /// Found by the configured-prefix test below: the workbench's own arms match
    /// on `(code, alt)`, so `C-b` fell through to `b` and opened the branch
    /// picker on a client whose prefix was `C-a`. Every unshifted letter the
    /// workbench binds had the same hole.
    #[test]
    fn a_ctrl_key_is_not_the_letter_under_it() {
        let keymap = Keymap::from_config("C-a", &Default::default()).0;
        for c in ['b', 'a', 'n', 'g', 'q'] {
            let mut view = View::default();
            let flow = press(&mut view, ctrl(c), &keymap);
            assert!(matches!(flow, Flow::Continue), "C-{c} acted as a bare {c}");
            assert!(view.overlay.is_none(), "C-{c} opened an overlay");
        }
    }

    /// A configured prefix is the one that works, and the one shown.
    #[test]
    fn a_configured_prefix_replaces_the_default() {
        let (keymap, warnings) = Keymap::from_config("C-a", &Default::default());
        assert!(warnings.is_empty(), "{warnings:?}");
        let mut view = View { prefix: keys::key_label(&keymap.prefix), ..Default::default() };
        assert_eq!(view.prefix, "^A");
        // The default prefix is now an ordinary key, and reaches the pane.
        assert!(matches!(press(&mut view, ctrl('b'), &keymap), Flow::Continue));
        assert!(!view.prefix_armed, "C-b armed a client configured for C-a");
        press(&mut view, ctrl('a'), &keymap);
        assert!(view.prefix_armed, "C-a did not arm");
    }

    /// `q` and `Esc` close the page. They must not reach the workbench's detach:
    /// reading a diff and pressing the universal "close this" should not end the
    /// session.
    #[test]
    fn closing_a_page_is_not_detaching() {
        for code in [event::KeyCode::Char('q'), event::KeyCode::Esc] {
            let mut view = View { page: Page::Diff, ..Default::default() };
            let mut diff = DiffView::default();
            let flow = handle_diff_key(key(code), &mut view, &mut diff);
            assert!(matches!(flow, Some(Flow::Continue)), "{code:?} escaped the diff page");
            assert_eq!(view.page, Page::Agents);

            let mut view = View { page: Page::Files, ..Default::default() };
            let mut files = Files::default();
            let flow = handle_files_key(key(code), &mut view, &mut files);
            assert!(matches!(flow, Some(Flow::Continue)), "{code:?} escaped the Files page");
            assert_eq!(view.page, Page::Agents);
        }
    }

    /// `x` on the Files page asks before it deletes, and the box opens on "no".
    ///
    /// Three things are being pinned. The keystroke does not delete — it only
    /// opens the question — the *answer* is what produces the flow, and a
    /// directory is refused before the question is asked at all.
    #[test]
    fn deleting_a_file_asks_first_and_never_a_directory() {
        let entries = vec![
            chrome::FileEntry {
                name: "src".into(),
                path: "src".into(),
                is_dir: true,
                changed: false,
            },
            chrome::FileEntry {
                name: "a.rs".into(),
                path: "src/a.rs".into(),
                is_dir: false,
                changed: false,
            },
        ];

        // The directory row: no box, and nothing to answer.
        let mut view = View { page: Page::Files, ..Default::default() };
        let mut files = Files { entries: entries.clone(), sel: 0, ..Default::default() };
        let flow = handle_files_key(key(event::KeyCode::Char('x')), &mut view, &mut files);
        assert!(matches!(flow, Some(Flow::Continue)), "the key should be claimed either way");
        assert!(view.overlay.is_none(), "a directory opened a delete box");

        // The file row: a box, opened on the safe answer.
        let mut files = Files { entries, sel: 1, ..Default::default() };
        handle_files_key(key(event::KeyCode::Char('x')), &mut view, &mut files);
        let Some(Overlay::Confirm(c)) = &view.overlay else {
            panic!("x on a file did not ask: {:?}", view.overlay)
        };
        assert!(!c.yes, "the box opened on the destructive answer");
        assert_eq!(c.kind, chrome::ConfirmKind::DeleteFile { path: "src/a.rs".into() });

        // Answering no throws the question away and deletes nothing.
        assert!(matches!(confirm(&mut view), Flow::Continue));
        assert!(view.overlay.is_none(), "the box stayed up");

        // Only "yes" reaches the route.
        view.overlay = Some(Overlay::Confirm(chrome::ConfirmOverlay {
            title: "DELETE".into(),
            header: "delete src/a.rs".into(),
            yes: true,
            kind: chrome::ConfirmKind::DeleteFile { path: "src/a.rs".into() },
        }));
        match confirm(&mut view) {
            Flow::DeleteFile(path) => assert_eq!(path, "src/a.rs"),
            other => panic!("a confirmed delete produced {other:?}"),
        }
    }

    /// In line-select the two are different questions: Esc backs out of the
    /// selection, and only then does it close the page.
    #[test]
    fn esc_backs_out_of_line_select_before_it_closes_the_page() {
        let mut view = View { page: Page::Diff, ..Default::default() };
        let mut diff = DiffView::new(
            DiffKind::Unstaged { path: None },
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,2 +1,2 @@\n-x\n+y\n",
        );
        diff.line_select();
        assert_eq!(diff.mode, DiffMode::Lines);
        handle_diff_key(key(event::KeyCode::Esc), &mut view, &mut diff);
        assert_eq!(diff.mode, DiffMode::Read, "esc should have cancelled line-select");
        assert_eq!(view.page, Page::Diff, "and left the page open");
        handle_diff_key(key(event::KeyCode::Esc), &mut view, &mut diff);
        assert_eq!(view.page, Page::Agents);
    }

    /// Enter applies the picked lines, and does *nothing* in read mode — a
    /// partial-staging tool must not quietly read "apply" as "the whole hunk".
    #[test]
    fn enter_applies_only_in_line_select() {
        let mut view = View { page: Page::Diff, ..Default::default() };
        let mut diff = DiffView::new(
            DiffKind::Unstaged { path: None },
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,2 +1,2 @@\n-x\n+y\n",
        );
        assert!(
            handle_diff_key(key(event::KeyCode::Enter), &mut view, &mut diff).is_none(),
            "enter should mean nothing while reading"
        );
        assert!(matches!(
            handle_diff_key(key(event::KeyCode::Char(' ')), &mut view, &mut diff),
            Some(Flow::ApplyDiff { discard: false })
        ));
        diff.line_select();
        assert!(matches!(
            handle_diff_key(key(event::KeyCode::Enter), &mut view, &mut diff),
            Some(Flow::ApplyDiff { discard: false })
        ));
    }

    /// The retry used to ride on the repaint. Anything animating makes that
    /// every 120ms, and every failure wrote its own line into the footer — so a
    /// machine that was simply off turned the one place that could have
    /// explained the situation into a strobe, over a stage that had already
    /// gone black.
    #[test]
    fn a_lost_stage_retries_on_a_clock_rather_than_on_every_repaint() {
        let (mut stage, _sent) = fake_stage();
        let t0 = Instant::now();
        assert!(!stage.reopen_due(t0), "a live stage has nothing to re-open");

        stage.mark_lost(t0);
        assert!(
            stage.reopen_due(t0),
            "the first attempt is immediate — a restart comes straight back"
        );
        assert!(!stage.reopen_due(t0), "not twice in the same instant");
        assert!(
            !stage.reopen_due(t0 + STAGE_RETRY - Duration::from_millis(1)),
            "not before the wait is up"
        );
        assert!(stage.reopen_due(t0 + STAGE_RETRY), "and again once it is");

        // Ten seconds of repainting at the fast tick is ~83 frames; the clock
        // above is what makes that ten connects rather than 83.
        let mut attempts = 0;
        for ms in 0..10_000u64 {
            if stage.reopen_due(t0 + Duration::from_millis(ms)) {
                attempts += 1;
            }
        }
        assert!(attempts <= 11, "{attempts} attempts in ten seconds is a retry per repaint");
    }

    /// **The age is time since the link went, not since the last failed retry.**
    /// Restarting it on each attempt would leave the notice reading "down 0s"
    /// for as long as the machine stayed away, which is the one number on it
    /// worth reading.
    #[test]
    fn the_age_on_the_notice_counts_from_the_drop() {
        let (mut stage, _sent) = fake_stage();
        let t0 = Instant::now();
        stage.mark_lost(t0);
        let first = stage.lost.expect("marked lost");
        stage.mark_lost(t0 + Duration::from_secs(30));
        assert_eq!(stage.lost, Some(first), "a second loss restarted the clock");
    }

    /// A pane that exited and a link that dropped arrive as the same absence of
    /// bytes, and telling them apart is the whole fix: one means "there is
    /// nothing to show" and the other means "we stopped being told".
    ///
    /// The cells are the evidence. A stage that is merely unreachable keeps
    /// them, because the program on the far machine is still running and its
    /// last frame is the best picture of it anyone here has.
    #[test]
    fn a_dropped_link_keeps_the_last_frame_and_a_closed_pane_does_not() {
        let (mut stage, _sent) = fake_stage();
        put_str_test(&mut stage.buf, "hello");
        assert!(stage.has_frame(), "the fixture should have cells to lose");

        stage.mark_lost(Instant::now());
        assert!(stage.lost.is_some(), "an end of stream must mark the stage, not clear it");
        assert!(stage.has_frame(), "the last frame was thrown away");
        assert!(stage.cursor.is_none(), "the caret stayed on a screen nobody is updating");

        // And the empty case is honest about being empty: a stage opened onto a
        // machine that is already down has no photograph to point at.
        let down = Stage::down(PaneId(1), Rect::new(0, 0, 10, 3), Instant::now());
        assert!(!down.has_frame());
    }

    /// Write `s` into a stage buffer's first row, the way a frame would.
    fn put_str_test(buf: &mut Buffer, s: &str) {
        for (x, ch) in s.chars().enumerate() {
            if let Some(cell) = buf.cell_mut((x as u16, 0)) {
                cell.set_char(ch);
            }
        }
    }
}
