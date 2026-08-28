//! `ServerCore`: the single-owner actor for all mutable daemon state.
//!
//! v2 workbench model: workspaces (one per project) with fixed *roles* for the
//! rails — agents, processes, system on the left, changes on the right —
//! around a single stage. Every mutation flows through one event loop;
//! rendering is coalesced to ~60fps and shipped as damage diffs.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::config::Config;
use butai_protocol::api::{
    AgentDto, AgentState, ApiEvent, ApiReply, ApiRequest, BrowseDto, BrowseEntry, ContainerDto,
    DiffDto, DiskDto, FileDto, GitOp, GitOpDto, GpuDto, NetDto, NotificationDto, NotificationKind,
    NotificationsDto, OutputFormat, OutputSource, PaneOutputDto, ProcessDto, StackDto, SysDto,
    TreeDto, TreeEntry, TreeFilter, UsageDto, WorkspaceDetail, WorkspaceSummary,
};
use butai_protocol::{
    AttachTarget, ClientMsg, Command, FrameUpdate, InputEvent, MouseButton, PaneId, ServerMsg,
    SessionId, SessionInfo, MAX_NOTICE_CHARS, MAX_PUT_FILE_BYTES, PROTOCOL_VERSION,
};
use ratatui::buffer::Buffer;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::pane::term_emu::RowFormat;
use crate::pane::terminal::{decode_dump, encode_dump, Detect, PaneDump, SpawnSpec, TerminalPane};
use crate::pane::PaneState;
use crate::render;
use crate::workbench::{AgentMeta, ProcMeta, SysStats, Workspace};

/// `(rows, cols)` for a pane spawned into a workspace no client has drawn.
///
/// A conventional terminal, deliberately: the real size is a fact about a
/// window, and there is no window. The first client to look at the pane
/// reports what it actually is and the PTY is resized then.
const UNWATCHED_PANE_SIZE: (u16, u16) = (24, 80);

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

/// After an agent's "working" marker disappears and its output goes quiet, wait
/// this long before declaring it finished (`working -> finished`). Debounces
/// brief lulls so a single turn never reads as multiple finishes.
const AGENT_SETTLE: Duration = Duration::from_secs(3);

/// A working run that never showed a busy marker must have lasted at least this
/// long to count as a turn worth a "finished" notification. Marker-less bursts
/// shorter than this are repaint noise (a resize from another client opening the
/// pane, say), and announcing them is pure spam. Runs *with* a marker notify
/// however short they were — the marker is proof the agent really was working.
const MIN_TURN: Duration = Duration::from_secs(3);

/// Cap on the retained notification feed (per daemon run). Clients drain by
/// sequence number; anything older than this many items is dropped.
const NOTIF_HISTORY: usize = 256;

/// Adaptive agent re-check bounds. A busy/settling agent is re-scanned every
/// tick (`MIN`); a stable one backs off by doubling up to `MAX` so long-idle
/// panes cost almost nothing.
const AGENT_CHECK_MIN: Duration = Duration::from_secs(1);
const AGENT_CHECK_MAX: Duration = Duration::from_secs(30);

/// Adaptive bounds for re-probing a non-git workspace (see
/// [`ServerCore::attach_new_repos`]). A `git init` should be noticed promptly,
/// but a directory that is simply not a repo must not be re-walked forever —
/// on a network mount each failed probe is a full parent-directory stat walk.
const REPO_PROBE_MIN: Duration = Duration::from_secs(2);
const REPO_PROBE_MAX: Duration = Duration::from_secs(60);

/// How many untracked files a single diff will print in full.
///
/// Untracked files have no side in the index for `git diff` to compare against,
/// so each one costs its own `git diff --no-index` (see
/// [`ServerCore::untracked_patch`]). That is nothing for the handful an agent
/// writes, and a fork per file for a tree that has never been committed — where
/// a whole-section diff would otherwise return the entire worktree as one
/// patch. CHANGES still lists every one of them.
const UNTRACKED_DIFF_LIMIT: usize = 200;

/// One pass of event handling plus rendering should stay well inside a frame.
/// Past this, the delay is visible as stalled output and dropped frames in
/// *every* pane, so it is worth a log line naming the culprit.
const SLOW_ITERATION: Duration = Duration::from_millis(50);

/// How soon after a restore an agent's exit still counts as "it could not
/// reopen its conversation" rather than "it ran and then stopped".
///
/// The failure being caught is a refusal to start at all, which every CLI
/// reports in well under a second. The window is generous against a cold cache
/// or a loaded machine, and short enough that an agent the user quits by hand
/// is never mistaken for one.
const RESUME_RETRY_WINDOW: Duration = Duration::from_secs(10);

pub type ClientId = u64;

/// Per-agent stable-status tracking that backs the notification feed. Kept
/// separate from the raw per-tick `Attention` so short output lulls within a
/// turn don't masquerade as the agent finishing.
struct AgentTrack {
    /// The debounced state we last published (and notified from).
    state: AgentState,
    /// Exit code once the agent has exited (so we notify the exit exactly once).
    exited: Option<u32>,
    /// When the pane first went quiet after working; drives the settle timer.
    quiet_since: Option<Instant>,
    /// When the current working run began; `None` when not working. With
    /// [`saw_marker`](Self::saw_marker) it decides whether a run was a real
    /// turn worth a "finished" notification.
    working_since: Option<Instant>,
    /// Whether a busy marker was ever visible during the current working run —
    /// proof the agent really was mid-turn rather than just repainting.
    saw_marker: bool,
    /// False until the first observation seeds a baseline — the baseline pass
    /// never notifies, so connecting to an already-finished agent stays silent.
    seeded: bool,
    /// Whether the your-move edge this agent last crossed is still unlooked-at,
    /// published as [`AgentDto::unread`].
    ///
    /// Lives here rather than on the pane because it is workbench bookkeeping,
    /// not terminal state: the emulator's job is what is on the screen, and
    /// "has the user read this" is a fact about the *user*. It also keeps the
    /// bit next to the debounced `state` whose edges set it — the two would
    /// drift the moment they lived in different structs.
    unread: bool,
    /// Adaptive re-check interval: grows while the agent is stable, snaps back
    /// to [`AGENT_CHECK_MIN`] on any change or fresh output.
    backoff: Duration,
    /// Next tick this agent is due for a full recompute.
    next_check: Instant,
}

impl AgentTrack {
    /// A fresh, unseeded track due for its first check immediately. Every agent
    /// gets one the moment it is spawned, so [`ServerCore::agent_dto`] always has
    /// a debounced state to publish and never has to guess from raw signals.
    fn new(now: Instant) -> Self {
        Self {
            state: AgentState::Idle,
            exited: None,
            quiet_since: None,
            working_since: None,
            saw_marker: false,
            seeded: false,
            unread: false,
            backoff: AGENT_CHECK_MIN,
            next_check: now,
        }
    }

    /// Clear the per-run bookkeeping: no working run is in flight anymore.
    fn end_run(&mut self) {
        self.quiet_since = None;
        self.working_since = None;
        self.saw_marker = false;
    }
}

/// (program, args, env, title label) for an exec-style pane spawn.
type ProgramSpec<'a> =
    (&'a str, &'a [String], &'a [(String, String)], Option<&'a str>, Option<Detect>);

/// Bytes read from a pane's PTY, carried on a dedicated **bounded** channel
/// (see [`OUTPUT_CHANNEL_CAP`]) rather than as an [`Event`]. Keeping high-volume
/// output off the control channel means a flooding process applies backpressure
/// to its own reader thread instead of burying control events (kill-server,
/// input) behind an unbounded backlog.
pub type OutputTx = tokio::sync::mpsc::Sender<(PaneId, Vec<u8>)>;
/// Receiving half of the PTY output channel; drained by [`ServerCore::run`].
pub type OutputRx = tokio::sync::mpsc::Receiver<(PaneId, Vec<u8>)>;

/// Bounded capacity of the per-daemon PTY output channel. When full, reader
/// threads block on send, which stalls PTY draining, fills the kernel pipe
/// buffer, and throttles the child — capping CPU and memory under a flood.
pub const OUTPUT_CHANNEL_CAP: usize = 256;

/// Maximum PTY-output bytes fed to emulators per drain pass. Feeding is
/// synchronous, so this bounds how long the run loop can go without checking the
/// (biased-first) control channel — keeping kill-server/input responsive even
/// while a pane floods. Leftover output stays queued for the next turn.
const OUTPUT_DRAIN_BYTES: usize = 128 * 1024;

/// Everything that can happen to the daemon, funneled into one control channel.
/// PTY output travels separately (see [`OutputTx`]).
pub enum Event {
    /// A new client connection; `tx` is its outbound message queue.
    ClientConnected(ClientId, UnboundedSender<ServerMsg>),
    Client(ClientId, ClientMsg),
    ClientGone(ClientId),
    PaneExited(PaneId, u32),
    /// A git status scan (run off-thread) finished for a changes pane.
    GitRefreshed(PaneId, crate::pane::git::GitSnapshot),
    /// An off-thread probe finished for a workspace that had no CHANGES rail.
    /// `Some(pane)` when the cwd turned out to be a repository.
    RepoProbed(SessionId, Option<crate::pane::git::GitPane>),
    /// Fresh machine telemetry from the sampler task.
    Sys(SysStats),
    /// A fresh account-standing roster from the usage sampler.
    Usage(UsageDto),
    /// A running git operation printed something. Throttled by the op task, not
    /// here — see [`crate::git_op`].
    GitOpProgress {
        ws: SessionId,
        seq: u64,
        line: String,
    },
    /// A git operation finished. Keyed by repository root rather than workspace
    /// because the write lock is: two workspaces can be open on one worktree.
    GitOpDone {
        root: PathBuf,
        ws: SessionId,
        seq: u64,
        result: Result<String, String>,
    },
    /// Animation clock for rail marquees (monotonic phase).
    Tick(u64),
    /// Faster animation clock for the ALL AGENTS panel's sprites.
    FastTick(u64),
    /// One-shot request from the HTTP/REST API handler (Docker-style socket).
    Api(ApiRequest, tokio::sync::oneshot::Sender<ApiReply>),
    /// An off-thread upload finished; the workspace's rails need a repaint.
    WorkspaceWritten(SessionId),
    /// An off-thread probe of a new workspace's directory finished. Creating the
    /// workspace itself has to happen back here, on the actor.
    NewWorkspaceResolved {
        name: Option<String>,
        cwd: Result<PathBuf, String>,
        reply: tokio::sync::oneshot::Sender<ApiReply>,
    },
    /// A new `GET /v1/events` subscriber; `tx` receives pushed state changes.
    ApiSubscribe(UnboundedSender<ApiEvent>),
    /// A self-update finished downloading. `Ok(None)` means the daemon was
    /// already on the latest release. The reply is carried along rather than
    /// answered off-thread because the decision to shut down and the answer to
    /// the client that asked for it have to be made in one place, in that
    /// order.
    UpdateReady {
        result: Result<Option<butai_update::Staged>, String>,
        reply: tokio::sync::oneshot::Sender<ApiReply>,
    },
    /// Graceful daemon shutdown (signal handler).
    Shutdown,
}

/// A git operation in flight, or the last one to have finished, for one
/// repository.
struct GitOpState {
    ws: SessionId,
    seq: u64,
    kind: &'static str,
    running: bool,
    progress: String,
    result: Option<Result<String, String>>,
    /// Dropped when the operation ends; sending on it kills the child.
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
}

impl GitOpState {
    fn to_dto(&self) -> GitOpDto {
        GitOpDto {
            ws: self.ws,
            seq: self.seq,
            kind: self.kind.to_string(),
            running: self.running,
            progress: self.progress.clone(),
            ok: self.result.as_ref().map(|r| r.is_ok()),
            summary: match &self.result {
                Some(Ok(s)) => s.clone(),
                Some(Err(e)) => e.clone(),
                None => String::new(),
            },
        }
    }
}

/// Why a git operation could not be started. Distinct from a plain string so
/// the REST layer can map each case to its own status code and the TUI can
/// show the same sentence.
enum GitOpRefusal {
    NoRepo(SessionId),
    /// Another operation holds this repository's write lock.
    Busy(String),
    /// A user-supplied value failed validation and never became argv.
    Invalid(String),
}

impl std::fmt::Display for GitOpRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitOpRefusal::NoRepo(ws) => write!(f, "workspace {ws} is not a git repository"),
            GitOpRefusal::Busy(kind) => write!(f, "a git operation is already running: {kind}"),
            GitOpRefusal::Invalid(e) => write!(f, "{e}"),
        }
    }
}

impl From<GitOpRefusal> for String {
    fn from(r: GitOpRefusal) -> String {
        r.to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreMode {
    /// One in-process client; exit when it goes away.
    Standalone,
    /// Socket daemon; exit when the last workspace dies (if configured).
    Daemon,
}

/// One connected client, as much of it as the daemon needs to know.
///
/// Which is little: where to send messages, what it is looking at, and how big
/// that is. It used to carry a whole terminal user interface — pickers, a
/// prompt, a commit buffer, a help-modal scroll offset, an armed confirmation,
/// a selection anchor — because the daemon drew one interface per client. Every
/// one of those is now the client's own, kept beside the screen it belongs to.
///
/// What is left is a *subject* (`session` or `pane`, never both) and the state
/// of the one thing still drawn here: the pane's last frame, so the next one
/// can be a diff.
struct ClientState {
    tx: UnboundedSender<ServerMsg>,
    /// Set by a session target (`attach` / `new` / `default`). Names the
    /// workspace a command acts on; carries no screen.
    session: Option<SessionId>,
    /// Set by a `pane` target. The pane this connection streams, full-bleed,
    /// with input routed straight to it. Mutually exclusive with `session`.
    pane: Option<PaneId>,
    cols: u16,
    rows: u16,
    control: bool,
    /// What this client was last sent, to diff the next frame against. `None`
    /// makes the next one full — which is what re-pointing a `watch` needs, so
    /// the client replaces the screen rather than patching a pane it is no
    /// longer showing.
    last_frame: Option<Buffer>,
    /// Where a left-button press started, for drag text-selection over the pane.
    sel_anchor: Option<(u16, u16)>,
    /// Active selection (anchor, current) in pane cells, drawn reversed and
    /// copied to the client's clipboard on release.
    sel: Option<((u16, u16), (u16, u16))>,
}

pub struct ServerCore {
    mode: CoreMode,
    config: Config,
    workspaces: HashMap<SessionId, Workspace>,
    order: Vec<SessionId>,
    panes: HashMap<PaneId, PaneState>,
    clients: HashMap<ClientId, ClientState>,
    events_tx: UnboundedSender<Event>,
    /// Bounded sender handed to every spawned PTY reader thread. Held here so
    /// the output channel never closes while the core is alive.
    output_tx: OutputTx,
    sys: Option<SysStats>,
    /// Latest roster from [`crate::usage`]. `None` until the first sample lands,
    /// which is why `GET /v1/usage` answers with an empty list rather than
    /// blocking: a client that polls before the daemon has probed anything gets
    /// "nothing known yet", not a stall.
    usage: Option<UsageDto>,
    next_pane: u64,
    next_ws: u64,
    dirty: bool,
    /// Marquee animation phase, advanced by `Event::Tick`.
    anim: u64,
    /// Whether the last frame drew scrolling text (so ticks keep repainting).
    wants_anim: bool,
    /// Sprite animation phase, advanced by `Event::FastTick`.
    fast_anim: u64,
    /// Whether the last frame drew a moving sprite. Gates the fast tick, so
    /// the extra clock costs nothing until the ALL AGENTS panel is open with
    /// an agent actually working.
    wants_fast_anim: bool,
    had_client: bool,
    had_workspace: bool,
    shutdown: bool,
    /// Live `GET /v1/events` subscribers (pruned as they disconnect).
    api_subs: Vec<UnboundedSender<ApiEvent>>,
    /// The last [`WorkspaceDetail`] pushed for each workspace, so the frame
    /// clock only broadcasts one that actually changed. PTY output marks the
    /// workbench dirty on nearly every frame while leaving the rails identical,
    /// so without this a subscriber would get a full snapshot per workspace per
    /// frame. Only populated while something is subscribed.
    last_detail: HashMap<SessionId, WorkspaceDetail>,
    /// Agent panes we've already rung the bell for (cleared when they stop
    /// needing attention), so each transition rings once.
    agent_alerted: std::collections::HashSet<PaneId>,
    /// Debounced per-agent status, the single source of truth for the accurate
    /// `AgentState` and the notification feed.
    agent_track: HashMap<PaneId, AgentTrack>,
    /// Bounded ring of recent agent notifications (finished / exited), newest
    /// last. Clients drain it by sequence via `GET /v1/notifications?since=`.
    notifications: std::collections::VecDeque<NotificationDto>,
    /// Highest notification sequence number assigned so far (0 = none yet).
    notif_seq: u64,
    /// Where to persist the open-workspace list so it survives daemon
    /// restarts/reboots. `None` disables persistence (tests, standalone).
    session_store: Option<PathBuf>,
    /// The socket this daemon actually bound, handed to every pane as
    /// `$BUTAI_SOCKET`. Defaults to the configured path for `standalone`, which
    /// binds nothing.
    socket: PathBuf,
    /// True while replaying persisted workspaces at startup, so the recreate
    /// path doesn't rewrite the store for every workspace.
    restoring: bool,
    /// Changes panes with a git status scan currently in flight (see
    /// [`request_git_refresh`](Self::request_git_refresh)). Dedupes so the ~2s
    /// sampler tick never piles up overlapping scans on a slow repo.
    git_refreshing: std::collections::HashSet<PaneId>,
    /// Panes whose scan was asked for *while one was already running*, so it
    /// has to be run again when that one lands.
    ///
    /// Dropping the request instead — which is what happened until this
    /// existed — loses the mutation that asked for it: the in-flight scan
    /// started *before* the commit, so it reports the tree from before the
    /// commit, and nothing schedules another. The rail then sits showing files
    /// as staged that are already committed, until something unrelated happens
    /// to trigger a refresh.
    git_refresh_again: std::collections::HashSet<PaneId>,
    /// The git operation running (or last finished) per **repository root**.
    ///
    /// Keyed by root rather than by workspace because it doubles as the write
    /// lock, and the thing that must not have two writers is the repository:
    /// two workspaces can be open on one worktree, and letting them interleave
    /// a rebase is how work gets lost.
    git_ops: HashMap<PathBuf, GitOpState>,
    /// Monotonic id for git operations, so a client can tell a new one from an
    /// update to the one it is already watching.
    git_op_seq: u64,
    /// Workspaces with a "did this become a git repo yet?" probe in flight (see
    /// [`attach_new_repos`](Self::attach_new_repos)).
    repo_probing: std::collections::HashSet<SessionId>,
    /// Earliest time to re-probe a workspace that is not a repository, and the
    /// current backoff for it. Without this, a plain non-git workspace on a slow
    /// mount pays a failed parent-directory walk every ~2s forever.
    repo_probe_next: HashMap<SessionId, (Instant, Duration)>,
    /// Agent panes launched to reopen a conversation, which have not yet been
    /// given their one fallback start. See
    /// [`retry_failed_resume`](Self::retry_failed_resume).
    resume_retry: std::collections::HashSet<PaneId>,
    /// Counter prefixed onto pasted scratch file names. Daemon-wide rather than
    /// per-workspace so the name is unique without consulting the directory,
    /// and monotonic so the names sort by age for pruning.
    put_seq: u64,
    /// A self-update is downloading. One at a time: the second would stage a
    /// second temp file beside the same binary and race the first to rename
    /// over it.
    updating: bool,
    /// A verified new binary, waiting for this daemon to finish shutting down
    /// so it can be swapped in. Set only by the update path, and the reason
    /// [`ServerCore::run`] has a return value at all — see `daemon::run`.
    restart_into: Option<butai_update::Staged>,
    /// Saved workspaces this start could not rebuild because their directory
    /// did not resolve. Carried so [`persist_session`](Self::persist_session)
    /// can write them back out — see [`restore_session`](Self::restore_session)
    /// for why losing them is not an option.
    deferred: Vec<PersistedWorkspace>,
}

/// The persisted daemon session: which project directories were open, in what
/// order, and enough about each one to bring the work back — the agents and
/// processes that were running, and which pane held the stage.
///
/// Terminal output is deliberately *not* in here. It is bulk binary that turns
/// over every few seconds, while this file is rewritten synchronously every
/// time a workspace opens or closes; the two live apart so each can be written
/// on the schedule that suits it. Dumps go under
/// [`panes_dir`](butai_protocol::paths::panes_dir), one directory per workspace.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct SessionState {
    workspaces: Vec<PersistedWorkspace>,
}

/// Every field past `cwd` is `#[serde(default)]`, which is what lets a
/// `session.json` written before any of this existed still load: it restores
/// exactly the workspaces it always did, then starts recording the rest.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct PersistedWorkspace {
    name: String,
    cwd: PathBuf,
    /// Directory under `panes/` holding this workspace's output dumps.
    #[serde(default)]
    key: String,
    #[serde(default)]
    agents: Vec<PersistedAgent>,
    /// Every process, including the shell each workspace opens with and any
    /// started by hand — not just the `.butai.toml` ones. Restore replays this
    /// list *instead of* the workspace file's autostart block, so a process
    /// removed from `.butai.toml` since does not come back and one started by
    /// hand is not lost.
    #[serde(default)]
    processes: Vec<PersistedProcess>,
    /// Dump name of the pane that held the stage, so the workspace comes back
    /// looking at what you were looking at.
    #[serde(default)]
    stage: Option<String>,
}

/// One agent pane. The `[[agents]]` name is stored rather than the command it
/// resolved to: command, args and detection patterns are config, so an agent
/// whose launcher has been edited since comes back under the new definition
/// rather than a stale copy.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct PersistedAgent {
    agent: String,
    dump: String,
    /// The conversation this pane was holding, so the restored process reopens
    /// *its own* rather than whichever one in this directory happens to be the
    /// most recent (see [`AgentMeta::session_id`]).
    ///
    /// `#[serde(default)]`, so a session file written before conversations were
    /// named still loads — those agents come back repainted but on a fresh
    /// conversation, which is what they did before this existed.
    #[serde(default)]
    session_id: Option<String>,
    /// Whether the agent was ever spoken to, and so whether the conversation
    /// named above was ever actually written (see [`AgentMeta::spoke`]).
    ///
    /// Defaults to `false` on an older session file, which costs one fresh
    /// start for agents saved before this field existed rather than the failed
    /// launch they would otherwise get.
    #[serde(default)]
    spoke: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct PersistedProcess {
    name: String,
    command: String,
    #[serde(default)]
    ready: Option<String>,
    dump: String,
}

/// Directory name for a workspace's output dumps: a readable slug of the
/// project directory plus a hash of its full path.
///
/// The slug alone would collide across the several `.../src` or `.../web`
/// directories a person has open at once, and those workspaces would then
/// replay each other's output into their panes. The hash alone would be
/// unreadable in a directory a user is expected to be able to delete by hand.
/// What an `[[agents]]` launcher writes where its conversation id belongs.
const SESSION_PLACEHOLDER: &str = "{session_id}";

/// The daemon's half of the handshake, in one place so the three attach paths
/// cannot answer with different versions.
///
/// `server_version` is what lets a client say "your daemon is old" instead of
/// showing the user its symptoms — see [`ServerMsg::Hello`] for why
/// `proto_version` cannot do that job.
fn server_hello(session: Option<SessionInfo>) -> ServerMsg {
    ServerMsg::Hello {
        proto_version: PROTOCOL_VERSION,
        session,
        server_version: Some(env!("CARGO_PKG_VERSION").to_string()),
    }
}

/// Substitute a conversation id into a launcher's argv.
///
/// `None` means this argv *needs* an id it has not been given. Passing the
/// placeholder through verbatim is not an option: the CLI would take the
/// literal text `{session_id}` for a conversation name and exit on it. The
/// caller reads `None` as "this launch is not possible" and falls back to one
/// that is.
///
/// An argv with no placeholder is returned untouched, which is what keeps every
/// `[[agents]]` block written before conversations were named — and every
/// launcher that has no session concept — behaving exactly as it did.
fn expand_args(args: &[String], session_id: Option<&str>) -> Option<Vec<String>> {
    if !args.iter().any(|a| a.contains(SESSION_PLACEHOLDER)) {
        return Some(args.to_vec());
    }
    let id = session_id?;
    Some(args.iter().map(|a| a.replace(SESSION_PLACEHOLDER, id)).collect())
}

/// Whether this launcher lets butai name the conversation.
///
/// Read off the argv rather than a separate config flag, so the two cannot
/// disagree: writing the placeholder *is* the declaration. A CLI that will not
/// accept an id simply never mentions one, and gets the old behaviour.
fn assigns_session(agent: &crate::config::AgentDef) -> bool {
    agent.args.iter().chain(agent.resume_args.iter()).any(|a| a.contains(SESSION_PLACEHOLDER))
}

/// Conversation id and argv for a launch that is not resuming anything.
///
/// A launcher that names its conversations gets a fresh id minted here, *before*
/// the process starts, which is what keeps two agents opened in the same
/// directory at the same moment from ever colliding — there is no window in
/// which the two could observe each other.
fn fresh_launch(agent: &crate::config::AgentDef) -> (Option<String>, Vec<String>) {
    let id = assigns_session(agent).then(crate::ids::uuid_v4);
    // Cannot fail: an id is minted precisely when the argv asks for one.
    let args = expand_args(&agent.args, id.as_deref()).unwrap_or_else(|| agent.args.to_vec());
    (id, args)
}

/// Load a pane's saved output, if there is any.
///
/// A missing, empty or unparseable dump means that pane comes back blank —
/// which is exactly what it did before any of this existed, and never a reason
/// to abandon the rest of the restore. Returned raw because [`PaneDump`]
/// borrows from it.
fn read_dump(path: &Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    (!bytes.is_empty()).then_some(bytes)
}

/// Delete everything in `dir` that `keep` rejects, files and subdirectories
/// alike.
///
/// Best-effort throughout: a directory that cannot be read is one that cannot
/// be cleaned, and every failure here costs disk rather than correctness — a
/// dump nobody deleted is only ever read by a workspace that lists it.
fn prune_dir(dir: &Path, keep: impl Fn(&std::ffi::OsStr) -> bool) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if keep(&name) {
            continue;
        }
        let path = entry.path();
        let _ = match entry.file_type().map(|t| t.is_dir()) {
            Ok(true) => std::fs::remove_dir_all(&path),
            _ => std::fs::remove_file(&path),
        };
    }
}

/// How many pasted files a workspace keeps. Old enough to have scrolled far out
/// of any agent's context, and a bound on a directory that would otherwise grow
/// for as long as the daemon runs.
const SCRATCH_KEEP: usize = 32;

/// Reduce a client-supplied file name to something safe to join onto a path.
///
/// Only the basename survives, and only characters that cannot mean anything to
/// a path or a shell. The client does not get to choose the final name anyway —
/// [`put_file`](ServerCore::put_file) prefixes a counter — so this is about
/// keeping the *extension*, which is the part that tells an agent what it was
/// handed.
fn scratch_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .map(
            |c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '-' },
        )
        .take(64)
        .collect();
    // `..`, a leading dot, or an empty name all mean the counter prefix is
    // doing the work on its own.
    let cleaned = cleaned.trim_matches('.');
    if cleaned.is_empty() {
        "paste".into()
    } else {
        cleaned.to_string()
    }
}

/// Drop all but the newest [`SCRATCH_KEEP`] files in a workspace's scratch
/// directory. Names carry a zero-padded counter, so sorting them is sorting by
/// age. Best-effort, for [`prune_dir`]'s reason: failing to clean costs disk,
/// not correctness.
fn prune_scratch(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut names: Vec<std::ffi::OsString> = entries.flatten().map(|e| e.file_name()).collect();
    if names.len() <= SCRATCH_KEEP {
        return;
    }
    names.sort();
    let doomed: HashSet<std::ffi::OsString> = names.drain(..names.len() - SCRATCH_KEEP).collect();
    prune_dir(dir, |name| !doomed.contains(name));
}

/// Dump file names. Position-based, and written by the same walk that builds
/// the persisted lists, so `agents[i]` and `agent-<i>.bin` cannot drift apart.
fn agent_dump(i: usize) -> String {
    format!("agent-{i}.bin")
}

fn proc_dump(i: usize) -> String {
    format!("proc-{i}.bin")
}

fn workspace_key(cwd: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in cwd.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let slug: String = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("ws")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(32)
        .collect();
    format!("{slug}-{hash:016x}")
}

