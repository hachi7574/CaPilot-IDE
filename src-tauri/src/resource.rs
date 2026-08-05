//! Resource monitor (DevPlan §10).
//!
//! Every second, samples each live agent's whole process tree (rooted at the
//! agent PTY pid) via `sysinfo`, sums CPU% + memory across the tree, pushes the
//! sample into a per-agent ring buffer (~60 s), and emits the batch to the
//! frontend as `resource://sample`.
//!
//! The `System` instance lives inside `ResourceMonitor` so CPU deltas are
//! computed correctly between consecutive 1 s refreshes (sysinfo needs two
//! refreshes before `cpu_usage()` is meaningful).

use crate::agent_runtime::pty::PtyManager;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tauri::{AppHandle, Emitter};

/// Event name emitted on every sampling tick.
pub const RESOURCE_EVENT: &str = "resource://sample";
/// Ring buffer length ≈ 60 s at 1 sample/s.
const HISTORY_LEN: usize = 60;

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
}

impl ResourceMonitor {
    pub fn new() -> Self {
        Self {
            sys: Mutex::new(System::new()),
            history: Mutex::new(HashMap::new()),
        }
    }

    /// Refresh sysinfo, walk each live agent's process tree, and record the
    /// sample into the history ring buffer. Returns the batch to emit.
    pub fn sample(&self, pty: &PtyManager) -> Vec<AgentResource> {
        let pids = pty.pids();
        if pids.is_empty() {
            return Vec::new();
        }

        let mut sys = self.sys.lock().unwrap();
        // `without_tasks()` skips /proc/<pid>/task/ enumeration — significantly
        // faster while still giving us CPU + memory per process.
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::everything().without_tasks(),
        );
        let processes = sys.processes();

        // Build parent → children index for the tree walk.
        let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
        for p in processes.values() {
            if let Some(parent) = p.parent() {
                children.entry(parent).or_default().push(p.pid());
            }
        }

        let mut out = Vec::with_capacity(pids.len());
        for (agent_id, pid) in pids {
            let (cpu_pct, mem_bytes) = sum_tree(Pid::from_u32(pid), processes, &children);
            self.push_history(&agent_id, cpu_pct, mem_bytes);
            out.push(AgentResource {
                agent_id,
                cpu_pct,
                mem_bytes,
            });
        }
        out
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

/// Spawn the background sampler. Samples every 1 s and emits `resource://sample`.
/// `sysinfo` I/O is synchronous, so the actual sampling runs on a blocking pool.
pub fn start_sampler(pty: Arc<PtyManager>, monitor: Arc<ResourceMonitor>, app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        // Consume the immediate first tick so the first real sample has a full
        // second of CPU delta data behind it.
        tick.tick().await;
        loop {
            tick.tick().await;
            let pty = pty.clone();
            let monitor = monitor.clone();
            let app = app.clone();
            let samples = tauri::async_runtime::spawn_blocking(move || monitor.sample(&pty)).await;
            if let Ok(batch) = samples {
                if !batch.is_empty() {
                    let _ = app.emit(RESOURCE_EVENT, &batch);
                }
            }
        }
    });
}
