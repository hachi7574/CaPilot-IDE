//! PATH shim orchestration server (DevPlan §5.2 / §5.3).
//!
//! `capilot dispatch|status|report` → shell shim → Unix socket → this
//! dispatcher → worker PTY.

use crate::agent_runtime::pty::PtyManager;
use crate::orchestration::smart_return;
use crate::persistence::{agent_dir, read_agent_meta, write_agent_meta, Persistence};
use serde::Serialize;
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

fn default_socket_path() -> PathBuf {
    // Prefer the per-user runtime dir; fall back to a private dir under HOME.
    // Never use a fixed world-visible path in /tmp (local DoS / injection).
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let dir = PathBuf::from(runtime_dir);
        if dir.is_dir() {
            return dir.join("capilot-orchestrator.sock");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".capilot")
        .join("run")
        .join("capilot-orchestrator.sock")
}

/// Where the shim looks for the socket path.
fn socket_path_file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".capilot").join("socket")
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WorkerStatus {
    #[default]
    Idle,
    Busy,
}

#[derive(Debug, Clone, Default)]
struct WorkerState {
    status: WorkerStatus,
    last_task: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkerInfo {
    pub id: String,
    pub title: String,
    pub runtime: String,
    pub status: String, // idle | busy | offline
    pub last_task: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkerReport {
    pub worker: String,
    pub summary: String,
    pub level: String, // full | summary | title | failure
    pub ts: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CascadeMode {
    Keep,
    Delete,
}

pub struct Dispatcher {
    pty: Arc<PtyManager>,
    persistence: Arc<Persistence>,
    workers: Mutex<HashMap<String, WorkerState>>,
    reports: Mutex<Vec<WorkerReport>>,
    master_id: Mutex<Option<String>>,
    smart_return: AtomicBool,
    socket_path: PathBuf,
}

impl Dispatcher {
    pub fn new(pty: Arc<PtyManager>, persistence: Arc<Persistence>) -> Self {
        let socket_path = default_socket_path();
        let _ = std::fs::remove_file(&socket_path);
        Self {
            pty,
            persistence,
            workers: Mutex::new(HashMap::new()),
            reports: Mutex::new(Vec::new()),
            master_id: Mutex::new(None),
            smart_return: AtomicBool::new(true),
            socket_path,
        }
    }

    #[allow(dead_code)]
    pub fn socket_path(&self) -> PathBuf {
        self.socket_path.clone()
    }

    pub fn set_smart_return(&self, enabled: bool) {
        self.smart_return.store(enabled, Ordering::Relaxed);
    }

    pub fn smart_return_enabled(&self) -> bool {
        self.smart_return.load(Ordering::Relaxed)
    }

    pub fn set_master(&self, id: Option<String>) {
        *self.master_id.lock().unwrap() = id;
    }

    /// Rebuild the in-memory worker pool from persisted sessions.
    pub fn refresh_workers(&self) {
        let db = self.persistence.db().lock().unwrap();
        if let Ok(sessions) = db.list_all() {
            let mut workers = self.workers.lock().unwrap();
            for s in sessions {
                if s.role == "worker" {
                    workers.entry(s.id.clone()).or_default();
                }
            }
        }
    }

    pub fn register_worker(&self, id: &str) {
        self.workers.lock().unwrap().entry(id.to_string()).or_default();
    }

    pub fn unregister_worker(&self, id: &str) {
        self.workers.lock().unwrap().remove(id);
    }

    /// Atomically check-and-mark a worker busy. Returns false if it was already
    /// busy. The check and the mark happen under a single lock acquisition so
    /// two concurrent dispatches can't both claim the same idle worker.
    fn try_mark_busy(&self, id: &str, task: &str) -> bool {
        let mut workers = self.workers.lock().unwrap();
        if workers
            .get(id)
            .is_some_and(|ws| ws.status == WorkerStatus::Busy)
        {
            return false;
        }
        let ws = workers.entry(id.to_string()).or_default();
        ws.status = WorkerStatus::Busy;
        ws.last_task = Some(task.to_string());
        true
    }

    /// Mark a worker idle; returns true if the status actually changed.
    pub fn mark_idle(&self, id: &str) -> bool {
        let mut workers = self.workers.lock().unwrap();
        let Some(ws) = workers.get_mut(id) else {
            return false;
        };
        if ws.status == WorkerStatus::Idle {
            return false;
        }
        ws.status = WorkerStatus::Idle;
        true
    }

    /// Mark a worker idle and notify the frontend if its state changed.
    fn set_worker_idle(&self, id: &str, app: &AppHandle) {
        if self.mark_idle(id) {
            self.emit_worker_status(id, app);
        }
    }

    /// A Busy worker that exits without `capilot report` has violated the
    /// completion contract. Report the failure before returning it to idle.
    pub fn worker_ended_naturally(&self, id: &str, exit_code: i32, app: &AppHandle) {
        let was_busy = self
            .workers
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|worker| worker.status == WorkerStatus::Busy);
        if !was_busy {
            return;
        }
        self.publish_report(unreported_exit_report(id, exit_code), app);
        self.set_worker_idle(id, app);
    }

    /// End all non-finished workers in the master's project.
    pub fn cascade_master(&self, master_id: &str, mode: CascadeMode, app: &AppHandle) {
        let (project, workers) = {
            let Ok(db) = self.persistence.db().lock() else {
                log::warn!("cannot cascade master {master_id}: persistence lock poisoned");
                return;
            };
            let Ok(Some(master)) = db.get(master_id) else {
                return;
            };
            let workers = db
                .list_all()
                .unwrap_or_default()
                .into_iter()
                .filter(|session| {
                    session.project == master.project
                        && session.role == "worker"
                        && session.status != "done"
                })
                .collect::<Vec<_>>();
            (master.project, workers)
        };
        if workers.is_empty() {
            return;
        }
        log::info!(
            "cascading {} worker(s) for master {master_id} in project {project} ({mode:?})",
            workers.len()
        );
        for worker in workers {
            let _ = self.pty.kill(&worker.id);
            match mode {
                CascadeMode::Keep => {
                    let now = chrono_now_ms();
                    if let Ok(db) = self.persistence.db().lock() {
                        let _ = db.update_status(&worker.id, "done", now);
                    }
                    if let Ok(mut meta) = read_agent_meta(&project, &worker.id) {
                        meta.status = "done".to_string();
                        meta.updated_at = now;
                        let _ = write_agent_meta(&project, &meta);
                    }
                    let _ = app.emit(
                        "agent://exited",
                        serde_json::json!({ "id": worker.id, "exit_code": 0 }),
                    );
                }
                CascadeMode::Delete => {
                    if let Ok(db) = self.persistence.db().lock() {
                        let _ = db.delete(&worker.id);
                    }
                    let dir = agent_dir(&project, &worker.id);
                    if dir.starts_with(crate::persistence::workspace_root()) && dir.exists() {
                        if let Err(error) = std::fs::remove_dir_all(&dir) {
                            log::warn!("failed to remove cascaded worker {}: {error}", worker.id);
                        }
                    }
                    let _ = app.emit(
                        "agent://removed",
                        serde_json::json!({ "id": worker.id }),
                    );
                }
            }
            self.unregister_worker(&worker.id);
        }
    }

    fn publish_report(&self, report: WorkerReport, app: &AppHandle) {
        self.reports.lock().unwrap().push(report.clone());
        let _ = app.emit("orchestration://report", report.clone());
        if let Some(master) = self.master_id.lock().unwrap().clone() {
            if self.pty.is_alive(&master) {
                let msg = format!(
                    "\r\n[编排] worker {} 完成：{}\r\n",
                    report.worker, report.summary
                );
                let _ = self.pty.write(&master, msg.as_bytes());
            }
        }
    }

    /// Emit a worker-status event (`orchestration://event`) so the UI stays in
    /// sync with busy/idle transitions.
    fn emit_worker_status(&self, id: &str, app: &AppHandle) {
        let (status, last_task) = {
            let workers = self.workers.lock().unwrap();
            match workers.get(id) {
                Some(ws) => {
                    let s = if ws.status == WorkerStatus::Busy {
                        "busy"
                    } else {
                        "idle"
                    };
                    (s.to_string(), ws.last_task.clone())
                }
                None => ("idle".to_string(), None),
            }
        };
        let _ = app.emit("orchestration://event", WorkerInfo {
            id: id.to_string(),
            title: id.to_string(),
            runtime: String::new(),
            status,
            last_task,
        });
    }

    /// Start the Unix socket listener and a periodic stale-busy sweeper in the
    /// background.
    pub fn start(self: &Arc<Self>, app: AppHandle) {
        let this = self.clone();
        let app_socket = app.clone();
        tauri::async_runtime::spawn(async move {
            this.run_socket(app_socket).await;
        });
        // Sweep: a worker whose PTY died (kill / session delete / crash) while
        // Busy must return to Idle so it can be dispatched again.
        let this2 = self.clone();
        let app_sweep = app.clone();
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(3));
            loop {
                tick.tick().await;
                this2.sweep_stale_busy(&app_sweep);
            }
        });
    }

    /// Mark any Busy worker whose PTY is no longer alive back to Idle.
    fn sweep_stale_busy(&self, app: &AppHandle) {
        let stale: Vec<String> = {
            let workers = self.workers.lock().unwrap();
            workers
                .iter()
                .filter(|(id, ws)| ws.status == WorkerStatus::Busy && !self.pty.is_alive(id))
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in stale {
            log::info!("worker {id} PTY is no longer alive — returning to idle");
            self.set_worker_idle(&id, app);
        }
    }

    async fn run_socket(self: Arc<Self>, app: AppHandle) {
        // Ensure the socket directory exists before binding.
        if let Some(parent) = self.socket_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                log::error!(
                    "capilot socket dir {} create failed: {e}",
                    parent.display()
                );
                return;
            }
        }
        let listener = match UnixListener::bind(&self.socket_path) {
            Ok(l) => l,
            Err(e) => {
                log::error!(
                    "capilot orchestrator socket bind FAILED at {}: {e} — capilot dispatch/status/report shim will not work",
                    self.socket_path.display()
                );
                return;
            }
        };
        // Restrict the socket to the current user regardless of umask.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&self.socket_path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o600);
                if let Err(e) = std::fs::set_permissions(&self.socket_path, perms) {
                    log::warn!("capilot socket chmod 0600 failed: {e}");
                }
            }
        }
        // Persist the socket path for the shim.
        if let Some(parent) = socket_path_file().parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(socket_path_file(), self.socket_path.to_string_lossy().as_bytes());

        log::info!("capilot orchestrator listening on {}", self.socket_path.display());
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let this = self.clone();
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        this.handle_conn(stream, app).await;
                    });
                }
                Err(e) => {
                    log::warn!("capilot socket accept error: {e}");
                }
            }
        }
    }

    async fn handle_conn(&self, mut stream: UnixStream, app: AppHandle) {
        // Peer auth: only accept connections from the socket owner's user. The
        // shim runs as the same user, so a different euid is hostile. Uses
        // SO_PEERCRED (tokio's peer_cred) and the socket file's owner uid.
        let socket_uid = std::fs::metadata(&self.socket_path).ok().map(|m| m.uid());
        match (stream.peer_cred(), socket_uid) {
            (Ok(cred), Some(expected)) if cred.uid() == expected => {}
            (Ok(cred), _) => {
                log::warn!(
                    "rejected capilot socket peer uid {} (expected {:?})",
                    cred.uid(),
                    socket_uid
                );
                return;
            }
            (Err(e), _) => {
                log::warn!("capilot socket peer_cred unavailable: {e}; rejecting connection");
                return;
            }
        }
        let (r, mut w) = stream.split();
        let mut reader = BufReader::new(r);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break, // EOF / error
                Ok(_) => {
                    let resp = self.handle_line(line.trim(), &app);
                    let _ = w.write_all(resp.as_bytes()).await;
                    let _ = w.write_all(b"\n").await;
                    let _ = w.flush().await;
                }
            }
        }
    }

    fn handle_line(&self, line: &str, app: &AppHandle) -> String {
        let mut parts = line.splitn(3, ' ');
        let cmd = parts.next().unwrap_or("").trim();
        match cmd {
            "dispatch" => {
                let worker = parts.next().unwrap_or("").trim().to_string();
                let prompt = parts.next().unwrap_or("").trim().to_string();
                self.dispatch(&worker, &prompt, app)
            }
            "status" => self.status(),
            "report" => {
                let first = parts.next().unwrap_or("").trim().to_string();
                let rest = parts.next().unwrap_or("").trim().to_string();
                self.report(&first, &rest, app)
            }
            "ping" => "OK pong".to_string(),
            _ => format!("ERR unknown command: {cmd}"),
        }
    }

    fn dispatch(&self, worker: &str, prompt: &str, app: &AppHandle) -> String {
        if prompt.is_empty() {
            return "ERR usage: dispatch <worker> <prompt>".to_string();
        }
        let Some(agent_id) = self.resolve_worker(worker) else {
            return format!("ERR worker not found: {worker}");
        };
        // Check busy + mark busy atomically (single lock acquisition) so two
        // concurrent dispatches can't both claim the same idle worker.
        if !self.try_mark_busy(&agent_id, prompt) {
            return format!("ERR worker busy: {worker}");
        }
        if !self.pty.is_alive(&agent_id) {
            // PTY is gone — undo the busy mark so the worker isn't stuck Busy.
            self.set_worker_idle(&agent_id, app);
            return format!("ERR worker has no live PTY (offline): {worker} — resume it in the IDE first");
        }
        // Inject the instruction into the worker's interactive TUI.
        let payload = format!("{}\r", prompt);
        match self.pty.write(&agent_id, payload.as_bytes()) {
            Ok(()) => {
                self.emit_worker_status(&agent_id, app);
                format!("OK dispatched to {worker}")
            }
            Err(e) => {
                self.set_worker_idle(&agent_id, app);
                format!("ERR write failed: {e}")
            }
        }
    }

    /// Structured worker status list (used by both the shim and the frontend).
    pub fn workers_list(&self) -> Vec<WorkerInfo> {
        let db = self.persistence.db().lock().unwrap();
        let sessions = db.list_all().unwrap_or_default();
        let workers = self.workers.lock().unwrap();
        let mut infos: Vec<WorkerInfo> = Vec::new();
        for s in sessions {
            if s.role != "worker" {
                continue;
            }
            let state = workers.get(&s.id);
            let live = self.pty.is_alive(&s.id);
            let status = if !live {
                "offline".to_string()
            } else if let Some(ws) = state {
                if ws.status == WorkerStatus::Busy {
                    "busy".to_string()
                } else {
                    "idle".to_string()
                }
            } else {
                "idle".to_string()
            };
            infos.push(WorkerInfo {
                id: s.id.clone(),
                title: s.title.clone(),
                runtime: s.runtime.clone(),
                status,
                last_task: state.and_then(|ws| ws.last_task.clone()),
            });
        }
        infos
    }

    fn status(&self) -> String {
        let infos = self.workers_list();
        serde_json::to_string(&infos).unwrap_or_else(|_| "[]".to_string())
    }

    /// `capilot report <worker> <summary>` — worker completion → smart return.
    fn report(&self, first: &str, rest: &str, app: &AppHandle) -> String {
        let worker_id = self.resolve_worker(first);
        let (worker, summary) = if worker_id.is_some() {
            (first.to_string(), rest.to_string())
        } else if first.is_empty() {
            ("unknown".to_string(), String::new())
        } else {
            // No known worker named `first` — treat the whole line as summary.
            ("unknown".to_string(), format!("{first} {rest}").trim().to_string())
        };

        let is_failure = summary.contains("失败") || summary.to_lowercase().contains("failed");
        let level = if is_failure {
            "failure".to_string()
        } else {
            match smart_return::classify(&summary, false) {
                smart_return::ReturnLevel::Full => "full".to_string(),
                smart_return::ReturnLevel::Summary => "summary".to_string(),
                smart_return::ReturnLevel::Title => "title".to_string(),
            }
        };

        // Smart-return ON → classify; OFF → always full.
        let presented = if self.smart_return_enabled() {
            if is_failure {
                smart_return::failure_report(&summary)
            } else {
                smart_return::summarize(&summary)
            }
        } else {
            summary.clone()
        };

        let report = WorkerReport {
            worker: worker.clone(),
            summary: presented.clone(),
            level: level.clone(),
            ts: chrono_now_ms(),
        };
        self.publish_report(report, app);

        // Worker completed its task — return it to idle so it can be dispatched
        // again (the frontend is notified via the status event).
        if let Some(wid) = worker_id {
            self.set_worker_idle(&wid, app);
        }
        format!("OK report registered ({level})")
    }

    /// Resolve a worker reference: exact id, title, or id prefix.
    fn resolve_worker(&self, reference: &str) -> Option<String> {
        if reference.is_empty() {
            return None;
        }
        let db = self.persistence.db().lock().unwrap();
        let sessions = db.list_all().ok()?;
        for s in &sessions {
            if s.role != "worker" {
                continue;
            }
            if s.id == reference || s.title == reference {
                return Some(s.id.clone());
            }
        }
        // Prefix match on id (shorter friendly names)
        for s in &sessions {
            if s.role == "worker" && s.id.starts_with(reference) {
                return Some(s.id.clone());
            }
        }
        None
    }
}

fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn unreported_exit_report(worker: &str, exit_code: i32) -> WorkerReport {
    WorkerReport {
        worker: worker.to_string(),
        summary: format!("worker 意外退出(exit={exit_code})，未回传结果"),
        level: "failure".to_string(),
        ts: chrono_now_ms(),
    }
}

#[cfg(test)]
mod tests {
    use super::unreported_exit_report;

    #[test]
    fn natural_busy_exit_becomes_failure_report() {
        let report = unreported_exit_report("worker-7", 137);
        assert_eq!(report.worker, "worker-7");
        assert_eq!(report.level, "failure");
        assert!(report.summary.contains("exit=137"));
        assert!(report.summary.contains("未回传结果"));
    }
}