impl ServerCore {
    pub fn new(
        config: Config,
        events_tx: UnboundedSender<Event>,
        output_tx: OutputTx,
        mode: CoreMode,
    ) -> Self {
        Self {
            mode,
            config,
            workspaces: HashMap::new(),
            order: Vec::new(),
            panes: HashMap::new(),
            clients: HashMap::new(),
            events_tx,
            output_tx,
            sys: None,
            usage: None,
            next_pane: 1,
            next_ws: 1,
            dirty: false,
            anim: 0,
            wants_anim: false,
            fast_anim: 0,
            wants_fast_anim: false,
            had_client: false,
            had_workspace: false,
            shutdown: false,
            api_subs: Vec::new(),
            last_detail: HashMap::new(),
            agent_alerted: std::collections::HashSet::new(),
            agent_track: HashMap::new(),
            notifications: std::collections::VecDeque::new(),
            notif_seq: 0,
            session_store: None,
            socket: butai_protocol::paths::socket_path(),
            restoring: false,
            git_refreshing: std::collections::HashSet::new(),
            git_refresh_again: std::collections::HashSet::new(),
            git_ops: HashMap::new(),
            git_op_seq: 0,
            repo_probing: std::collections::HashSet::new(),
            repo_probe_next: HashMap::new(),
            resume_retry: std::collections::HashSet::new(),
            put_seq: 0,
            updating: false,
            restart_into: None,
            deferred: Vec::new(),
        }
    }

    /// Enable workspace persistence, restoring any previously saved session
    /// immediately. Call before `run()`. Only the socket daemon does this.
    pub fn set_session_store(&mut self, path: PathBuf) {
        self.session_store = Some(path);
    }

    /// Record the socket the listener is actually bound to.
    ///
    /// Not the same as `paths::socket_path()`, which re-reads the *daemon's own*
    /// `$BUTAI_SOCKET`. That is unset under `daemon::serve`, so without this
    /// every pane in every test — and under `butai standalone` — would be told
    /// the default path and shell back to the wrong daemon, or none.
    pub fn set_socket(&mut self, path: PathBuf) {
        self.socket = path;
    }

    /// Run until the last workspace dies, a signal arrives, or `kill-server`.
    ///
    /// Returns the binary this daemon stopped in order to *become*, when it
    /// stopped for a self-update. Carrying it out of the loop rather than
    /// acting on it here is deliberate: the swap has to happen after the
    /// session snapshot is written and the socket is unbound, and neither of
    /// those is this function's to do.
    pub async fn run(
        mut self,
        mut rx: UnboundedReceiver<Event>,
        mut out_rx: OutputRx,
    ) -> Option<butai_update::Staged> {
        self.restore_session();
        let mut last_render = Instant::now() - FRAME_INTERVAL;
        loop {
            // Handle all pending control events first (priority), then a capped
            // batch of PTY output. `saturated` means output was left queued.
            let work_started = Instant::now();
            let saturated = self.drain_ready(&mut rx, &mut out_rx);
            if self.shutdown || self.should_exit() {
                break;
            }

            let now = Instant::now();
            if self.dirty && now >= last_render + FRAME_INTERVAL {
                self.render_all();
                // On the same clock as the frames, for the same reason: a client
                // drawing the rails itself needs them to change when the pane
                // beside them does, not on the next ~2s sampler tick.
                self.broadcast_ws_details();
                self.dirty = false;
                last_render = now;
            }
            // This loop is the only thread that owns panes, drains PTY output,
            // and renders, so a slow iteration freezes every pane and client at
            // once. Blocking filesystem work on a network-mounted workspace was
            // the usual cause; log it rather than let it read as "butai is slow".
            let busy = work_started.elapsed();
            if busy >= SLOW_ITERATION {
                warn!("core loop blocked for {}ms", busy.as_millis());
            }

            if saturated {
                // A pane is flooding: more output is queued. Don't spin on it —
                // pace processing to the frame clock. Waiting until the next
                // frame parks the task (so the I/O reactor and co-located tasks
                // run, and CPU stays bounded), while `biased` control events
                // interrupt the wait immediately. Excess output stays queued as
                // backpressure; there is no point parsing faster than we paint.
                let deadline = last_render + FRAME_INTERVAL;
                tokio::select! {
                    biased;
                    ev = rx.recv() => { match ev { Some(ev) => self.handle(ev), None => break } }
                    _ = tokio::time::sleep_until(deadline) => {}
                }
                if self.shutdown || self.should_exit() {
                    break;
                }
                continue;
            }

            // Idle: block until a control event, PTY output, or the render
            // deadline. The control channel is `biased` first so it wins any tie.
            if self.dirty {
                let deadline = last_render + FRAME_INTERVAL;
                tokio::select! {
                    biased;
                    ev = rx.recv() => { match ev { Some(ev) => self.handle(ev), None => break } }
                    out = out_rx.recv() => { if let Some((p, mut b)) = out { self.on_pty_output(p, &mut b); } }
                    _ = tokio::time::sleep_until(deadline) => {}
                }
            } else {
                tokio::select! {
                    biased;
                    ev = rx.recv() => { match ev { Some(ev) => self.handle(ev), None => break } }
                    out = out_rx.recv() => { if let Some((p, mut b)) = out { self.on_pty_output(p, &mut b); } }
                }
            }
            if self.shutdown || self.should_exit() {
                break;
            }
        }
        info!("server core exiting");
        self.restart_into.take()
    }

    /// Non-blocking drain of both channels. All pending control events are
    /// handled first (priority), then a capped batch of PTY output is coalesced
    /// per pane and fed to the emulators. Returns `true` when the output budget
    /// was hit with more still queued (a flooding pane) — the caller yields and
    /// loops rather than blocking. The output channel is bounded, so leftover
    /// output is backpressure on reader threads, not a growing queue.
    fn drain_ready(&mut self, rx: &mut UnboundedReceiver<Event>, out_rx: &mut OutputRx) -> bool {
        while let Ok(ev) = rx.try_recv() {
            self.handle(ev);
            if self.shutdown {
                return false;
            }
        }
        // Coalesce by pane, preserving per-pane byte order; order across distinct
        // panes doesn't matter since they are independent emulators. Cap the
        // total bytes taken per drain: feeding the VT emulator is synchronous, so
        // an unbounded coalesce (up to the whole channel) would block the loop —
        // and starve control events — for as long as that parse takes. Whatever
        // is left stays queued (the channel is bounded, so this is backpressure,
        // not a leak) and is picked up on the next turn, after a control check.
        let mut merged: Vec<(PaneId, Vec<u8>)> = Vec::new();
        let mut budget = OUTPUT_DRAIN_BYTES;
        let mut saturated = false;
        loop {
            if budget == 0 {
                // Hit the cap; assume a flooding pane has more queued.
                saturated = true;
                break;
            }
            match out_rx.try_recv() {
                Ok((pane, mut bytes)) => {
                    budget = budget.saturating_sub(bytes.len());
                    if let Some(entry) = merged.iter_mut().find(|(p, _)| *p == pane) {
                        entry.1.append(&mut bytes);
                    } else {
                        merged.push((pane, bytes));
                    }
                }
                Err(_) => break,
            }
        }
        for (pane, bytes) in &mut merged {
            self.on_pty_output(*pane, bytes);
        }
        saturated
    }

    fn should_exit(&self) -> bool {
        match self.mode {
            CoreMode::Standalone => self.had_client && self.clients.is_empty(),
            CoreMode::Daemon => {
                self.config.general.exit_when_empty
                    && self.had_workspace
                    && self.workspaces.is_empty()
            }
        }
    }

