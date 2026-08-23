//! Structured HTTP/REST API surface (the "Docker-style" control face).
//!
//! These are the JSON shapes served over the same Unix socket as the framed
//! client protocol, distinguished by the first byte of the connection (a
//! framed hello starts with `0x00`, an HTTP request with an ASCII method).
//! The daemon speaks them via `Event::Api`/`Event::ApiSubscribe`; see
//! `butai-server`'s `http_conn` module for the router.
//!
//! [`ApiRequest`]/[`ApiReply`] are the in-process bridge between the HTTP
//! handler task and the single-owner core actor and are intentionally *not*
//! serialized. Everything reachable by a client is a `#[derive(Serialize)]`
//! DTO built by the core from its live state.

use serde::{Deserialize, Serialize};

use crate::{InputEvent, PaneId, SessionId};

/// Attention state of an agent pane (rendered by the apps as
/// `[?]`/`[~]`/`[v]`/`[ ]`/`[x]`). `Waiting` and `Finished` are distinct on
/// purpose: a mid-task question needs you *now*, a finished turn is just your
/// move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Blocked on your input mid-task — asked a question or a decision prompt is
    /// on screen. Act now (`[?]`).
    Waiting,
    /// Recent output — actively working (`[~]`).
    Working,
    /// Finished its turn and settled at the prompt; your move (`[v]`).
    Finished,
    /// Quiet, nothing pending (`[ ]`).
    Idle,
    /// The process is gone (`[x]`); its code is in [`AgentDto::exited`]. A dead
    /// agent used to report `Idle`, which made a corpse indistinguishable from a
    /// quiet live agent for any client that forgot to check `exited` — and no
    /// client should have to. Added after `Idle`, so a client built against an
    /// older daemon decodes it as its unknown/default arm.
    Exited,
}

/// One agent row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct AgentDto {
    pub pane: PaneId,
    /// Live title (agent name, or the OSC title the program set).
    pub title: String,
    pub state: AgentState,
    /// Exit code once the agent process has exited; `null` while running.
    pub exited: Option<u32>,
    /// Whether a decision prompt is actually on screen, as opposed to the agent
    /// merely having rung the bell. Both produce [`AgentState::Waiting`] — this
    /// separates "it asked you something" from "it wants attention", so a client
    /// can render the two differently.
    #[serde(default)]
    pub question: bool,
    /// Unix epoch millis when this agent's process started.
    ///
    /// An absolute instant rather than an age, so a client can run the clock
    /// itself. An age would be stale the moment it was serialized and would
    /// force a push per second to keep a timer honest — which is the opposite of
    /// what the diffed event stream is for. "How long has this been alive" is
    /// what makes a five-minute-old agent read differently from a two-hour one.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub started_ms: u64,
    /// Unix epoch millis when the current turn began, or `null` when the agent
    /// is not working.
    ///
    /// Same reasoning as [`started_ms`](Self::started_ms): the client ticks its
    /// own clock. This is the number behind "it has been thinking for 4:32",
    /// which is the difference between a rail that says an agent is busy and one
    /// that says whether to worry about it.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub working_since_ms: Option<u64>,
    /// The agent reached a your-move state and you have not looked at it since.
    ///
    /// [`AgentState`] says what an agent *is*; this says whether you already
    /// know. Without it a turn you read an hour ago and one that landed while
    /// you were away are the same row — `Finished` holds until the agent works
    /// again, so "done" is equally true of both and equally uninformative.
    ///
    /// Set on the edges into [`Finished`](AgentState::Finished) and
    /// [`Exited`](AgentState::Exited) — the two states that are your move — and
    /// cleared when the pane is looked at, which is the same gesture that
    /// already clears a bell (staging it, or an explicit `AckPane` from a list
    /// client). Deliberately *not* set for [`Waiting`](AgentState::Waiting): an
    /// unanswered question stays urgent no matter how many times you have read
    /// it, and a "read" flag beside it would only invite a client to quieten
    /// the one state that must not be quietened.
    ///
    /// Never true while the agent is working or idle, so a client can treat it
    /// as "there is something here for me" without also checking `state`.
    ///
    /// Added after 0.6, and `false` is the honest default: a client built
    /// against a newer daemon talking to an older one sees nothing unread and
    /// degrades to the behaviour it had before this field existed.
    #[serde(default)]
    pub unread: bool,
}

/// One managed-process row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct ProcessDto {
    pub pane: PaneId,
    pub name: String,
    pub command: String,
    /// Human-readable status: `ok`, `run`, `done`, `FAIL(<code>)`, or `...`.
    pub status: String,
    pub exited: Option<u32>,
}

/// One GPU's live utilization + VRAM.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct GpuDto {
    pub pct: f32,
    pub mem_used_gb: f32,
    pub mem_total_gb: f32,
    /// Recent `pct` samples, oldest first. See [`SysDto::cpu_hist`].
    #[serde(default)]
    pub hist: Vec<f32>,
    /// Model name, already shortened by the daemon (`RTX 4090`); empty when the
    /// monitor did not report one.
    #[serde(default)]
    pub name: String,
    /// Core temperature in Celsius.
    #[serde(default)]
    pub temp_c: Option<f32>,
    /// Board power draw in watts.
    #[serde(default)]
    pub power_w: Option<f32>,
}

/// What kind of device an interface is.
///
/// A fact about the machine, not a display choice: the daemon reports every
/// interface it can see and says what each one *is*, and the client decides
/// which to draw. Filtering here would hand every client the terminal's
/// opinion with no way to disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "lowercase")]
pub enum NetKind {
    Wired,
    Wireless,
    Loopback,
    /// A docker/LXC-style bridge. Its bytes are counted again on whatever
    /// interface they egress from, so summing it double-counts.
    Bridge,
    /// One end of a container's veth pair. Same double-counting as `Bridge`.
    Veth,
    Vpn,
    #[default]
    Other,
}

/// One network interface's throughput.
///
/// Rates, not counters: the daemon differences the kernel's byte counters over
/// the sample interval, the same way [`SysDto::cpu_pct`] is differenced from
/// jiffies, so a client never has to know when the last sample was taken.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct NetDto {
    /// Kernel name — `eth0`, `wlan0`, `docker0`.
    pub name: String,
    /// Bytes per second, averaged over the last sample interval.
    pub rx_bps: f32,
    pub tx_bps: f32,
    /// Recent `rx_bps`/`tx_bps` samples, oldest first.
    #[serde(default)]
    pub rx_hist: Vec<f32>,
    #[serde(default)]
    pub tx_hist: Vec<f32>,
    #[serde(default)]
    pub kind: NetKind,
    /// The link is up and has carrier.
    pub carrier: bool,
    /// This interface carries the default route — the one a person means by
    /// "the network". Reported rather than inferred, because the client has no
    /// routing table.
    #[serde(default)]
    pub default_route: bool,
    /// Negotiated link speed in Mb/s, where the driver publishes one. Absent for
    /// wireless and for the virtual interfaces, which have no such number.
    #[serde(default)]
    pub speed_mbps: Option<u32>,
    /// Kernel driver bound to the device (`r8169`, `iwlwifi`). Absent for
    /// interfaces with no device behind them.
    #[serde(default)]
    pub driver: Option<String>,
}

