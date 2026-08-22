//! The v2 workspace model and chrome geometry.
//!
//! A workspace (one per project directory) replaces the v1 session+windows
//! model: fixed rails (agents, processes, system on the left; changes on
//! the right) around a single swap-in stage.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use butai_protocol::{PaneId, SessionId, SessionInfo};

#[derive(Debug, Clone)]
pub struct ProcMeta {
    pub name: String,
    pub command: String,
    /// Substring that flips the status to "ok" when seen in output.
    pub ready: Option<String>,
    pub ready_seen: bool,
    /// Tail of the previous output burst, kept so a `ready` marker split across
    /// two bursts is still matched. Output arrives coalesced per drain, and a
    /// server's startup banner is routinely written in more than one syscall or
    /// lands on a 64 KiB read boundary — without this the row stays `run`
    /// forever even though the marker was printed. Bounded by the marker
    /// length, so it costs nothing.
    pub ready_carry: String,
}

/// What restart restore needs to know about one agent pane.
#[derive(Debug, Clone)]
pub struct AgentMeta {
    /// The `[[agents]]` name this pane was launched from.
    ///
    /// Kept rather than read back off the pane because the pane's label is not
    /// a stable answer: agents rewrite their OSC title continuously to show the
    /// current task, and that title is what the rails render. Restore needs the
    /// name the launcher was configured under, not what the agent is calling
    /// itself this second.
    pub name: String,
    /// The conversation this pane is holding, when the launcher has a notion of
    /// one that butai can name.
    ///
    /// This is what makes restore correct with more than one agent open. The
    /// obvious resume flags — `claude --continue`, `gemini --resume latest` —
    /// all mean "the most recent conversation *in this directory*", which is
    /// ambiguous exactly when a workspace runs two agents: both would reopen
    /// the same transcript and interleave into it. Naming the conversation
    /// removes the ambiguity.
    ///
    /// `None` for a launcher with no session concept at all (aider), which
    /// simply restores the way it did before: repainted, but starting fresh.
    pub session_id: Option<String>,
    /// Whether this pane has ever been sent input.
    ///
    /// A conversation does not exist until the agent is spoken to: both CLIs
    /// create the transcript on the *first user message*, not at launch, so an
    /// id minted here names nothing until then. Asking to reopen one of those
    /// is not a no-op — `claude --resume <unwritten id>` prints "No conversation
    /// found with session ID" and exits 1 — so without this every agent you
    /// opened and never typed into would die on restart and come back through
    /// the fallback path, which is meant to be the rare case rather than the
    /// common one.
    ///
    /// Deliberately "was input routed here" rather than anything read out of a
    /// vendor's storage directory: it is the same question one layer up, and it
    /// stays true for a CLI whose files butai has never heard of.
    ///
    /// A proxy, so it can say yes where the answer is no — a keystroke that
    /// reached the pane while the CLI was still starting, or one that never
    /// became a message. Those land back on the missing-conversation fallback,
    /// which is what it is for; the point of this flag is that they are now the
    /// exception rather than every idle agent on every restart.
    pub spoke: bool,
}

pub struct Workspace {
    pub id: SessionId,
    pub name: String,
    pub cwd: PathBuf,
    pub agents: Vec<PaneId>,
    /// What each agent pane was launched as, and which conversation it holds.
    pub agent_meta: HashMap<PaneId, AgentMeta>,
    pub processes: Vec<PaneId>,
    pub proc_meta: HashMap<PaneId, ProcMeta>,
    /// Terminal pane following the selected container's logs on the docker
    /// space (re-pointed as the selection changes).
    pub docker_logs: Option<PaneId>,
    /// The workspace's git status, cached; `None` outside a repository. Not a
    /// rail any more — it is where `ChangesDto` and every git route read from.
    pub changes: Option<PaneId>,
    /// What currently occupies the stage.
    pub stage: Option<PaneId>,
    /// The stage's interior, as the last client to draw this workspace reported
    /// it — `(rows, cols)`.
    ///
    /// A pane's size belongs to whoever is looking at it: the client decides how
    /// wide its rails are, so only the client knows how big the hole in the
    /// middle is. It tells us whenever it points at a pane or resizes one, and
    /// since the only watchable pane is the one on the stage, that measurement
    /// *is* the stage. It is also the only measurement there is — this side
    /// stopped computing the rectangle when it stopped drawing the rails.
    ///
    /// Kept so the *next* pane can be born the right size instead of being born
    /// small and reflowed the moment it is staged — a program that reads its
    /// window size once, at startup, only gets one chance.
    pub stage_size: Option<(u16, u16)>,
}

impl Workspace {
    pub fn new(id: SessionId, name: String, cwd: PathBuf) -> Self {
        Self {
            id,
            name,
            cwd,
            agents: Vec::new(),
            agent_meta: HashMap::new(),
            processes: Vec::new(),
            proc_meta: HashMap::new(),
            docker_logs: None,
            changes: None,
            stage: None,
            stage_size: None,
        }
    }