    /// Apply a (already coalesced) run of PTY output for one pane: ready-pattern
    /// detection, feed the VT emulator, and refresh agent state. Called once per
    /// pane per drain from [`ServerCore::run`], so the per-chunk work here runs
    /// per burst rather than per 64 KB read.
    fn on_pty_output(&mut self, pane: PaneId, bytes: &mut [u8]) {
        // Ready-pattern scan for process rows (cheap substring check).
        if let Some(ws) = self.workspaces.values_mut().find(|w| w.proc_meta.contains_key(&pane)) {
            if let Some(meta) = ws.proc_meta.get_mut(&pane) {
                if !meta.ready_seen {
                    if let Some(ready) = &meta.ready {
                        // Search the previous burst's tail plus this one, so a
                        // marker straddling the boundary still matches.
                        let text = String::from_utf8_lossy(bytes);
                        let mut window = std::mem::take(&mut meta.ready_carry);
                        window.push_str(&text);
                        if window.contains(ready.as_str()) {
                            meta.ready_seen = true;
                        } else if ready.len() > 1 {
                            // Keep just enough to complete a split marker.
                            let keep = ready.len() - 1;
                            let start = window
                                .char_indices()
                                .rev()
                                .map(|(i, _)| i)
                                .find(|i| window.len() - i >= keep)
                                .unwrap_or(0);
                            meta.ready_carry = window[start..].to_string();
                        }
                    }
                }
            }
        }
        let mut announced = Vec::new();
        if let Some(PaneState::Terminal(t)) = self.panes.get_mut(&pane) {
            t.feed_output(bytes);
            announced = t.take_announcements();
            self.dirty = true;
        }
        for a in announced {
            self.on_remote_announce(pane, a);
        }
        self.check_agent_bell(pane);
        // Output means this agent is active; make sure it's re-evaluated
        // promptly (resets its adaptive backoff) rather than on a long
        // stable interval.
        self.touch_agent(pane);
    }

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::ClientConnected(id, tx) => {
                self.had_client = true;
                self.clients.insert(
                    id,
                    ClientState {
                        tx,
                        session: None,
                        pane: None,
                        cols: 80,
                        rows: 24,
                        control: false,
                        last_frame: None,
                        sel_anchor: None,
                        sel: None,
                    },
                );
            }
            Event::Client(id, msg) => self.handle_client_msg(id, msg),
            Event::ClientGone(id) => {
                self.clients.remove(&id);
            }
            Event::PaneExited(pane, status) => self.on_pane_exited(pane, status),
            Event::GitRefreshed(pane, snap) => {
                self.git_refreshing.remove(&pane);
                if let Some(PaneState::Git(g)) = self.panes.get_mut(&pane) {
                    g.apply(snap);
                    self.dirty = true;
                }
                // Something changed the tree while this scan was running, so
                // what it just reported is already out of date. Run it again
                // rather than leaving the rail showing the state from before.
                if self.git_refresh_again.remove(&pane) {
                    self.request_git_refresh(pane);
                }
            }
            Event::RepoProbed(sid, git) => self.on_repo_probed(sid, git),
            // No repaint: nothing in the daemon-drawn chrome shows usage. The
            // page is a client's, and it polls.
            Event::Usage(dto) => self.usage = Some(dto),
            Event::Sys(stats) => {
                self.sys = Some(stats);
                // Piggyback on the ~2s sampler tick to refresh git state so
                // the CHANGES rail tracks edits made outside butai. The scan runs
                // off-thread (in-flight-deduped) so a slow repo never stalls the
                // loop between ticks.
                let changes: Vec<PaneId> =
                    self.workspaces.values().filter_map(|w| w.changes).collect();
                for id in changes {
                    self.request_git_refresh(id);
                }
                // A repo may appear after the workspace opened (`git init`,
                // a clone landing in the cwd) — pick it up on the same tick.
                self.attach_new_repos();
                self.dirty = true;
                // Recompute debounced agent status + emit any finished/needs-you
                // notifications on this same ~2s tick (unconditionally: the feed
                // is recorded for pollers even with no live subscribers).
                self.update_agent_tracking();
                // Same tick carries the restore snapshot, so a machine that
                // loses power still comes back to within a couple of seconds of
                // where it was rather than to whenever a workspace last opened.
                //
                // Both halves, and in this order. The roster and the dumps are
                // read from the same state here, so they agree; writing only the
                // dumps — which is what this did — left `session.json` describing
                // whichever agents were open the last time a workspace opened or
                // closed. Since the dumps are keyed by position and pruned to the
                // live set, a hard kill then restored a stale roster against
                // fresh files: agents started since were lost outright, and
                // closing one shifted the rest so a pane came back holding its
                // own conversation under a neighbour's screen.
                //
                // Written unconditionally rather than diffed against the last
                // copy: it is a ~1 KiB temp-then-rename beside the pane dumps
                // this same tick rewrites at up to `restore_bytes` each.
                self.persist_session();
                self.capture_panes();
                // Feed API subscribers: fresh telemetry + a workspace snapshot
                // (git refresh + attention recompute happen on this same tick).
                if !self.api_subs.is_empty() {
                    let sys = self.build_sys_dto();
                    let ws = self.build_ws_summaries();
                    self.broadcast_api(ApiEvent::System(sys));
                    self.broadcast_api(ApiEvent::Workspaces(ws));
                }
            }
            Event::Api(req, reply) => {
                // Anything that reads or writes files is answered off-thread, so
                // an unreachable mount can't freeze the actor.
                let Some((req, reply)) = self.offload_api(req, reply) else { return };
                // Actions that add or remove a pane/workspace change what every
                // other client should be showing. Without this they'd only notice
                // on the next ~2s sampler tick, so a kill in one window lingers as
                // a phantom row in another.
                let structural = matches!(
                    req,
                    ApiRequest::KillPane { .. }
                        | ApiRequest::KillWorkspace(_)
                        | ApiRequest::SpawnAgent { .. }
                        | ApiRequest::NewProcess { .. }
                        | ApiRequest::RestartProcess { .. }
                );
                let resp = self.handle_api(req);
                // Only on success — a 404'd kill changed nothing worth pushing.
                let succeeded = matches!(resp, ApiReply::Ok | ApiReply::Created(_));
                let _ = reply.send(resp);
                if structural && succeeded {
                    let ws = self.build_ws_summaries();
                    self.broadcast_api(ApiEvent::Workspaces(ws));
                }
            }
            Event::UpdateReady { result, reply } => {
                self.updating = false;
                match result {
                    Err(e) => {
                        warn!("update: {e}");
                        let _ = reply.send(ApiReply::Error(e));
                    }
                    Ok(None) => {
                        info!("update: already on {}", butai_update::CURRENT);
                        let _ = reply.send(ApiReply::Update(butai_protocol::api::UpdateDto {
                            current: butai_update::CURRENT.to_string(),
                            latest: Some(butai_update::CURRENT.to_string()),
                            updating: false,
                        }));
                    }
                    Ok(Some(staged)) => {
                        info!(
                            "update: {} -> {} staged, restarting",
                            butai_update::CURRENT,
                            staged.version
                        );
                        // Answered *before* the teardown, because the teardown
                        // ends in this process being replaced and an unsent
                        // reply would go with it.
                        let _ = reply.send(ApiReply::Update(butai_protocol::api::UpdateDto {
                            current: butai_update::CURRENT.to_string(),
                            latest: Some(staged.version.clone()),
                            updating: true,
                        }));
                        self.restart_into = Some(staged);
                        // Plain `kill-server`, detach reason and all. Clients
                        // match on `DETACH_SERVER_SHUTDOWN` to tell a daemon
                        // that is coming back from a pane that is gone, so a
                        // restart that invented its own reason would blank the
                        // stage of every workbench attached to it — see
                        // `ServerMsg::Detached`.
                        self.kill_server(false);
                    }
                }
            }
            Event::WorkspaceWritten(sid) => {
                // An upload landed: repaint so the git rail shows the new file.
                if self.workspaces.contains_key(&sid) {
                    self.dirty = true;
                }
            }
            Event::NewWorkspaceResolved { name, cwd, reply } => {
                self.new_workspace_resolved(name, cwd, reply);
            }
            Event::ApiSubscribe(tx) => {
                // Hand the new subscriber the current state at once. Otherwise it
                // renders empty for up to a full sampler tick before the first
                // push arrives.
                let sys = self.build_sys_dto();
                let ws = self.build_ws_summaries();
                let _ = tx.send(ApiEvent::System(sys));
                let _ = tx.send(ApiEvent::Workspaces(ws));
                self.api_subs.push(tx);
            }
            Event::Tick(phase) => {
                self.anim = phase;
                // Only repaint when the last frame actually had scrolling text.
                if self.wants_anim {
                    self.dirty = true;
                }
            }
            Event::FastTick(phase) => {
                self.fast_anim = phase;
                // Same bargain as `Tick`, against the sprite panel: no open
                // panel with a working agent, no repaint.
                if self.wants_fast_anim {
                    self.dirty = true;
                }
            }
            Event::GitOpProgress { ws, seq, line } => self.on_git_progress(ws, seq, line),
            Event::GitOpDone { root, ws, seq, result } => {
                self.on_git_op_done(root, ws, seq, result)
            }
            Event::Shutdown => {
                // Snapshot, then freeze: both writers no-op once `shutdown` is
                // set, so that the teardown below — which kills the workspaces
                // one at a time — cannot whittle the saved session down to
                // nothing before the next launch reads it.
                self.snapshot_for_restart();
                self.shutdown = true;
                let ids: Vec<ClientId> = self.clients.keys().copied().collect();
                for cid in ids {
                    self.detach(cid, butai_protocol::DETACH_SERVER_SHUTDOWN);
                }
                let sids: Vec<SessionId> = self.order.clone();
                for sid in sids {
                    self.kill_workspace(sid);
                }
            }
        }
    }

    // -- attach / lifecycle -------------------------------------------------

    fn handle_client_msg(&mut self, id: ClientId, msg: ClientMsg) {
        match msg {
            ClientMsg::Hello { proto_version, cols, rows, target, cwd, .. } => {
                if proto_version != PROTOCOL_VERSION {
                    self.send(
                        id,
                        ServerMsg::Error(format!(
                            "protocol version mismatch: client {proto_version}, server {}",
                            PROTOCOL_VERSION
                        )),
                    );
                    self.detach(id, "protocol mismatch");
                    return;
                }
                self.attach_client(id, cols, rows, target, cwd);
            }
            ClientMsg::Input(ev) => self.route_input(id, ev),
            ClientMsg::Resize { cols, rows } => self.resize_client(id, cols, rows),
            ClientMsg::Command(cmd) => self.exec_command(id, cmd),
            ClientMsg::Watch { pane } => self.watch_pane(id, pane),
            // The client hit something it has to report and has no footer of
            // its own to report it in. Truncated on a char boundary, so a
            // client sending something enormous costs a short flash, not a
            // panic.
            ClientMsg::Notice(msg) => {
                let msg = match msg.char_indices().nth(MAX_NOTICE_CHARS) {
                    Some((i, _)) => format!("{}…", &msg[..i]),
                    None => msg,
                };
                self.report_error(id, msg);
            }
            ClientMsg::Detach => self.detach(id, "detached"),
        }
    }

    /// Re-point a pane connection at another pane ([`ClientMsg::Watch`]).
    ///
    /// The same bookkeeping as the [`AttachTarget::Pane`] branch of
    /// [`attach_client`](Self::attach_client), minus the handshake: clearing
    /// `last_frame` is what makes the next render a full one, so the client
    /// replaces the screen rather than receiving a diff against a pane it is no
    /// longer showing.
    ///
    /// Refusals leave the existing pane streaming. A client asking for a pane
    /// that just exited should keep the one it has and be told why, not go
    /// blank — and since the request races pane death by nature, that is the
    /// common case rather than a client bug.
    /// Size a pane to the client that is looking at it, and remember the
    /// measurement for the workspace.
    ///
    /// Every path that points a client at a pane comes through here: the
    /// `Pane` attach, a later `Watch`, and a `Resize` on a connection already
    /// holding one. They must agree — a size recorded on two of the three is a
    /// workspace whose next pane is born right or wrong depending on which
    /// gesture the client happened to use.
    ///
    /// The only pane a client can stream is the one on the stage (the others
    /// have no screen and are refused), so this measurement *is* the stage's
    /// interior, taken by the one party that knows how wide its own rails are.
    fn size_pane_for_viewer(&mut self, pane: PaneId, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(2));
        if let Some(p) = self.panes.get_mut(&pane) {
            p.resize(rows, cols);
        }
        if let Some(ws) = self.workspaces.values_mut().find(|w| w.all_panes().contains(&pane)) {
            ws.stage_size = Some((rows, cols));
        }
    }

    fn watch_pane(&mut self, id: ClientId, pane: PaneId) {
        let Some(client) = self.clients.get(&id) else { return };
        if client.pane.is_none() {
            let msg = "watch is only valid on a pane connection".to_string();
            self.send(id, ServerMsg::Error(msg));
            return;
        }
        let (cols, rows) = (client.cols, client.rows);
        if !self.panes.contains_key(&pane) {
            self.send(id, ServerMsg::Error(format!("no pane {pane}")));
            return;
        }
        if let Some(client) = self.clients.get_mut(&id) {
            client.pane = Some(pane);
            client.last_frame = None;
        }
        self.size_pane_for_viewer(pane, rows, cols);
        // Watching a pane is looking at it, same as attaching to one.
        self.look_at_pane(pane);
        self.dirty = true;
    }

    fn attach_client(
        &mut self,
        id: ClientId,
        cols: u16,
        rows: u16,
        target: AttachTarget,
        cwd: PathBuf,
    ) {
        let ws_id = match &target {
            AttachTarget::Control => {
                if let Some(c) = self.clients.get_mut(&id) {
                    c.control = true;
                }
                self.send(id, server_hello(None));
                return;
            }
            AttachTarget::Pane { pane } => {
                let pane = *pane;
                if !self.panes.contains_key(&pane) {
                    self.send(id, ServerMsg::Error(format!("no pane {pane}")));
                    self.detach(id, "no such pane");
                    return;
                }
                if let Some(c) = self.clients.get_mut(&id) {
                    c.pane = Some(pane);
                    c.session = None;
                    c.cols = cols;
                    c.rows = rows;
                    c.last_frame = None;
                }
                self.size_pane_for_viewer(pane, rows, cols);
                // Streaming a pane is looking at it — the same gesture as the
                // TUI staging one, which is where `look_at_pane` is otherwise
                // called from. Clears the bell so the agent stops reporting
                // `waiting` once the user has actually seen it, and marks a
                // finished turn read.
                self.look_at_pane(pane);
                self.send(id, server_hello(None));
                self.dirty = true;
                return;
            }
            AttachTarget::Attach { name } => {
                match self.workspaces.values().find(|s| &s.name == name).map(|s| s.id) {
                    Some(sid) => sid,
                    None => {
                        self.send(id, ServerMsg::Error(format!("no workspace named {name:?}")));
                        self.detach(id, "no such workspace");
                        return;
                    }
                }
            }
            AttachTarget::New { name, .. } => {
                let name = name.clone().unwrap_or_else(|| default_ws_name(&cwd));
                if self.workspaces.values().any(|s| s.name == name) {
                    self.send(id, ServerMsg::Error(format!("workspace {name:?} already exists")));
                    self.detach(id, "duplicate workspace");
                    return;
                }
                match self.create_workspace(name, cwd) {
                    Ok(sid) => sid,
                    Err(e) => {
                        self.send(
                            id,
                            ServerMsg::Error(format!("failed to create workspace: {e:#}")),
                        );
                        self.detach(id, "workspace create failed");
                        return;
                    }
                }
            }
            AttachTarget::Default => {
                // `butai` in a directory lands on that directory's workspace:
                // reuse the one already open for it, else open a new one. Only
                // when the cwd is unknown ("/" or empty) do we fall back to the
                // most-recent workspace so a bare re-attach still works.
                let has_cwd = !cwd.as_os_str().is_empty() && cwd != Path::new("/");
                let resolved = if has_cwd {
                    self.workspace_for_cwd(&cwd)
                } else if let Some(sid) = self.order.last().copied() {
                    Ok(sid)
                } else {
                    self.create_workspace(default_ws_name(&cwd), cwd.clone())
                };
                match resolved {
                    Ok(sid) => sid,
                    Err(e) => {
                        self.send(
                            id,
                            ServerMsg::Error(format!("failed to create workspace: {e:#}")),
                        );
                        self.detach(id, "workspace create failed");
                        return;
                    }
                }
            }
        };

        let info = self.ws_info(ws_id);
        if let Some(c) = self.clients.get_mut(&id) {
            c.session = Some(ws_id);
            c.cols = cols;
            c.rows = rows;
            c.last_frame = None;
        }
        self.send(id, server_hello(info));
        self.dirty = true;
    }

    fn create_workspace(&mut self, name: String, cwd: PathBuf) -> anyhow::Result<SessionId> {
        self.create_workspace_restoring(name, cwd, None)
    }

    /// [`create_workspace`](Self::create_workspace), optionally rebuilding the
    /// panes a previous daemon had open instead of starting from the workspace
    /// file. Only [`restore_session`](Self::restore_session) passes `saved`.
    fn create_workspace_restoring(
        &mut self,
        name: String,
        cwd: PathBuf,
        saved: Option<&PersistedWorkspace>,
    ) -> anyhow::Result<SessionId> {
        let sid = SessionId(self.next_ws);
        self.next_ws += 1;
        let cwd = if cwd.as_os_str().is_empty() { PathBuf::from("/") } else { cwd };
        let mut ws = Workspace::new(sid, name, cwd.clone());
        // Project workspace file: name, managed processes, agent autostart.
        let (ws_file, warnings) = crate::config::WorkspaceFile::load(&cwd);
        for w in warnings {
            warn!("workspace config: {w}");
        }
        // Nobody is looking at this workspace yet — it does not exist until the
        // end of this function — so the first shell gets the unwatched default
        // and is resized by the first client to draw it.
        let (srows, scols) = UNWATCHED_PANE_SIZE;

        // Every workspace starts with a shell process on the stage — except a
        // restored one, whose shell is simply the first entry of the process
        // list it saved, and gets rebuilt with the rest of them below. Spawning
        // one here too would leave every restored workspace with a spare.
        let rebuilding = saved.filter(|s| !s.processes.is_empty());
        let mut shell_pane = None;
        if rebuilding.is_none() {
            let shell = self.config.shell();
            let pane = self.spawn_terminal(sid, &cwd, None, None, false, srows, scols)?;
            ws.processes.push(pane);
            ws.proc_meta.insert(
                pane,
                ProcMeta {
                    name: "shell".into(),
                    command: shell,
                    ready: None,
                    ready_seen: true,
                    ready_carry: String::new(),
                },
            );
            ws.stage = Some(pane);
            shell_pane = Some(pane);
        }

        // Changes rail when inside a git repository. The status scan is heavy
        // (a full worktree walk — slow on big repos or network filesystems), so
        // the pane opens empty and its first status is computed off-thread.
        if let Ok(git) = crate::pane::git::GitPane::new(&cwd) {
            let id = self.alloc_pane_id();
            self.panes.insert(id, PaneState::Git(git));
            ws.changes = Some(id);
            self.request_git_refresh(id);
        }

        self.workspaces.insert(sid, ws);
        self.order.push(sid);
        self.had_workspace = true;
        // This directory is open for real now, so the held-back copy of it is
        // stale. Left in place it would outlive the live workspace and reappear
        // in the store the moment that one is closed.
        self.deferred.retain(|d| d.cwd != cwd);

        match rebuilding {
            // What was running, not what the workspace file says should be:
            // the saved list already contains the file's processes and agents
            // (they were spawned from it and then recorded), plus anything
            // started by hand since, minus anything closed. Replaying the file
            // on top would duplicate the first group and lose the other two.
            Some(saved) => self.rebuild_workspace_panes(sid, &cwd, saved),
            None => {
                // Managed processes + autostarted agents from the workspace file.
                for def in &ws_file.processes {
                    let meta = ProcMeta {
                        name: def.name.clone(),
                        command: def.cmd.clone(),
                        ready: def.ready.clone(),
                        ready_seen: false,
                        ready_carry: String::new(),
                    };
                    if let Err(e) = self.spawn_process(sid, &cwd, meta, false, None) {
                        warn!("process {:?}: {e:#}", def.name);
                    }
                }
                for agent in &ws_file.agents.autostart {
                    if let Err(e) = self.spawn_agent(sid, agent) {
                        warn!("autostart agent {agent:?}: {e:#}");
                    }
                }
                // The shell keeps the stage on open regardless of autostarts.
                if let (Some(ws), Some(pane)) = (self.workspaces.get_mut(&sid), shell_pane) {
                    ws.stage = Some(pane);
                }
            }
        }

        self.dirty = true;
        self.persist_session();
        Ok(sid)
    }

    /// Rebuild the panes a previous daemon had open in this workspace: its
    /// processes, then its agents, each painted with the output it was showing
    /// and each agent asked to reopen its conversation.
    ///
    /// The children are new — nothing survives a restart but the bytes on
    /// disk. What the replay buys is that the pane you come back to reads as
    /// the pane you left, so the transcript above the prompt is the work rather
    /// than a blank screen you have to reconstruct from memory.
    fn rebuild_workspace_panes(&mut self, sid: SessionId, cwd: &Path, saved: &PersistedWorkspace) {
        let Some(dumps) = self.panes_dir().map(|d| d.join(&saved.key)) else { return };
        // Dump name -> the pane it was replayed into, so the stage can be put
        // back on whichever pane was holding it.
        let mut staged = None;
        for p in &saved.processes {
            let meta = ProcMeta {
                name: p.name.clone(),
                command: p.command.clone(),
                ready: p.ready.clone(),
                // The restored child is a new one: it has not signalled ready
                // yet, so the row starts at `run` and flips when it does. Same
                // rule a hand-started process uses.
                ready_seen: p.ready.is_none(),
                ready_carry: String::new(),
            };
            let raw = read_dump(&dumps.join(&p.dump));
            let replay = raw.as_deref().and_then(decode_dump);
            match self.spawn_process(sid, cwd, meta, false, replay) {
                Ok(pane) if saved.stage.as_deref() == Some(p.dump.as_str()) => {
                    staged = Some(pane);
                }
                Ok(_) => {}
                Err(e) => warn!("restore process {:?}: {e:#}", p.name),
            }
        }
        for a in &saved.agents {
            let raw = read_dump(&dumps.join(&a.dump));
            let replay = raw.as_deref().and_then(decode_dump);
            match self.spawn_agent_restoring(
                sid,
                &a.agent,
                replay,
                a.session_id.as_deref(),
                a.spoke,
                false,
            ) {
                Ok(pane) if saved.stage.as_deref() == Some(a.dump.as_str()) => {
                    staged = Some(pane);
                }
                Ok(_) => {}
                Err(e) => warn!("restore agent {:?}: {e:#}", a.agent),
            }
        }
        let Some(ws) = self.workspaces.get_mut(&sid) else { return };
        // Each agent spawn takes the stage as it lands, so without this the
        // workspace would always come back on whichever agent happened to be
        // last rather than on what was in front of you.
        if let Some(pane) = staged.or_else(|| ws.processes.first().copied()) {
            ws.stage = Some(pane);
        }
    }

    fn kill_workspace(&mut self, sid: SessionId) {
        let Some(ws) = self.workspaces.remove(&sid) else { return };
        self.order.retain(|s| *s != sid);
        self.persist_session();
        self.repo_probe_next.remove(&sid);
        for pane in ws.all_panes() {
            self.panes.remove(&pane); // drop kills children
            self.git_refreshing.remove(&pane);
            // ...and any re-run it was owed. The pane is gone; rescanning for
            // it would find nothing to apply the snapshot to.
            self.git_refresh_again.remove(&pane);
        }
        let orphaned: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, c)| c.session == Some(sid))
            .map(|(id, _)| *id)
            .collect();
        for cid in orphaned {
            // Fall back to another workspace if one exists.
            match self.order.last().copied() {
                Some(other) => {
                    if let Some(c) = self.clients.get_mut(&cid) {
                        c.session = Some(other);
                        c.last_frame = None;
                    }
                }
                None => self.detach(cid, "workspace closed"),
            }
        }
        info!("workspace {} killed", ws.name);
        self.dirty = true;
    }

    fn on_pane_exited(&mut self, pane: PaneId, status: u32) {
        debug!("pane {pane} exited with {status}");
        if let Some(PaneState::Terminal(t)) = self.panes.get_mut(&pane) {
            t.mark_exited(status);
        }
        // A restored agent that died on its own conversation gets one clean
        // start instead — ahead of the rail bookkeeping below, so the failure
        // never reaches the notification feed as an agent the user should look
        // at. It is butai's problem, not theirs.
        if self.retry_failed_resume(pane, status) {
            return;
        }
        self.resume_retry.remove(&pane);
        // Agents: a clean exit (0) is auto-removed from the rail; a failure
        // lingers as a red [x] row so it's noticed, until dismissed with `x`.
        if let Some(sid) = self.workspaces.values().find(|w| w.agents.contains(&pane)).map(|w| w.id)
        {
            if status == 0 {
                // Clean exit is intentional teardown — auto-removed, no alert.
                self.drop_pane(sid, pane);
            } else {
                // A failed agent lingers as a corpse row; alert once (but never
                // during startup replay, before the agent was ever seeded).
                let seeded = self.agent_track.get(&pane).is_some_and(|tr| tr.seeded);
                if seeded {
                    let info = self.workspaces.get(&sid).map(|w| w.name.clone()).and_then(|name| {
                        if let Some(PaneState::Terminal(t)) = self.panes.get(&pane) {
                            Some((name, t.agent_title()))
                        } else {
                            None
                        }
                    });
                    if let Some((ws_name, title)) = info {
                        self.push_notification(
                            sid,
                            &ws_name,
                            pane,
                            title,
                            NotificationKind::Exited,
                            Some(status),
                        );
                    }
                }
                if let Some(tr) = self.agent_track.get_mut(&pane) {
                    tr.exited = Some(status);
                }
            }
            self.dirty = true;
            return;
        }
        // A lone shell exiting closes its workspace (typing `exit` quits).
        let lone_shell_ws = self.workspaces.values().find_map(|ws| {
            let lone = ws.agents.is_empty()
                && ws.processes == vec![pane]
                && ws.proc_meta.get(&pane).map(|m| m.name == "shell").unwrap_or(false);
            lone.then_some(ws.id)
        });
        if let Some(sid) = lone_shell_ws {
            self.kill_workspace(sid);
            self.dirty = true;
            return;
        }
        // Other processes. A shell (user typed `exit`) is always removed. A
        // command/log row is auto-removed on a clean exit (0) but kept as a red
        // FAIL corpse on failure so its final output can still be read.
        if let Some(sid) =
            self.workspaces.values().find(|w| w.processes.contains(&pane)).map(|w| w.id)
        {
            let is_shell = self
                .workspaces
                .get(&sid)
                .and_then(|w| w.proc_meta.get(&pane))
                .map(|m| m.name == "shell")
                .unwrap_or(false);
            if is_shell || status == 0 {
                self.drop_pane(sid, pane);
            }
        }
        self.dirty = true;
    }

    /// Forget a pane from a workspace (re-pointing the stage/selection) and drop
    /// its state.
    /// Give a restored agent one clean start when reopening its conversation
    /// killed it. Returns whether the pane was replaced.
    ///
    /// The CLIs do not degrade: asked for a conversation that is no longer
    /// there, they exit rather than start without it. That is reachable through
    /// no fault of the user — a transcript aged out of Claude Code's 30-day
    /// retention, a history someone cleared, an agent that was launched but
    /// never actually said anything, a CLI upgraded to one that spells its
    /// flags differently. Any of those would otherwise turn every restart into
    /// a row of corpses.
    ///
    /// Deliberately generic rather than a per-CLI check against each vendor's
    /// storage layout: butai does not need to know *why* the launch failed to
    /// know that the fallback is the same either way.
    fn retry_failed_resume(&mut self, pane: PaneId, status: u32) -> bool {
        // A clean exit is the user quitting, not a failure to reopen.
        if status == 0 || !self.resume_retry.remove(&pane) {
            return false;
        }
        let young = match self.panes.get(&pane) {
            Some(PaneState::Terminal(t)) => t.age() < RESUME_RETRY_WINDOW,
            _ => false,
        };
        // An agent that ran a while and then failed did not fail to *start*, so
        // its exit is real news and belongs in the notification feed.
        if !young {
            return false;
        }
        let Some((sid, idx)) = self
            .workspaces
            .values()
            .find_map(|w| w.agents.iter().position(|p| *p == pane).map(|i| (w.id, i)))
        else {
            return false;
        };
        let Some(name) =
            self.workspaces.get(&sid).and_then(|w| w.agent_meta.get(&pane)).map(|m| m.name.clone())
        else {
            return false;
        };
        // Carry the screen across. The dying pane's ring already holds the
        // transcript that was replayed into it (plus whatever the CLI said on
        // its way out, which is the explanation the user wants), so taking it
        // from memory here beats re-reading the dump — the sampler tick has
        // very likely already rewritten that file with this pane's contents.
        let mut saved = match self.panes.get(&pane) {
            Some(PaneState::Terminal(t)) => {
                let (cols, rows) = t.size();
                let bytes = t.history();
                (!bytes.is_empty()).then(|| encode_dump(cols, rows, &bytes))
            }
            _ => None,
        };
        // Say so in the pane itself, not only in the flash below. The overwhelming
        // case for this path is a restore, which runs when the daemon starts —
        // before any client has attached, so a flash reaches nobody. Written into
        // the transcript, the explanation is still there whenever someone looks.
        if let Some(buf) = &mut saved {
            buf.extend_from_slice(
                "\r\n\x1b[33m[butai] could not reopen this conversation — started a fresh one\x1b[0m\r\n"
                    .as_bytes(),
            );
        }
        // Whether the user was actually looking at this pane. A retry can fire
        // seconds into a session, and yanking the stage onto a pane that just
        // died and came back is the last thing they asked for.
        let was_staged = self.workspaces.get(&sid).map(|w| w.stage == Some(pane)).unwrap_or(false);
        let prior_stage = self.workspaces.get(&sid).and_then(|w| w.stage);
        self.drop_pane(sid, pane);
        // `session: None` is what makes this a fresh start: the replay paints
        // the pane, but the launcher is given its ordinary args and a new
        // conversation rather than the one that just refused to open.
        let replay = saved.as_deref().and_then(decode_dump);
        // `spoke: false` alongside `session: None` — the fresh conversation this
        // starts has not been written yet either, so a restart before the user
        // types would otherwise fail to reopen it for the same reason.
        let fresh = match self.spawn_agent_restoring(sid, &name, replay, None, false, false) {
            Ok(p) => p,
            Err(e) => {
                warn!("restarting agent {name:?} after a failed resume: {e:#}");
                return true;
            }
        };
        // Put it back where it was: the rails are ordered, and the pane dumps
        // are keyed by position, so appending would silently renumber both.
        if let Some(ws) = self.workspaces.get_mut(&sid) {
            if let Some(cur) = ws.agents.iter().position(|p| *p == fresh) {
                let target = idx.min(ws.agents.len().saturating_sub(1));
                let moved = ws.agents.remove(cur);
                ws.agents.insert(target, moved);
            }
            // Spawning always claims the stage, which is right for an agent the
            // user asked for and wrong for one butai restarted behind their back.
            if !was_staged {
                if let Some(prior) = prior_stage.filter(|p| *p != pane) {
                    ws.stage = Some(prior);
                }
            }
        }
        let msg = format!("{name}: could not reopen its conversation — started a fresh one");
        let watching: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, c)| c.session == Some(sid))
            .map(|(id, _)| *id)
            .collect();
        for id in watching {
            self.report_error(id, msg.clone());
        }
        info!("agent {name:?} failed to resume (exit {status}); started fresh");
        self.persist_session();
        self.dirty = true;
        true
    }

    fn drop_pane(&mut self, ws_id: SessionId, pane: PaneId) {
        if let Some(ws) = self.workspaces.get_mut(&ws_id) {
            ws.forget_pane(pane);
        }
        self.panes.remove(&pane);
        self.resume_retry.remove(&pane);
        // The agent roster changed, so the saved session is now describing a
        // workspace that no longer exists. See `note_agent_spoken`.
        self.persist_session();
    }

    /// Record that an agent pane has been typed into, which is what makes its
    /// conversation real enough to reopen (see [`AgentMeta::spoke`]).
    ///
    /// Keys and pastes only. Scrolling and mouse selection are reading, not
    /// speaking: neither writes a transcript, and treating a wheel event as a
    /// conversation would put back exactly the failed launch this prevents.
    ///
    /// Persists on the transition, not on every keystroke — the flag is only
    /// interesting the first time it flips, and `session.json` is rewritten
    /// synchronously.
    fn note_agent_spoken(&mut self, pane: PaneId, ev: &InputEvent) {
        if !matches!(ev, InputEvent::Key(_) | InputEvent::Paste(_)) {
            return;
        }
        let flipped = self
            .workspaces
            .values_mut()
            .find_map(|w| w.agent_meta.get_mut(&pane))
            .is_some_and(|meta| !std::mem::replace(&mut meta.spoke, true));
        if flipped {
            self.persist_session();
        }
    }

    // -- relayed hosts ------------------------------------------------------

    /// A `butai` started over ssh inside `pane` announced itself. Connect the
    /// machine it is on, so its projects appear in the bar.
    ///
    /// The dial-back command is recovered from the pane's own foreground
    /// process: it is running the `ssh` that got there, so reusing its
    /// arguments reaches the same host the same way, through the same jump
    /// hosts, with the same key. Only if that read fails do we fall back to the
    /// `user@host` the far side derived from its `$SSH_CONNECTION`, which is
    /// right far less often — behind NAT it is an address that means nothing
    /// here.
    fn on_remote_announce(&mut self, pane: PaneId, a: crate::pane::terminal::RemoteAnnounce) {
        let dial_back = match self.panes.get(&pane) {
            Some(PaneState::Terminal(t)) => t.ssh_dial_back(),
            _ => None,
        };
        // Tell every subscriber where the machine is, whatever this daemon then
        // does about it. A client that draws its own tab bar connects the far
        // daemon itself — that is the whole point of it being a client — and it
        // needs the ssh arguments to do it, which only this side can recover.
        let (ssh_args, ssh_target) = dial_back.clone().unwrap_or_default();
        self.broadcast_api(ApiEvent::RemoteAnnounce(butai_protocol::api::RemoteAnnounceDto {
            pane,
            hint: a.hint.clone(),
            socket: a.socket.clone(),
            ssh_target,
            ssh_args,
        }));
    }

    // -- input routing -------------------------------------------------------

    /// Route a client's input to the pane it is streaming.
    ///
    /// Which is the only place input can go. A session connection used to drive
    /// a whole workbench from here — the rails, the overlays, the keymap, the
    /// mouse — and every one of those is the client's own now, resolved against
    /// the screen it drew rather than against a screen the daemon guessed it
    /// was looking at. So a session connection has no subject for a keystroke:
    /// it names a workspace, not a pane, and the client that opened it is
    /// holding a `pane` connection alongside for exactly this.
    fn route_input(&mut self, id: ClientId, ev: InputEvent) {
        let Some(pane) = self.clients.get(&id).and_then(|c| c.pane) else {
            let msg = "input needs a pane — attach to one, or send a command".to_string();
            self.report_error(id, msg);
            return;
        };
        self.route_pane_input(id, pane, ev);
    }

    // -- mouse ---------------------------------------------------------------

    /// Extract the client's active selection from its last frame, ship it to
    /// the client's clipboard, and clear the selection.
    ///
    /// This is how a client with no VT parser selects text: the daemon owns the
    /// pane's grid, so it does the extraction. A client that composes its own
    /// screen selects against that instead and never gets here. The whole frame
    /// is the pane — full-bleed, no chrome — so there is no neighbouring column
    /// to clip the drag against.
    fn finish_selection(&mut self, id: ClientId) {
        let sel = self.clients.get(&id).and_then(|c| c.sel);
        if let Some((a, b)) = sel {
            let text = self
                .clients
                .get(&id)
                .and_then(|c| c.last_frame.as_ref())
                .map(|buf| extract_selection(buf, a, b))
                .unwrap_or_default();
            if !text.trim().is_empty() {
                if let Some(c) = self.clients.get(&id) {
                    let _ = c.tx.send(ServerMsg::SetClipboard(text));
                }
            }
        }
        if let Some(c) = self.clients.get_mut(&id) {
            c.sel = None;
            c.sel_anchor = None;
        }
        self.dirty = true;
    }

    // -- picker / prompt overlays -------------------------------------------

    /// Write the current open-workspace list (directories + order) to disk so
    /// the daemon can rebuild it after a restart. No-op without a store, or
    /// while replaying a restore.
    fn persist_session(&self) {
        let Some(path) = &self.session_store else { return };
        if self.restoring {
            return;
        }
        // During teardown every workspace is killed in turn; persisting each
        // removal would leave the store empty and nothing would come back on the
        // next launch. Freeze the store so the workspaces that were open at
        // shutdown are restored, matching what a hard crash already does.
        if self.shutdown {
            return;
        }
        let mut state = SessionState {
            workspaces: self
                .order
                .iter()
                .filter_map(|sid| self.workspaces.get(sid))
                .map(|w| {
                    let agents: Vec<PersistedAgent> = w
                        .agents
                        .iter()
                        .enumerate()
                        .filter_map(|(i, pane)| {
                            // An agent whose launcher name we never recorded
                            // cannot be respawned, so it is not worth a slot.
                            let meta = w.agent_meta.get(pane)?;
                            Some(PersistedAgent {
                                agent: meta.name.clone(),
                                dump: agent_dump(i),
                                session_id: meta.session_id.clone(),
                                spoke: meta.spoke,
                            })
                        })
                        .collect();
                    let processes: Vec<PersistedProcess> = w
                        .processes
                        .iter()
                        .enumerate()
                        .filter_map(|(i, pane)| {
                            let meta = w.proc_meta.get(pane)?;
                            Some(PersistedProcess {
                                name: meta.name.clone(),
                                command: meta.command.clone(),
                                ready: meta.ready.clone(),
                                dump: proc_dump(i),
                            })
                        })
                        .collect();
                    // Only a terminal pane can be restored onto the stage; when
                    // an editor or a diff holds it, the workspace comes back on
                    // its shell, which is where it opens anyway.
                    let stage = w.stage.and_then(|s| {
                        (w.agents.iter().position(|p| *p == s).map(agent_dump))
                            .or_else(|| w.processes.iter().position(|p| *p == s).map(proc_dump))
                    });
                    PersistedWorkspace {
                        name: w.name.clone(),
                        cwd: w.cwd.clone(),
                        key: workspace_key(&w.cwd),
                        agents,
                        processes,
                        stage,
                    }
                })
                .collect(),
        };
        // Workspaces this start could not rebuild go back into the file, so a
        // directory that was merely unmounted is still there to restore next
        // time. Dropped the moment one is opened for real — the live entry
        // above is the current truth and this one is a stale copy of it.
        let held: Vec<PersistedWorkspace> = self
            .deferred
            .iter()
            .filter(|d| !state.workspaces.iter().any(|w| w.cwd == d.cwd))
            .cloned()
            .collect();
        state.workspaces.extend(held);
        let Ok(json) = serde_json::to_string_pretty(&state) else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Write to a temp file then rename, so a crash mid-write can't leave a
        // truncated session file that fails to parse on the next start.
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// Where this daemon keeps its pane dumps: a `panes/` directory beside its
    /// own session store, `None` when persistence is off.
    ///
    /// Derived from the store this core was given rather than read from
    /// [`butai_protocol::paths`], so the two halves of a restore cannot be pointed
    /// at different sessions. The production daemon passes
    /// [`session_state_path`](butai_protocol::paths::session_state_path), so this is
    /// the documented `~/.butai/panes` there; a test or a second daemon running
    /// against its own store gets its dumps beside that store instead of
    /// replaying the real session's output into its panes — or writing over it.
    fn panes_dir(&self) -> Option<PathBuf> {
        let store = self.session_store.as_ref()?;
        Some(store.parent().unwrap_or_else(|| Path::new(".")).join("panes"))
    }

    /// Stop the daemon. `clear` decides what the *next* start comes back to.
    ///
    /// Keeping the session is the default, and deliberately: stopping the daemon
    /// is something you do to restart it — after an upgrade, or to get out of a
    /// wedged terminal — and none of those are a decision to throw the work
    /// away. `clear` is the explicit "come up empty next time".
    fn kill_server(&mut self, clear: bool) {
        if clear {
            info!("kill-server requested, clearing the persisted session");
            // Do this *before* `shutdown` goes up, for the same reason the
            // snapshot does: once it is set, `persist_session` no-ops, so a
            // later write could not overwrite the file anyway — and removing it
            // first means a daemon killed between here and exit still comes up
            // empty, which is what was asked for.
            self.forget_session();
        } else {
            info!("kill-server requested, keeping the persisted session");
            // Snapshot and freeze first, so the workspaces and panes open right
            // now are what the daemon comes back to (see the `Event::Shutdown`
            // arm).
            self.snapshot_for_restart();
        }
        self.shutdown = true;
        let ids: Vec<ClientId> = self.clients.keys().copied().collect();
        for cid in ids {
            self.detach(cid, butai_protocol::DETACH_SERVER_SHUTDOWN);
        }
        let sids: Vec<SessionId> = self.order.clone();
        for sid in sids {
            self.kill_workspace(sid);
        }
    }

    /// Remove both halves of the restore state: the workspace list and the
    /// per-pane output dumps.
    ///
    /// Both, because either one left behind is worse than neither. A session
    /// file with no dumps restores workspaces to blank panes; dumps with no
    /// session file are unreferenced files that the next restore's pruning
    /// never visits, so they would sit there until the directory was reused.
    fn forget_session(&mut self) {
        if let Some(path) = self.session_store.clone() {
            match std::fs::remove_file(&path) {
                Ok(()) => info!("removed {}", path.display()),
                // Already absent is the desired state, not a failure.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warn!("could not remove {}: {e}", path.display()),
            }
        }
        if let Some(dir) = self.panes_dir() {
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => info!("removed {}", dir.display()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warn!("could not remove {}: {e}", dir.display()),
            }
        }
        // Anything the *next* persist would have written back has to go too:
        // workspaces this start could not resolve ride along in `deferred` and
        // are re-emitted on every write, so leaving them would resurrect the
        // session that was just cleared.
        self.deferred.clear();
    }

    // -- pasted files --------------------------------------------------------

    /// Where [`Command::PutFile`] writes — `~/.butai/scratch/<workspace-key>/`.
    ///
    /// Follows [`panes_dir`](Self::panes_dir) off the session store so an
    /// alternate daemon keeps its scratch beside its own state. Unlike that
    /// one it falls back to [`butai_dir`](butai_protocol::paths::butai_dir) rather
    /// than giving up, because `session_store` is also `None` for `standalone`,
    /// and there is no reason pasting an image should stop working just because
    /// the session is not being persisted.
    ///
    /// **Deliberately not inside the workspace.** `POST .../upload` writes into
    /// the project and repaints the changes rail, which is what you want for a
    /// file you meant to add and not for a screenshot: pasted images would
    /// otherwise show up as untracked files and ride along in someone's commit.
    fn scratch_dir(&self, ws: SessionId) -> Option<PathBuf> {
        let root = match self.session_store.as_ref() {
            Some(store) => store.parent().unwrap_or_else(|| Path::new(".")).join("scratch"),
            None => butai_protocol::paths::butai_dir().join("scratch"),
        };
        let cwd = &self.workspaces.get(&ws)?.cwd;
        Some(root.join(workspace_key(cwd)))
    }

    /// Which workspace a client's paste belongs to.
    ///
    /// A `pane` target (the web stage) has no session of its own, so the
    /// workspace is the one owning the pane it streams — the same answer its
    /// input already reaches.
    fn put_file_ws(&self, id: ClientId) -> Option<SessionId> {
        let client = self.clients.get(&id)?;
        if let Some(sid) = client.session {
            return Some(sid);
        }
        let pane = client.pane?;
        self.order
            .iter()
            .copied()
            .find(|sid| self.workspaces.get(sid).is_some_and(|w| w.all_panes().contains(&pane)))
    }

    /// Write a client-supplied file into the workspace scratch directory and
    /// paste its absolute path where that client's input would have gone.
    ///
    /// The paste goes through [`route_input`](Self::route_input) rather than
    /// straight at a pane so it lands wherever typing would have: the streamed
    /// pane for a `pane` client, the stage for a workbench one, and the text
    /// field of a prompt or commit box if one is open.
    ///
    /// Returns the path written, for the client to show.
    fn put_file(&mut self, id: ClientId, name: &str, data: &str) -> Result<PathBuf, String> {
        let ws = self.put_file_ws(id).ok_or("no workspace to paste into")?;

        // Reject on the encoded length first: 4 base64 characters carry 3
        // bytes, so this bounds the decode without allocating the copy.
        let limit_mib = MAX_PUT_FILE_BYTES / 1024 / 1024;
        if data.len() / 4 * 3 > MAX_PUT_FILE_BYTES {
            return Err(format!("file too large (limit {limit_mib} MiB)"));
        }
        let bytes = butai_protocol::b64::decode(data)?;
        if bytes.is_empty() {
            return Err("empty file".into());
        }
        if bytes.len() > MAX_PUT_FILE_BYTES {
            return Err(format!("file too large (limit {limit_mib} MiB)"));
        }

        let dir = self.scratch_dir(ws).ok_or("no workspace to paste into")?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        self.put_seq += 1;
        // Zero-padded so the names sort chronologically, which is what
        // `prune_scratch` relies on to decide which are the oldest.
        let path = dir.join(format!("{:06}-{}", self.put_seq, scratch_name(name)));
        std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        prune_scratch(&dir);

        // The bare path, unquoted: agent CLIs read it as a path and quoting
        // would be noise. Nothing butai chooses here contains a space —
        // `workspace_key` and `scratch_name` both sanitize — so the only way to
        // get one is a home directory that has one.
        self.route_input(id, InputEvent::Paste(path.display().to_string()));
        Ok(path)
    }

    /// Write every terminal pane's recent output under `panes/<key>/`, so a
    /// restart can paint the panes back the way [`persist_session`] brings the
    /// workspaces back.
    ///
    /// Called on the ~2s sampler tick and once more as the daemon goes down.
    /// Sampling rather than writing on change is what keeps it affordable: this
    /// copies bounded per-pane rings and writes them, touching no terminal
    /// state, but a busy pane produces output far faster than any disk should
    /// be asked to follow. The cost of sampling is that a hard crash loses the
    /// last couple of seconds — the same bound a crash already had on the
    /// workspace list.
    fn capture_panes(&self) {
        if self.restoring || self.config.general.restore_bytes == 0 {
            return;
        }
        let Some(root) = self.panes_dir() else { return };
        let mut live: std::collections::HashSet<std::ffi::OsString> =
            std::collections::HashSet::new();
        for sid in &self.order {
            let Some(ws) = self.workspaces.get(sid) else { continue };
            let key = workspace_key(&ws.cwd);
            let dir = root.join(&key);
            if std::fs::create_dir_all(&dir).is_err() {
                continue;
            }
            live.insert(key.into());
            let mut wrote = std::collections::HashSet::new();
            for (i, pane) in ws.agents.iter().enumerate() {
                if self.write_dump(&dir, &agent_dump(i), *pane) {
                    wrote.insert(std::ffi::OsString::from(agent_dump(i)));
                }
            }
            for (i, pane) in ws.processes.iter().enumerate() {
                if self.write_dump(&dir, &proc_dump(i), *pane) {
                    wrote.insert(std::ffi::OsString::from(proc_dump(i)));
                }
            }
            // Dumps left by panes that have since been closed. Without this a
            // workspace that had four agents and now has two would replay the
            // two dead ones into the next restore, because the persisted list
            // shrank but the files did not.
            prune_dir(&dir, |name| wrote.contains(name));
        }
        // A workspace held back for a later start still owns its dumps: the
        // session entry survives an unreadable directory (see `restore_session`),
        // and deleting the screens it names would restore it blank.
        for d in &self.deferred {
            if !d.key.is_empty() {
                live.insert(d.key.clone().into());
            }
        }
        // Whole workspaces that have been closed since the last pass.
        prune_dir(&root, |name| live.contains(name));
    }

    /// Persist one pane's output ring. Returns whether a dump now exists for
    /// it — false for a non-terminal pane or one that has produced nothing,
    /// which is also what stops an empty write from replacing a good dump.
    fn write_dump(&self, dir: &Path, name: &str, pane: PaneId) -> bool {
        let Some(PaneState::Terminal(t)) = self.panes.get(&pane) else { return false };
        let bytes = t.history();
        if bytes.is_empty() {
            return false;
        }
        let (cols, rows) = t.size();
        let bytes = encode_dump(cols, rows, &bytes);
        // Temp-then-rename, for the reason the session file does it: a restore
        // reading a half-written dump would replay a truncated escape stream.
        let tmp = dir.join(format!("{name}.tmp"));
        if std::fs::write(&tmp, &bytes).is_ok() && std::fs::rename(&tmp, dir.join(name)).is_ok() {
            return true;
        }
        let _ = std::fs::remove_file(&tmp);
        false
    }

    /// Everything a restart needs, written once before the daemon starts
    /// tearing itself down.
    ///
    /// Ordering matters: [`persist_session`] and [`capture_panes`] both no-op
    /// once `shutdown` is set, and they have to, because teardown kills the
    /// workspaces one at a time and each removal would otherwise write a
    /// smaller session than the one the user actually had open. So the final
    /// snapshot is taken here, before that flag goes up.
    fn snapshot_for_restart(&self) {
        self.persist_session();
        self.capture_panes();
    }

    /// Recreate what a previous run had open: the workspaces, and inside each
    /// one the processes and agents that were running, painted with the output
    /// they were showing.
    ///
    /// A workspace whose directory does not resolve right now is **kept, not
    /// dropped**. Absent is not the same as gone: a network share or an
    /// external disk that has not finished mounting reads exactly like a
    /// deleted folder, and this runs at daemon start, which is precisely when a
    /// mount is least likely to be up. Discarding on that reading cost the
    /// user everything the entry named — its agents, their conversation ids,
    /// its process list — on a single transient miss, with no way back, because
    /// the store is rewritten from the workspaces that *did* come up. So the
    /// unresolved ones ride along in [`deferred`](Self::deferred) and are
    /// written back out on every persist, leaving the next start free to
    /// rebuild them once the directory is there again.
    fn restore_session(&mut self) {
        let Some(path) = self.session_store.clone() else { return };
        let Ok(data) = std::fs::read_to_string(&path) else { return };
        let Ok(state) = serde_json::from_str::<SessionState>(&data) else {
            warn!("ignoring unreadable session file {}", path.display());
            return;
        };
        if state.workspaces.is_empty() {
            return;
        }
        self.restoring = true;
        for pw in &state.workspaces {
            if !pw.cwd.is_dir() {
                info!(
                    "workspace {:?} not restored: {} is not readable right now; \
                     keeping it for the next start",
                    pw.name,
                    pw.cwd.display()
                );
                self.deferred.push(pw.clone());
                continue;
            }
            if self.workspaces.values().any(|w| w.cwd == pw.cwd) {
                continue;
            }
            if let Err(e) =
                self.create_workspace_restoring(pw.name.clone(), pw.cwd.clone(), Some(pw))
            {
                warn!("restore workspace {:?}: {e:#}", pw.cwd);
                // Same reasoning as an unreadable directory: a spawn that failed
                // this once is not a reason to forget the work it described.
                self.deferred.push(pw.clone());
            }
        }
        self.restoring = false;
        // Normalize the store: rewrites what came up, and re-emits what did not.
        self.persist_session();
    }

    /// Resolve a directory to a workspace: reuse the one already open for it,
    /// otherwise create a new one with a unique name. Keeps "one space per
    /// project directory" consistent between the folder picker and the plain
    /// `butai` attach.
    fn workspace_for_cwd(&mut self, dir: &Path) -> anyhow::Result<SessionId> {
        if let Some(sid) = self.workspaces.values().find(|w| w.cwd == dir).map(|w| w.id) {
            return Ok(sid);
        }
        let base = default_ws_name(dir);
        let mut name = base.clone();
        let mut n = 2;
        while self.workspaces.values().any(|w| w.name == name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        self.create_workspace(name, dir.to_path_buf())
    }

    // -- context menu --------------------------------------------------------

    // -- commands ------------------------------------------------------------

    fn exec_command(&mut self, id: ClientId, cmd: Command) {
        let ws_id = self.clients.get(&id).and_then(|c| c.session);
        match cmd {
            Command::SpawnAgent(name) => {
                if let Some(sid) = ws_id {
                    if let Err(e) = self.spawn_agent(sid, &name) {
                        self.report_error(id, format!("{e:#}"));
                    }
                }
            }
            Command::NewProcess { name, command } => {
                if let Some(sid) = ws_id {
                    if let Err(e) = self.new_process(sid, &name, Some(command)) {
                        self.report_error(id, format!("{e:#}"));
                    }
                }
            }
            Command::ClosePane => {
                if let Some(sid) = ws_id {
                    let stage = self.workspaces.get(&sid).and_then(|w| w.stage);
                    if let Some(pane_id) = stage {
                        self.drop_pane(sid, pane_id);
                        self.dirty = true;
                    }
                }
            }
            // Scrolls the pane this connection is *looking at*: the stage for a
            // workbench client, and its own pane for one streaming a single
            // pane — which is every non-TUI client, and now the TUI too. Without
            // the second half, scrollback is the one thing a `pane` attach
            // cannot reach in the pane it is already holding open.
            Command::ScrollPage(pages) => {
                let target = self
                    .clients
                    .get(&id)
                    .and_then(|c| c.pane)
                    .or_else(|| ws_id.and_then(|sid| self.workspaces.get(&sid))?.stage);
                if let Some(pane) = target.and_then(|p| self.panes.get_mut(&p)) {
                    pane.scroll_page(pages);
                    self.dirty = true;
                }
            }
            Command::RenameWindow(name) => {
                if let Some(sid) = ws_id {
                    if let Some(ws) = self.workspaces.get_mut(&sid) {
                        ws.name = name;
                        self.dirty = true;
                    }
                }
            }
            Command::NewSession { name, .. } => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
                let name = name.unwrap_or_else(|| default_ws_name(&cwd));
                match self.create_workspace(name, cwd) {
                    Ok(sid) => {
                        if let Some(c) = self.clients.get_mut(&id) {
                            c.session = Some(sid);
                            c.last_frame = None;
                        }
                        self.dirty = true;
                    }
                    Err(e) => self.report_error(id, format!("{e:#}")),
                }
            }
            Command::KillSession(name) => {
                let sid = self.workspaces.values().find(|s| s.name == name).map(|s| s.id);
                match sid {
                    Some(sid) => {
                        self.kill_workspace(sid);
                        self.send(id, ServerMsg::Ok);
                    }
                    None => self.report_error(id, format!("no workspace named {name:?}")),
                }
            }
            Command::ListSessions => {
                let list: Vec<SessionInfo> =
                    self.order.iter().filter_map(|sid| self.ws_info(*sid)).collect();
                self.send(id, ServerMsg::SessionList(list));
            }
            Command::KillServer => self.kill_server(false),
            Command::KillServerClear => self.kill_server(true),
            // The list, always — never a picker. Which agent to open is the
            // user's choice to make on a screen, and the screen belongs to the
            // client; the daemon's part is knowing what is configured.
            Command::ListAgents => {
                let names = self.config.agents.iter().map(|a| a.name.clone()).collect();
                self.send(id, ServerMsg::AgentList(names));
            }
            Command::ReloadConfig => {
                let (config, warnings) = Config::load();
                for w in warnings {
                    self.report_error(id, format!("config: {w}"));
                }
                self.config = config;
                self.dirty = true;
            }
            // Only the client can see its own clipboard; it answers with
            // `put_file`.
            Command::PasteImage => self.send(id, ServerMsg::ReadClipboardImage),
            Command::PutFile { name, data } => match self.put_file(id, &name, &data) {
                // The path is already pasted into the pane; `file_put` is so a
                // client can say *where* it went, which is the client's to say.
                Ok(path) => self.send(id, ServerMsg::FilePut { path }),
                Err(e) => self.report_error(id, e),
            },
            Command::SplitPane { .. }
            | Command::FocusDir(_)
            | Command::FocusPane(_)
            | Command::ResizePane { .. }
            | Command::NewWindow
            | Command::NextWindow
            | Command::PrevWindow
            | Command::SelectWindow(_)
            | Command::ApplyLayout(_) => {
                self.report_error(
                    id,
                    "the workbench has fixed rails, not free panes — Alt-a/p/g/e focus them, Alt-l resizes, the agent picker and :process fill them".into(),
                );
            }
            // Opening a menu, zooming a rail away and unfolding the ALL
            // AGENTS panel are all the same kind of thing: a change to one
            // screen. The daemon keeps no screen to change — every client draws
            // its own workbench from `/v1/*` and decides for itself what is
            // folded — and honouring them here would move every viewer at once,
            // which is what they used to do and why they are refused rather
            // than quietly ignored.
            Command::GitMenu | Command::ZoomToggle | Command::ToggleAllAgents => {
                self.report_error(
                    id,
                    "the workbench has fixed rails, not free panes — and menus, zoom and the agent panel are each client's own view, drawn from /v1/*".into(),
                );
            }
            // A theme colours a screen, and the daemon has none: the only thing
            // it draws is a program's own cells, which carry the program's own
            // colours. Every client picks its palette from its own config —
            // which is also what lets one terminal be dark and another light
            // while both watch the same workspace.
            Command::OpenFile(_) => {
                self.report_error(
                    id,
                    "opening a file is the client's — read it from /v1/workspaces/{id}/file and draw it".into(),
                );
            }
            Command::SetTheme(_) | Command::ListThemes => {
                self.report_error(
                    id,
                    "themes are the client's — the daemon sends a program's own colours and nothing else".into(),
                );
            }
            // The last command that asked the daemon to write a *client's*
            // preference. The pin is what your `[+ agent]` spawns without
            // asking; it lives in your config, this side never reads it, and
            // `GET /v1/agents` is where the names come from.
            Command::SetDefaultAgent(_) => {
                self.report_error(
                    id,
                    "the pinned agent is the client's — set `[general] default_agent` in your own config".into(),
                );
            }
        }
    }

    // -- HTTP/REST API (Docker-style socket) ---------------------------------

    /// Answer the filesystem-bound API requests on a blocking thread, returning
    /// `None` once a request has been taken over (its reply is sent from there)
    /// and `Some(req, reply)` for everything the actor should handle itself.
    ///
    /// The core is a single actor: every workspace, every attached TUI and every
    /// HTTP client is served from this one thread. A `read`/`stat`/`git` call on
    /// a workspace whose directory has gone away — an unmounted share, a dropped
    /// VPN, a hung NFS/SMB server — blocks in the kernel and cannot be
    /// interrupted, so serving it here froze the entire daemon: no workspace
    /// would repaint, and no client could close anything (not even a healthy
    /// workspace on a different disk). The pane refreshes already run off-thread
    /// for exactly this reason; these are the rest.
    ///
    /// Only the workspace lookup stays here, because it needs `&self` — it is a
    /// map lookup and touches no files.
    fn offload_api(
        &mut self,
        req: ApiRequest,
        reply: tokio::sync::oneshot::Sender<ApiReply>,
    ) -> Option<(ApiRequest, tokio::sync::oneshot::Sender<ApiReply>)> {
        /// Resolve a workspace's worktree root, or bail out with a 404. Not the
        /// cwd: git commands and status paths are both relative to the root.
        macro_rules! git_root_or_404 {
            ($ws:expr) => {
                match self.git_root($ws) {
                    Some(root) => root,
                    None => {
                        let _ = reply.send(ApiReply::NotFound(format!(
                            "workspace {} is not a git repository",
                            $ws
                        )));
                        return None;
                    }
                }
            };
        }

        /// Resolve a workspace's cwd, or bail out with a 404 reply.
        macro_rules! cwd_or_404 {
            ($ws:expr) => {
                match self.workspaces.get(&$ws) {
                    Some(w) => w.cwd.clone(),
                    None => {
                        let _ = reply.send(ApiReply::NotFound(format!("no workspace {}", $ws)));
                        return None;
                    }
                }
            };
        }

        let tx = self.events_tx.clone();
        match req {
            // Not a filesystem call, but off-thread for the same reason and
            // then some: an HTTPS round trip to GitHub and a SHA-256 over a
            // ~6 MB tarball, either of which would stop every pane repainting
            // for as long as it took.
            ApiRequest::Update => {
                if !self.config.update.allow_remote {
                    let _ = reply.send(ApiReply::BadRequest(
                        "this daemon does not take update requests — set `[update] \
                         allow_remote = true` in ~/.butai/config.toml (and \
                         `reload-config`), or run `butai update` on the machine it \
                         is running on"
                            .into(),
                    ));
                    return None;
                }
                if self.updating {
                    let _ = reply.send(ApiReply::Busy("an update is already running".into()));
                    return None;
                }
                self.updating = true;
                // This machine's channel, not the asking client's: the daemon
                // is the thing being replaced, and the track it follows is a
                // property of the install here.
                let channel = self.config.update.channel;
                tokio::spawn(async move {
                    // Blocking throughout — `ureq`, sha2, gzip — so it goes on
                    // the blocking pool rather than a runtime worker.
                    let result = tokio::task::spawn_blocking(move || {
                        butai_update::check(channel)?
                            .map(|offer| butai_update::stage(&offer))
                            .transpose()
                    })
                    .await
                    .map_err(|e| format!("the download did not finish: {e}"))
                    .and_then(|r| r.map_err(|e: anyhow::Error| format!("{e:#}")));
                    let _ = tx.send(Event::UpdateReady { result, reply });
                });
            }
            ApiRequest::Tree { ws, path, filter } => {
                // The markers come from the cached git pane, so they are read
                // here rather than off-thread — but only as an `Arc` clone now.
                // This used to rebuild a `HashSet` of every changed path, on
                // this loop, once per directory listed.
                let (cwd, marked) = match self.workspaces.get(&ws) {
                    Some(w) => (w.cwd.clone(), self.marked_set(w)),
                    None => {
                        let _ = reply.send(ApiReply::NotFound(format!("no workspace {ws}")));
                        return None;
                    }
                };
                spawn_reply(reply, move || {
                    match Self::build_tree(&cwd, marked.as_deref(), filter, &path) {
                        Ok(t) => ApiReply::Tree(t),
                        Err(e) => e,
                    }
                });
            }
            ApiRequest::File { ws, path } => {
                let cwd = cwd_or_404!(ws);
                spawn_reply(reply, move || match Self::build_file(&cwd, &path) {
                    Ok(f) => ApiReply::File(f),
                    Err(e) => e,
                });
            }
            ApiRequest::Search { ws, query } => {
                let cwd = cwd_or_404!(ws);
                // Walks the tree and greps, so it goes to the blocking pool
                // like every other filesystem answer here.
                spawn_reply(reply, move || {
                    let hits = crate::search::run_search(&cwd, &query)
                        .into_iter()
                        .map(|h| butai_protocol::api::SearchHitDto {
                            path: h.path.to_string_lossy().into_owned(),
                            line: h.line,
                            preview: h.preview,
                        })
                        .collect();
                    ApiReply::Search(butai_protocol::api::SearchDto { query, hits })
                });
            }
            ApiRequest::Diff { ws, path, staged } => {
                let cwd = cwd_or_404!(ws);
                // `path` came from `/changes`, so it is repo-root relative. Run
                // git from the worktree root and bound the escape check there
                // too: anchoring both on the workspace cwd made every diff in a
                // workspace opened below the root name the wrong file. The root
                // is the honest boundary — it is exactly the set of paths
                // `/changes` already reports. Falls back to the cwd when there
                // is no repository, so git still produces the 404.
                let root = self.git_root(ws).unwrap_or(cwd);
                spawn_reply(reply, move || match Self::build_diff(&root, &path, staged) {
                    Ok(d) => ApiReply::Diff(d),
                    Err(e) => e,
                });
            }
            // Runs here rather than in `handle_api` because the reply is
            // deferred: the op task answers within the grace window with the
            // real result, or with "accepted" once it is clear this will take
            // longer than an HTTP request should be held open for.
            ApiRequest::GitRun { ws, op } => match self.start_git_op(ws, op) {
                Ok((dto, done)) => {
                    tokio::spawn(async move {
                        let _ = reply.send(match done.await {
                            Ok(Some(Ok(summary))) => ApiReply::GitOp(GitOpDto {
                                running: false,
                                ok: Some(true),
                                summary,
                                ..dto
                            }),
                            // The operation ran and failed. That is a 200 with
                            // `ok:false`, not a 4xx: it is a true report of a
                            // completed request, and the same failure has to be
                            // reported this way anyway once the op outlives the
                            // grace window and no status code is left to carry.
                            Ok(Some(Err(e))) => ApiReply::GitOp(GitOpDto {
                                running: false,
                                ok: Some(false),
                                summary: e,
                                ..dto
                            }),
                            Ok(None) | Err(_) => ApiReply::Accepted(dto),
                        });
                    });
                }
                Err(e) => {
                    let _ = reply.send(match e {
                        GitOpRefusal::NoRepo(_) => ApiReply::NotFound(e.to_string()),
                        GitOpRefusal::Busy(_) => ApiReply::Busy(e.to_string()),
                        GitOpRefusal::Invalid(_) => ApiReply::BadRequest(e.to_string()),
                    });
                }
            },
            // History and the ref lists all shell out, so they belong here
            // rather than on the actor — the same rule `/diff` and `/show`
            // already follow.
            ApiRequest::GitLog { ws, limit, skip, rev, path, all } => {
                let root = git_root_or_404!(ws);
                spawn_reply(reply, move || {
                    match Self::build_log(&root, limit, skip, rev.as_deref(), path.as_deref(), all)
                    {
                        Ok(l) => ApiReply::Log(l),
                        Err(e) => e,
                    }
                });
            }
            ApiRequest::GitStashes(ws) => {
                let root = git_root_or_404!(ws);
                spawn_reply(reply, move || ApiReply::Stashes(Self::build_stashes(&root)));
            }
            ApiRequest::GitRemotes(ws) => {
                let root = git_root_or_404!(ws);
                spawn_reply(reply, move || ApiReply::Remotes(Self::build_remotes(&root)));
            }
            ApiRequest::GitTags(ws) => {
                let root = git_root_or_404!(ws);
                spawn_reply(reply, move || ApiReply::Tags(Self::build_tags(&root)));
            }
            ApiRequest::GitWorktrees(ws) => {
                let root = git_root_or_404!(ws);
                // Which workspace is already open on which path is actor state,
                // so it is collected here and carried into the off-thread read
                // rather than reached for from another thread.
                let open: Vec<(PathBuf, SessionId)> =
                    self.workspaces.iter().map(|(id, w)| (w.cwd.clone(), *id)).collect();
                spawn_reply(reply, move || {
                    ApiReply::Worktrees(Self::build_worktrees(&root, &open))
                });
            }
            ApiRequest::GitConflict { ws, path } => {
                let root = git_root_or_404!(ws);
                spawn_reply(reply, move || match Self::build_conflict(&root, &path) {
                    Ok(c) => ApiReply::Conflict(c),
                    Err(e) => e,
                });
            }
            ApiRequest::Branches(ws) => {
                // Opening the repository is a `Repository::discover`, which is
                // filesystem work: it belongs off the actor like every other
                // scan. It ran here until 0.6.
                let Some(root) = self.git_root(ws) else {
                    let _ = reply.send(ApiReply::NotFound(format!(
                        "workspace {ws} is not a git repository"
                    )));
                    return None;
                };
                spawn_reply(reply, move || {
                    ApiReply::Branches(crate::pane::git::GitPane::branches_at(&root))
                });
            }
            ApiRequest::Show { ws, id } => {
                let cwd = cwd_or_404!(ws);
                spawn_reply(reply, move || match Self::build_show(&cwd, &id) {
                    Ok(d) => ApiReply::Diff(d),
                    Err(e) => e,
                });
            }
            ApiRequest::Download { ws, path } => {
                let cwd = cwd_or_404!(ws);
                spawn_reply(reply, move || match Self::build_download(&cwd, &path) {
                    Ok(r) => r,
                    Err(e) => e,
                });
            }
            ApiRequest::Upload { ws, path, data } => {
                let cwd = cwd_or_404!(ws);
                spawn_reply(reply, move || {
                    let out = Self::build_upload(&cwd, &path, &data);
                    // A successful write changes the git rail; ask the actor to
                    // repaint rather than touching `self` from this thread.
                    if matches!(out, ApiReply::Ok) {
                        let _ = tx.send(Event::WorkspaceWritten(ws));
                    }
                    out
                });
            }
            ApiRequest::DeleteFile { ws, path } => {
                let cwd = cwd_or_404!(ws);
                spawn_reply(reply, move || {
                    let out = Self::build_delete_file(&cwd, &path);
                    // Same repaint as an upload: deleting a tracked file is a
                    // worktree change, and the CHANGES rail is stale until the
                    // actor rescans.
                    if matches!(out, ApiReply::Ok) {
                        let _ = tx.send(Event::WorkspaceWritten(ws));
                    }
                    out
                });
            }
            ApiRequest::BrowseFs { path } => {
                spawn_reply(reply, move || match build_browse(path.as_deref()) {
                    Ok(b) => ApiReply::Browse(b),
                    Err(e) => e,
                });
            }
            ApiRequest::MakeDir { path, name } => {
                spawn_reply(reply, move || match make_dir(path.as_deref(), &name) {
                    Ok(b) => ApiReply::Browse(b),
                    Err(e) => e,
                });
            }
            // Creating a workspace has to touch the actor (it spawns panes), so
            // only the directory probe moves off-thread; the result comes back
            // as `Event::NewWorkspaceResolved`.
            ApiRequest::NewWorkspace { name, path, .. } => {
                tokio::task::spawn_blocking(move || {
                    // Open in the requested directory, else the daemon's cwd.
                    let cwd = match path.as_deref().filter(|p| !p.is_empty()) {
                        Some(p) => {
                            let dir = PathBuf::from(p);
                            if dir.is_dir() {
                                Ok(dir.canonicalize().unwrap_or(dir))
                            } else {
                                Err(format!("not a directory: {p}"))
                            }
                        }
                        None => Ok(std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))),
                    };
                    let _ = tx.send(Event::NewWorkspaceResolved { name, cwd, reply });
                });
            }
            other => return Some((other, reply)),
        }
        None
    }

    /// Create a workspace whose directory has already been probed off-thread.
    fn new_workspace_resolved(
        &mut self,
        name: Option<String>,
        cwd: Result<PathBuf, String>,
        reply: tokio::sync::oneshot::Sender<ApiReply>,
    ) {
        let cwd = match cwd {
            Ok(cwd) => cwd,
            Err(e) => {
                let _ = reply.send(ApiReply::BadRequest(e));
                return;
            }
        };
        let name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| default_ws_name(&cwd));
        let resp = match self.create_workspace(name, cwd) {
            Ok(sid) => ApiReply::Created(sid),
            Err(e) => ApiReply::Error(format!("{e:#}")),
        };
        let created = matches!(resp, ApiReply::Created(_));
        let _ = reply.send(resp);
        if created {
            let ws = self.build_ws_summaries();
            self.broadcast_api(ApiEvent::Workspaces(ws));
        }
    }

    /// Serve one structured API request against live state. Runs inside the
    /// core event loop, so it has exclusive access with no locking.
    fn handle_api(&mut self, req: ApiRequest) -> ApiReply {
        match req {
            ApiRequest::ListWorkspaces => ApiReply::Workspaces(self.build_ws_summaries()),
            ApiRequest::Workspace(sid) => match self.build_ws_detail(sid) {
                Some(d) => ApiReply::Workspace(d),
                None => ApiReply::NotFound(format!("no workspace {sid}")),
            },
            ApiRequest::Agents(sid) => match self.workspaces.get(&sid) {
                Some(_) => ApiReply::Agents(self.build_agents(sid)),
                None => ApiReply::NotFound(format!("no workspace {sid}")),
            },
            ApiRequest::Processes(sid) => match self.workspaces.get(&sid) {
                Some(_) => ApiReply::Processes(self.build_processes(sid)),
                None => ApiReply::NotFound(format!("no workspace {sid}")),
            },
            ApiRequest::Changes(sid) => match self.workspaces.get(&sid) {
                None => ApiReply::NotFound(format!("no workspace {sid}")),
                Some(_) => match self.build_changes(sid) {
                    Some(c) => ApiReply::Changes(c),
                    None => ApiReply::NotFound(format!("workspace {sid} is not a git repository")),
                },
            },
            // Everything that touches the filesystem is answered by
            // `offload_api` on a blocking thread and never reaches here — see
            // the note there for why.
            ApiRequest::Tree { .. }
            | ApiRequest::File { .. }
            | ApiRequest::Diff { .. }
            | ApiRequest::Search { .. }
            | ApiRequest::Show { .. }
            | ApiRequest::Branches(_)
            | ApiRequest::Download { .. }
            | ApiRequest::Upload { .. }
            | ApiRequest::DeleteFile { .. }
            | ApiRequest::BrowseFs { .. }
            | ApiRequest::MakeDir { .. }
            | ApiRequest::NewWorkspace { .. }
            | ApiRequest::GitRun { .. }
            | ApiRequest::GitLog { .. }
            | ApiRequest::GitStashes(_)
            | ApiRequest::GitRemotes(_)
            | ApiRequest::GitTags(_)
            | ApiRequest::GitWorktrees(_)
            | ApiRequest::GitConflict { .. }
            // Same: `offload_api` answers this one from a spawned task, because
            // it is an HTTPS round trip and a checksum over a tarball.
            | ApiRequest::Update => {
                ApiReply::Error("internal: off-thread request was not offloaded".into())
            }
            // Index and ref work: libgit2, on the actor, answered synchronously.
            // A rename answering 202 would be absurd for something that takes a
            // microsecond and cannot fail halfway.
            ApiRequest::GitResolve { ws, path, take } => {
                self.api_git(ws, |g| g.resolve_path(&path, take))
            }
            // Partial staging. Through `api_git` for the one-writer lock:
            // applying to the index while a rebase is rewriting it is exactly
            // the race that lock exists to stop.
            ApiRequest::GitApply { ws, patch, target, reverse } => self.api_git(ws, |g| {
                let root = g.repo_root().to_path_buf();
                crate::pane::git::apply_patch(&root, &patch, target, reverse)
            }),
            ApiRequest::GitBranchCreate { ws, name, from } => {
                self.api_git(ws, |g| g.create_branch(&name, from.as_deref()))
            }
            ApiRequest::GitBranchDelete { ws, name, force } => {
                self.api_git(ws, |g| g.delete_branch(&name, force))
            }
            ApiRequest::GitBranchRename { ws, from, to } => {
                self.api_git(ws, |g| g.rename_branch(from.as_deref(), &to))
            }
            ApiRequest::GitOpStatus(ws) => match self.git_op_status(ws) {
                Some(dto) => ApiReply::GitOp(dto),
                // No operation has ever run here. A 404 rather than an empty
                // body so "never ran" and "ran, produced nothing" stay
                // distinguishable.
                None => ApiReply::NotFound(format!("no git operation for workspace {ws}")),
            },
            ApiRequest::GitOpCancel(ws) => match self.cancel_git_op(ws) {
                Some(dto) => ApiReply::GitOp(dto),
                None => ApiReply::NotFound(format!("nothing running for workspace {ws}")),
            },
            ApiRequest::System => ApiReply::System(self.build_sys_dto()),
            ApiRequest::AgentTypes => {
                ApiReply::AgentTypes(self.config.agents.iter().map(|a| a.name.clone()).collect())
            }
            ApiRequest::Usage => ApiReply::Usage(self.build_usage_dto()),
            ApiRequest::Notifications { since } => {
                ApiReply::Notifications(self.notifications_since(since))
            }
            ApiRequest::KillWorkspace(sid) => {
                if self.workspaces.contains_key(&sid) {
                    self.kill_workspace(sid);
                    ApiReply::Ok
                } else {
                    ApiReply::NotFound(format!("no workspace {sid}"))
                }
            }
            ApiRequest::SpawnAgent { ws, name, background } => {
                if !self.workspaces.contains_key(&ws) {
                    return ApiReply::NotFound(format!("no workspace {ws}"));
                }
                match self.spawn_agent_restoring(ws, &name, None, None, false, background) {
                    Ok(_) => ApiReply::Ok,
                    Err(e) => ApiReply::BadRequest(format!("{e:#}")),
                }
            }
            ApiRequest::NewProcess { ws, name, command } => {
                if !self.workspaces.contains_key(&ws) {
                    return ApiReply::NotFound(format!("no workspace {ws}"));
                }
                // An empty command is "give me a shell", not "run nothing":
                // `new_process` already takes `None` for exactly that.
                let command = Some(command).filter(|c: &String| !c.trim().is_empty());
                match self.new_process(ws, &name, command) {
                    Ok(()) => ApiReply::Ok,
                    Err(e) => ApiReply::BadRequest(format!("{e:#}")),
                }
            }
            ApiRequest::RestartProcess { ws, pane } => self.api_restart_process(ws, pane),
            ApiRequest::KillPane { ws, pane } => {
                let Some(w) = self.workspaces.get_mut(&ws) else {
                    return ApiReply::NotFound(format!("no workspace {ws}"));
                };
                if !w.all_panes().contains(&pane) {
                    return ApiReply::NotFound(format!("no pane {pane} in workspace {ws}"));
                }
                self.drop_pane(ws, pane);
                self.dirty = true;
                ApiReply::Ok
            }
            ApiRequest::PaneOutput { ws, pane, lines, source, format } => {
                let known = match self.workspaces.get(&ws) {
                    Some(w) => w.all_panes().contains(&pane),
                    None => return ApiReply::NotFound(format!("no workspace {ws}")),
                };
                if !known {
                    return ApiReply::NotFound(format!("no pane {pane} in workspace {ws}"));
                }
                let row_format = match format {
                    OutputFormat::Text => RowFormat::Text,
                    OutputFormat::Ansi => RowFormat::Ansi,
                };
                match self.panes.get_mut(&pane) {
                    Some(PaneState::Terminal(t)) => {
                        // `text_output` owns the trailing-blank rule: a
                        // scrollback read has to know where the output ends
                        // before it can count back from it, while `screen` and
                        // `footer` return the viewport padding and all.
                        let (rows_out, more) = t.text_output(
                            lines,
                            source == OutputSource::Scrollback,
                            source == OutputSource::Footer,
                            row_format,
                        );
                        let (cols, rows) = t.size();
                        ApiReply::PaneOutput(PaneOutputDto {
                            pane,
                            cols,
                            rows,
                            source,
                            format,
                            lines: rows_out,
                            more,
                            alt_screen: t.alternate_screen(),
                            cursor: t.cursor(),
                            exited: t.exit_code(),
                        })
                    }
                    // Editors, the file tree and the git/diff rails render
                    // themselves rather than running a PTY, so there is no
                    // terminal output to read.
                    Some(_) => ApiReply::BadRequest(format!("pane {pane} is not a terminal")),
                    None => ApiReply::NotFound(format!("no pane {pane}")),
                }
            }
            ApiRequest::PaneInput { ws, pane, input } => {
                // Validate the pane belongs to the workspace, then write the key
                // straight to its PTY. Unlike the framed attach path
                // (`route_pane_input`), this never resizes the pane or touches any
                // client's focus — it just delivers the event.
                let known = match self.workspaces.get(&ws) {
                    Some(w) => w.all_panes().contains(&pane),
                    None => return ApiReply::NotFound(format!("no workspace {ws}")),
                };
                if !known {
                    return ApiReply::NotFound(format!("no pane {pane} in workspace {ws}"));
                }
                self.note_agent_spoken(pane, &input);
                if !self.panes.contains_key(&pane) {
                    return ApiReply::NotFound(format!("no pane {pane}"));
                }
                // Acting on a pane is looking at it: clear any pending bell, or
                // an Accept/Stop would leave the agent pinned to `waiting` even
                // though the user just answered it.
                self.look_at_pane(pane);
                match self.panes.get_mut(&pane) {
                    Some(p) => {
                        p.handle_input(&input);
                        self.dirty = true;
                        ApiReply::Ok
                    }
                    None => ApiReply::NotFound(format!("no pane {pane}")),
                }
            }
            ApiRequest::AckPane { ws, pane } => {
                let known = match self.workspaces.get(&ws) {
                    Some(w) => w.all_panes().contains(&pane),
                    None => return ApiReply::NotFound(format!("no workspace {ws}")),
                };
                if !known {
                    return ApiReply::NotFound(format!("no pane {pane} in workspace {ws}"));
                }
                match self.panes.get(&pane) {
                    // Only a terminal has a bell to clear; acking anything else is
                    // a no-op rather than an error, so a client can ack blindly.
                    Some(PaneState::Terminal(_)) => {
                        self.look_at_pane(pane);
                        self.dirty = true;
                        ApiReply::Ok
                    }
                    Some(_) => ApiReply::Ok,
                    None => ApiReply::NotFound(format!("no pane {pane}")),
                }
            }
            ApiRequest::StageFile { ws, path } => self.api_git(ws, |g| g.stage_path(&path)),
            ApiRequest::UnstageFile { ws, path } => self.api_git(ws, |g| g.unstage_path(&path)),
            ApiRequest::DiscardFile { ws, path } => self.api_git(ws, |g| g.discard_path(&path)),
            ApiRequest::Commit { ws, message } => {
                self.api_git(ws, |g| g.commit_with(&message).map(|_| ()))
            }
            ApiRequest::CommitAll { ws, message } => self.api_git(ws, |g| {
                // The rows still show the pre-stage state (the rescan is
                // off-thread), so count what `stage_all` just staged.
                let just_staged = g.stage_all()?;
                if just_staged + g.staged_summary().0 == 0 {
                    return Err("nothing to commit".into());
                }
                g.commit_with(&message).map(|_| ())
            }),
            ApiRequest::Checkout { ws, branch, create } => {
                self.api_git(ws, |g| g.checkout(&branch, create))
            }
        }
    }

    /// Give a CHANGES rail to any workspace that has become a git repository
    /// since it opened. The rail is only created at workspace creation, so a
    /// `git init` (or a clone finishing) in the cwd would otherwise stay
    /// invisible until the workspace was reopened.
    ///
    /// The probe itself (`Repository::discover`) runs off-thread: it stats its
    /// way up the directory tree, which is cheap locally but not on a network
    /// mount, and this runs on every ~2s sampler tick. Results arrive as
    /// [`Event::RepoProbed`]. Workspaces that are not repositories back off
    /// from [`REPO_PROBE_MIN`] to [`REPO_PROBE_MAX`] so they stop costing
    /// anything, while an in-flight guard keeps the tick from stacking probes.
    fn attach_new_repos(&mut self) {
        let now = Instant::now();
        let candidates: Vec<(SessionId, PathBuf)> = self
            .workspaces
            .iter()
            .filter(|(sid, w)| {
                w.changes.is_none()
                    && !self.repo_probing.contains(sid)
                    && self.repo_probe_next.get(sid).is_none_or(|(at, _)| now >= *at)
            })
            .map(|(sid, w)| (*sid, w.cwd.clone()))
            .collect();
        for (sid, cwd) in candidates {
            self.repo_probing.insert(sid);
            let tx = self.events_tx.clone();
            tokio::task::spawn_blocking(move || {
                let pane = crate::pane::git::GitPane::new(&cwd).ok();
                let _ = tx.send(Event::RepoProbed(sid, pane));
            });
        }
    }

    /// Install (or decline) a CHANGES rail from a finished [`attach_new_repos`]
    /// probe. Re-checks `changes.is_none()` because the workspace may have
    /// gained a rail — or gone away entirely — while the probe was in flight.
    fn on_repo_probed(&mut self, sid: SessionId, git: Option<crate::pane::git::GitPane>) {
        self.repo_probing.remove(&sid);
        let Some(git) = git else {
            // Not a repository: back off, doubling up to the cap.
            let next = self
                .repo_probe_next
                .get(&sid)
                .map(|(_, backoff)| (*backoff * 2).min(REPO_PROBE_MAX))
                .unwrap_or(REPO_PROBE_MIN);
            self.repo_probe_next.insert(sid, (Instant::now() + next, next));
            return;
        };
        self.repo_probe_next.remove(&sid);
        let Some(w) = self.workspaces.get(&sid) else { return };
        if w.changes.is_some() {
            return; // raced with another attach
        }
        let id = self.alloc_pane_id();
        self.panes.insert(id, PaneState::Git(git));
        if let Some(w) = self.workspaces.get_mut(&sid) {
            w.changes = Some(id);
        }
        self.request_git_refresh(id);
        self.dirty = true;
    }

    /// Kick off a git status scan for a changes pane on a blocking thread and
    /// deliver the result as [`Event::GitRefreshed`]. The scan walks the whole
    /// worktree — running it on the core loop froze the whole TUI on big repos
    /// and network filesystems. A per-pane in-flight guard keeps the periodic
    /// sampler from stacking scans faster than they complete.
    ///
    /// **A request that arrives while one is running is deferred, not
    /// dropped.** The running scan started before whatever prompted this call,
    /// so its answer cannot include it — dropping the request leaves the rail
    /// showing the tree from before a commit, with nothing scheduled to
    /// correct it. See [`git_refresh_again`](Self::git_refresh_again).
    fn request_git_refresh(&mut self, pane: PaneId) {
        let Some(PaneState::Git(g)) = self.panes.get(&pane) else { return };
        if !self.git_refreshing.insert(pane) {
            // One is already running, and it started before whatever prompted
            // this call — so its answer will be stale. Remember to run again
            // when it lands.
            self.git_refresh_again.insert(pane);
            return;
        }
        let workdir = g.workdir().to_path_buf();
        let tx = self.events_tx.clone();
        tokio::task::spawn_blocking(move || {
            let snap = crate::pane::git::GitPane::compute(&workdir);
            let _ = tx.send(Event::GitRefreshed(pane, snap));
        });
    }

    /// Run a mutating closure against a workspace's git pane, mapping errors
    /// to API replies. Shared by stage/unstage/commit.
    fn api_git(
        &mut self,
        ws: SessionId,
        f: impl FnOnce(&mut crate::pane::git::GitPane) -> Result<(), String>,
    ) -> ApiReply {
        let Some(w) = self.workspaces.get(&ws) else {
            return ApiReply::NotFound(format!("no workspace {ws}"));
        };
        let Some(pane) = w.changes else {
            return ApiReply::NotFound(format!("workspace {ws} is not a git repository"));
        };
        // One writer per repository. A stage racing a rebase's checkout writes
        // to an index git is in the middle of rewriting; reads and refreshes
        // stay allowed, so the rail keeps updating while an operation runs.
        if let Some(kind) = self.git_write_locked(ws) {
            return ApiReply::Busy(format!("a git operation is already running: {kind}"));
        }
        let Some(PaneState::Git(g)) = self.panes.get_mut(&pane) else {
            return ApiReply::Error("git pane missing".into());
        };
        match f(g) {
            Ok(()) => {
                // The mutation no longer rescans inline (that is a whole-worktree
                // walk); the refreshed rows land via `Event::GitRefreshed`.
                self.request_git_refresh(pane);
                self.dirty = true;
                ApiReply::Ok
            }
            Err(e) => ApiReply::BadRequest(e),
        }
    }

    /// Restart a managed process by pane id (API equivalent of the rail `r`).
    fn api_restart_process(&mut self, ws: SessionId, pane: PaneId) -> ApiReply {
        let Some(w) = self.workspaces.get_mut(&ws) else {
            return ApiReply::NotFound(format!("no workspace {ws}"));
        };
        let Some(meta) = w.proc_meta.get(&pane).cloned() else {
            return ApiReply::NotFound(format!("no process pane {pane} in workspace {ws}"));
        };
        let cwd = w.cwd.clone();
        w.forget_pane(pane);
        self.panes.remove(&pane);
        let meta = ProcMeta { ready_seen: false, ready_carry: String::new(), ..meta };
        match self.spawn_process(ws, &cwd, meta, false, None) {
            Ok(_) => ApiReply::Ok,
            Err(e) => ApiReply::Error(format!("{e:#}")),
        }
    }

    /// The sampler's roster, with the live panes stitched in.
    ///
    /// The split is deliberate: the sampler probes the machine and knows
    /// nothing about panes, and the core owns the panes and knows nothing about
    /// transcripts. `panes` is therefore filled here, at the moment of asking,
    /// so it is never stale by a sampler interval — a pane that started two
    /// seconds ago is on the account now, and the page's whole claim is that it
    /// can say so.
    ///
    /// Every workspace this daemon serves is counted, because the account is
    /// burned by all of them. A remote machine's agents are on its own daemon's
    /// roster and are counted there — the fleet view is the client's to
    /// assemble, since only it knows which daemons it is holding.
    fn build_usage_dto(&self) -> UsageDto {
        let Some(sampled) = &self.usage else { return UsageDto::default() };
        let mut dto = sampled.clone();
        for cli in &mut dto.clis {
            cli.panes = self
                .workspaces
                .values()
                .flat_map(|w| w.agent_meta.iter())
                .filter(|(pane, meta)| meta.name == cli.name && self.panes.contains_key(pane))
                .map(|(pane, _)| *pane)
                .collect();
            cli.panes.sort_unstable();
        }
        dto
    }

    fn build_sys_dto(&self) -> SysDto {
        let Some(s) = &self.sys else { return SysDto::default() };
        SysDto {
            cpu_pct: s.cpu_pct,
            cpu_temp: s.cpu_temp,
            cpu_hist: s.cpu_hist.clone(),
            cpu_model: s.cpu_model.clone(),
            cpu_cores: s.cpu_cores,
            cpu_threads: s.cpu_threads,
            ram_used_gb: s.ram_used_gb,
            ram_total_gb: s.ram_total_gb,
            ram_hist: s.ram_hist.clone(),
            swap_used_gb: s.swap_used_gb,
            swap_total_gb: s.swap_total_gb,
            gpus: s
                .gpus
                .iter()
                .map(|g| GpuDto {
                    pct: g.pct,
                    mem_used_gb: g.mem_used_gb,
                    mem_total_gb: g.mem_total_gb,
                    hist: g.hist.clone(),
                    name: g.name.clone(),
                    temp_c: g.temp_c,
                    power_w: g.power_w,
                })
                .collect(),
            net: s
                .net
                .iter()
                .map(|n| NetDto {
                    name: n.name.clone(),
                    rx_bps: n.rx_bps,
                    tx_bps: n.tx_bps,
                    rx_hist: n.rx_hist.clone(),
                    tx_hist: n.tx_hist.clone(),
                    kind: n.kind,
                    carrier: n.carrier,
                    default_route: n.default_route,
                    speed_mbps: n.speed_mbps,
                    driver: n.driver.clone(),
                })
                .collect(),
            disks: s
                .disks
                .iter()
                .map(|d| DiskDto {
                    mount: d.mount.clone(),
                    source: d.source.clone(),
                    fstype: d.fstype.clone(),
                    kind: d.kind,
                    used_gb: d.used_gb,
                    total_gb: d.total_gb,
                    stale: d.stale,
                })
                .collect(),
            containers: s
                .containers
                .iter()
                .map(|c| ContainerDto { name: c.name.clone(), state: c.state.clone() })
                .collect(),
            stacks: build_stack_dtos(&s.containers),
        }
    }

    fn attached_count(&self, sid: SessionId) -> usize {
        self.clients.values().filter(|c| c.session == Some(sid) && !c.control).count()
    }

    fn build_ws_summaries(&self) -> Vec<WorkspaceSummary> {
        self.order
            .iter()
            .filter_map(|sid| self.workspaces.get(sid))
            .map(|w| WorkspaceSummary {
                id: w.id,
                name: w.name.clone(),
                cwd: w.cwd.display().to_string(),
                agents: w.agents.len(),
                waiting: self.count_agents(w, AgentState::Waiting),
                working: self.count_agents(w, AgentState::Working),
                finished: self.count_agents(w, AgentState::Finished),
                exited: self.count_agents(w, AgentState::Exited),
                questions: self.count_questions(w),
                unread: self.count_unread(w),
                processes: w.processes.len(),
                changes: self.git_pane(w).map(|g| g.change_count()).unwrap_or(0),
                conflicts: self.git_pane(w).map(|g| g.conflict_count()).unwrap_or(0),
                repo_state: self.git_pane(w).map(|g| g.state()).unwrap_or_default(),
                attached_clients: self.attached_count(w.id),
            })
            .collect()
    }

    fn build_ws_detail(&mut self, sid: SessionId) -> Option<WorkspaceDetail> {
        // Read the workspace's own fields out first: building the process rows
        // borrows `self` mutably (see `build_processes`).
        let (id, name, cwd, stage) = {
            let w = self.workspaces.get(&sid)?;
            (w.id, w.name.clone(), w.cwd.display().to_string(), w.stage)
        };
        Some(WorkspaceDetail {
            id,
            name,
            cwd,
            agents: self.build_agents(sid),
            processes: self.build_processes(sid),
            changes: self.build_changes(sid),
            stage,
        })
    }

    fn agent_dto(&self, pane: PaneId) -> Option<AgentDto> {
        let PaneState::Terminal(t) = self.panes.get(&pane)? else { return None };
        // The debounced status is the single source of truth. Every agent is
        // seeded into `agent_track` at spawn (see `spawn_agent`), so there is no
        // untracked window to fall back for — and the old fallback read the raw
        // `Attention`, which has no `Finished`, so it could publish a state the
        // state machine would never produce.
        let track = self.agent_track.get(&pane);
        let state = track.map(|tr| tr.state).unwrap_or(AgentState::Idle);
        Some(AgentDto {
            pane,
            title: t.agent_title(),
            state,
            exited: t.exit_code(),
            // Read live rather than from the track: it describes what is on
            // screen right now, and unlike the state it needs no debouncing.
            question: t.shows_input_prompt(),
            started_ms: t.started_ms(),
            working_since_ms: t.working_since_ms(),
            // An untracked pane has crossed no edge this daemon saw, so it has
            // no news to report — the same answer the `unwrap_or` above gives.
            unread: track.is_some_and(|tr| tr.unread),
        })
    }

    fn build_agents(&self, sid: SessionId) -> Vec<AgentDto> {
        let Some(w) = self.workspaces.get(&sid) else { return Vec::new() };
        w.agents.iter().filter_map(|p| self.agent_dto(*p)).collect()
    }

    /// Count a workspace's agents currently in `state` — feeds the list-view
    /// "needs you" / "working" badges in [`build_ws_summaries`]. A dead agent
    /// reports [`AgentState::Exited`], so it falls out of the live buckets on its
    /// own and no separate `exited.is_none()` guard is needed.
    fn count_agents(&self, w: &Workspace, state: AgentState) -> usize {
        w.agents.iter().filter(|p| self.agent_dto(**p).is_some_and(|a| a.state == state)).count()
    }

    /// Count a workspace's agents with a decision prompt actually on screen — the
    /// subset of `waiting` that is a real question rather than a bell.
    fn count_questions(&self, w: &Workspace) -> usize {
        w.agents.iter().filter(|p| self.agent_dto(**p).is_some_and(|a| a.question)).count()
    }

    /// Count a workspace's agents holding an unlooked-at your-move edge — what
    /// backs "two turns landed here while you were gone" in a list view.
    fn count_unread(&self, w: &Workspace) -> usize {
        w.agents.iter().filter(|p| self.agent_dto(**p).is_some_and(|a| a.unread)).count()
    }

    /// Takes `&mut self` because relabelling a shell row reads the pane's
    /// foreground command, which memoizes its `/proc` lookup.
    fn build_processes(&mut self, sid: SessionId) -> Vec<ProcessDto> {
        let Some(w) = self.workspaces.get(&sid) else { return Vec::new() };
        let rows: Vec<(PaneId, String, String, bool)> = w
            .processes
            .iter()
            .filter_map(|p| {
                let meta = w.proc_meta.get(p)?;
                Some((*p, meta.name.clone(), meta.command.clone(), meta.ready_seen))
            })
            .collect();

        rows.into_iter()
            .filter_map(|(pane, name, command, ready_seen)| {
                let PaneState::Terminal(t) = self.panes.get_mut(&pane)? else { return None };
                use crate::pane::terminal::Attention;
                let status = match t.exit_code() {
                    Some(0) => "done".to_string(),
                    Some(code) => format!("FAIL({code})"),
                    None if ready_seen => "ok".to_string(),
                    None if t.attention() == Attention::Working => "...".to_string(),
                    None => "run".to_string(),
                };
                // Same rule the TUI rail uses (`proc_row_name`): a generic
                // "shell" row is named by whatever it is actually running, so
                // six shells are not six identical rows. Without this a client
                // that draws natively from the REST API — the web client, the
                // native apps — cannot show what the TUI shows.
                let name =
                    if name == "shell" { t.foreground_cmdline().unwrap_or(name) } else { name };
                Some(ProcessDto { pane, name, command, status, exited: t.exit_code() })
            })
            .collect()
    }

    fn build_changes(&self, sid: SessionId) -> Option<butai_protocol::api::ChangesDto> {
        let w = self.workspaces.get(&sid)?;
        let pane = w.changes?;
        match self.panes.get(&pane)? {
            PaneState::Git(g) => Some(g.to_dto()),
            _ => None,
        }
    }

    /// The worktree root of a workspace's repository, read off the cached git
    /// pane so it costs no filesystem work. `None` when the workspace is gone or
    /// has no repository — callers turn that into the same 404 either way.
    ///
    /// Status paths are relative to *this*, not to the workspace cwd, which may
    /// be deeper: a workspace opened at `repo/crates/foo` still sees changes
    /// reported as `crates/foo/src/lib.rs`.
    fn git_root(&self, sid: SessionId) -> Option<PathBuf> {
        let w = self.workspaces.get(&sid)?;
        let pane = w.changes?;
        match self.panes.get(&pane)? {
            PaneState::Git(g) => Some(g.repo_root().to_path_buf()),
            _ => None,
        }
    }

    /// A workspace's git pane, if it has one. Every summary field reads from it,
    /// and it is a cached snapshot, so this costs no filesystem work.
    fn git_pane(&self, w: &Workspace) -> Option<&crate::pane::git::GitPane> {
        match self.panes.get(&w.changes?)? {
            PaneState::Git(g) => Some(g),
            _ => None,
        }
    }

    /// The marker set for a workspace's tree, borrowed from its git pane.
    ///
    /// An `Arc` clone. This used to copy every changed path into a fresh
    /// `HashSet` — on the core loop, once per directory listed.
    fn marked_set(&self, w: &Workspace) -> Option<Arc<crate::pane::git::Marked>> {
        match self.panes.get(&w.changes?)? {
            PaneState::Git(g) => Some(g.marked()),
            _ => None,
        }
    }

    /// Directory listing under the workspace cwd, with git-changed markers.
    ///
    /// `filter` decides the rows *and* the markers, which is the whole point of
    /// it: a `●` on a directory promises a marked row somewhere below, and a
    /// filter applied to only one of the two makes that promise a lie. A
    /// workspace with no git pane has no markers and every entry answers
    /// `changed: false`.
    //
    // The `Err` here is not an error value that gets converted into a reply —
    // it *is* the reply, handed straight back to the client. Boxing it to get
    // under clippy's size bound would mean an allocation on every rejected
    // request and an unwrap at the one place that consumes it. The same applies
    // to the other `Result<_, ApiReply>` builders below.
    #[allow(clippy::result_large_err)]
    fn build_tree(
        cwd: &Path,
        marked: Option<&crate::pane::git::Marked>,
        filter: TreeFilter,
        rel: &str,
    ) -> Result<TreeDto, ApiReply> {
        let dir = safe_join(cwd, rel)
            .ok_or_else(|| ApiReply::BadRequest("path escapes workspace".into()))?;
        if !dir.is_dir() {
            return Err(ApiReply::NotFound(format!("no directory {rel:?}")));
        }
        let mut entries = Vec::new();
        let rd = std::fs::read_dir(&dir).map_err(|e| ApiReply::Error(format!("{e}")))?;
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_string();
            if name == ".git" {
                continue;
            }
            let is_dir = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if !filter.keeps(&name, is_dir) {
                continue;
            }
            let full = ent.path();
            let path = full.strip_prefix(cwd).unwrap_or(&full).to_string_lossy().replace('\\', "/");
            // One lookup, files and directories alike: the set is closed over
            // the ancestors of every changed path, so a directory is in it
            // exactly when something the filter keeps sits below it.
            let changed = marked.is_some_and(|m| m.contains(&full, filter));
            let size = if is_dir { 0 } else { ent.metadata().map(|m| m.len()).unwrap_or(0) };
            entries.push(TreeEntry { name, path, is_dir, changed, size });
        }
        // Directories first, then case-insensitive by name (like the TUI tree).
        entries.sort_by(|a, b| {
            b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(TreeDto { path: rel.to_string(), entries })
    }

    /// A file's UTF-8 text (lossy), capped so a giant file can't blow the reply.
    // `Err` is the reply itself; see `build_tree`.
    #[allow(clippy::result_large_err)]
    fn build_file(cwd: &Path, rel: &str) -> Result<FileDto, ApiReply> {
        const CAP: usize = 512 * 1024;
        let path = safe_join(cwd, rel)
            .ok_or_else(|| ApiReply::BadRequest("path escapes workspace".into()))?;
        let bytes = std::fs::read(&path).map_err(|e| ApiReply::NotFound(format!("{rel}: {e}")))?;
        let truncated = bytes.len() > CAP;
        let slice = &bytes[..bytes.len().min(CAP)];
        Ok(FileDto {
            path: rel.to_string(),
            text: String::from_utf8_lossy(slice).into_owned(),
            truncated,
        })
    }

    /// A workspace file's raw bytes for download (any type, no text cap).
    // `Err` is the reply itself; see `build_tree`.
    #[allow(clippy::result_large_err)]
    fn build_download(cwd: &Path, rel: &str) -> Result<ApiReply, ApiReply> {
        const CAP: usize = 64 * 1024 * 1024;
        let path = safe_join(cwd, rel)
            .ok_or_else(|| ApiReply::BadRequest("path escapes workspace".into()))?;
        let meta =
            std::fs::metadata(&path).map_err(|e| ApiReply::NotFound(format!("{rel}: {e}")))?;
        if meta.is_dir() {
            return Err(ApiReply::BadRequest(format!("{rel} is a directory")));
        }
        if meta.len() as usize > CAP {
            return Err(ApiReply::BadRequest("file too large to download".into()));
        }
        let data = std::fs::read(&path).map_err(|e| ApiReply::Error(format!("{rel}: {e}")))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "download".into());
        Ok(ApiReply::Bytes {
            data,
            content_type: "application/octet-stream".into(),
            download_name: Some(name),
        })
    }

    /// Write uploaded bytes to a workspace file, creating parent dirs.
    fn build_upload(cwd: &Path, rel: &str, data: &[u8]) -> ApiReply {
        if rel.trim().is_empty() {
            return ApiReply::BadRequest("empty upload path".into());
        }
        let Some(path) = safe_join(cwd, rel) else {
            return ApiReply::BadRequest("path escapes workspace".into());
        };
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return ApiReply::Error(format!("mkdir {}: {e}", parent.display()));
            }
        }
        match std::fs::write(&path, data) {
            Ok(()) => ApiReply::Ok,
            Err(e) => ApiReply::Error(format!("write {rel}: {e}")),
        }
    }

    /// Delete one workspace file. The inverse of [`Self::build_upload`], and
    /// deliberately narrower than it in two ways.
    ///
    /// **Files only.** A directory is refused rather than removed recursively:
    /// the client's confirm box asks about one row, and `remove_dir_all` on a
    /// mistyped row is the difference between losing a file and losing `src`.
    /// The check is `symlink_metadata`, so a symlink *to* a directory is deleted
    /// as the link it is instead of being refused for what it points at.
    ///
    /// **Missing is a 404, not a success.** Deleting something that was already
    /// gone is a client working from a stale listing, and a silent `ok` would
    /// leave it believing it had just removed a file that something else did.
    fn build_delete_file(cwd: &Path, rel: &str) -> ApiReply {
        if rel.trim().is_empty() {
            return ApiReply::BadRequest("empty delete path".into());
        }
        let Some(path) = safe_join(cwd, rel) else {
            return ApiReply::BadRequest("path escapes workspace".into());
        };
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ApiReply::NotFound(format!("no such file: {rel}"));
            }
            Err(e) => return ApiReply::Error(format!("stat {rel}: {e}")),
            Ok(md) if md.is_dir() => {
                return ApiReply::BadRequest(format!("{rel} is a directory"));
            }
            Ok(_) => {}
        }
        match std::fs::remove_file(&path) {
            Ok(()) => ApiReply::Ok,
            Err(e) => ApiReply::Error(format!("delete {rel}: {e}")),
        }
    }

    /// Run a read-only `git` command in `root` and hand back its stdout.
    ///
    /// Reads only: nothing here writes the repository, so none of it needs the
    /// operation runner's lock, timeouts or progress. `GIT_OPTIONAL_LOCKS=0`
    /// keeps a listing from taking the index lock and racing a real operation.
    fn git_read(root: &Path, args: &[&str]) -> Result<String, String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .arg("--no-pager")
            .args(args)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|e| format!("git: {e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// A page of history.
    ///
    /// Asks for one commit more than requested: that is how `more` is known
    /// without a second count, and a count over a big history is not cheap.
    // `Err` is the reply itself; see `build_tree`.
    #[allow(clippy::result_large_err)]
    fn build_log(
        root: &Path,
        limit: usize,
        skip: usize,
        rev: Option<&str>,
        path: Option<&str>,
        all: bool,
    ) -> Result<butai_protocol::api::LogDto, ApiReply> {
        use butai_protocol::api::{LogDto, LogEntryDto};
        // Unit-separated so a summary containing the separator cannot split a
        // record — `--format` with a tab or a pipe eventually does. `%P` and
        // `%D` are the graph: parents are its edges, decoration its labels.
        // `--decorate=full` rather than the default shorthand so a ref's kind
        // is read off its `refs/heads|remotes|tags/` prefix instead of guessed
        // — a tag and a branch may share a name.
        const FMT: &str = "--format=%H%x1f%s%x1f%an%x1f%aI%x1f%P%x1f%D";
        let skip_s = skip.to_string();
        let n_s = (limit + 1).to_string();
        // `--topo-order` because `parents` is only usable if a child is never
        // listed after its parent: a graph renderer assigns lanes in one pass
        // down the page, and the default date order lets a rebased or
        // cherry-picked commit arrive early enough to break that. For a linear
        // history the two orders agree, so nothing that reads this today moves.
        let mut args: Vec<&str> =
            vec!["log", FMT, "--decorate=full", "--topo-order", "-n", &n_s, "--skip", &skip_s];
        if all && rev.is_some() {
            return Err(ApiReply::BadRequest("?all= and ?rev= name different walks".into()));
        }
        if all {
            // Not `--all`: that includes `refs/stash`, and a stash is two
            // synthetic commits ("WIP on main", "index on main") that would sit
            // in the graph as if someone had committed them. Stashes have their
            // own endpoint and their own list. `refs/notes` goes the same way.
            //
            // `HEAD` joins the union because a detached checkout is on no
            // branch: without it the graph omits the commit you are standing
            // on, which is the one row a client is certain to want.
            args.extend(["--branches", "--tags", "--remotes", "HEAD"]);
        }
        if let Some(rev) = rev {
            crate::git_op::valid_rev(rev).map_err(ApiReply::BadRequest)?;
            args.push(rev);
        }
        if let Some(p) = path {
            if p.split('/').any(|c| c == "..") {
                return Err(ApiReply::BadRequest("path escapes the repository".into()));
            }
            args.push("--");
            args.push(p);
        }
        let out = Self::git_read(root, &args).map_err(|e| {
            // An empty repository has no HEAD to walk; that is a normal state,
            // not a failure, and answers with an empty page.
            if e.contains("does not have any commits") {
                ApiReply::Log(LogDto { commits: Vec::new(), more: false })
            } else {
                ApiReply::BadRequest(e)
            }
        });
        let out = match out {
            Ok(o) => o,
            // The empty-repo case above is already a finished reply.
            Err(reply @ ApiReply::Log(_)) => return Err(reply),
            Err(e) => return Err(e),
        };
        let mut commits: Vec<LogEntryDto> = out
            .lines()
            .filter(|l| !l.is_empty())
            .filter_map(|line| {
                let mut f = line.split('\u{1f}');
                Some(LogEntryDto {
                    id: f.next()?.to_string(),
                    summary: f.next().unwrap_or_default().to_string(),
                    author: f.next().unwrap_or_default().to_string(),
                    date: f.next().unwrap_or_default().to_string(),
                    parents: f
                        .next()
                        .unwrap_or_default()
                        .split_whitespace()
                        .map(str::to_string)
                        .collect(),
                    refs: crate::pane::git::parse_decoration(f.next().unwrap_or_default()),
                })
            })
            .collect();
        let more = commits.len() > limit;
        commits.truncate(limit);
        Ok(LogDto { commits, more })
    }

    fn build_stashes(root: &Path) -> Vec<butai_protocol::api::StashDto> {
        use butai_protocol::api::StashDto;
        let Ok(out) = Self::git_read(root, &["stash", "list", "--format=%gd%x1f%gs"]) else {
            return Vec::new();
        };
        out.lines()
            .filter(|l| !l.is_empty())
            .enumerate()
            .map(|(i, line)| {
                let mut f = line.split('\u{1f}');
                let _ref = f.next().unwrap_or_default();
                let subject = f.next().unwrap_or_default();
                // `WIP on main: abc1234 summary` — split the branch out so a
                // client can show it separately without parsing it again.
                let (branch, message) = match subject.split_once(": ") {
                    Some((head, rest)) => {
                        (head.rsplit(' ').next().unwrap_or("").to_string(), rest.to_string())
                    }
                    None => (String::new(), subject.to_string()),
                };
                StashDto { index: i, branch, message }
            })
            .collect()
    }

    fn build_remotes(root: &Path) -> Vec<butai_protocol::api::RemoteDto> {
        use butai_protocol::api::RemoteDto;
        let Ok(out) = Self::git_read(root, &["remote", "-v"]) else {
            return Vec::new();
        };
        let mut seen: Vec<RemoteDto> = Vec::new();
        for line in out.lines() {
            // `origin\tgit@host:repo.git (fetch)` — one line per direction.
            let mut f = line.split_whitespace();
            let (Some(name), Some(url)) = (f.next(), f.next()) else { continue };
            if seen.iter().any(|r| r.name == name) {
                continue;
            }
            seen.push(RemoteDto { name: name.to_string(), url: url.to_string() });
        }
        seen
    }

    fn build_tags(root: &Path) -> Vec<String> {
        Self::git_read(root, &["tag", "--sort=-creatordate"])
            .map(|o| o.lines().filter(|l| !l.is_empty()).map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// Every checkout of this repository, with the butai workspace already open
    /// on each one where there is such a workspace.
    ///
    /// That last field is what makes the list useful rather than informational:
    /// a worktree is a directory, a butai workspace is a directory, so the
    /// answer to "take me there" is either "switch to workspace 3" or "open
    /// one", and a client cannot tell which without this.
    fn build_worktrees(
        root: &Path,
        open: &[(PathBuf, SessionId)],
    ) -> Vec<butai_protocol::api::WorktreeDto> {
        use butai_protocol::api::WorktreeDto;
        let text = Self::git_read(root, &["worktree", "list", "--porcelain"]).unwrap_or_default();
        crate::git_worktree::parse_list(&text)
            .into_iter()
            .map(|w| {
                // Compare canonically: the same directory reached through a
                // symlink or a trailing slash is the same worktree, and saying
                // "not open" about a workspace that is open would send the user
                // to open a second one on top of it.
                let canon = w.path.canonicalize().unwrap_or_else(|_| w.path.clone());
                let workspace = open
                    .iter()
                    .find(|(cwd, _)| cwd.canonicalize().unwrap_or_else(|_| cwd.clone()) == canon)
                    .map(|(_, id)| *id);
                WorktreeDto {
                    path: w.path.to_string_lossy().into_owned(),
                    branch: w.branch,
                    head: w.head,
                    is_main: w.is_main,
                    detached: w.detached,
                    locked: w.locked,
                    prunable: w.prunable,
                    workspace,
                }
            })
            .collect()
    }

    /// The three sides of one conflicted file, from the index's merge stages.
    ///
    /// The one thing a browser client genuinely cannot reconstruct for itself:
    /// the combined diff shows the conflict, but not the three original files.
    // `Err` is the reply itself; see `build_tree`.
    #[allow(clippy::result_large_err)]
    fn build_conflict(
        root: &Path,
        path: &str,
    ) -> Result<butai_protocol::api::ConflictDto, ApiReply> {
        use butai_protocol::api::ConflictDto;
        if path.is_empty() || path.split('/').any(|c| c == "..") {
            return Err(ApiReply::BadRequest("path escapes the repository".into()));
        }
        // Stages 1/2/3 are base/ours/theirs. A missing stage is not an error —
        // a delete/modify conflict has only two — so it reads as empty.
        let side = |stage: u8| {
            Self::git_read(root, &["show", &format!(":{stage}:{path}")]).unwrap_or_default()
        };
        let (base, ours, theirs) = (side(1), side(2), side(3));
        if base.is_empty() && ours.is_empty() && theirs.is_empty() {
            return Err(ApiReply::NotFound(format!("{path} is not conflicted")));
        }
        Ok(ConflictDto { path: path.to_string(), base, ours, theirs })
    }

    /// A unified diff for one changed file — or for the whole worktree when
    /// `rel` is empty — straight from `git diff`.
    // `Err` is the reply itself; see `build_tree`.
    #[allow(clippy::result_large_err)]
    fn build_diff(cwd: &Path, rel: &str, staged: bool) -> Result<DiffDto, ApiReply> {
        // Validate the path stays inside the workspace, but pass the relative
        // form to git (which wants a repo-relative pathspec).
        safe_join(cwd, rel).ok_or_else(|| ApiReply::BadRequest("path escapes workspace".into()))?;
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(cwd).arg("--no-pager").arg("diff");
        if staged {
            cmd.arg("--cached");
        }
        // An empty `path` is "the whole section", and the way to say that is to
        // pass no pathspec at all. It used to be spelled `-- ""`, on the reading
        // that git takes an empty string as "everything" — it did until 2.16,
        // and since then it is `fatal: empty string is not a valid pathspec`,
        // so every whole-section diff came back a 500 with git's own error in
        // the body.
        if !rel.is_empty() {
            cmd.arg("--").arg(rel);
        }
        let out = cmd.output().map_err(|e| ApiReply::Error(format!("git: {e}")))?;
        if !out.status.success() && out.stdout.is_empty() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            // A workspace that is not a repository is an ordinary state, not a
            // daemon failure: `/changes` answers 404 for it, so `/diff` does
            // too. Otherwise a client cannot tell "no repo here" from "the
            // daemon broke", and the body is a page of git's own CLI usage.
            // Matched case-insensitively — git capitalises it in the `diff`
            // warning ("Not a git repository") but not everywhere else.
            if stderr.to_lowercase().contains("not a git repository") {
                return Err(ApiReply::NotFound("workspace is not a git repository".into()));
            }
            return Err(ApiReply::Error(stderr.into_owned()));
        }
        let mut patch = String::from_utf8_lossy(&out.stdout).into_owned();
        // `git diff` compares the index against the worktree, and an untracked
        // file is on neither side of that — so a brand new file, which is the
        // one CHANGES lists as `?` and even gives a `+n` line count to, printed
        // nothing at all and the diff view said "(no differences)". The staged
        // side never had the problem: once added, `git diff --cached` prints it
        // as an ordinary new file.
        if !staged {
            patch.push_str(&Self::untracked_patch(cwd, rel));
        }
        Ok(DiffDto { path: rel.to_string(), staged, patch })
    }

    /// New-file patches for the untracked files `rel` names — one file, or every
    /// untracked file in the worktree when `rel` is empty.
    ///
    /// `--exclude-standard` is what keeps this the same set the status scan
    /// reports: ignored paths stay out of the diff exactly as they stay out of
    /// CHANGES. Each file is then diffed against `/dev/null`, which is the only
    /// way to ask git for a patch of something it does not track, and which
    /// prints the identical `new file mode` patch it would print for that file
    /// once staged — so what the diff view shows is what `s` would stage, and
    /// hunk-staging one of its hunks applies.
    fn untracked_patch(root: &Path, rel: &str) -> String {
        let mut ls = std::process::Command::new("git");
        ls.arg("-C").arg(root).args(["ls-files", "--others", "--exclude-standard", "-z"]);
        if !rel.is_empty() {
            ls.arg("--").arg(rel);
        }
        let Ok(out) = ls.output() else { return String::new() };
        let names = String::from_utf8_lossy(&out.stdout);
        let paths: Vec<&str> = names.split('\0').filter(|p| !p.is_empty()).collect();
        // A whole-worktree diff of a tree that has never been committed would
        // otherwise be a fork and a file read per untracked file, and a reply
        // the size of the tree. CHANGES still lists every one of them.
        if paths.len() > UNTRACKED_DIFF_LIMIT {
            tracing::warn!(
                "diff: {} untracked files, showing the first {UNTRACKED_DIFF_LIMIT}",
                paths.len()
            );
        }
        let mut patch = String::new();
        for path in paths.iter().take(UNTRACKED_DIFF_LIMIT) {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .arg("--no-pager")
                .args(["diff", "--no-index", "--", "/dev/null"])
                .arg(path)
                .output();
            // `--no-index` exits 1 whenever the two sides differ, which here is
            // always, so the status says nothing — the output is the signal.
            if let Ok(out) = out {
                patch.push_str(&String::from_utf8_lossy(&out.stdout));
            }
        }
        patch
    }

    /// `git show --stat -p <rev>` for a whole commit (the web "recent commits"
    /// list). The rev is constrained to a git object-ish token so it can't be
    /// turned into extra `git` arguments.
    // `Err` is the reply itself; see `build_tree`.
    #[allow(clippy::result_large_err)]
    fn build_show(cwd: &Path, id: &str) -> Result<DiffDto, ApiReply> {
        crate::git_op::valid_show_rev(id).map_err(ApiReply::BadRequest)?;
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .arg("--no-pager")
            .arg("show")
            .arg("--stat")
            .arg("-p")
            // Without this a merge commit answers with a header and no patch:
            // `git show` diffs a merge against *all* its parents, and a clean
            // merge differs from none of them, so the useful reading — what
            // this merge brought onto the branch — was the one nobody could
            // get. A no-op on an ordinary commit.
            .arg("--first-parent")
            .arg(id)
            .output()
            .map_err(|e| ApiReply::Error(format!("git: {e}")))?;
        if !out.status.success() && out.stdout.is_empty() {
            return Err(ApiReply::Error(String::from_utf8_lossy(&out.stderr).into_owned()));
        }
        Ok(DiffDto {
            path: id.to_string(),
            staged: false,
            patch: String::from_utf8_lossy(&out.stdout).into_owned(),
        })
    }

    fn broadcast_api(&mut self, ev: ApiEvent) {
        self.api_subs.retain(|tx| tx.send(ev.clone()).is_ok());
    }

    /// Push every workspace whose rail contents changed since the last frame.
    ///
    /// The diffing is the point. `dirty` is set by pane output, so this runs on
    /// nearly every frame, while the rails it describes change orders of
    /// magnitude less often — a shell echoing a keystroke moves no agent row.
    /// Comparing against the last snapshot turns "a push per frame per
    /// workspace" into "a push when something a client would redraw actually
    /// moved", which is what makes this affordable over ssh.
    ///
    /// Costs nothing when nobody is listening: with no subscribers it returns
    /// before building a single DTO, and drops the snapshots it was holding so
    /// an idle daemon is not carrying a copy of every rail.
    fn broadcast_ws_details(&mut self) {
        if self.api_subs.is_empty() {
            self.last_detail.clear();
            return;
        }
        let ids: Vec<SessionId> = self.order.clone();
        // A workspace that is gone should not leave its last snapshot behind to
        // be compared against if the id is ever reused.
        self.last_detail.retain(|id, _| ids.contains(id));
        for sid in ids {
            let Some(detail) = self.build_ws_detail(sid) else { continue };
            if self.last_detail.get(&sid) == Some(&detail) {
                continue;
            }
            self.last_detail.insert(sid, detail.clone());
            self.broadcast_api(ApiEvent::WorkspaceDetail(detail));
        }
    }

    // -- agent status tracking + notifications -------------------------------

    /// Nudge an agent to be re-evaluated on the next tick (called cheaply on
    /// output). Resets its adaptive backoff so a pane that just came alive is
    /// checked promptly instead of on its long stable interval.
    fn touch_agent(&mut self, pane: PaneId) {
        if let Some(tr) = self.agent_track.get_mut(&pane) {
            tr.backoff = AGENT_CHECK_MIN;
            tr.next_check = Instant::now();
        }
    }

    /// The user looked at a pane: clear its bell and mark its last your-move
    /// edge read.
    ///
    /// The single spelling of "looked at", called from every gesture that means
    /// it — staging, watching, streaming, sending input, and an explicit
    /// `AckPane` from a list client. Both halves belong to the same event, and
    /// when they were separate the bell was cleared in four places and nothing
    /// cleared `unread` in any of them.
    ///
    /// Not every pane is an agent, and a pane with no track is simply one with
    /// no unread state to clear — acking anything is a no-op rather than an
    /// error, which is what lets a client ack blindly.
    fn look_at_pane(&mut self, pane: PaneId) {
        if let Some(PaneState::Terminal(t)) = self.panes.get_mut(&pane) {
            t.acknowledge();
        }
        if let Some(tr) = self.agent_track.get_mut(&pane) {
            tr.unread = false;
        }
    }

    /// Recompute the debounced status of every agent that is due, and record any
    /// notify-worthy state transition (waiting / finished). This is the single place
    /// "an agent finished" is decided, so every client that drains the feed
    /// agrees. Cheap: only agents past their (backing-off) `next_check` are
    /// scanned, and quiet ones back off up to [`AGENT_CHECK_MAX`].
    fn update_agent_tracking(&mut self) {
        let now = Instant::now();

        // Which agent panes exist (for cleanup), and which are due to recheck.
        let mut live: std::collections::HashSet<PaneId> = std::collections::HashSet::new();
        let mut due: Vec<(PaneId, SessionId, String)> = Vec::new();
        for w in self.workspaces.values() {
            for &pane in &w.agents {
                if !matches!(self.panes.get(&pane), Some(PaneState::Terminal(_))) {
                    continue;
                }
                live.insert(pane);
                let is_due = self.agent_track.get(&pane).is_none_or(|t| now >= t.next_check);
                if is_due {
                    due.push((pane, w.id, w.name.clone()));
                }
            }
        }
        self.agent_track.retain(|p, _| live.contains(p));
        // Same sweep for the resume fallback's arming set. Closing a workspace
        // drops its panes without going through `drop_pane`, so without this a
        // long-running daemon accumulates an entry per agent of every project
        // ever opened.
        self.resume_retry.retain(|p| live.contains(p));

        for (pane, ws, ws_name) in due {
            // Raw signals (immutable borrow, scoped). `waiting` is the positive
            // "blocked on your input" signal (bell or a visible question prompt).
            let (title, exited, marker, busy, waiting) = {
                let Some(PaneState::Terminal(t)) = self.panes.get(&pane) else { continue };
                // One grid scan for both footer signals. Same composition as
                // `attention`/`is_busy`, but keeping the marker apart: it is the
                // evidence that a working run was a genuine turn.
                let signals = t.footer_signals();
                let waiting = t.bell_pending() || signals.prompt;
                let busy = signals.busy || t.sustained_output();
                (t.agent_title(), t.exit_code(), signals.busy, busy, waiting)
            };

            // Fold into the debounced state machine (mutable borrow, scoped so
            // it ends before we may push a notification).
            let (prev, new_state, seeded_before) = {
                let track = self.agent_track.entry(pane).or_insert_with(|| AgentTrack::new(now));
                let prev = track.state;
                let seeded_before = track.seeded;

                let new_state = if exited.is_some() {
                    track.end_run();
                    // A corpse is not idle. The exit code still rides in
                    // `AgentDto::exited`; this just stops a client that only looks
                    // at `state` from painting a dead agent as a quiet live one.
                    AgentState::Exited
                } else if waiting {
                    // A question/decision prompt (or bell): needs you *now*.
                    track.end_run();
                    AgentState::Waiting
                } else if busy {
                    track.quiet_since = None;
                    if prev != AgentState::Working {
                        track.working_since = Some(now);
                        track.saw_marker = false;
                    }
                    track.saw_marker |= marker;
                    AgentState::Working
                } else {
                    // Quiet: a sustained lull *after working* — with no prompt on
                    // screen — means the turn finished. `Finished` and `Idle` hold.
                    match prev {
                        AgentState::Working => {
                            let q = *track.quiet_since.get_or_insert(now);
                            if now.duration_since(q) >= AGENT_SETTLE {
                                // Ran long enough (or showed a marker) to have
                                // been a turn? Then it finished. Otherwise it
                                // was repaint noise: fall back to idle quietly.
                                let ran = q.duration_since(track.working_since.unwrap_or(q));
                                let real = track.saw_marker || ran >= MIN_TURN;
                                track.end_run();
                                if real {
                                    AgentState::Finished
                                } else {
                                    AgentState::Idle
                                }
                            } else {
                                AgentState::Working
                            }
                        }
                        // Nothing is blocking on you anymore: the prompt left the
                        // screen or the bell was acknowledged. This used to fall
                        // into the catch-all and *hold* `Waiting`, so an agent that
                        // ever asked anything stayed "needs you" for the rest of
                        // its life unless it happened to start working again —
                        // dismissing a prompt with Stop pinned it permanently, and
                        // any "needs you" badge built on the count only ever grew.
                        AgentState::Waiting => {
                            track.end_run();
                            AgentState::Idle
                        }
                        other => {
                            track.end_run();
                            other
                        }
                    }
                };

                // A your-move edge you were not already standing on is news, and
                // stays news until you look. Gated on `seeded_before` for the
                // same reason the notification below is: the baseline pass is
                // the daemon catching up on what was already true, so attaching
                // to a workbench of long-finished agents must not light every
                // one of them up as if it had just landed.
                //
                // Edge, not level: re-entering `Finished` from `Finished` is not
                // a second turn, and marking it unread again would undo a read
                // you had already given it.
                if seeded_before
                    && new_state != prev
                    && matches!(new_state, AgentState::Finished | AgentState::Exited)
                {
                    track.unread = true;
                }

                track.state = new_state;
                track.exited = exited;
                track.seeded = true;

                // Adaptive backoff: grow only while fully stable; snap back the
                // moment anything is in flight (working or still settling).
                let settling = new_state == AgentState::Working && track.quiet_since.is_some();
                let stable = new_state == prev && new_state != AgentState::Working && !settling;
                track.backoff =
                    if stable { (track.backoff * 2).min(AGENT_CHECK_MAX) } else { AGENT_CHECK_MIN };
                track.next_check = now + track.backoff;

                (prev, new_state, seeded_before)
            };

            // Notify once on each distinct edge: a fresh question (waiting) and a
            // completed turn (finished) are separate events with separate copy.
            // Exits are handled in `on_pane_exited` (so clean, auto-removed exits
            // stay silent).
            if seeded_before && exited.is_none() {
                if new_state == AgentState::Waiting && prev != AgentState::Waiting {
                    self.push_notification(
                        ws,
                        &ws_name,
                        pane,
                        title,
                        NotificationKind::Waiting,
                        None,
                    );
                } else if new_state == AgentState::Finished && prev != AgentState::Finished {
                    self.push_notification(
                        ws,
                        &ws_name,
                        pane,
                        title,
                        NotificationKind::Finished,
                        None,
                    );
                }
            }
        }
    }

    /// Append to the bounded notification feed and push it to live subscribers.
    fn push_notification(
        &mut self,
        ws: SessionId,
        ws_name: &str,
        pane: PaneId,
        title: String,
        kind: NotificationKind,
        exited: Option<u32>,
    ) {
        self.notif_seq += 1;
        let at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let note = NotificationDto {
            seq: self.notif_seq,
            at_ms,
            ws,
            ws_name: ws_name.to_string(),
            pane,
            title,
            kind,
            exited,
        };
        debug!("notify seq={} pane={:?} kind={:?}", note.seq, pane, kind);
        if self.notifications.len() >= NOTIF_HISTORY {
            self.notifications.pop_front();
        }
        self.notifications.push_back(note.clone());
        self.broadcast_api(ApiEvent::Notification(note));
    }

    /// Items with `seq > since`, plus the current head (so a fresh client can
    /// jump its cursor to now without replaying history).
    fn notifications_since(&self, since: u64) -> NotificationsDto {
        NotificationsDto {
            head: self.notif_seq,
            items: self.notifications.iter().filter(|n| n.seq > since).cloned().collect(),
        }
    }

    // -- pane spawning -------------------------------------------------------

    fn alloc_pane_id(&mut self) -> PaneId {
        let id = PaneId(self.next_pane);
        self.next_pane += 1;
        id
    }

    // Eight parameters, one over clippy's default. They are the spawn inputs,
    // all independent and all required; bundling them into a struct would only
    // move the same list to the call sites.
    #[allow(clippy::too_many_arguments)]
    fn spawn_terminal(
        &mut self,
        ws: SessionId,
        cwd: &Path,
        program: Option<ProgramSpec<'_>>,
        command_string: Option<&str>,
        via_shell: bool,
        rows: u16,
        cols: u16,
    ) -> anyhow::Result<PaneId> {
        self.spawn_terminal_replaying(ws, cwd, program, command_string, via_shell, rows, cols, None)
    }

    /// [`spawn_terminal`](Self::spawn_terminal), plus output captured by a
    /// previous daemon to paint into the pane before the new child starts.
    /// Only the restore path passes `replay`.
    #[allow(clippy::too_many_arguments)]
    fn spawn_terminal_replaying(
        &mut self,
        ws: SessionId,
        cwd: &Path,
        program: Option<ProgramSpec<'_>>,
        command_string: Option<&str>,
        via_shell: bool,
        rows: u16,
        cols: u16,
        replay: Option<PaneDump<'_>>,
    ) -> anyhow::Result<PaneId> {
        let id = self.alloc_pane_id();
        let socket = self.socket.clone();
        let shell = self.config.shell();
        let scrollback = self.config.general.scrollback;
        let restore_bytes = self.config.general.restore_bytes;
        let spec = match (program, command_string) {
            (Some((prog, args, env, label, detect)), _) => SpawnSpec {
                pane: id,
                ws,
                socket: &socket,
                program: Some(prog),
                args,
                env,
                cwd,
                shell: &shell,
                via_shell: false,
                label,
                detect,
                replay,
            },
            (None, Some(cmd)) => SpawnSpec {
                pane: id,
                ws,
                socket: &socket,
                program: Some(cmd),
                args: &[],
                env: &[],
                cwd,
                shell: &shell,
                via_shell: true,
                label: None,
                detect: None,
                replay,
            },
            (None, None) => SpawnSpec {
                pane: id,
                ws,
                socket: &socket,
                program: None,
                args: &[],
                env: &[],
                cwd,
                shell: &shell,
                via_shell,
                label: None,
                detect: None,
                replay,
            },
        };
        let pane = TerminalPane::spawn(
            id,
            spec,
            rows,
            cols,
            scrollback,
            restore_bytes,
            self.events_tx.clone(),
            self.output_tx.clone(),
        )?;
        self.panes.insert(id, PaneState::Terminal(pane));
        Ok(id)
    }

    /// How big to make a pane that nobody is looking at yet.
    ///
    /// The honest answer comes from a client: it drew this workspace's stage,
    /// so it measured the hole its own rails left. Failing that — a workspace
    /// created over the API and never drawn — there is no better answer than a
    /// conventional terminal, because the size of a stage is a fact about a
    /// client's window and no client has one open here.
    ///
    /// This side used to compute the rectangle itself, from its own copy of the
    /// rail widths. That only ever worked while it was also the thing drawing
    /// the rails; once the client owned its layout, the arithmetic was a guess
    /// dressed up as geometry.
    fn stage_size(&self, ws_id: SessionId) -> (u16, u16) {
        match self.workspaces.get(&ws_id).and_then(|w| w.stage_size) {
            Some((rows, cols)) => (rows.max(2), cols.max(2)),
            None => UNWATCHED_PANE_SIZE,
        }
    }

    fn spawn_agent(&mut self, ws_id: SessionId, name: &str) -> anyhow::Result<()> {
        self.spawn_agent_restoring(ws_id, name, None, None, false, false).map(|_| ())
    }

    /// [`spawn_agent`](Self::spawn_agent), plus the pane's saved output and the
    /// launcher's [`resume_args`](crate::config::AgentDef::resume_args).
    ///
    /// The two restore inputs are independent. `replay` repaints the screen the
    /// pane was showing; `session` names the conversation the agent was holding,
    /// so the CLI reopens *that* one rather than whichever in this directory
    /// happens to be most recent. A pane can have either without the other: an
    /// agent whose screen was cleared has a conversation and nothing to repaint,
    /// and a launcher with no `resume_args` gets its scrollback back but starts
    /// a new conversation under it.
    ///
    /// `spoke` says whether that conversation was ever written — a pane that was
    /// opened and never typed into names one that does not exist, and is started
    /// fresh instead of being asked to reopen nothing.
    fn spawn_agent_restoring(
        &mut self,
        ws_id: SessionId,
        name: &str,
        replay: Option<PaneDump<'_>>,
        session: Option<&str>,
        spoke: bool,
        // Leave the stage and focus alone — for a helper spawned by another
        // agent, where taking the view would interrupt whoever is watching.
        background: bool,
    ) -> anyhow::Result<PaneId> {
        let agent = self
            .config
            .agent(name)
            .ok_or_else(|| anyhow::anyhow!("no agent named {name:?} in config"))?
            .clone();
        let cwd = self
            .workspaces
            .get(&ws_id)
            .map(|w| w.cwd.clone())
            .ok_or_else(|| anyhow::anyhow!("workspace gone"))?;
        let (rows, cols) = self.stage_size(ws_id);
        let env: Vec<(String, String)> =
            agent.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let detect = Detect::compile(
            &agent.name,
            agent.waiting_pattern.as_deref(),
            agent.busy_pattern.as_deref(),
        );
        // Conversation and argv are decided together, because each constrains
        // the other: only a pane with an id worth naming can be resumed, and
        // only a launcher that takes one can be told about it.
        //
        // Reopening needs a conversation to name, a launcher that knows how to
        // reopen one, and that conversation to have been *written* — an agent
        // that was never spoken to holds an id naming a transcript neither CLI
        // has created yet, and asking for it back is a launch that exits rather
        // than a launch that starts empty (see [`AgentMeta::spoke`]).
        //
        // Deliberately *not* conditioned on there being output to replay: the
        // transcript on screen and the conversation the agent holds are separate
        // things, and an agent that has said nothing since its last reply — or
        // any agent at all under `restore_bytes = 0` — still has a conversation
        // worth returning to. Without a name for it this is a fresh start, which
        // is also the path an old `session.json` takes.
        let reopening = session.is_some() && spoke && !agent.resume_args.is_empty();
        let (session_id, args, resumed) = match reopening {
            true => match expand_args(&agent.resume_args, session) {
                Some(args) => (session.map(str::to_string), args, true),
                // `resume_args` asks for an id this pane does not have.
                None => {
                    let (id, args) = fresh_launch(&agent);
                    (id, args, false)
                }
            },
            false => {
                let (id, args) = fresh_launch(&agent);
                (id, args, false)
            }
        };
        let pane = self.spawn_terminal_replaying(
            ws_id,
            &cwd,
            Some((&agent.command, &args, &env, Some(&agent.name), Some(detect))),
            None,
            false,
            rows,
            cols,
            replay,
        )?;
        if let Some(ws) = self.workspaces.get_mut(&ws_id) {
            ws.agents.push(pane);
            // A resumed pane is by definition one that had already been spoken
            // to, so it stays resumable across a second restart without waiting
            // for the user to type into it again.
            ws.agent_meta
                .insert(pane, AgentMeta { name: agent.name.clone(), session_id, spoke: resumed });
            if !background {
                ws.stage = Some(pane);
            }
        }
        // Arm the one-shot fallback: a CLI asked to reopen a conversation that
        // is no longer there exits immediately rather than starting without it
        // (verified: `claude --resume <unknown>` exits 1, gemini exits on a
        // fatal input error), and a pane that dies on every restart is worse
        // than one that comes back empty. See `retry_failed_resume`.
        if resumed {
            self.resume_retry.insert(pane);
        }
        // Seed the status track up front so a client querying between the spawn
        // and the first sampler tick still reads a real debounced state.
        self.agent_track.insert(pane, AgentTrack::new(Instant::now()));
        // Record the new roster now rather than at the next workspace open or a
        // clean shutdown. The pane dumps are already rewritten every sampler
        // tick, so without this the two halves of a restore disagree after a
        // hard kill: the dumps describe the agents running at the crash, while
        // `session.json` still describes whichever set was open the last time a
        // workspace opened or closed. Restore reads the stale list against the
        // fresh, position-keyed dumps — so an agent can come back painted with
        // another one's screen, and any agent started since is simply gone.
        //
        // No-op while restoring, so rebuilding a workspace does not write a
        // half-rebuilt roster over the one being read.
        self.persist_session();
        self.dirty = true;
        Ok(pane)
    }

    fn new_process(
        &mut self,
        ws_id: SessionId,
        name: &str,
        command: Option<String>,
    ) -> anyhow::Result<()> {
        let cwd = self
            .workspaces
            .get(&ws_id)
            .map(|w| w.cwd.clone())
            .ok_or_else(|| anyhow::anyhow!("workspace gone"))?;
        let meta = ProcMeta {
            name: name.to_string(),
            command: command.clone().unwrap_or_else(|| self.config.shell()),
            ready: None,
            ready_seen: command.is_none(),
            ready_carry: String::new(),
        };
        self.spawn_process(ws_id, &cwd, meta, true, None).map(|_| ())
    }

    /// Start a managed process in `ws_id`. `replay` is output saved by a
    /// previous daemon, painted into the pane before the new child runs; only
    /// the restore path passes it.
    fn spawn_process(
        &mut self,
        ws_id: SessionId,
        cwd: &Path,
        meta: ProcMeta,
        stage: bool,
        replay: Option<PaneDump<'_>>,
    ) -> anyhow::Result<PaneId> {
        let (rows, cols) = self.stage_size(ws_id);
        let is_shell = meta.command == self.config.shell();
        let pane = if is_shell {
            self.spawn_terminal_replaying(ws_id, cwd, None, None, false, rows, cols, replay)?
        } else {
            self.spawn_terminal_replaying(
                ws_id,
                cwd,
                None,
                Some(&meta.command),
                true,
                rows,
                cols,
                replay,
            )?
        };
        if let Some(ws) = self.workspaces.get_mut(&ws_id) {
            ws.processes.push(pane);
            ws.proc_meta.insert(pane, meta);
            if stage {
                ws.stage = Some(pane);
            }
        }
        self.dirty = true;
        Ok(pane)
    }

    /// Start a git operation against a workspace's repository.
    ///
    /// This is the single gate: it resolves the repository, refuses a second
    /// writer, validates every user-supplied string, and registers the op
    /// before spawning. `Err` is the message a caller shows or returns as 400 /
    /// 404 / 409.
    fn start_git_op(
        &mut self,
        ws_id: SessionId,
        op: GitOp,
    ) -> Result<
        (GitOpDto, tokio::sync::oneshot::Receiver<Option<crate::git_op::OpResult>>),
        GitOpRefusal,
    > {
        let Some(root) = self.git_root(ws_id) else {
            return Err(GitOpRefusal::NoRepo(ws_id));
        };
        if let Some(running) = self.git_ops.get(&root).filter(|s| s.running) {
            return Err(GitOpRefusal::Busy(running.kind.to_string()));
        }
        // A sequence verb's subcommand depends on what is actually running, and
        // only the pane knows that — so it is resolved here rather than in the
        // otherwise-pure `argv`.
        let args = match &op {
            GitOp::Sequence { action } => {
                let state = self
                    .workspaces
                    .get(&ws_id)
                    .and_then(|w| self.git_pane(w))
                    .map(|g| g.state())
                    .unwrap_or_default();
                crate::git_op::sequence_argv(state, *action)
            }
            _ => crate::git_op::argv(&op),
        }
        .map_err(GitOpRefusal::Invalid)?;

        self.git_op_seq += 1;
        let seq = self.git_op_seq;
        let state = GitOpState {
            ws: ws_id,
            seq,
            kind: op.kind(),
            running: true,
            progress: String::new(),
            result: None,
            cancel: None,
        };
        let dto = state.to_dto();
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        // Answered by the op task within the grace window. The TUI path has no
        // caller waiting on it and simply drops the receiver.
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.git_ops.insert(root.clone(), GitOpState { cancel: Some(cancel_tx), ..state });
        crate::git_op::spawn_op(
            self.events_tx.clone(),
            root,
            ws_id,
            seq,
            op,
            args,
            reply_tx,
            cancel_rx,
        );
        self.broadcast_api(ApiEvent::GitOp(dto.clone()));
        Ok((dto, reply_rx))
    }

    /// Cancel the operation running against a workspace's repository.
    fn cancel_git_op(&mut self, ws_id: SessionId) -> Option<GitOpDto> {
        let root = self.git_root(ws_id)?;
        let state = self.git_ops.get_mut(&root)?;
        if !state.running {
            return None;
        }
        // Dropping the sender would also fire the receiver, but sending says
        // "deliberately cancelled" rather than "the core went away".
        if let Some(tx) = state.cancel.take() {
            let _ = tx.send(());
        }
        Some(state.to_dto())
    }

    /// The operation running (or last finished) for a workspace's repository.
    fn git_op_status(&self, ws_id: SessionId) -> Option<GitOpDto> {
        let root = self.git_root(ws_id)?;
        self.git_ops.get(&root).map(|s| s.to_dto())
    }

    /// Whether a git operation currently holds this workspace's write lock.
    /// Index mutations are refused while one runs: a stage racing a rebase
    /// checkout writes to an index git is in the middle of rewriting.
    fn git_write_locked(&self, ws_id: SessionId) -> Option<&'static str> {
        let root = self.git_root(ws_id)?;
        self.git_ops.get(&root).filter(|s| s.running).map(|s| s.kind)
    }

    fn on_git_progress(&mut self, ws: SessionId, seq: u64, line: String) {
        let Some(root) = self.git_root(ws) else { return };
        let Some(state) = self.git_ops.get_mut(&root).filter(|s| s.seq == seq) else { return };
        state.progress = line;
        let dto = state.to_dto();
        self.dirty = true;
        // One path for progress now: the `git_op` event carries `kind` and the
        // line, and every client shows it where it wants to. The rail used to
        // get a second, private copy written into the pane's notice slot.
        self.broadcast_api(ApiEvent::GitOp(dto));
    }

    fn on_git_op_done(
        &mut self,
        root: PathBuf,
        ws: SessionId,
        seq: u64,
        result: Result<String, String>,
    ) {
        let Some(state) = self.git_ops.get_mut(&root).filter(|s| s.seq == seq) else { return };
        state.running = false;
        state.cancel = None;
        state.progress = String::new();
        state.result = Some(result);
        let dto = state.to_dto();
        if let Some(pane) = self.workspaces.get(&ws).and_then(|w| w.changes) {
            // A fetch moves the upstream, a push moves it back: either way the
            // rail's ahead/behind is now wrong until the next scan.
            self.request_git_refresh(pane);
        }
        self.dirty = true;
        self.broadcast_api(ApiEvent::GitOp(dto));
    }

    // -- sizing --------------------------------------------------------------

    fn resize_client(&mut self, id: ClientId, cols: u16, rows: u16) {
        let Some(client) = self.clients.get_mut(&id) else { return };
        client.cols = cols;
        client.rows = rows;
        client.last_frame = None;
        // Only a client streaming a pane has a size that means anything: it is
        // the size of that pane. A workspace-scoped connection draws nothing and
        // watches nothing, so its terminal's dimensions describe no pane here.
        if let Some(pid) = client.pane {
            self.size_pane_for_viewer(pid, rows, cols);
        }
        self.dirty = true;
    }

    // -- output --------------------------------------------------------------

    fn send(&self, id: ClientId, msg: ServerMsg) {
        if let Some(c) = self.clients.get(&id) {
            let _ = c.tx.send(msg);
        }
    }

    /// Tell a client something went wrong.
    ///
    /// Always a message now. This used to write into the client's footer when
    /// the daemon was drawing one for it, and send `error` otherwise; every
    /// client draws its own footer today, so there is one path and the client
    /// decides where the text goes.
    fn report_error(&mut self, id: ClientId, msg: String) {
        self.send(id, ServerMsg::Error(msg));
    }

    fn detach(&mut self, id: ClientId, reason: &str) {
        if let Some(c) = self.clients.remove(&id) {
            let _ = c.tx.send(ServerMsg::Detached { reason: reason.to_string() });
        }
    }

    fn ws_info(&self, sid: SessionId) -> Option<SessionInfo> {
        let attached = self.clients.values().filter(|c| c.session == Some(sid)).count();
        self.workspaces.get(&sid).map(|s| s.info(attached))
    }

    /// Ring the client bell once when an agent pane first needs attention.
    fn check_agent_bell(&mut self, pane: PaneId) {
        use crate::pane::terminal::Attention;
        let needs = matches!(
            self.panes.get(&pane),
            Some(PaneState::Terminal(t)) if t.attention() == Attention::Waiting
        );
        let ws_id = self.workspaces.values().find(|w| w.agents.contains(&pane)).map(|w| w.id);
        let Some(ws_id) = ws_id else { return };
        if needs {
            // insert() returns true only on the rising edge.
            if self.agent_alerted.insert(pane) {
                self.ring_bell(ws_id);
            }
        } else {
            self.agent_alerted.remove(&pane);
        }
    }

    /// Ring the terminal bell on every interactive client viewing `ws_id`.
    ///
    /// "Viewing" includes a client streaming any *pane* of the workspace, which
    /// is what a workbench looks like from here now: the TUI holds one pane
    /// connection and reads the rest over `/v1/*`, so a bell that only reached
    /// session connections reached nobody. The pane need not be the agent's —
    /// the whole point of the bell is that it finds you when you are looking at
    /// something else.
    fn ring_bell(&self, ws_id: SessionId) {
        let panes = self.workspaces.get(&ws_id).map(|w| w.all_panes()).unwrap_or_default();
        let holds_a_pane_here = |c: &ClientState| c.pane.is_some_and(|p| panes.contains(&p));
        for c in self.clients.values() {
            if !c.control && (c.session == Some(ws_id) || holds_a_pane_here(c)) {
                let _ = c.tx.send(ServerMsg::Bell);
            }
        }
    }

    /// Route input from a pane-scoped client (web stage) straight to its pane.
    fn route_pane_input(&mut self, id: ClientId, pane: PaneId, ev: InputEvent) {
        // Active-client-wins sizing: a shared PTY can only hold one size, so
        // the client that's actively driving it reclaims that size. Resize the
        // pane to this (web) viewer's dims on input; `resize` no-ops when the
        // size already matches, so this is free once sizes have converged.
        if let Some((cols, rows)) = self.clients.get(&id).map(|c| (c.cols, c.rows)) {
            if let Some(p) = self.panes.get_mut(&pane) {
                p.resize(rows.max(2), cols.max(2));
            }
        }
        if !self.panes.contains_key(&pane) {
            self.detach(id, "pane closed");
            return;
        }
        // Mouse works exactly as on the workspace stage: apps that grabbed the
        // mouse get click/drag/wheel forwarded; otherwise a drag paints a
        // server-side text selection copied to the clipboard on release, and
        // the wheel scrolls scrollback. Coordinates arrive full-bleed
        // (pane-relative) already, so there's no chrome to hit-test.
        match &ev {
            // A pane-scoped client renders one pane full-bleed, so there is no
            // chrome for a context menu to attach to; right-clicks are dropped.
            InputEvent::MouseDown { x, y, alt, button } => {
                if *button == MouseButton::Left {
                    self.pane_mouse_down(id, pane, *x, *y, *alt);
                }
                return;
            }
            InputEvent::MouseDrag { x, y, alt } => {
                self.pane_mouse_drag(id, pane, *x, *y, *alt);
                return;
            }
            InputEvent::MouseUp { .. } => {
                self.finish_selection(id);
                return;
            }
            InputEvent::ScrollUp { x, y } => {
                self.pane_wheel(id, pane, *x, *y, true);
                return;
            }
            InputEvent::ScrollDown { x, y } => {
                self.pane_wheel(id, pane, *x, *y, false);
                return;
            }
            _ => {}
        }
        self.note_agent_spoken(pane, &ev);
        if let Some(p) = self.panes.get_mut(&pane) {
            p.handle_input(&ev);
        }
        self.dirty = true;
    }

    /// A press on the pane stage: forward to a mouse-hungry app, else begin a
    /// text selection (Alt forces a selection even when the app wants the mouse).
    fn pane_mouse_down(&mut self, id: ClientId, pane: PaneId, x: u16, y: u16, alt: bool) {
        let wants =
            matches!(self.panes.get(&pane), Some(PaneState::Terminal(t)) if t.wants_mouse());
        if wants && !alt {
            if let Some(PaneState::Terminal(t)) = self.panes.get_mut(&pane) {
                t.send_mouse(crate::pane::terminal::MouseKind::Click, x, y);
            }
            self.dirty = true;
            return;
        }
        if let Some(c) = self.clients.get_mut(&id) {
            c.sel_anchor = Some((x, y));
            c.sel = None;
        }
        self.dirty = true;
    }

    /// Extend the text selection as the pointer drags. A mouse-hungry app keeps
    /// the drag unless Alt is held.
    fn pane_mouse_drag(&mut self, id: ClientId, pane: PaneId, x: u16, y: u16, alt: bool) {
        if !alt && matches!(self.panes.get(&pane), Some(PaneState::Terminal(t)) if t.wants_mouse())
        {
            return;
        }
        if let Some(c) = self.clients.get_mut(&id) {
            if let Some(anchor) = c.sel_anchor {
                c.sel = Some((anchor, (x, y)));
                self.dirty = true;
            }
        }
    }

    /// Wheel on the pane stage: forward to a mouse-hungry app, else scroll
    /// scrollback by [`WHEEL_LINES`].
    fn pane_wheel(&mut self, id: ClientId, pane: PaneId, x: u16, y: u16, up: bool) {
        let _ = id;
        if let Some(PaneState::Terminal(t)) = self.panes.get_mut(&pane) {
            if t.wants_mouse() {
                let kind = if up {
                    crate::pane::terminal::MouseKind::WheelUp
                } else {
                    crate::pane::terminal::MouseKind::WheelDown
                };
                t.send_mouse(kind, x, y);
                self.dirty = true;
                return;
            }
        }
        if let Some(p) = self.panes.get_mut(&pane) {
            use crate::pane::terminal::WHEEL_LINES;
            p.scroll_lines(if up { -WHEEL_LINES } else { WHEEL_LINES });
        }
        self.dirty = true;
    }

    /// Render one pane-scoped client: just its pane, full-bleed at the client's
    /// size, diffed and sent like any other frame.
    fn render_pane_client(&mut self, id: ClientId) {
        let Some(client) = self.clients.get(&id) else { return };
        let Some(pane_id) = client.pane else { return };
        let (cols, rows) = (client.cols.max(2), client.rows.max(2));
        if !self.panes.contains_key(&pane_id) {
            self.detach(id, "pane closed");
            return;
        }
        let area = ratatui::layout::Rect::new(0, 0, cols, rows);
        let mut buf = Buffer::empty(area);
        let (cursor, wants_mouse) = {
            let pane = self.panes.get_mut(&pane_id).unwrap();
            if !pane.render(&mut buf, area) {
                // A pane with no picture — a file listing, a worktree's status,
                // a patch. Streaming one would send an empty grid forever, so
                // say so instead: the state is on `/v1/*` and the client draws
                // it, which is why this side has nothing to paint.
                let msg = "that pane has no screen — read it from /v1/* instead".to_string();
                self.send(id, ServerMsg::Error(msg));
                self.detach(id, "pane has no screen");
                return;
            }
            // Only this side knows: it is parsing the program's output. The
            // client needs it to decide whether a drag is its own text
            // selection or the program's.
            let wants = matches!(pane, PaneState::Terminal(t) if t.wants_mouse());
            (pane.cursor(), wants)
        };
        // Reverse-video the active drag selection (copied on mouse-up).
        if let Some((a, b)) = self.clients.get(&id).and_then(|c| c.sel) {
            highlight_selection(&mut buf, a, b);
        }
        let Some(client) = self.clients.get_mut(&id) else { return };
        let mut update: FrameUpdate =
            render::diff_to_frame(client.last_frame.as_ref(), &buf, cursor);
        update.wants_mouse = wants_mouse;
        client.last_frame = Some(buf);
        if update.full || !update.cells.is_empty() || update.cursor.is_some() {
            let _ = client.tx.send(ServerMsg::Frame(update));
        }
    }

    /// Draw every client that is streaming a pane.
    ///
    /// That is all of them. A session target used to get a whole composed
    /// workbench — rails, tab bar, overlays, the lot — and there is no such
    /// client any more: every client draws its own chrome from `/v1/*`, the way
    /// the web and native ones always did, and the only thing left for the
    /// daemon to render is the one thing only it can — a program's cells coming
    /// off a PTY.
    fn render_all(&mut self) {
        let pane_client_ids: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, c)| !c.control && c.pane.is_some())
            .map(|(id, _)| *id)
            .collect();
        for id in pane_client_ids {
            self.render_pane_client(id);
        }
    }
}