/// What kind of filesystem a mount is.
///
/// A fact about the machine, not a display choice — the same contract
/// [`NetKind`] states, and for the same reason: the daemon reports every mount
/// and says what each one *is*, and the client decides which are worth drawing.
/// Filtering here would hand every client the terminal's opinion about which
/// disk matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "lowercase")]
pub enum DiskKind {
    /// A real block device: `/dev/nvme0n1p2`, `/dev/sda1`.
    Local,
    /// Network-backed: nfs, cifs/smb, sshfs. The kind whose server can go away
    /// while the mount stays, which is why [`DiskDto::stale`] exists.
    Network,
    /// RAM-backed: tmpfs, devtmpfs, ramfs. Its bytes are already counted as
    /// RAM, so drawing them as storage reports the same gigabytes twice — the
    /// same double-counting [`NetKind::Bridge`] describes.
    Memory,
    /// A container or image layer: overlay, squashfs, aufs. Its space is
    /// charged again to whatever filesystem it is built on.
    Layer,
    #[default]
    Other,
}

/// One mounted filesystem's capacity.
///
/// Gigabytes rather than blocks: block size is a per-filesystem fact a client
/// would have to be told in order to use, and every other capacity on
/// [`SysDto`] is already in GB.
///
/// Throughput is deliberately absent. I/O belongs to a *device*, not a mount,
/// so two mounts on one disk would each report the whole device's traffic —
/// that is a list of its own keyed by kernel device name, not a pair of fields
/// here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct DiskDto {
    /// Mount point — `/`, `/home`, `/media/fast`.
    pub mount: String,
    /// The source as the mount table spells it: `/dev/nvme0n1p2`, `nas:/vol0`.
    pub source: String,
    /// Filesystem type verbatim: `ext4`, `apfs`, `cifs`.
    pub fstype: String,
    #[serde(default)]
    pub kind: DiskKind,
    /// Total minus *available*, not minus free: the blocks a filesystem
    /// reserves for root are not space a build can have, and `df` reports it
    /// the same way.
    pub used_gb: f32,
    pub total_gb: f32,
    /// The capacity call did not come back in time, and these numbers are the
    /// last good ones — or zero, if there never were any.
    ///
    /// Reported rather than dropped, because a row that vanished reads as a
    /// filesystem somebody unmounted, and that is a different fact about the
    /// machine. See `sys.rs`: `statvfs` on a mount whose server has gone away
    /// blocks uninterruptibly, so the daemon gives it a deadline.
    #[serde(default)]
    pub stale: bool,
}

/// A docker container visible to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct ContainerDto {
    pub name: String,
    pub state: String,
}

/// A group of containers belonging to one compose project (a standalone
/// container forms a one-member stack). Mirrors the PROCESSES/docker rail's
/// stack rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct StackDto {
    /// Display name: compose project, or the container name when standalone.
    pub label: String,
    /// Compose project name; empty when standalone.
    pub project: String,
    /// Compose project working_dir; empty when standalone. The client compares
    /// this against a workspace cwd to flag the stack as "mine".
    pub workdir: String,
    pub running: usize,
    pub total: usize,
    pub containers: Vec<ContainerDto>,
}

/// What a daemon did when it was asked to update itself.
///
/// The answer to `POST /v1/update`, and the only place the versions are
/// reported: the detach that follows deliberately carries the ordinary
/// `DETACH_SERVER_SHUTDOWN` reason, because clients match on that string to
/// tell "the daemon is restarting, keep your cells" from "your pane is gone"
/// — see [`crate::ServerMsg::Detached`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct UpdateDto {
    /// The version the daemon is running as it answers.
    pub current: String,
    /// The latest published version, when the check reached GitHub.
    pub latest: Option<String>,
    /// Whether anything is happening. `false` means `current` was already the
    /// latest and nothing was changed; `true` means a verified binary is in
    /// place and this daemon is going down to come back on it.
    pub updating: bool,
}

/// Machine telemetry (the SYSTEM rail), sampled ~every 2s.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct SysDto {
    pub cpu_pct: f32,
    pub cpu_temp: Option<f32>,
    /// Recent `cpu_pct` samples, oldest first, one per sample interval.
    ///
    /// History is the daemon's to keep: it is the only thing awake often enough
    /// to take a sample, and a client that buffers its own gets a trend that
    /// starts when it attached and disagrees with every other client's.
    #[serde(default)]
    pub cpu_hist: Vec<f32>,
    /// CPU model, already shortened by the daemon (`Ryzen 7 5700`). `None` where
    /// the platform does not publish one.
    ///
    /// A static fact, unlike everything else here: read once when the sampler
    /// starts rather than every two seconds, and resent on each tick only
    /// because it rides the same struct.
    #[serde(default)]
    pub cpu_model: Option<String>,
    /// Physical cores and scheduler-visible threads. Both, because they differ
    /// on every machine with SMT and the pair is the fact a person wants.
    #[serde(default)]
    pub cpu_cores: Option<u16>,
    #[serde(default)]
    pub cpu_threads: Option<u16>,
    pub ram_used_gb: f32,
    pub ram_total_gb: f32,
    /// Recent RAM samples as a *percentage* of total, oldest first — the shape
    /// the rail draws, and the one thing here that is not in the same unit as
    /// the value beside it.
    #[serde(default)]
    pub ram_hist: Vec<f32>,
    /// Swap in use and configured. Zero total means no swap, which is a normal
    /// machine and not a missing reading.
    ///
    /// The DDR type and speed a person means by "which RAM is it" are not here
    /// and cannot be: they live in DMI, which is root-only on Linux. Swap is
    /// what the RAM row can honestly say beyond the totals.
    #[serde(default)]
    pub swap_used_gb: f32,
    #[serde(default)]
    pub swap_total_gb: f32,
    pub gpus: Vec<GpuDto>,
    /// Every interface the daemon can see, in kernel order. Unfiltered on
    /// purpose — see [`NetKind`].
    #[serde(default)]
    pub net: Vec<NetDto>,
    /// Every mounted filesystem that has a capacity to report, largest first.
    ///
    /// Largest rather than fullest, because the list is capped and the two
    /// orders disagree about what to drop: a read-only image mount — every
    /// installed snap is one — is 100% full by construction, so fullest-first
    /// would fill the cap with squashfs before naming a single real disk.
    ///
    /// Unfiltered in the same sense `net` is — see [`DiskKind`] — with one
    /// exception: the pseudo filesystems (`proc`, `sysfs`, `cgroup`, `devpts`)
    /// are not here at all. That is not the daemon holding an opinion about
    /// which disk matters; they have no capacity, so there is no number to
    /// publish and nothing a client could decide differently.
    ///
    /// Carries no history, unlike every other series on this struct. A disk
    /// does not visibly move across the sampler's two-and-a-half-minute window,
    /// so the trace would be a flat line — and `keeps_history` records what
    /// per-entry history costs when the entries are numerous.
    #[serde(default)]
    pub disks: Vec<DiskDto>,
    pub containers: Vec<ContainerDto>,
    /// Containers grouped into compose stacks (running-first-ish; the client
    /// sorts "mine" ahead using each stack's `workdir` vs the workspace cwd).
    pub stacks: Vec<StackDto>,
}