    pub fn all_panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        out.extend(&self.agents);
        out.extend(&self.processes);
        out.extend(self.docker_logs);
        out.extend(self.changes);
        out
    }

    /// Drop a pane from every slot; returns true if it was present.
    pub fn forget_pane(&mut self, id: PaneId) -> bool {
        let mut hit = false;
        if let Some(i) = self.agents.iter().position(|p| *p == id) {
            self.agents.remove(i);
            self.agent_meta.remove(&id);
            hit = true;
        }
        if let Some(i) = self.processes.iter().position(|p| *p == id) {
            self.processes.remove(i);
            self.proc_meta.remove(&id);
            hit = true;
        }
        if self.docker_logs == Some(id) {
            self.docker_logs = None;
            hit = true;
        }
        if self.changes == Some(id) {
            self.changes = None;
            hit = true;
        }
        if self.stage == Some(id) {
            self.stage = self.processes.first().copied().or_else(|| self.agents.first().copied());
        }
        hit
    }

    pub fn info(&self, attached_clients: usize) -> SessionInfo {
        SessionInfo {
            id: self.id,
            name: self.name.clone(),
            windows: 1,
            attached_clients,
            cwd: self.cwd.clone(),
        }
    }
}

/// One GPU's live utilization + VRAM, with a short history for its sparkline.
#[derive(Debug, Clone, Default)]
pub struct GpuStat {
    pub pct: f32,
    pub mem_used_gb: f32,
    pub mem_total_gb: f32,
    /// Model name, shortened (e.g. "RTX 4090"); empty when unknown.
    pub name: String,
    /// Core temperature in Celsius.
    pub temp_c: Option<f32>,
    /// Board power draw in watts.
    pub power_w: Option<f32>,
    pub hist: Vec<f32>,
}

/// One network interface, with its throughput differenced from the kernel's
/// byte counters.
///
/// The counters themselves never leave the daemon: they are monotonic since
/// boot and wrap at 32 bits on some drivers, so a client differencing them
/// would have to know both facts plus when the last sample landed.
#[derive(Debug, Clone, Default)]
pub struct NetStat {
    pub name: String,
    pub rx_bps: f32,
    pub tx_bps: f32,
    pub rx_hist: Vec<f32>,
    pub tx_hist: Vec<f32>,
    pub kind: butai_protocol::api::NetKind,
    pub carrier: bool,
    pub default_route: bool,
    /// Negotiated link speed in Mb/s, where the driver publishes one.
    pub speed_mbps: Option<u32>,
    /// Kernel driver bound to the device.
    pub driver: Option<String>,
}

/// One mounted filesystem's capacity, as the sampler carries it.
///
/// Keeps no history, unlike [`GpuStat`] and [`NetStat`]: a disk does not
/// visibly move across the retained window, so the series would be a flat line
/// at 320 bytes per mount on every push to every client.
///
/// It does carry its numbers *between* ticks, which is the point of it being a
/// struct the sampler owns rather than one it rebuilds. A mount whose
/// `statvfs` timed out keeps the last good reading and says so, and `total_gb`
/// survives even that — the size of a filesystem is fixed for the life of the
/// mount, so a stale row can still say how big the disk is.
#[derive(Debug, Clone, Default)]
pub struct DiskStat {
    pub mount: String,
    pub source: String,
    pub fstype: String,
    pub kind: butai_protocol::api::DiskKind,
    pub used_gb: f32,
    pub total_gb: f32,
    pub stale: bool,
}

/// A single docker container as reported by `docker ps`.
#[derive(Debug, Clone, Default)]
pub struct Container {
    pub name: String,
    pub state: String,
    /// Compose project name; empty for standalone containers.
    pub project: String,
    /// Compose project working_dir; empty for standalone containers.
    pub workdir: String,
}

/// Containers grouped by compose project (standalone containers form a
/// one-member stack). Rendered as one row in the PROCESSES rail.
#[derive(Debug, Clone, Default)]
pub struct Stack {
    /// Display name: compose project, or the container name when standalone.
    pub label: String,
    /// Compose project name; empty when standalone.
    pub project: String,
    pub workdir: String,
    pub containers: Vec<String>,
    /// Per-container running flag, aligned with `containers`.
    pub states: Vec<bool>,
    pub running: usize,
    pub total: usize,
    /// True when the compose project lives at/under/over the workspace cwd.
    pub mine: bool,
}

/// Machine telemetry sampled by the daemon (V2-3 fills this in).
#[derive(Debug, Clone, Default)]
pub struct SysStats {
    pub cpu_pct: f32,
    pub cpu_hist: Vec<f32>,
    pub cpu_temp: Option<f32>,
    /// Model name, shortened (e.g. "Ryzen 7 5700"); `None` when unknown. Static,
    /// so the sampler reads it once rather than every tick.
    pub cpu_model: Option<String>,
    pub cpu_cores: Option<u16>,
    pub cpu_threads: Option<u16>,
    pub ram_used_gb: f32,
    pub ram_total_gb: f32,
    pub ram_hist: Vec<f32>,
    pub swap_used_gb: f32,
    pub swap_total_gb: f32,
    /// One entry per detected GPU; empty when no GPU is present.
    pub gpus: Vec<GpuStat>,
    /// Every interface the kernel reports, unfiltered. Which one is worth
    /// drawing is the client's call; this is just what is there.
    pub net: Vec<NetStat>,
    /// Every mounted filesystem with a capacity to report, largest first.
    /// Which ones are worth drawing is the client's call, the same way.
    pub disks: Vec<DiskStat>,
    /// Every docker container; empty when docker is absent.
    pub containers: Vec<Container>,
}

