//! Resource monitor (DevPlan §10).
//!
//! Samples each live agent's whole process tree (rooted at the agent PTY pid)
//! every 3 s via `sysinfo`, sums CPU% + memory across the tree, pushes the
//! sample into a per-agent ring buffer (~3 min), and emits the batch to the
//! frontend as `resource://sample`.
//!
//! To keep idle CPU low, the expensive per-process refresh only touches each
//! agent's known process tree; a cheap metadata-only `/proc` pass discovers new
//! children each tick. The global CPU/MEM snapshot is cached on every tick so
//! the frontend's `system_stats` command never re-locks the shared `System`.
//!
//! The `System` instance lives inside `ResourceMonitor` so CPU deltas are
//! computed correctly between consecutive refreshes (sysinfo needs two
//! refreshes before `cpu_usage()` is meaningful).

use crate::agent_runtime::pty::PtyManager;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tauri::{AppHandle, Emitter};

/// Event name emitted on every sampling tick.
pub const RESOURCE_EVENT: &str = "resource://sample";
/// Ring buffer length ≈ 3 min at the 3 s sampling interval.
const HISTORY_LEN: usize = 60;
/// Sampling interval — slow enough that the `/proc` passes stay cheap.
const SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

/// One agent's resource snapshot (serialized to the frontend).
#[derive(Debug, Clone, Serialize)]
pub struct AgentResource {
    pub agent_id: String,
    /// Sum of CPU% across the whole process tree (can exceed 100 on multi-core).
    pub cpu_pct: f32,
    /// Sum of RSS memory in bytes across the whole process tree.
    pub mem_bytes: u64,
}

/// A single buffered history point (for the sparkline curve overlay).
#[derive(Debug, Clone, Serialize)]
pub struct HistoryPoint {
    pub cpu_pct: f32,
    pub mem_bytes: u64,
}

/// Tauri-managed resource monitor. Holds the `sysinfo::System` (for CPU delta
/// math across samples) and a per-agent ring buffer of recent samples.
pub struct ResourceMonitor {
    sys: Mutex<System>,
    history: Mutex<HashMap<String, VecDeque<HistoryPoint>>>,
    /// Last-known process tree (root + descendants) per agent, so the expensive
    /// per-process refresh only touches the pids we track.
    trees: Mutex<HashMap<String, Vec<u32>>>,
    /// Global CPU% / used-mem / total-mem cached on every sampling tick.
    snapshot: Mutex<(f32, u64, u64)>,
}

impl ResourceMonitor {
    pub fn new() -> Self {
        Self {
            sys: Mutex::new(System::new()),
            history: Mutex::new(HashMap::new()),
            trees: Mutex::new(HashMap::new()),
            snapshot: Mutex::new((0.0, 0, 0)),
        }
    }

    /// Refresh global CPU/mem (cheap `/proc/stat` + `/proc/meminfo`), cache them
    /// for `system_stats`, then sample each live agent's process tree. Returns
    /// the batch to emit (empty when no agents are running).
    pub fn tick(&self, pty: &PtyManager) -> Vec<AgentResource> {
        let mut sys = self.sys.lock().unwrap();

        sys.refresh_cpu_usage();
        sys.refresh_memory();
        *self.snapshot.lock().unwrap() = (
            sys.global_cpu_usage(),
            sys.used_memory(),
            sys.total_memory(),
        );

        let pids = pty.pids();
        if pids.is_empty() {
            return Vec::new();
        }

        // Cheap pass: refresh only the process table (pid/ppid/name) so we can
        // discover children spawned since the last tick without reading every
        // process's full stat/statm/status.
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );

        // Parent → children edges, owned so the mutable refresh below can borrow `sys`.
        let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for p in sys.processes().values() {
            if let Some(parent) = p.parent() {
                children.entry(parent).or_default().push(p.pid());
            }
        }