/// What is known about one agent CLI's account, for the USAGE page.
///
/// The states are ordered by how much the daemon could find out, and every one
/// of them is a thing worth drawing: a client that only rendered [`Self::Metered`]
/// would show an empty page on a machine where nothing publishes a limit, which
/// is most machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum CliState {
    /// This CLI's windows have a denominator, so they draw as a proportion.
    ///
    /// The ceiling came from one of two places, and [`CliUsageDto::source`] says
    /// which: the provider published it ([`UsageSource::Published`]) or the user
    /// declared it ([`UsageSource::Declared`]). The distinction matters to a
    /// reader and not to a renderer, which is why it rides on `source` rather
    /// than splitting this state in two.
    Metered,
    /// Installed and signed in, and the daemon can count what has been spent in
    /// each window — but nothing published what the ceiling is, so the windows
    /// carry a total with `of: None`.
    ///
    /// **This is the honest default for a CLI that caches no limits.** Most do
    /// not; `claude` is the exception, and only once it has run at least once
    /// and written them. See `docs/processes.md`.
    Counted,
    /// Installed, and probably signed in, but butai does not know how to read
    /// this CLI's usage — it keeps no transcript butai can parse, or keeps one
    /// in a shape nothing here understands.
    ///
    /// Distinct from [`Self::NoAccount`], and the distinction is the point: one
    /// says *there is nothing to meter*, the other says *there is something and
    /// we cannot see it*. Collapsing them would tell a user their subscription
    /// CLI has no limits.
    Unknown,
    /// Installed, but there is no account to meter — it runs on an API key you
    /// supply and the provider bills you directly.
    NoAccount,
    /// Configured as an `[[agents]]` entry, but its command is not on `PATH`.
    /// Still listed: "you have not installed this one" is an answer, and a row
    /// that vanishes is indistinguishable from a row that never existed.
    Absent,
}

/// What a window's numbers are counted in.
///
/// The unit travels with the number rather than being baked into a percentage,
/// because the providers do not agree: one meters a subscription in opaque
/// "usage", one publishes a request-per-day quota. A client formats
/// `118 / 1,000 requests` from this; a percentage on the wire would have thrown
/// the fact away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum UsageUnit {
    Tokens,
    Requests,
    Percent,
}

/// One accounting window — "the last five hours", "the last seven days".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct UsageWindowDto {
    /// Human label, written by the daemon because it knows what it counted.
    pub label: String,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub used: u64,
    /// The ceiling, when one is known. `None` means *nothing published a limit*
    /// — draw the total, not a bar. A client that renders `used/of` with a
    /// zero or invented denominator is reporting a limit nobody stated.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub of: Option<u64>,
    pub unit: UsageUnit,
    /// Unix epoch millis at which this window empties, when that is knowable.
    /// A rolling window has no such instant and leaves it `None`.
    ///
    /// An absolute instant rather than a countdown, for the reason
    /// [`AgentDto::started_ms`] is one: only the client knows what time it is
    /// when it draws.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub resets_ms: Option<u64>,
}

/// Where a CLI's numbers came from. Drawn on the page, because a limit whose
/// provenance is unclear is one nobody can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    /// The provider's own numbers, out of a limit the CLI cached on disk. The
    /// only source here that did not come from arithmetic on this machine, and
    /// the only one whose windows have a true reset instant rather than a
    /// rolling boundary.
    Published,
    /// Summed out of the CLI's own transcripts on this machine.
    Transcripts,
    /// A budget the user wrote in `config.toml`.
    Declared,
    /// Nothing was countable.
    None,
}

/// One CLI's row on the USAGE page.
///
/// **Account limits, not spend.** The question is which account stops you first
/// and when it comes back, which is a different question from what you have
/// spent and is answered by different numbers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct CliUsageDto {
    /// The `[[agents]]` name — what `butai agent spawn <name>` takes.
    pub name: String,
    /// The binary the entry launches, resolved or not.
    pub command: String,
    pub state: CliState,
    /// `<command> --version`, trimmed; `None` when absent or the probe failed.
    pub version: Option<String>,
    /// The signed-in account, when the CLI records one somewhere that is not a
    /// secret. Never read from a credential store — see `docs/processes.md`.
    pub account: Option<String>,
    /// The plan, as the CLI's own config names it.
    pub plan: Option<String>,
    pub windows: Vec<UsageWindowDto>,
    /// Panes running on this account *right now*, across every workspace this
    /// daemon serves. Ids rather than a count, so a client can offer to jump to
    /// them — the count is `panes.len()`.
    pub panes: Vec<PaneId>,
    pub source: UsageSource,
    /// One line for the states that have nothing to meter: why not, and what
    /// would change it.
    pub note: Option<String>,
}

/// Every configured CLI's account standing (`GET /v1/usage`).
///
/// Machine-scoped rather than workspace-scoped: an account limit is not a
/// property of a project, and the same limit is being burned by every workspace
/// this daemon serves.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct UsageDto {
    pub clis: Vec<CliUsageDto>,
    /// Unix epoch millis of the sample. A stale limit is worse than no limit,
    /// so the age is on the wire and the page draws it.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub sampled_ms: u64,
}

/// A single changed file in the CHANGES rail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct FileChange {
    pub path: String,
    /// git status code (`M`, `A`, `?`, `D`, ...).
    pub code: String,
    pub added: usize,
    pub deleted: usize,
}

/// A recent commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct CommitDto {
    pub id: String,
    pub summary: String,
}

/// A file with unresolved merge conflicts.
///
/// Separate from [`FileChange`] because the useful fields differ: a conflicted
/// file has no meaningful diffstat until it is resolved, and what you want to
/// know instead is which sides of the merge still exist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct ConflictFile {
    pub path: String,
    /// Whether the merge base, "ours" and "theirs" stages are present. A
    /// delete/modify conflict is missing one of the last two; a both-added
    /// conflict is missing the base.
    pub base: bool,
    pub ours: bool,
    pub theirs: bool,
}

/// What the repository is in the middle of, from `git2::Repository::state()`.
///
/// `Rebase` collapses libgit2's three rebase flavours — `git rebase --continue`
/// covers all of them, so a client has no use for the distinction. `Unknown`
/// exists so a state this version does not model renders as "something is in
/// progress, use a shell" rather than silently as clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum RepoState {
    #[default]
    Clean,
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
    Unknown,
}

impl RepoState {
    /// Whether a sequence is in progress, so `continue`/`abort` apply.
    pub fn in_progress(self) -> bool {
        self != RepoState::Clean
    }

    /// The label a client shows while this is running.
    pub fn label(self) -> &'static str {
        match self {
            RepoState::Clean => "",
            RepoState::Merge => "MERGING",
            RepoState::Rebase => "REBASING",
            RepoState::CherryPick => "CHERRY-PICKING",
            RepoState::Revert => "REVERTING",
            RepoState::Bisect => "BISECTING",
            RepoState::Unknown => "IN PROGRESS",
        }
    }
}

/// The git CHANGES rail for a workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct ChangesDto {
    pub branch: String,
    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
    pub recent_commits: Vec<CommitDto>,
    /// Files with unresolved conflicts.
    ///
    /// A conflicted file appears here and **not** in `unstaged`, so a client
    /// that ignores this field cannot stage half a merge by accident. Added
    /// after 0.5; clients built against an older daemon see an empty list.
    #[serde(default)]
    pub conflicted: Vec<ConflictFile>,
    /// The tracking branch (`origin/main`), or `null` when the branch has no
    /// upstream configured.
    #[serde(default)]
    pub upstream: Option<String>,
    /// Commits on this branch that the upstream does not have, and vice versa.
    /// Both zero when there is no upstream. Capped at [`AHEAD_BEHIND_CAP`].
    #[serde(default)]
    pub ahead: usize,
    #[serde(default)]
    pub behind: usize,
    /// What the repository is in the middle of, if anything.
    #[serde(default)]
    pub state: RepoState,
    /// HEAD points at a commit rather than a branch, so `branch` is a rev.
    #[serde(default)]
    pub detached: bool,
}