/// Row-major span `[x0, x1]` covered by a linear selection on row `y`.
fn selection_span(
    area: ratatui::layout::Rect,
    start: (u16, u16),
    end: (u16, u16),
    y: u16,
) -> (u16, u16) {
    let last = area.right().saturating_sub(1);
    if start.1 == end.1 {
        (start.0.min(end.0), start.0.max(end.0))
    } else if y == start.1 {
        (start.0, last)
    } else if y == end.1 {
        (area.x, end.0)
    } else {
        (area.x, last)
    }
}

/// Clamp a (x, y) endpoint into a region's cell range.
fn clamp_point(p: (u16, u16), area: ratatui::layout::Rect) -> (u16, u16) {
    (
        p.0.clamp(area.x, area.right().saturating_sub(1)),
        p.1.clamp(area.y, area.bottom().saturating_sub(1)),
    )
}

/// Extract the linearly-selected text from a pane's frame, trimming trailing
/// blanks per line (standard terminal-selection behaviour).
///
/// The whole buffer is in play because the whole buffer is the pane. A drag
/// used to be clipped to the rail column it started in, so it could not run
/// into the tree sidebar or a box border; a client that draws its own chrome
/// does that clipping itself, against a screen it actually laid out.
fn extract_selection(buf: &Buffer, a: (u16, u16), b: (u16, u16)) -> String {
    let area = buf.area;
    if area.width == 0 || area.height == 0 {
        return String::new();
    }
    let (a, b) = (clamp_point(a, area), clamp_point(b, area));
    let (start, end) = if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) };
    let mut lines: Vec<String> = Vec::new();
    for y in start.1..=end.1 {
        let (x0, x1) = selection_span(area, start, end, y);
        let mut line = String::new();
        for x in x0..=x1 {
            let s = buf[(x, y)].symbol();
            line.push_str(if s.is_empty() { " " } else { s });
        }
        while line.ends_with(' ') {
            line.pop();
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// Draw the active selection reversed over a pane's frame.
fn highlight_selection(buf: &mut Buffer, a: (u16, u16), b: (u16, u16)) {
    let area = buf.area;
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (a, b) = (clamp_point(a, area), clamp_point(b, area));
    let (start, end) = if (a.1, a.0) <= (b.1, b.0) { (a, b) } else { (b, a) };
    for y in start.1..=end.1 {
        let (x0, x1) = selection_span(area, start, end, y);
        for x in x0..=x1 {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.modifier |= ratatui::style::Modifier::REVERSED;
            }
        }
    }
}

fn default_ws_name(cwd: &Path) -> String {
    cwd.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "main".to_string())
}