        // Collect each agent's current tree from the metadata table.
        let mut agents: Vec<(String, u32, Vec<u32>)> = Vec::with_capacity(pids.len());
        {
            let processes = sys.processes();
            for (agent_id, pid) in &pids {
                let root = Pid::from_u32(*pid);
                agents.push((agent_id.clone(), *pid, collect_tree(root, processes, &children)));
            }
        }

        // Expensive pass: full detail (CPU/MEM, `without_tasks()`) for exactly
        // the tracked trees, not every process on the system.
        let tracked: Vec<Pid> = agents
            .iter()
            .flat_map(|(_, _, tp)| tp.iter().map(|&p| Pid::from_u32(p)))
            .collect();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&tracked),
            true,
            ProcessRefreshKind::everything().without_tasks(),
        );

        let mut trees = self.trees.lock().unwrap();
        let mut out = Vec::with_capacity(agents.len());
        for (agent_id, root, tree_pids) in agents {
            let (cpu_pct, mem_bytes) = sum_tree(Pid::from_u32(root), sys.processes(), &children);
            trees.insert(agent_id.clone(), tree_pids);
            self.push_history(&agent_id, cpu_pct, mem_bytes);
            out.push(AgentResource {
                agent_id,
                cpu_pct,
                mem_bytes,
            });
        }
        out
    }

    /// Cached global CPU/mem from the last sampling tick.
    pub fn snapshot(&self) -> (f32, u64, u64) {
        *self.snapshot.lock().unwrap()
    }

    /// Buffered history for one agent (oldest → newest).
    pub fn history(&self, agent_id: &str) -> Vec<HistoryPoint> {
        self.history
            .lock()
            .unwrap()
            .get(agent_id)
            .cloned()
            .unwrap_or_default()
            .into()
    }

    fn push_history(&self, agent_id: &str, cpu_pct: f32, mem_bytes: u64) {
        let mut hist = self.history.lock().unwrap();
        let buf = hist.entry(agent_id.to_string()).or_default();
        buf.push_back(HistoryPoint {
            cpu_pct,
            mem_bytes,
        });
        while buf.len() > HISTORY_LEN {
            buf.pop_front();
        }
    }
}

/// Collect the pids in the process tree rooted at `root` by walking the
/// parent→children edges from the metadata pass.
fn collect_tree(
    root: Pid,
    processes: &HashMap<Pid, sysinfo::Process>,
    children: &HashMap<Pid, Vec<Pid>>,
) -> Vec<u32> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if processes.contains_key(&pid) {
            out.push(pid.as_u32());
        }
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    out
}

/// Sum CPU% + RSS bytes over the process tree rooted at `root`.
fn sum_tree(
    root: Pid,
    processes: &HashMap<Pid, sysinfo::Process>,
    children: &HashMap<Pid, Vec<Pid>>,
) -> (f32, u64) {
    let mut cpu_pct = 0.0_f32;
    let mut mem_bytes = 0_u64;
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        if let Some(proc) = processes.get(&pid) {
            cpu_pct += proc.cpu_usage();
            mem_bytes += proc.memory();
        }
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    (cpu_pct, mem_bytes)
}

/// Spawn the background sampler. Samples every 3 s and emits `resource://sample`.
/// `sysinfo` I/O is synchronous, so the actual sampling runs on a blocking pool.
pub fn start_sampler(pty: Arc<PtyManager>, monitor: Arc<ResourceMonitor>, app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(SAMPLE_INTERVAL);
        // Consume the immediate first tick so the first real sample has a full
        // interval of CPU delta data behind it.
        tick.tick().await;
        loop {
            tick.tick().await;
            let pty = pty.clone();
            let monitor = monitor.clone();
            let app = app.clone();
            let samples = tauri::async_runtime::spawn_blocking(move || monitor.tick(&pty)).await;
            if let Ok(batch) = samples {
                if !batch.is_empty() {
                    let _ = app.emit(RESOURCE_EVENT, &batch);
                }
            }
        }
    });
}