/// One `git` invocation the daemon will run on the user's behalf.
///
/// Everything that writes the repository beyond the index goes through this one
/// type: network access, hooks, credential helpers, signing and sequencer state
/// all live in the user's git config, and only the real `git` binary honours
/// them (libgit2 is built here without any network transport at all).
///
/// It is data, not a command line. Turning it into argv is
/// `butai_server::git_op::argv`, a pure function, so every user-supplied string
/// is validated in one place that can be tested without spawning a process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GitOp {
    Fetch {
        #[serde(default)]
        remote: Option<String>,
        #[serde(default)]
        all: bool,
        #[serde(default)]
        prune: bool,
    },
    Pull {
        #[serde(default)]
        remote: Option<String>,
        #[serde(default)]
        branch: Option<String>,
        #[serde(default)]
        rebase: bool,
        #[serde(default)]
        ff_only: bool,
    },
    Push {
        #[serde(default)]
        remote: Option<String>,
        #[serde(default)]
        branch: Option<String>,
        #[serde(default)]
        set_upstream: bool,
        /// `--force-with-lease`, never a bare `--force`: it refuses when the
        /// remote moved since you last saw it, which is the case a plain force
        /// silently destroys.
        #[serde(default)]
        force_with_lease: bool,
    },
    /// Set aside the working tree. `include_untracked` matters more than it
    /// looks: without it a stash leaves new files behind, which is the usual
    /// reason "I stashed and the branch was still dirty".
    Stash {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        include_untracked: bool,
    },
    /// Restore a stash entry. `pop` drops it afterwards.
    StashApply {
        #[serde(default)]
        index: usize,
        #[serde(default)]
        pop: bool,
    },
    StashDrop {
        #[serde(default)]
        index: usize,
    },
    /// Replace the last commit. Without a message, git reuses the old one.
    Amend {
        #[serde(default)]
        message: Option<String>,
    },
    /// Move HEAD. `Hard` also throws away the working tree, which is why it is
    /// the one reset a client should confirm.
    Reset {
        #[serde(default)]
        rev: Option<String>,
        #[serde(default)]
        mode: ResetMode,
    },
    /// Undo a commit by making a new one, or copy one onto this branch. Both
    /// can conflict, and both leave the sequencer state that `Sequence` drives.
    Revert {
        rev: String,
    },
    CherryPick {
        rev: String,
    },
    /// Bring another branch's commits in.
    Merge {
        branch: String,
        #[serde(default)]
        no_ff: bool,
    },
    Rebase {
        onto: String,
    },
    /// Drive whatever sequence is in progress. One pair for merge, rebase,
    /// cherry-pick and revert alike: they share git's sequencer state, and the
    /// daemon picks the right subcommand from [`RepoState`].
    Sequence {
        action: SequenceAction,
    },
    /// Create or delete a tag.
    Tag {
        name: String,
        #[serde(default)]
        rev: Option<String>,
        #[serde(default)]
        message: Option<String>,
    },
    TagDelete {
        name: String,
    },
    /// Add a second checkout of this repository at `path`.
    ///
    /// A write that runs hooks and checks out a whole tree, so it goes through
    /// the runner like every other one. `new_branch` creates `branch` rather
    /// than requiring it to exist, which is the common case — a worktree is
    /// usually made *to* start something.
    WorktreeAdd {
        path: String,
        #[serde(default)]
        branch: Option<String>,
        #[serde(default)]
        new_branch: bool,
    },
    /// Remove a worktree. `force` is needed when it has uncommitted changes or
    /// is locked; without it git refuses, which is the behaviour worth keeping.
    WorktreeRemove {
        path: String,
        #[serde(default)]
        force: bool,
    },
    /// Forget worktrees whose directories are gone.
    WorktreePrune,
    /// Configure a new remote.
    ///
    /// The one operation here that takes a **URL** rather than a name. A URL is
    /// an arbitrary-code-execution vector (`ext::sh -c …` runs a shell), so
    /// `butai_server::git_op::valid_remote_url` allows a fixed set of transports
    /// and refuses everything else.
    RemoteAdd {
        name: String,
        url: String,
    },
    RemoteRemove {
        name: String,
    },
}

/// One checkout of a repository (`GET .../git/worktrees`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct WorktreeDto {
    pub path: String,
    /// Short branch name, absent when detached.
    #[serde(default)]
    pub branch: Option<String>,
    pub head: String,
    /// The repository's main worktree. Cannot be removed.
    pub is_main: bool,
    #[serde(default)]
    pub detached: bool,
    #[serde(default)]
    pub locked: bool,
    /// Its directory is gone; `prune` clears the record.
    #[serde(default)]
    pub prunable: bool,
    /// The id of the butai workspace already open on this path, when there is
    /// one — so a client can offer "go there" rather than "open it again".
    #[serde(default)]
    pub workspace: Option<SessionId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum ResetMode {
    /// Move HEAD, keep the index and the worktree.
    Soft,
    /// Move HEAD and the index, keep the worktree. git's own default.
    #[default]
    Mixed,
    /// Move all three — discards uncommitted work irrecoverably.
    Hard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum SequenceAction {
    Continue,
    Abort,
    /// Rebase and cherry-pick only; a merge has nothing to skip.
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum ResolveSide {
    /// Keep this branch's version.
    Ours,
    /// Keep the incoming version.
    Theirs,
    /// The file was edited by hand and is settled — this is `git add`.
    Resolved,
}

impl GitOp {
    /// The short name a client shows and the API reports (`"fetch"`).
    pub fn kind(&self) -> &'static str {
        match self {
            GitOp::Fetch { .. } => "fetch",
            GitOp::Pull { .. } => "pull",
            GitOp::Push { .. } => "push",
            GitOp::Stash { .. } => "stash",
            GitOp::StashApply { pop, .. } => {
                if *pop {
                    "stash pop"
                } else {
                    "stash apply"
                }
            }
            GitOp::StashDrop { .. } => "stash drop",
            GitOp::Amend { .. } => "amend",
            GitOp::Reset { .. } => "reset",
            GitOp::Revert { .. } => "revert",
            GitOp::CherryPick { .. } => "cherry-pick",
            GitOp::Merge { .. } => "merge",
            GitOp::Rebase { .. } => "rebase",
            GitOp::Sequence { action } => match action {
                SequenceAction::Continue => "continue",
                SequenceAction::Abort => "abort",
                SequenceAction::Skip => "skip",
            },
            GitOp::Tag { .. } => "tag",
            GitOp::TagDelete { .. } => "tag delete",
            GitOp::WorktreeAdd { .. } => "worktree add",
            GitOp::WorktreeRemove { .. } => "worktree remove",
            GitOp::WorktreePrune => "worktree prune",
            GitOp::RemoteAdd { .. } => "remote add",
            GitOp::RemoteRemove { .. } => "remote remove",
        }
    }
}

/// Where a patch is applied by `POST .../git/apply`.
///
/// This is what partial staging is: the client sends back a patch containing
/// only the hunks or lines it chose, and says which of the two copies of the
/// file it should land on. `Index` stages without touching the worktree;
/// `Worktree` (with `reverse`) is how a hunk is discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum ApplyTarget {
    /// The index only — the file on disk is left exactly as it is.
    #[default]
    Index,
    /// The working tree.
    Worktree,
}

/// One entry from `git stash list`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct StashDto {
    pub index: usize,
    /// The branch it was taken from, when git recorded one.
    pub branch: String,
    pub message: String,
}

/// What kind of ref points at a commit.
///
/// Told apart by `--decorate=full`'s prefixes rather than guessed from the
/// shorthand, because a tag and a branch are allowed to share a name and a
/// client that guessed would draw one of them wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    /// `HEAD` itself. Emitted *beside* the branch it points at, so a client can
    /// mark the checked-out tip without re-deriving it.
    Head,
    Branch,
    /// A remote-tracking branch, named `origin/main`.
    Remote,
    Tag,
}