/// Group docker containers into compose stacks for the API (standalone
/// containers form one-member stacks). Unlike the rail's `stacks()`, this keeps
/// stopped stacks and each container's raw state string, and skips the cwd-based
/// "mine" sort — the web client sorts using each stack's `workdir`.
fn build_stack_dtos(containers: &[crate::workbench::Container]) -> Vec<StackDto> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<String, StackDto> = BTreeMap::new();
    for c in containers {
        let key = if c.project.is_empty() { format!("\u{0}{}", c.name) } else { c.project.clone() };
        let entry = groups.entry(key).or_insert_with(|| StackDto {
            label: if c.project.is_empty() { c.name.clone() } else { c.project.clone() },
            project: c.project.clone(),
            workdir: c.workdir.clone(),
            running: 0,
            total: 0,
            containers: Vec::new(),
        });
        if entry.workdir.is_empty() && !c.workdir.is_empty() {
            entry.workdir = c.workdir.clone();
        }
        entry.total += 1;
        if c.state == "running" {
            entry.running += 1;
        }
        entry.containers.push(ContainerDto { name: c.name.clone(), state: c.state.clone() });
    }
    // Running stacks first, then by label — a stable default before the client
    // re-sorts its own project's stacks to the top.
    let mut stacks: Vec<StackDto> = groups.into_values().collect();
    stacks
        .sort_by(|a, b| (b.running > 0).cmp(&(a.running > 0)).then_with(|| a.label.cmp(&b.label)));
    stacks
}