impl SysStats {
    /// Docker grouped into started compose stacks (plus standalone running
    /// containers), the workspace's own stack(s) sorted first. Only stacks
    /// with at least one running container are returned.
    pub fn stacks(&self, cwd: &Path) -> Vec<Stack> {
        use std::collections::BTreeMap;
        let mut groups: BTreeMap<String, Stack> = BTreeMap::new();
        for c in &self.containers {
            // Standalone containers get a unique key so they don't merge.
            let key =
                if c.project.is_empty() { format!("\u{0}{}", c.name) } else { c.project.clone() };
            let entry = groups.entry(key).or_insert_with(|| Stack {
                label: if c.project.is_empty() { c.name.clone() } else { c.project.clone() },
                project: c.project.clone(),
                workdir: c.workdir.clone(),
                ..Default::default()
            });
            if entry.workdir.is_empty() && !c.workdir.is_empty() {
                entry.workdir = c.workdir.clone();
            }
            entry.containers.push(c.name.clone());
            let running = c.state == "running";
            entry.states.push(running);
            entry.total += 1;
            if running {
                entry.running += 1;
            }
        }
        let mut stacks: Vec<Stack> = groups.into_values().filter(|s| s.running > 0).collect();
        for s in &mut stacks {
            s.mine = !s.workdir.is_empty() && {
                let wd = Path::new(&s.workdir);
                cwd.starts_with(wd) || wd.starts_with(cwd)
            };
        }
        stacks.sort_by(|a, b| b.mine.cmp(&a.mine).then_with(|| a.label.cmp(&b.label)));
        stacks
    }

    /// The docker stacks belonging to this workspace's project (compose
    /// working_dir at/under/over the cwd). Falls back to every started stack
    /// when none match, so the docker space is never mysteriously empty.
    pub fn project_stacks(&self, cwd: &Path) -> Vec<Stack> {
        let all = self.stacks(cwd);
        if all.iter().any(|s| s.mine) {
            all.into_iter().filter(|s| s.mine).collect()
        } else {
            all
        }
    }
}

/// A selectable row on the docker space: a stack header, or a container
/// belonging to a (multi-container) stack.
#[derive(Debug, Clone)]
pub enum DockerRow {
    /// A stack header; the value indexes the stacks slice it came from.
    Stack(usize),
    Container {
        stack: usize,
        name: String,
        running: bool,
    },
}

/// Flatten stacks into their selectable rows: each stack header followed by
/// its containers. A standalone stack (one container == the header) isn't
/// repeated.
pub fn docker_rows(stacks: &[Stack]) -> Vec<DockerRow> {
    let mut rows = Vec::new();
    for (i, s) in stacks.iter().enumerate() {
        rows.push(DockerRow::Stack(i));
        if s.containers.len() > 1 {
            for (name, running) in s.containers.iter().zip(&s.states) {
                rows.push(DockerRow::Container { stack: i, name: name.clone(), running: *running });
            }
        }
    }
    rows
}

// ---------------------------------------------------------------------------
// Chrome geometry
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stacks_group_by_compose_project() {
        let ctr = |name: &str, state: &str, project: &str, workdir: &str| Container {
            name: name.into(),
            state: state.into(),
            project: project.into(),
            workdir: workdir.into(),
        };
        let sys = SysStats {
            containers: vec![
                ctr("app-web-1", "running", "app", "/proj/app"),
                ctr("app-db-1", "running", "app", "/proj/app"),
                ctr("elsewhere-1", "running", "elsewhere", "/elsewhere"),
                ctr("manual", "running", "", ""),
                ctr("stopped", "exited", "dead", "/dead"),
            ],
            ..Default::default()
        };
        let stacks = sys.stacks(Path::new("/proj/app"));
        // Two compose stacks + one standalone; the exited-only stack drops.
        assert_eq!(stacks.len(), 3);
        // The workspace's own stack sorts first and is grouped.
        assert_eq!(stacks[0].label, "app");
        assert_eq!(stacks[0].total, 2);
        assert!(stacks[0].mine);
        assert!(stacks.iter().any(|s| s.label == "manual" && !s.mine));
        assert!(!stacks.iter().any(|s| s.label == "dead"));
    }

    #[test]
    fn workspace_forget_pane_updates_stage() {
        let mut ws = Workspace::new(SessionId(1), "t".into(), "/tmp".into());
        ws.agents.push(PaneId(1));
        ws.processes.push(PaneId(2));
        ws.stage = Some(PaneId(2));
        assert!(ws.forget_pane(PaneId(2)));
        assert_eq!(ws.stage, Some(PaneId(1)));
        assert!(ws.processes.is_empty());
    }
}