/// One ref pointing at a commit, for the graph's branch chips.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct RefDecoration {
    /// Shorthand: `main`, `origin/main`, `v0.8.0`, or `HEAD`.
    pub name: String,
    pub kind: RefKind,
}

/// One commit from `git log`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct LogEntryDto {
    pub id: String,
    pub summary: String,
    pub author: String,
    /// Author date, ISO-8601. A string rather than a number because the daemon
    /// never does arithmetic on it and every client formats it differently.
    pub date: String,
    /// Parent commit ids, first parent first.
    ///
    /// **The graph's edges.** Without these a history is a list, not a tree, and
    /// no client can recover them: the relation lives in the object database,
    /// not in the page of commits it was sent. Empty for a root commit; two or
    /// more entries mean a merge. Added after 0.8; an older daemon sends
    /// nothing and a client falls back to drawing the page linearly.
    #[serde(default)]
    pub parents: Vec<String>,
    /// Branches, tags and HEAD pointing at this commit. Usually empty — only
    /// tips carry decoration.
    #[serde(default)]
    pub refs: Vec<RefDecoration>,
}

/// A page of history.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct LogDto {
    pub commits: Vec<LogEntryDto>,
    /// Whether another page exists after this one.
    pub more: bool,
}

/// The configured remotes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct RemoteDto {
    pub name: String,
    pub url: String,
}

/// The three sides of one conflicted file, for a client that wants to show a
/// real three-way view. Absent stages come back empty — a delete/modify
/// conflict has no `ours` or no `theirs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct ConflictDto {
    pub path: String,
    pub base: String,
    pub ours: String,
    pub theirs: String,
}

/// A git operation in flight, or the last one to finish.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct GitOpDto {
    /// The workspace it belongs to. Required, not decorative: `ApiEvent` is
    /// fanned out to every SSE subscriber with no filtering, so an event that
    /// did not name its workspace would be unattributable.
    pub ws: SessionId,
    /// Monotonic per-daemon id, so a client can tell a new op from an update to
    /// the one it already knows about.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub seq: u64,
    pub kind: String,
    pub running: bool,
    /// git's most recent progress line, or empty. Human text for display only —
    /// never parse it; ask `/changes` what actually happened.
    #[serde(default)]
    pub progress: String,
    /// `None` while running.
    #[serde(default)]
    pub ok: Option<bool>,
    /// The closing line of git's output, or the reason it failed.
    #[serde(default)]
    pub summary: String,
}

/// Ahead/behind counts are a revwalk bounded by how far the branches have
/// diverged, which is cheap in the normal case and linear in the pathological
/// one. Past this the exact number stops being decision-relevant, so the walk
/// stops and the count is reported as the cap.
pub const AHEAD_BEHIND_CAP: usize = 999;

/// A workspace at a glance (list view).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct WorkspaceSummary {
    pub id: SessionId,
    pub name: String,
    pub cwd: String,
    pub agents: usize,
    /// Agents currently blocked on the user (`AgentState::Waiting`). Lets a list
    /// view surface a "needs you" badge without fetching each workspace's detail.
    #[serde(default)]
    pub waiting: usize,
    /// Agents actively working (`AgentState::Working`).
    #[serde(default)]
    pub working: usize,
    /// Agents that finished their turn (`AgentState::Finished`) — your move, but
    /// not blocking. Counted separately from `waiting` so a badge can say "needs
    /// you now" and "your move" with the right urgency.
    #[serde(default)]
    pub finished: usize,
    /// Live agents with a decision prompt on screen ([`AgentDto::question`]) — a
    /// subset of `waiting`, the rest being bells.
    #[serde(default)]
    pub questions: usize,
    /// Agents whose process is gone but whose row is still around
    /// (`AgentState::Exited`). Excluded from every count above.
    #[serde(default)]
    pub exited: usize,
    /// Agents with [`AgentDto::unread`] set — finished or exited, and not looked
    /// at since. A subset of `finished + exited`.
    ///
    /// Counted here so a list view can say "two turns landed here while you were
    /// gone" from the `workspaces` event alone, without fetching each
    /// workspace's detail to look at the rows.
    #[serde(default)]
    pub unread: usize,
    pub processes: usize,
    pub changes: usize,
    /// Files with unresolved conflicts. The `workspaces` event is all a list
    /// view gets, so "this tab is mid-merge" has to be visible from here.
    #[serde(default)]
    pub conflicts: usize,
    /// What the workspace's repository is in the middle of, if anything.
    #[serde(default)]
    pub repo_state: RepoState,
    pub attached_clients: usize,
}

/// A workspace with its full rail contents (detail view).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct WorkspaceDetail {
    pub id: SessionId,
    pub name: String,
    pub cwd: String,
    pub agents: Vec<AgentDto>,
    pub processes: Vec<ProcessDto>,
    /// `null` when the workspace is not inside a git repository.
    pub changes: Option<ChangesDto>,
    /// Pane currently on the stage, if any — the web client's default pane to
    /// stream. `null` when nothing is staged.
    pub stage: Option<PaneId>,
}

/// One entry in a directory listing (`GET /v1/workspaces/{id}/tree`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct TreeEntry {
    pub name: String,
    /// Path relative to the workspace cwd (POSIX separators).
    pub path: String,
    pub is_dir: bool,
    /// True when this file (or, for a directory, something under it) has a git
    /// change — mirrors the tree's yellow `●` marker.
    pub changed: bool,
    /// File size in bytes (0 for directories). Lets a client decide whether a
    /// file is too large to preview and should be downloaded instead.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub size: u64,
}

/// A directory listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct TreeDto {
    /// The listed directory, relative to cwd (`""` = the workspace root).
    pub path: String,
    pub entries: Vec<TreeEntry>,
}

/// Which rows a tree listing is *about* — `?filter=` on `GET …/tree`.
///
/// ## Why the daemon knows this at all
///
/// It used to not, and that was the better-looking answer: one route, and each
/// client decided what its pages showed. The seam it left is that
/// [`TreeEntry::changed`] on a directory is computed over the *whole* change
/// set while the rows are filtered afterwards, so a directory kept a marker
/// earned by a file the reader would never be shown. Following the marker down
/// the DOCS rail landed on an empty listing, every time, in every client — the
/// terminal and the web page reproduced it identically, because they share the
/// bug rather than either one causing it.
///
/// A filter is only sound where the marker is decided, so it moved here. The
/// rule this trades away is real; what it buys is that a marker cannot promise
/// a row the same request then withholds.
///
/// [`All`](Self::All) is the default and is byte-identical to the listing this
/// route has always returned — an embedder that never sends `?filter=` sees no
/// change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TreeFilter {
    /// Every entry on disk but `.git`. What the route has always answered.
    #[default]
    All,
    /// A project's own writing: markdown and READMEs, and every directory but
    /// the two nobody means. Both the rows *and* the markers are decided by it.
    Docs,
}

impl TreeFilter {
    /// Parse the `?filter=` value. `None` for anything else, so a typo is a
    /// 400 rather than a silently unfiltered listing.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "" | "all" => Some(Self::All),
            "docs" => Some(Self::Docs),
            _ => None,
        }
    }

    /// Does this filter keep the entry?
    pub fn keeps(self, name: &str, is_dir: bool) -> bool {
        match self {
            Self::All => true,
            Self::Docs => is_doc(name, is_dir),
        }
    }
}

/// Is this entry a project's *writing* rather than the code it is about?
///
/// The one definition. It was written twice — once in the terminal client and
/// once in the web client's `docs.ts` — back when filtering was each client's
/// own business; the daemon now decides the markers under the same rule, and
/// two copies of a predicate that has to agree with a marker is two things that
/// drift.
pub fn is_doc(name: &str, is_dir: bool) -> bool {
    if is_dir {
        return !matches!(name, "target" | "node_modules");
    }
    let lower = name.to_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown") || lower.starts_with("readme")
}