/// List a host directory for the folder picker. `None`/empty defaults to the
/// user's home directory (falling back to `/`). Directories are listed first.
// `Err` is the reply itself; see `build_tree`.
#[allow(clippy::result_large_err)]
fn build_browse(path: Option<&str>) -> Result<BrowseDto, ApiReply> {
    let dir = match path.filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => dirs_home().unwrap_or_else(|| PathBuf::from("/")),
    };
    let dir = dir.canonicalize().unwrap_or(dir);
    let rd = std::fs::read_dir(&dir)
        .map_err(|e| ApiReply::BadRequest(format!("{}: {e}", dir.display())))?;
    let mut entries = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        // Skip dotfiles to keep the picker readable (dot-dirs are rarely a
        // project root); "up" navigation still reaches any parent.
        if name.starts_with('.') {
            continue;
        }
        let is_dir = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
        entries.push(BrowseEntry { name, path: ent.path().to_string_lossy().into_owned(), is_dir });
    }
    entries.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(BrowseDto {
        path: dir.to_string_lossy().into_owned(),
        parent: dir.parent().map(|p| p.to_string_lossy().into_owned()),
        entries,
    })
}

/// Create one directory under `parent`, then list it so the folder picker can
/// navigate straight in. `name` must be a single path component — no separators,
/// no `.`/`..` — so the picker can never write outside the folder on screen.
// `Err` is the reply itself; see `build_tree`.
#[allow(clippy::result_large_err)]
fn make_dir(parent: Option<&str>, name: &str) -> Result<BrowseDto, ApiReply> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err(ApiReply::BadRequest(format!("bad folder name: {name:?}")));
    }
    let base = match parent.filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => dirs_home().unwrap_or_else(|| PathBuf::from("/")),
    };
    if !base.is_dir() {
        return Err(ApiReply::BadRequest(format!("not a directory: {}", base.display())));
    }
    let dir = base.join(name);
    match std::fs::create_dir(&dir) {
        Ok(()) => {}
        // Already a directory: succeed, so a double-tap just navigates into it.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => {}
        Err(e) => return Err(ApiReply::Error(format!("mkdir {}: {e}", dir.display()))),
    }
    build_browse(Some(&dir.to_string_lossy()))
}