/// A file's text content (`GET /v1/workspaces/{id}/file`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct FileDto {
    pub path: String,
    pub text: String,
    /// True when the file was larger than the read cap and `text` is a prefix.
    pub truncated: bool,
}

/// Which band of a pane to read (`GET /v1/workspaces/{id}/panes/{pane}/output`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum OutputSource {
    /// Recent history ending at the live screen — the default, and what you
    /// want to know what a sibling has been doing.
    Scrollback,
    /// Exactly the visible viewport, blank rows and all.
    Screen,
    /// The band the agent-state detector scans. Reading it answers "why does
    /// butai think this agent is working?", which is otherwise unanswerable
    /// from outside the daemon.
    Footer,
}

/// How to render the rows of a pane read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    /// Plain text, no escape sequences. What a program reading a sibling pane
    /// almost always wants.
    Text,
    /// SGR-formatted, for a client that wants to reproduce the colors.
    Ansi,
}

/// A pane's rendered output as text
/// (`GET /v1/workspaces/{id}/panes/{pane}/output`).
///
/// This is the server-side answer to reading a pane: the daemon already runs
/// the VT emulator, so it resolves wide graphemes and trailing blanks once,
/// here, instead of every client reimplementing the cell-grid rules that the
/// framed `frame` messages require.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct PaneOutputDto {
    pub pane: PaneId,
    /// Pane size at the time of the read — the rows are wrapped to `cols`.
    pub cols: u16,
    pub rows: u16,
    pub source: OutputSource,
    pub format: OutputFormat,
    /// Oldest first, ending at the bottom of the live screen. Right-trimmed.
    pub lines: Vec<String>,
    /// True when older lines exist that this read did not return — either
    /// because `lines=` asked for fewer, or because they are out of the
    /// emulator's reach. Never assume `lines` is the whole history.
    pub more: bool,
    /// True while a full-screen application (vim, htop) owns the pane. Such a
    /// pane has no scrollback at all, so a `scrollback` read of one can only
    /// ever return the visible screen.
    pub alt_screen: bool,
    /// Cursor (col, row), or `null` when hidden.
    pub cursor: Option<(u16, u16)>,
    /// Exit code once the pane's process has exited; `null` while running.
    pub exited: Option<u32>,
}

/// A unified diff for one changed file (`GET /v1/workspaces/{id}/diff`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct DiffDto {
    pub path: String,
    /// True for index-vs-HEAD (staged), false for worktree-vs-index (unstaged).
    pub staged: bool,
    /// Raw unified-diff patch text (as `git diff` prints it).
    pub patch: String,
}

/// One entry in a filesystem directory listing (`GET /v1/fs`). Unlike the
/// workspace tree, this browses anywhere on the host — it backs the "create
/// workspace in a folder" picker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct BrowseEntry {
    pub name: String,
    /// Absolute path of the entry.
    pub path: String,
    pub is_dir: bool,
}

/// A host directory listing (`GET /v1/fs?path=`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct BrowseDto {
    /// The absolute directory that was listed.
    pub path: String,
    /// The parent directory, or `null` at the filesystem root.
    pub parent: Option<String>,
    pub entries: Vec<BrowseEntry>,
}

/// One thing a search found: a file, or a line inside one.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct SearchHitDto {
    /// Workspace-relative path.
    pub path: String,
    /// 1-based line number for a content match; absent for a filename match.
    #[serde(default)]
    pub line: Option<u32>,
    /// The matching line, trimmed. Empty for a filename match.
    #[serde(default)]
    pub preview: String,
}

/// What a workspace search found (`GET .../search?q=`).
///
/// Filename matches first (fuzzy), then content matches, both capped — this is
/// a jump-to, not a report, and a client that wanted every occurrence would be
/// better off running `grep` in a pane.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct SearchDto {
    pub query: String,
    pub hits: Vec<SearchHitDto>,
}

/// One branch, with everything a list of branches wants to show.
///
/// Deliberately carries no worktree field. A client that draws "checked out
/// somewhere else" already has `GET .../git/worktrees`, whose entries name
/// their branch — so the answer is a cross-reference the client can do, not a
/// second revwalk the daemon has to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct BranchDto {
    /// Shorthand: `main`, or `origin/main` when `remote`.
    pub name: String,
    /// A remote-tracking branch rather than a local one.
    pub remote: bool,
    /// The branch this one tracks (`origin/main`), when it has an upstream.
    /// Always `null` for a remote branch.
    pub upstream: Option<String>,
    /// Commits this branch has that its upstream does not, and vice versa.
    /// Both zero without an upstream. Capped at [`AHEAD_BEHIND_CAP`], the same
    /// bound [`ChangesDto`] uses for the current branch.
    pub ahead: usize,
    pub behind: usize,
    /// The tip commit's full id, so a client can point the graph at it without
    /// resolving the name again.
    ///
    /// Deliberately the id alone. A client that wants the tip's summary or date
    /// asks `GET .../git/log?rev=<name>&limit=1`, which already returns both —
    /// and returns the date in the one spelling the wire uses, which a second
    /// producer here would eventually contradict.
    pub tip: String,
}

/// The branches of a workspace's git repository (`GET .../branches`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct BranchesDto {
    /// The checked-out branch (shorthand), or `null` on a detached HEAD.
    pub current: Option<String>,
    /// Local branch names, current first.
    ///
    /// Kept beside the richer [`BranchesDto::entries`] rather than replaced by
    /// it: this is what the branch picker and every published client already
    /// read, and a list of names is all most of them want.
    pub branches: Vec<String>,
    /// Local **and** remote-tracking branches, with upstream and ahead/behind.
    /// Local first, each group sorted, the current branch at the front. Added
    /// after 0.8; an older daemon sends nothing and `branches` still answers.
    #[serde(default)]
    pub entries: Vec<BranchDto>,
}

/// Why an agent notification fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    /// An agent asked a question / needs input mid-task (`-> waiting`). Urgent.
    Waiting,
    /// An agent finished its turn and settled at the prompt (`working ->
    /// finished`). Your move, but not blocking.
    Finished,
    /// An agent process exited.
    Exited,
}

/// One agent state-transition worth telling the user about. The daemon computes
/// these *once* (single source of truth) and every client drains the same feed,
/// so all apps agree and no client re-derives transitions from raw snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct NotificationDto {
    /// Monotonic sequence number (per daemon run). Clients pass the highest they
    /// have seen back as `?since=` to get only newer items.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub seq: u64,
    /// Unix epoch millis when the transition was recorded.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub at_ms: u64,
    pub ws: SessionId,
    pub ws_name: String,
    pub pane: PaneId,
    /// The agent's display title at the moment it fired.
    pub title: String,
    pub kind: NotificationKind,
    /// Exit code when `kind == exited`.
    pub exited: Option<u32>,
}

/// Reply to `GET /v1/notifications?since=N`: the items after `since`, plus the
/// current head so a client can advance its cursor even when `items` is empty.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct NotificationsDto {
    /// Highest seq the daemon has assigned (0 if none yet). Advance your cursor
    /// to this after processing, so a fresh client doesn't replay old items.
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub head: u64,
    pub items: Vec<NotificationDto>,
}