/// The user's home directory from `$HOME` (no external crate).
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// Resolve a client-supplied relative path against a workspace root, refusing
/// anything that escapes it (`..`, absolute paths). Purely lexical, so it works
/// for paths that no longer exist (e.g. a deleted file being diffed).
/// Run `work` on a blocking thread and send whatever it returns to `reply`.
/// Used for every API request that touches the filesystem, so a directory that
/// never answers parks one pool thread instead of the whole daemon.
fn spawn_reply<F>(reply: tokio::sync::oneshot::Sender<ApiReply>, work: F)
where
    F: FnOnce() -> ApiReply + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let _ = reply.send(work());
    });
}

fn safe_join(root: &Path, rel: &str) -> Option<PathBuf> {
    use std::path::Component;
    let mut p = PathBuf::from(root);
    for comp in Path::new(rel).components() {
        match comp {
            Component::Normal(c) => p.push(c),
            Component::CurDir => {}
            Component::ParentDir => {
                if !p.pop() || !p.starts_with(root) {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    p.starts_with(root).then_some(p)
}

#[cfg(test)]
mod tests {
    use butai_protocol::KeyCode;

    use super::*;
    use crate::config::AgentDef;
    use butai_protocol::{Encoding, KeyEvent};
    use tokio::sync::mpsc::unbounded_channel;

    /// A buffer whose rows are the given lines, left-aligned and space-padded.
    fn buf_of(lines: &[&str]) -> Buffer {
        let w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as u16;
        let mut buf = Buffer::empty(ratatui::layout::Rect::new(0, 0, w, lines.len() as u16));
        for (y, line) in lines.iter().enumerate() {
            for (x, ch) in line.chars().enumerate() {
                buf[(x as u16, y as u16)].set_symbol(&ch.to_string());
            }
        }
        buf
    }

    /// A drag across rows takes whole lines in between, the way a terminal
    /// selection does — not the rectangle the two corners describe.
    #[test]
    fn a_multi_row_selection_takes_the_lines_between_its_ends() {
        let buf = buf_of(&["first line", "second line", "third line"]);
        // From the middle of row 0 to the middle of row 2.
        let text = extract_selection(&buf, (6, 0), (5, 2));
        assert_eq!(text, "line\nsecond line\nthird");
        // Backwards is the same selection: the anchor can be either end.
        assert_eq!(extract_selection(&buf, (5, 2), (6, 0)), text);
        // Trailing blanks are dropped per line, as every terminal does.
        let padded = buf_of(&["a   ", "b   "]);
        assert_eq!(extract_selection(&padded, (0, 0), (3, 1)), "a\nb");
    }

    /// A commit that lands *while* a status scan is running must still reach
    /// the rail.
    ///
    /// The scan walks the worktree off-thread and a per-pane guard stops the
    /// ~2s sampler stacking them. That guard used to **drop** any request that
    /// arrived mid-scan — but the running scan started before the commit, so
    /// it reports the tree from before it, and nothing was left to correct
    /// that. The rail then showed files as staged that were already committed
    /// until something unrelated triggered another scan.
    ///
    /// This surfaced as three intermittently-failing tests that all looked
    /// like ordinary polling races.
    #[tokio::test]
    async fn a_change_during_a_scan_is_not_lost_with_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();

        let (etx, _erx) = unbounded_channel();
        let (otx, _orx) = tokio::sync::mpsc::channel(OUTPUT_CHANNEL_CAP);
        let mut core = ServerCore::new(test_config(), etx, otx, CoreMode::Standalone);

        let pane = PaneId(1);
        let git = crate::pane::git::GitPane::new(dir.path()).unwrap();
        core.panes.insert(pane, PaneState::Git(git));

        // A scan starts.
        core.request_git_refresh(pane);
        assert!(core.git_refreshing.contains(&pane), "no scan was started");

        // A commit lands while it is running.
        core.request_git_refresh(pane);
        assert!(
            core.git_refresh_again.contains(&pane),
            "the request that arrived mid-scan was dropped"
        );

        // The in-flight scan lands, carrying pre-commit data — and another is
        // started rather than the rail being left with it.
        let snap = crate::pane::git::GitPane::compute(dir.path());
        core.handle(Event::GitRefreshed(pane, snap));
        assert!(
            core.git_refreshing.contains(&pane),
            "the deferred scan was never run, so the rail keeps the stale snapshot"
        );
        assert!(!core.git_refresh_again.contains(&pane), "the deferral was not cleared");

        // A pane that goes away is owed nothing.
        core.git_refresh_again.insert(pane);
        core.panes.remove(&pane);
        core.git_refreshing.remove(&pane);
        core.git_refresh_again.remove(&pane);
        core.handle(Event::GitRefreshed(pane, crate::pane::git::GitPane::compute(dir.path())));
        assert!(!core.git_refreshing.contains(&pane), "rescanned for a pane that is gone");
    }

    /// A file that exists only in the worktree still has a diff.
    ///
    /// `git diff` compares the index against the worktree and an untracked file
    /// is on neither side, so it printed nothing at all: the row CHANGES marks
    /// `?` — and gives a `+n` line count to, from a status walk that *does*
    /// read untracked content — opened on "(no differences)".
    ///
    /// The whole-section diff was the same question asked the other way and had
    /// its own failure: the empty pathspec it was spelled with (`-- ""`) has
    /// been fatal since git 2.16, so it answered with an error rather than the
    /// tree.
    #[test]
    fn a_brand_new_file_has_a_diff() {
        use butai_protocol::api::ApplyTarget;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let repo = git2::Repository::init(root).unwrap();
        let mut cfg = repo.config().unwrap();
        cfg.set_str("user.name", "Test").unwrap();
        cfg.set_str("user.email", "test@example.com").unwrap();
        std::fs::write(root.join("tracked.txt"), "one\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("tracked.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();

        // One tracked edit, one brand new file in a subdirectory, and one file
        // that is ignored — the three things the unstaged section can hold.
        std::fs::write(root.join("tracked.txt"), "one\ntwo\n").unwrap();
        std::fs::create_dir_all(root.join("brand")).unwrap();
        std::fs::write(root.join("brand/new.txt"), "alpha\nbeta\n").unwrap();
        std::fs::write(root.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(root.join("noisy.log"), "not yours\n").unwrap();

        let d = ServerCore::build_diff(root, "brand/new.txt", false).unwrap();
        assert!(d.patch.contains("new file mode"), "no new-file header:\n{}", d.patch);
        assert!(d.patch.contains("+alpha"), "the file's own lines are missing:\n{}", d.patch);
        assert!(
            d.patch.contains("b/brand/new.txt"),
            "the path is not repo-root relative:\n{}",
            d.patch
        );

        // The whole section, which is the same route with no path: both changes,
        // and nothing git was told to ignore.
        let all = ServerCore::build_diff(root, "", false).unwrap();
        assert!(all.patch.contains("+two"), "the tracked edit is missing:\n{}", all.patch);
        assert!(all.patch.contains("brand/new.txt"), "the new file is missing:\n{}", all.patch);
        assert!(
            !all.patch.contains("noisy.log"),
            "an ignored file reached the diff:\n{}",
            all.patch
        );

        // And what you are shown is what stages: the patch applies to the index,
        // which is what `space` on one of its hunks does.
        crate::pane::git::apply_patch(root, &d.patch, ApplyTarget::Index, false).unwrap();
        let mut index = repo.index().unwrap();
        index.read(true).unwrap();
        let entry = index.get_path(Path::new("brand/new.txt"), 0).expect("not staged");
        let staged = repo.find_blob(entry.id).unwrap().content().to_vec();
        assert_eq!(String::from_utf8(staged).unwrap(), "alpha\nbeta\n");

        // Staged, it is the staged side's business and no longer the worktree's.
        let unstaged = ServerCore::build_diff(root, "brand/new.txt", false).unwrap();
        assert!(!unstaged.patch.contains("+alpha"), "still unstaged:\n{}", unstaged.patch);
        let staged = ServerCore::build_diff(root, "brand/new.txt", true).unwrap();
        assert!(staged.patch.contains("+alpha"), "the staged side lost it:\n{}", staged.patch);
    }

    fn test_config() -> Config {
        let mut cfg = Config::with_defaults();
        cfg.general.default_shell = Some("/bin/sh".into());
        cfg.agents = vec![AgentDef {
            name: "fakeagent".into(),
            command: "/bin/sh".into(),
            args: vec![],
            resume_args: vec![],
            env: Default::default(),
            waiting_pattern: None,
            busy_pattern: None,
        }];
        cfg
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn resumable_agent() -> AgentDef {
        AgentDef {
            name: "claude".into(),
            command: "claude".into(),
            args: argv(&["--skip", "--session-id", "{session_id}"]),
            resume_args: argv(&["--skip", "--resume", "{session_id}"]),
            env: Default::default(),
            waiting_pattern: None,
            busy_pattern: None,
        }
    }

    /// The placeholder is the whole mechanism: it is both how a launcher says
    /// it can be told which conversation to open, and where the id goes.
    #[test]
    fn the_session_placeholder_is_substituted_where_it_appears() {
        let agent = resumable_agent();
        assert!(assigns_session(&agent), "the placeholder is the declaration");
        assert_eq!(
            expand_args(&agent.resume_args, Some("abc-123")),
            Some(argv(&["--skip", "--resume", "abc-123"])),
        );

        // A launcher that never mentions a conversation is passed through
        // untouched — this is every `[[agents]]` block written before any of
        // this existed, and every CLI that has no session concept.
        let plain =
            AgentDef { args: argv(&["--yes-always"]), resume_args: vec![], ..resumable_agent() };
        assert!(!assigns_session(&plain));
        assert_eq!(expand_args(&plain.args, None), Some(argv(&["--yes-always"])));
        assert_eq!(expand_args(&plain.args, Some("abc-123")), Some(argv(&["--yes-always"])));
    }

    /// An argv that needs an id and has none must not run. Substituting nothing
    /// would hand the CLI the literal text `{session_id}` as a conversation
    /// name, which it would reject — so the caller has to be told to fall back.
    #[test]
    fn an_argv_needing_an_id_it_does_not_have_refuses_to_expand() {
        let agent = resumable_agent();
        assert_eq!(expand_args(&agent.resume_args, None), None);
    }

    /// A fresh launch mints its own conversation, and does so before the
    /// process starts — which is what stops two agents opened in the same
    /// directory at the same moment from ever landing on the same transcript.
    #[test]
    fn every_fresh_launch_names_its_own_conversation() {
        let agent = resumable_agent();
        let (first, first_args) = fresh_launch(&agent);
        let (second, _) = fresh_launch(&agent);
        assert!(first.is_some() && second.is_some(), "both were given one");
        assert_ne!(first, second, "two agents must not share a conversation");
        // The launch args carry it, not the resume args.
        assert_eq!(first_args, argv(&["--skip", "--session-id", first.as_ref().unwrap()]));

        // A launcher with no session concept is given none, rather than an id
        // it has nowhere to put.
        let plain =
            AgentDef { args: argv(&["--yes-always"]), resume_args: vec![], ..resumable_agent() };
        assert_eq!(fresh_launch(&plain), (None, argv(&["--yes-always"])));
    }

    /// Two projects whose directories share a basename must not share a dump
    /// directory, or restoring one replays the other's output into its panes.
    #[test]
    fn workspace_dump_keys_distinguish_same_named_directories() {
        let a = workspace_key(Path::new("/home/p/alpha/src"));
        let b = workspace_key(Path::new("/home/p/beta/src"));
        assert_ne!(a, b, "same basename, different projects");
        assert!(a.starts_with("src-"), "readable enough to delete by hand: {a}");
        assert_eq!(a, workspace_key(Path::new("/home/p/alpha/src")), "stable across runs");
    }

    async fn drain_until_frame(
        rx: &mut UnboundedReceiver<ServerMsg>,
        pred: impl Fn(&FrameUpdate) -> bool,
    ) -> FrameUpdate {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await
                .expect("timed out waiting for frame")
                .expect("server hung up");
            if let ServerMsg::Frame(f) = msg {
                if pred(&f) {
                    return f;
                }
            }
        }
    }

    /// A handshake streaming one pane, which is the only thing the daemon
    /// draws. A session target names a workspace, not a screen, so it would
    /// produce no frames to read.
    fn hello(pane: PaneId) -> ClientMsg {
        ClientMsg::Hello {
            proto_version: PROTOCOL_VERSION,
            encoding: Encoding::Json,
            cols: 120,
            rows: 30,
            target: AttachTarget::Pane { pane },
            cwd: PathBuf::from("/"),
        }
    }

    /// Everything a run of frames painted, as text — for asserting on a pane's
    /// output without reimplementing a terminal.
    fn frame_text(frames: &[FrameUpdate]) -> String {
        let mut grid: Vec<Vec<char>> = vec![vec![' '; 120]; 30];
        for f in frames {
            for run in &f.cells {
                for (i, cell) in run.cells.iter().enumerate() {
                    let (x, y) = (run.x as usize + i, run.y as usize);
                    if y < grid.len() && x < grid[0].len() {
                        grid[y][x] = cell.ch.chars().next().unwrap_or(' ');
                    }
                }
            }
        }
        grid.iter().map(|r| r.iter().collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    /// What happens to an agent's row when the agent's process ends, which is two
    /// different things: a clean exit means the work is done and the row goes,
    /// a failure means there is something to read and the row stays, marked.
    ///
    /// Asserted against `AgentDto`, which is what every client draws from —
    /// this used to read a rail out of a composed frame, and the daemon draws
    /// no rail.
    #[test]
    fn a_clean_exit_clears_the_agent_row_and_a_failure_keeps_it() {
        for (status, survives) in [(0u32, false), (3, true)] {
            let dir = tempfile::tempdir().unwrap();
            let (etx, _erx) = unbounded_channel();
            let (otx, _orx) = tokio::sync::mpsc::channel(OUTPUT_CHANNEL_CAP);
            let mut core = ServerCore::new(test_config(), etx, otx, CoreMode::Standalone);
            let sid = core.create_workspace("t".into(), dir.path().to_path_buf()).unwrap();
            core.spawn_agent(sid, "fakeagent").unwrap();

            let pane = *core.workspaces[&sid].agents.first().expect("agent did not start");
            core.on_pane_exited(pane, status);
            // The published state is debounced through `agent_track`, which the
            // sampler tick advances; without this the row would still read
            // whatever it was doing a moment before it died.
            core.update_agent_tracking();

            let agents = core.build_agents(sid);
            assert_eq!(
                agents.len(),
                usize::from(survives),
                "exit {status}: {} row(s), wanted {}",
                agents.len(),
                usize::from(survives)
            );
            if survives {
                assert_eq!(agents[0].state, AgentState::Exited);
                assert_eq!(
                    agents[0].exited,
                    Some(status),
                    "the row should say what it exited with"
                );
            }
        }
    }

    /// The read/unread bit, end to end in the daemon: an agent that reaches a
    /// your-move state is unread until somebody looks, and looking is what
    /// clears it.
    ///
    /// Driven through the exit edge rather than the finish one because a finish
    /// needs a real working run and `AGENT_SETTLE` of wall clock to debounce,
    /// while both edges set the bit in the same place. What is specific to this
    /// test is the *clearing*, which is shared by every look-at gesture.
    #[test]
    fn a_your_move_edge_is_unread_until_the_pane_is_looked_at() {
        let dir = tempfile::tempdir().unwrap();
        let (etx, _erx) = unbounded_channel();
        let (otx, _orx) = tokio::sync::mpsc::channel(OUTPUT_CHANNEL_CAP);
        let mut core = ServerCore::new(test_config(), etx, otx, CoreMode::Standalone);
        let sid = core.create_workspace("t".into(), dir.path().to_path_buf()).unwrap();
        core.spawn_agent(sid, "fakeagent").unwrap();
        let pane = *core.workspaces[&sid].agents.first().expect("agent did not start");

        // Seed the baseline, so the edge below is one the daemon watched happen
        // rather than one it merely found already true.
        core.update_agent_tracking();

        // Now cross the edge for real.
        core.on_pane_exited(pane, 3);
        core.touch_agent(pane);
        core.update_agent_tracking();
        let dto = core.agent_dto(pane).unwrap();
        assert_eq!(dto.state, AgentState::Exited);
        assert!(dto.unread, "crossing into a your-move state should be news");
        assert_eq!(core.build_ws_summaries()[0].unread, 1, "the workspace should count it");

        // Looking at it is what makes it read — and the state is untouched,
        // because reading a thing does not un-happen it.
        core.look_at_pane(pane);
        let dto = core.agent_dto(pane).unwrap();
        assert!(!dto.unread, "looking at the pane should have marked it read");
        assert_eq!(dto.state, AgentState::Exited, "reading it changed the state");
        assert_eq!(core.build_ws_summaries()[0].unread, 0);

        // And it stays read: a later tick that finds nothing new must not
        // resurrect the mark, or every sampler pass would undo the last look.
        core.touch_agent(pane);
        core.update_agent_tracking();
        assert!(!core.agent_dto(pane).unwrap().unread, "a quiet tick re-marked it unread");
    }

    /// The baseline pass reports, it does not announce.
    ///
    /// An agent that is *already* in a your-move state the first time the daemon
    /// looks at it has not crossed an edge anyone was watching — that is the
    /// daemon catching up, and the notification feed stays silent for the same
    /// reason. Without the `seeded_before` guard, restarting the daemon under a
    /// workbench of long-finished agents lights every one of them up as news,
    /// which is precisely the wall of undifferentiated rows `unread` exists to
    /// get rid of.
    #[test]
    fn an_agent_already_gone_when_first_seen_is_not_news() {
        let dir = tempfile::tempdir().unwrap();
        let (etx, _erx) = unbounded_channel();
        let (otx, _orx) = tokio::sync::mpsc::channel(OUTPUT_CHANNEL_CAP);
        let mut core = ServerCore::new(test_config(), etx, otx, CoreMode::Standalone);
        let sid = core.create_workspace("t".into(), dir.path().to_path_buf()).unwrap();
        core.spawn_agent(sid, "fakeagent").unwrap();
        let pane = *core.workspaces[&sid].agents.first().expect("agent did not start");

        // Dead before the tracker's first look, so the very first observation is
        // the your-move state itself.
        core.on_pane_exited(pane, 3);
        core.update_agent_tracking();

        let dto = core.agent_dto(pane).unwrap();
        assert_eq!(dto.state, AgentState::Exited, "the row should still say what happened");
        assert!(!dto.unread, "the daemon's first look at an agent invented news");
        assert_eq!(core.build_ws_summaries()[0].unread, 0);
    }

    #[tokio::test]
    async fn repo_created_after_open_gets_a_changes_rail() {
        let dir = tempfile::tempdir().unwrap();
        let (etx, mut erx) = unbounded_channel();
        let (otx, _orx) = tokio::sync::mpsc::channel(OUTPUT_CHANNEL_CAP);
        let mut core = ServerCore::new(test_config(), etx, otx, CoreMode::Standalone);

        // The probe runs off-thread, so drive it the way the run loop does:
        // kick it off, then feed the resulting event back into `handle`.
        async fn pump(core: &mut ServerCore, erx: &mut UnboundedReceiver<Event>) {
            core.attach_new_repos();
            while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(5), erx.recv()).await
            {
                let done = matches!(ev, Event::RepoProbed(..));
                core.handle(ev);
                if done {
                    return;
                }
            }
            panic!("repo probe never reported");
        }

        let sid = core.create_workspace("t".into(), dir.path().to_path_buf()).unwrap();
        assert!(core.workspaces[&sid].changes.is_none(), "no repo yet");

        // Not a repo yet: the probe reports nothing and schedules a backoff.
        pump(&mut core, &mut erx).await;
        assert!(core.workspaces[&sid].changes.is_none(), "no repo yet");
        assert!(core.repo_probe_next.contains_key(&sid), "backoff not armed");

        // `git init` after the workspace opened: a later tick picks it up. Clear
        // the backoff to stand in for the elapsed wait.
        git2::Repository::init(dir.path()).unwrap();
        core.repo_probe_next.remove(&sid);
        pump(&mut core, &mut erx).await;
        let changes = core.workspaces[&sid].changes.expect("rail missing after git init");
        assert!(matches!(core.panes.get(&changes), Some(PaneState::Git(_))));

        // Idempotent: a second pass must not replace the pane. The workspace now
        // has a rail, so it is no longer a probe candidate at all.
        core.attach_new_repos();
        assert!(core.repo_probing.is_empty(), "probed a workspace that already has a rail");
        assert_eq!(core.workspaces[&sid].changes, Some(changes));
    }

    /// A new pane is born the size the client watching this workspace says the
    /// stage is — not the size this side guesses from a terminal it cannot see.
    ///
    /// The TUI attaches to a *pane* now, not a workspace, so `Workspace::cols`
    /// and `rows` never hear about its terminal and stay at the 80x24 default.
    /// A pane spawned into a workspace the TUI was looking at therefore came up
    /// at the default minus chrome and was reflowed the instant it was staged.
    /// A program that reads its window size once, at startup, only gets one
    /// chance — so it has to be right before the program runs, not after.
    #[tokio::test]
    async fn a_new_pane_is_born_the_size_the_client_says_the_stage_is() {
        let dir = tempfile::tempdir().unwrap();
        let (etx, _erx) = unbounded_channel();
        let (otx, _orx) = tokio::sync::mpsc::channel(OUTPUT_CHANNEL_CAP);
        let mut core = ServerCore::new(test_config(), etx, otx, CoreMode::Standalone);

        // Created over the API, so the only size on offer is the default.
        let sid = core.create_workspace("t".into(), dir.path().to_path_buf()).unwrap();
        let guessed = core.stage_size(sid);
        let stage = core.workspaces[&sid].stage.expect("no stage pane");

        // A client attaches to that pane and reports the hole its rails left.
        // The size arrives in the *handshake*, not in a later `Resize`: a TUI
        // that opens at its final size never sends one, so recording it only on
        // resize left the common case unfixed.
        let (ctx, _crx) = unbounded_channel();
        let cid: ClientId = 1;
        core.handle(Event::ClientConnected(cid, ctx));
        core.attach_client(cid, 92, 44, AttachTarget::Pane { pane: stage }, dir.path().into());

        assert_eq!(core.stage_size(sid), (44, 92), "the handshake size was ignored");
        assert_ne!(guessed, (44, 92), "the guess already matched; this proves nothing");

        // The window changes: the later measurement wins.
        core.resize_client(cid, 100, 50);
        assert_eq!(core.stage_size(sid), (50, 100), "the resize was ignored");

        // Switching tabs is a `Watch`, and it carries the size too: the second
        // workspace has never been drawn, so without this its first spawned
        // pane would come up at the guess even though a client is right there
        // looking at it.
        let other_ws = core.create_workspace("u".into(), dir.path().to_path_buf()).unwrap();
        assert_eq!(core.workspaces[&other_ws].stage_size, None, "drawn already?");
        let other_stage = core.workspaces[&other_ws].stage.expect("no stage pane");
        core.watch_pane(cid, other_stage);
        assert_eq!(core.stage_size(other_ws), (50, 100), "the watch carried no size");
    }

    #[test]
    fn make_dir_creates_and_lists_the_new_folder() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().to_string_lossy().into_owned();

        let dto = make_dir(Some(&parent), "my-project").expect("mkdir failed");
        let created = dir.path().join("my-project");
        assert!(created.is_dir(), "directory not created");
        // The reply lists the *new* directory, so a picker navigates into it.
        assert_eq!(dto.path, created.canonicalize().unwrap().to_string_lossy());
        assert!(dto.entries.is_empty());

        // Idempotent: creating it again just lists it, no error.
        let again = make_dir(Some(&parent), "my-project").expect("second mkdir failed");
        assert_eq!(again.path, dto.path);
    }

    #[test]
    fn make_dir_refuses_names_that_escape_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().to_string_lossy().into_owned();

        for bad in ["", "  ", ".", "..", "../escape", "a/b", "/abs"] {
            assert!(
                matches!(make_dir(Some(&parent), bad), Err(ApiReply::BadRequest(_))),
                "accepted bad folder name {bad:?}"
            );
        }
        // A missing parent is a bad request too, not a silent mkdir -p.
        let missing = dir.path().join("nope").to_string_lossy().into_owned();
        assert!(matches!(make_dir(Some(&missing), "x"), Err(ApiReply::BadRequest(_))));
    }

    #[test]
    fn scratch_name_keeps_the_extension_and_drops_the_rest() {
        assert_eq!(scratch_name("clipboard.png"), "clipboard.png");
        // Only the basename survives, so no client can climb out of the dir.
        assert_eq!(scratch_name("../../etc/passwd"), "passwd");
        assert_eq!(scratch_name("/abs/path/shot.jpeg"), "shot.jpeg");
        assert_eq!(scratch_name(r"C:\Users\me\a.png"), "a.png");
        // Anything that could mean something to a shell becomes a dash.
        assert_eq!(scratch_name("a b;rm -rf *.png"), "a-b-rm--rf--.png");
        // Names that carry no usable stem still get a file to write to.
        assert_eq!(scratch_name(".."), "paste");
        assert_eq!(scratch_name(""), "paste");
        assert_eq!(scratch_name("///"), "paste");
        assert!(!scratch_name(&"x".repeat(500)).is_empty());
        assert!(scratch_name(&"x".repeat(500)).len() <= 64);
    }

    #[test]
    fn prune_scratch_keeps_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        for i in 1..=SCRATCH_KEEP + 5 {
            std::fs::write(dir.path().join(format!("{i:06}-shot.png")), b"x").unwrap();
        }
        prune_scratch(dir.path());
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left.len(), SCRATCH_KEEP);
        // The counter is zero-padded precisely so this is the *newest* set and
        // not the lexicographically-last one ("9" > "37" unpadded).
        assert_eq!(left.first().unwrap(), &format!("{:06}-shot.png", 6));
        assert_eq!(left.last().unwrap(), &format!("{:06}-shot.png", SCRATCH_KEEP + 5));
    }

    /// Pull messages off a client until one matches, so a test can wait for a
    /// reply without caring how many frames the repaint produced first.
    ///
    /// Frames passed over are collected into `seen` rather than dropped: the
    /// repaint carrying the effect a test wants to assert on often arrives
    /// *before* the reply it is waiting for, and swallowing it here would leave
    /// the test watching for a paint that already happened.
    async fn drain_until<T>(
        rx: &mut UnboundedReceiver<ServerMsg>,
        seen: &mut Vec<FrameUpdate>,
        pick: impl Fn(&ServerMsg) -> Option<T>,
    ) -> T {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await
                .expect("timed out waiting for message")
                .expect("server hung up");
            if let Some(v) = pick(&msg) {
                return v;
            }
            if let ServerMsg::Frame(f) = msg {
                seen.push(f);
            }
        }
    }

    #[tokio::test]
    async fn put_file_writes_to_scratch_and_pastes_the_path() {
        let store = tempfile::tempdir().unwrap();
        let (etx, erx) = unbounded_channel();
        let (otx, orx) = tokio::sync::mpsc::channel(OUTPUT_CHANNEL_CAP);
        let mut core = ServerCore::new(test_config(), etx.clone(), otx, CoreMode::Standalone);
        // Also keeps the test off the real `~/.butai/scratch`.
        core.set_session_store(store.path().join("session.json"));
        let sid = core.create_workspace("t".into(), store.path().to_path_buf()).unwrap();
        let pane = core.workspaces[&sid].stage.expect("the workspace opened with a shell");
        let handle = tokio::spawn(core.run(erx, orx));

        let (ctx, mut crx) = unbounded_channel();
        etx.send(Event::ClientConnected(1, ctx)).unwrap();
        etx.send(Event::Client(1, hello(pane))).unwrap();
        let mut seen = vec![drain_until_frame(&mut crx, |f| f.full).await];

        // Type the front of a command, so the pasted path completes it and the
        // shell's own output proves the paste landed. Asserting on the echoed
        // path instead would be a test of line wrapping: a scratch path is
        // longer than the pane is wide.
        for ch in "wc -c < ".chars() {
            etx.send(Event::Client(1, ClientMsg::Input(InputEvent::Key(KeyEvent::char(ch)))))
                .unwrap();
        }

        // Every byte value, so nothing is hiding an encoding that only survives
        // ASCII, and a distinctive length for `wc` to report back.
        let png: Vec<u8> = (0..4241).map(|i| (i % 256) as u8).collect();
        etx.send(Event::Client(
            1,
            ClientMsg::Command(Command::PutFile {
                name: "../clip board.png".into(),
                data: butai_protocol::b64::encode(&png),
            }),
        ))
        .unwrap();

        let path = drain_until(&mut crx, &mut seen, |m| match m {
            ServerMsg::FilePut { path } => Some(path.clone()),
            _ => None,
        })
        .await;

        assert_eq!(std::fs::read(&path).unwrap(), png, "bytes did not survive the round trip");
        assert!(
            path.starts_with(store.path().join("scratch")),
            "wrote outside the scratch dir: {}",
            path.display()
        );
        assert!(
            path.file_name().unwrap().to_string_lossy().ends_with("-clip-board.png"),
            "unexpected file name: {}",
            path.display()
        );

        // Run the completed command. If the path reached the pane, the shell
        // reads the file butai just wrote and reports its size.
        etx.send(Event::Client(
            1,
            ClientMsg::Input(InputEvent::Key(KeyEvent {
                code: KeyCode::Enter,
                mods: <_>::default(),
            })),
        ))
        .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            seen.push(drain_until_frame(&mut crx, |_| true).await);
            if frame_text(&seen).contains(&png.len().to_string()) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "pasted path never reached the pane:\n{}",
                frame_text(&seen)
            );
        }

        etx.send(Event::Client(1, ClientMsg::Detach)).unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    async fn put_file_rejects_garbage_without_writing() {
        let store = tempfile::tempdir().unwrap();
        let (etx, erx) = unbounded_channel();
        let (otx, orx) = tokio::sync::mpsc::channel(OUTPUT_CHANNEL_CAP);
        let mut core = ServerCore::new(test_config(), etx.clone(), otx, CoreMode::Standalone);
        core.set_session_store(store.path().join("session.json"));
        let handle = tokio::spawn(core.run(erx, orx));

        let (ctx, mut crx) = unbounded_channel();
        etx.send(Event::ClientConnected(1, ctx)).unwrap();
        etx.send(Event::Client(
            1,
            ClientMsg::Hello {
                proto_version: PROTOCOL_VERSION,
                encoding: Encoding::Json,
                cols: 120,
                rows: 30,
                target: AttachTarget::Control,
                cwd: PathBuf::from("/"),
            },
        ))
        .unwrap();

        for (name, data) in [("a.png", "not base64!"), ("a.png", "")] {
            etx.send(Event::Client(
                1,
                ClientMsg::Command(Command::PutFile { name: name.into(), data: data.into() }),
            ))
            .unwrap();
            let err = drain_until(&mut crx, &mut Vec::new(), |m| match m {
                ServerMsg::Error(e) => Some(e.clone()),
                _ => None,
            })
            .await;
            assert!(!err.is_empty(), "empty error for {data:?}");
        }
        assert!(!store.path().join("scratch").exists(), "a rejected put still made a directory");

        etx.send(Event::Client(1, ClientMsg::Detach)).unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), handle).await;
    }

    // -- context menu --------------------------------------------------------
}