/// A pushed event on the `GET /v1/events` stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
#[serde(tag = "event", content = "data", rename_all = "snake_case")]
pub enum ApiEvent {
    /// Fresh machine telemetry (every sampler tick).
    System(SysDto),
    /// Workspace/agent/process state changed — a fresh snapshot list.
    Workspaces(Vec<WorkspaceSummary>),
    /// One workspace's **full rail contents**, pushed when they change.
    ///
    /// [`Workspaces`](Self::Workspaces) carries counts, which is enough to badge
    /// a tab but not to draw the AGENTS / PROCESSES / CHANGES rails. Without
    /// this a client had to poll `/v1/workspaces/{id}` on a 1–2s timer, which
    /// every shipping client does today; a client that renders those rails as
    /// its own UI cannot be a second or two behind the pane it is drawing beside.
    ///
    /// Emitted on the frame clock rather than the sampler tick, and only when
    /// the detail actually differs from the last one sent — pane output marks
    /// the workbench dirty far more often than it changes a rail, and pushing a
    /// snapshot per frame would flood a subscriber on a slow link for no gain.
    ///
    /// Added after 0.6. A client that does not know this tag must ignore it, as
    /// [`docs/building-a-client.md`] requires of any unknown event.
    WorkspaceDetail(WorkspaceDetail),
    /// An agent transition worth notifying the user about (finished / exited).
    Notification(NotificationDto),
    /// A git operation started, made progress, or finished. Emitted on start
    /// and finish, and at most a few times a second in between — `git push`
    /// prints a line per percent, and relaying every one of them would flood
    /// each subscriber for no gain.
    ///
    /// Added after 0.5. A client that does not know this tag must ignore it,
    /// as [`docs/building-a-client.md`] requires of any unknown event.
    GitOp(GitOpDto),
    /// A `butai` started over ssh inside a pane announced the machine it is on.
    ///
    /// Added after 0.6. A client that does not know this tag must ignore it,
    /// as [`docs/building-a-client.md`] requires of any unknown event.
    RemoteAnnounce(RemoteAnnounceDto),
}

/// Where a far machine is, reported when a `butai` run over ssh inside a pane
/// announces itself.
///
/// **The daemon reports; it does not act.** It is the only party that can
/// *detect* this — it parses every byte a pane writes — but connecting a second
/// machine is a client decision: whose tab bar the far projects appear in is a
/// property of the client, and one daemon dialling another to answer it is the
/// relay this refactor removes. So the detection stays here and the connecting
/// moves out.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export, export_to = "protocol.ts"))]
pub struct RemoteAnnounceDto {
    /// The pane the announcement came out of.
    pub pane: PaneId,
    /// `user@host` as the far side derived it from `$SSH_CONNECTION`. A
    /// fallback only: behind NAT it is an address that means nothing here.
    pub hint: String,
    /// The far daemon's socket path, for forwarding (`ssh -L`).
    pub socket: String,
    /// The ssh destination recovered from the pane's own foreground process —
    /// it is running the `ssh` that got there, so these arguments reach the
    /// same host the same way, through the same jump hosts, with the same key.
    /// Empty when the process could not be read, in which case `hint` is all
    /// there is.
    #[serde(default)]
    pub ssh_target: String,
    #[serde(default)]
    pub ssh_args: Vec<String>,
}

/// A request from the HTTP handler into the core actor.
#[derive(Debug, Clone)]
pub enum ApiRequest {
    // -- queries --
    ListWorkspaces,
    Workspace(SessionId),
    Agents(SessionId),
    Processes(SessionId),
    Changes(SessionId),
    Tree {
        ws: SessionId,
        path: String,
        /// Which rows the listing is about — and, because the two must agree,
        /// which rows its `changed` markers are computed over.
        filter: TreeFilter,
    },
    File {
        ws: SessionId,
        path: String,
    },
    Diff {
        ws: SessionId,
        path: String,
        staged: bool,
    },
    /// Fuzzy filename search plus a content grep, rooted at the workspace.
    ///
    /// Server-side because the files are: a workspace on another machine is
    /// reachable only through its own daemon, so "search this project" cannot
    /// be client work however much of the rest of the interface is.
    Search {
        ws: SessionId,
        query: String,
    },
    /// Whole-commit diff (`git show`) for a revision — used by the web client's
    /// "recent commits" list. Returns a [`DiffDto`] whose `path` is the rev.
    Show {
        ws: SessionId,
        id: String,
    },
    /// The git operation running (or last finished) for a workspace's repo.
    GitOpStatus(SessionId),
    /// A page of history. `path` narrows it to one file's history.
    GitLog {
        ws: SessionId,
        limit: usize,
        skip: usize,
        rev: Option<String>,
        path: Option<String>,
        /// Walk every ref, not just HEAD — what a commit *graph* needs, since a
        /// walk from HEAD alone can only ever draw one branch. Refused
        /// alongside `rev`: the two name different walks, and silently
        /// preferring one would make a client's bug look like a daemon's.
        all: bool,
    },
    GitStashes(SessionId),
    GitRemotes(SessionId),
    GitTags(SessionId),
    /// Every checkout of the workspace's repository.
    GitWorktrees(SessionId),
    /// The three sides of one conflicted file.
    GitConflict {
        ws: SessionId,
        path: String,
    },
    System,
    AgentTypes,
    /// Every configured CLI's account standing. Machine-scoped, so it takes no
    /// workspace id.
    Usage,
    /// List local git branches for a workspace.
    Branches(SessionId),
    /// Browse a host directory (for the folder picker). `None` = a sensible
    /// default (home directory).
    BrowseFs {
        path: Option<String>,
    },
    /// Create a folder named `name` under `path` (`None` = the browse default
    /// dir) and return the new directory's listing, so the folder picker can
    /// step straight into it in one round-trip.
    MakeDir {
        path: Option<String>,
        name: String,
    },
    /// Drain the agent-notification feed: items with `seq > since`.
    Notifications {
        since: u64,
    },
    /// Read a workspace file's raw bytes (`GET .../download`).
    Download {
        ws: SessionId,
        path: String,
    },
    /// Read a pane's rendered output as text. A *query*: it must not resize the
    /// pane or clear its bell, which is what makes it safe for a scripted
    /// reader in a way a transient framed attach is not.
    PaneOutput {
        ws: SessionId,
        pane: PaneId,
        /// Maximum rows to return, counting back from the live screen.
        lines: usize,
        source: OutputSource,
        format: OutputFormat,
    },
    // -- actions --
    NewWorkspace {
        name: Option<String>,
        layout: Option<String>,
        path: Option<String>,
    },
    KillWorkspace(SessionId),
    /// Start an agent. `background` leaves the stage and focus where they are,
    /// so an agent spawning a helper does not yank the view out from under
    /// whoever is watching; the default keeps the existing take-the-stage
    /// behavior every GUI client relies on.
    SpawnAgent {
        ws: SessionId,
        name: String,
        background: bool,
    },
    NewProcess {
        ws: SessionId,
        name: String,
        command: String,
    },
    RestartProcess {
        ws: SessionId,
        pane: PaneId,
    },
    KillPane {
        ws: SessionId,
        pane: PaneId,
    },
    /// Inject a single input event (a keystroke or paste) into a pane's PTY
    /// without attaching a streaming client — so a mobile/list UI can approve or
    /// interrupt an agent (Enter / Esc) without opening the terminal and without
    /// resizing the pane the way a transient framed attach would.
    PaneInput {
        ws: SessionId,
        pane: PaneId,
        input: InputEvent,
    },
    /// Mark a pane as looked-at: clears its pending bell, so an agent that rang
    /// the bell stops reporting [`AgentState::Waiting`]. The TUI does this
    /// implicitly when a pane takes the stage; this is the same gesture for a
    /// client that dismisses from a list without opening the terminal.
    AckPane {
        ws: SessionId,
        pane: PaneId,
    },
    StageFile {
        ws: SessionId,
        path: String,
    },
    UnstageFile {
        ws: SessionId,
        path: String,
    },
    /// Throw away one file's worktree changes (`git restore <path>`, or delete
    /// for an untracked file). Destructive: the caller is expected to have
    /// confirmed with the user. Only unstaged files qualify.
    DiscardFile {
        ws: SessionId,
        path: String,
    },
    Commit {
        ws: SessionId,
        message: String,
    },
    /// Stage every changed file, then commit with `message` — the one-round-trip
    /// analog of the CHANGES rail's `C` shortcut. 400 if there is nothing to commit.
    CommitAll {
        ws: SessionId,
        message: String,
    },
    /// Check out (`create` = create-and-switch) a git branch.
    Checkout {
        ws: SessionId,
        branch: String,
        create: bool,
    },
    /// Write raw bytes to a workspace file (`POST .../upload`), creating parent
    /// directories as needed.
    Upload {
        ws: SessionId,
        path: String,
        data: Vec<u8>,
    },
    /// Delete one workspace file (`DELETE .../file`). Destructive and
    /// unrecoverable — no trash, no index copy — so the caller is expected to
    /// have confirmed with the user, exactly as [`DiscardFile`] expects.
    ///
    /// Files only: a directory is a 400 rather than a recursive removal, so one
    /// confirmed keystroke cannot take out `src`.
    ///
    /// [`DiscardFile`]: ApiRequest::DiscardFile
    DeleteFile {
        ws: SessionId,
        path: String,
    },
    /// Run a git operation. One variant for every `POST .../git/*` route,
    /// because [`GitOp`] is already the typed union and validation belongs in
    /// one place rather than spread across twenty near-identical arms.
    GitRun {
        ws: SessionId,
        op: GitOp,
    },
    /// Kill the running git operation, if any.
    GitOpCancel(SessionId),
    /// Settle one conflicted file. Index work, so it answers synchronously
    /// rather than going through the operation runner.
    GitResolve {
        ws: SessionId,
        path: String,
        take: ResolveSide,
    },
    /// Apply a patch to the index or the worktree — partial staging.
    ///
    /// The client sends back a patch containing only the hunks or lines it
    /// chose. Index work, so it answers synchronously; the patch is applied by
    /// libgit2, which never runs a hook or touches a ref.
    GitApply {
        ws: SessionId,
        patch: String,
        target: ApplyTarget,
        /// Apply backwards. With `Index` that unstages; with `Worktree` it
        /// discards.
        reverse: bool,
    },
    /// Create a branch, optionally from a given start point.
    GitBranchCreate {
        ws: SessionId,
        name: String,
        from: Option<String>,
    },
    GitBranchDelete {
        ws: SessionId,
        name: String,
        force: bool,
    },
    GitBranchRename {
        ws: SessionId,
        from: Option<String>,
        to: String,
    },
    /// Update the daemon itself: check the release, download it, verify it,
    /// swap the binary and restart onto it.
    ///
    /// Refused unless `[update] allow_remote` is set, because anything that
    /// can reach the socket could otherwise replace the program the machine
    /// runs. One request rather than a check and an install, because a check
    /// whose answer is acted on separately is a version that can change in
    /// between.
    Update,
}

/// The core actor's reply, mapped to an HTTP status + JSON body by the router.
#[derive(Debug, Clone)]
pub enum ApiReply {
    Workspaces(Vec<WorkspaceSummary>),
    Workspace(WorkspaceDetail),
    Agents(Vec<AgentDto>),
    Processes(Vec<ProcessDto>),
    Changes(ChangesDto),
    Tree(TreeDto),
    File(FileDto),
    Diff(DiffDto),
    Search(SearchDto),
    System(SysDto),
    AgentTypes(Vec<String>),
    Usage(UsageDto),
    Branches(BranchesDto),
    Browse(BrowseDto),
    Notifications(NotificationsDto),
    PaneOutput(PaneOutputDto),
    /// 200 with a raw (non-JSON) body — file downloads. `download_name`, when
    /// set, drives a `Content-Disposition: attachment` filename.
    Bytes {
        data: Vec<u8>,
        content_type: String,
        download_name: Option<String>,
    },
    Log(LogDto),
    Stashes(Vec<StashDto>),
    Remotes(Vec<RemoteDto>),
    Tags(Vec<String>),
    Worktrees(Vec<WorktreeDto>),
    Conflict(ConflictDto),
    /// 200 with a git operation's state.
    ///
    /// A **finished** operation, whether it succeeded or not: check `ok`. A
    /// push the remote rejected is a successful API call reporting a failed
    /// operation, not a 400 — and it has to be, because the same failure is
    /// reported this way when the operation outlives the grace window and there
    /// is no status code left to carry it.
    GitOp(GitOpDto),
    /// 202: the operation is still running. Poll `GET .../git/op` or watch the
    /// `git_op` SSE event for the outcome.
    Accepted(GitOpDto),
    /// 200 when nothing was to be done, **202** when the daemon is going down
    /// to come back on a new binary. 202 because by the time this is read the
    /// work is accepted but not finished — the same reading as
    /// [`ApiReply::Accepted`].
    Update(UpdateDto),
    /// 200 with `{"ok":true}`.
    Ok,
    /// 201 with `{"id":<n>}`.
    Created(SessionId),
    /// 404.
    NotFound(String),
    /// 400.
    BadRequest(String),
    /// 409: another git operation holds this repository's write lock. Named
    /// `Busy` rather than `Conflict` so it is never confused with a *merge*
    /// conflict, which this API talks about constantly.
    Busy(String),
    /// 500.
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule the DOCS rail is made of, in the one place it now lives.
    ///
    /// It was written twice before — `chrome::is_doc` and `docs.ts`'s `isDoc` —
    /// which was fine while filtering was each client's own business. It stopped
    /// being fine when the daemon started deciding the markers by the same rule.
    #[test]
    fn is_doc_keeps_writing_and_drops_code() {
        for (name, dir) in [("README.md", false), ("readme", false), ("x.MARKDOWN", false)] {
            assert!(is_doc(name, dir), "{name} is writing");
        }
        for (name, dir) in [("main.rs", false), ("Cargo.toml", false), ("LICENSE", false)] {
            assert!(!is_doc(name, dir), "{name} is not writing");
        }
        // Every directory is kept but the two that are only ever build output —
        // the writing is *in* the directories, so dropping them drops it too.
        assert!(is_doc("src", true));
        assert!(!is_doc("target", true));
        assert!(!is_doc("node_modules", true));
    }

    /// An unknown `?filter=` has to be an error, not a default.
    ///
    /// Falling back to `All` would answer the unfiltered listing for a typo,
    /// which reads as the filter silently doing nothing — the exact failure
    /// this parameter exists to end.
    #[test]
    fn an_unknown_filter_is_rejected_rather_than_defaulted() {
        assert_eq!(TreeFilter::parse(""), Some(TreeFilter::All));
        assert_eq!(TreeFilter::parse("all"), Some(TreeFilter::All));
        assert_eq!(TreeFilter::parse("docs"), Some(TreeFilter::Docs));
        assert_eq!(TreeFilter::parse("Docs"), None);
        assert_eq!(TreeFilter::parse("markdown"), None);
    }

    #[test]
    fn the_default_filter_keeps_everything() {
        assert!(TreeFilter::All.keeps("main.rs", false));
        assert!(TreeFilter::All.keeps("target", true));
        assert!(!TreeFilter::Docs.keeps("main.rs", false));
        assert!(TreeFilter::Docs.keeps("README.md", false));
    }
}
