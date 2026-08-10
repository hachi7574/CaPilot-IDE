//! Local-IPC orchestration server used by the Rust `capilot` CLI.
//!
//! `capilot dispatch|status|report` → shell shim → Unix socket → this
//! dispatcher → worker PTY.

use crate::agent_runtime::pty::PtyManager;
use crate::orchestration::smart_return;
use crate::orchestration::{TaskDispatchRequest, TaskRecord, TaskReportRequest, TaskStatus};
use crate::persistence::{
    agent_dir, read_agent_meta, write_agent_meta, AgentSessionRecord, Persistence,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

const MASTER_RESULT_MAX_CHARS: usize = 8_000;

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
    active_task_id: Option<String>,
    current_task_title: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<TaskStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct DispatchResponse {
    ok: bool,
    task_id: String,
    status: TaskStatus,
    worker_id: String,
    worker_display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct TaskReportResponse {
    ok: bool,
    task_id: String,
    status: TaskStatus,
    worker_id: String,
    worker_display_name: String,
}

#[derive(Debug, Clone)]
struct TaskCompletion {
    response: TaskReportResponse,
    report: WorkerReport,
    master_agent_id: String,
    master_message: String,
    worker_released: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ErrorResponse {
    ok: bool,
    code: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reference: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    available_workers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    candidates: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkerResolveError {
    NotFound {
        reference: String,
        available_workers: Vec<String>,
    },
    Ambiguous {
        reference: String,
        candidates: Vec<String>,
    },
    Storage(String),
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SocketRequest {
    Dispatch {
        worker: String,
        title: Option<String>,
        prompt: String,
    },
    Status,
    Report {
        task_id: String,
        #[serde(default)]
        reporter_agent_id: String,
        status: String,
        result: Option<String>,
        error: Option<String>,
    },
    Ping,
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
    /// Phase 1 dispatches are serialized from availability check through PTY
    /// write so two concurrent callers cannot both claim the same idle worker.
    dispatch_lock: Mutex<()>,
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
            dispatch_lock: Mutex::new(()),
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
        self.workers
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_default();
    }

    pub fn unregister_worker(&self, id: &str) {
        self.workers.lock().unwrap().remove(id);
    }

    /// Type into an interactive coding-agent TUI and submit it like a user.
    /// Sending `text + \r` in one PTY write is treated as a paste burst by
    /// Claude/Codex: the text appears in the composer, but Enter is not acted
    /// on. Keep the submit keystroke in a separate write after the TUI's paste
    /// detector has settled.
    fn write_and_submit(&self, id: &str, text: &str) -> Result<(), String> {
        self.pty
            .write(id, text.as_bytes())
            .map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(50));
        self.pty.write(id, b"\r").map_err(|e| e.to_string())
    }

    /// Atomically check-and-mark a worker busy. Returns false if it was already
    /// busy. The check and the mark happen under a single lock acquisition so
    /// two concurrent dispatches can't both claim the same idle worker.
    fn try_mark_busy(&self, id: &str, task_id: &str, title: &str) -> bool {
        let mut workers = self.workers.lock().unwrap();
        if workers
            .get(id)
            .is_some_and(|ws| ws.status == WorkerStatus::Busy)
        {
            return false;
        }
        let ws = workers.entry(id.to_string()).or_default();
        ws.status = WorkerStatus::Busy;
        ws.active_task_id = Some(task_id.to_string());
        ws.current_task_title = Some(title.to_string());
        true
    }

    fn worker_is_busy(&self, id: &str) -> bool {
        self.workers
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|worker| worker.status == WorkerStatus::Busy)
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
        ws.active_task_id = None;
        ws.current_task_title = None;
        true
    }

    /// Mark a worker idle and notify the frontend if its state changed.
    fn set_worker_idle(&self, id: &str, app: &AppHandle) {
        if self.mark_idle(id) {
            self.emit_worker_status(id, app);
        }
    }

    /// Release a Worker only when the completed Task is still the Task bound
    /// to it. A late report for an older Task must never unlock a newer one.
    fn release_worker_task(&self, worker_id: &str, task_id: &str) -> bool {
        let mut workers = self.workers.lock().unwrap();
        let Some(worker) = workers.get_mut(worker_id) else {
            return false;
        };
        if worker.active_task_id.as_deref() != Some(task_id) {
            return false;
        }
        worker.status = WorkerStatus::Idle;
        worker.active_task_id = None;
        worker.current_task_title = None;
        true
    }

    /// A Busy worker that exits without `capilot report` has violated the
    /// completion contract. Report the failure before returning it to idle.
    pub fn worker_ended_naturally(&self, id: &str, exit_code: i32, app: &AppHandle) -> bool {
        match self.apply_worker_exit(id, exit_code) {
            Ok(None) => return false,
            Ok(Some(completion)) => {
                self.publish_task_completion(completion, Some(app), |master_id, message| {
                    if self.pty.is_alive(master_id) {
                        self.write_and_submit(master_id, message)
                    } else {
                        Ok(())
                    }
                });
            }
            Err(response) => {
                log::warn!("failed to close active Task after Worker {id} exited: {response}");
                self.set_worker_idle(id, app);
            }
        }
        self.mark_attention(id, "error", app);
        true
    }

    fn apply_worker_exit(
        &self,
        id: &str,
        exit_code: i32,
    ) -> Result<Option<TaskCompletion>, String> {
        let active_task_id = self
            .workers
            .lock()
            .unwrap()
            .get(id)
            .filter(|worker| worker.status == WorkerStatus::Busy)
            .and_then(|worker| worker.active_task_id.clone());
        let Some(task_id) = active_task_id else {
            return Ok(None);
        };
        let error =
            format!("Worker process exited before reporting task completion (exit={exit_code})");
        self.apply_task_report(TaskReportRequest {
            task_id,
            reporter_agent_id: id.to_string(),
            status: TaskStatus::Failed,
            result: None,
            error: Some(error),
            artifact: None,
        })
        .map(Some)
    }

    pub fn mark_attention(&self, id: &str, reason: &str, app: &AppHandle) {
        let now = chrono_now_ms();
        let project = if let Ok(db) = self.persistence.db().lock() {
            let _ = db.update_attention(id, Some(reason), now);
            db.get(id).ok().flatten().map(|record| record.project)
        } else {
            None
        };
        if let Some(project) = project {
            if let Ok(mut meta) = read_agent_meta(&project, id) {
                meta.requires_attention = true;
                meta.attention_reason = Some(reason.to_string());
                meta.updated_at = now;
                let _ = write_agent_meta(&project, &meta);
            }
        }
        let payload = serde_json::json!({ "id": id, "reason": reason });
        let _ = app.emit("agent://attention", &payload);
        let handle = app.clone();
        let esp_payload = serde_json::json!({ "op": "attention", "id": id, "reason": reason });
        tauri::async_runtime::spawn(async move {
            let manager = handle.state::<crate::esp::manager::EspManager>();
            if let Err(error) = manager.send_json(&esp_payload).await {
                if !matches!(error, crate::esp::transport::EspError::NotConnected) {
                    log::warn!("failed to forward attention to ESP: {error}");
                }
            }
        });
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
                    let _ = app.emit("agent://removed", serde_json::json!({ "id": worker.id }));
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
                let msg = format!("[编排] worker {} 完成：{}", report.worker, report.summary);
                if let Err(error) = self.write_and_submit(&master, &msg) {
                    log::warn!("failed to submit worker report to master {master}: {error}");
                }
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
                    (s.to_string(), ws.current_task_title.clone())
                }
                None => ("idle".to_string(), None),
            }
        };
        let session = self
            .persistence
            .db()
            .lock()
            .ok()
            .and_then(|db| db.get(id).ok().flatten());
        let _ = app.emit(
            "orchestration://event",
            WorkerInfo {
                id: id.to_string(),
                title: session
                    .as_ref()
                    .map(|record| record.title.clone())
                    .unwrap_or_else(|| id.to_string()),
                runtime: session
                    .as_ref()
                    .map(|record| record.runtime.clone())
                    .unwrap_or_default(),
                status,
                last_task,
            },
        );
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
            log::info!("worker {id} PTY is no longer alive — failing its active Task");
            self.worker_ended_naturally(&id, -1, app);
        }
    }

    async fn run_socket(self: Arc<Self>, app: AppHandle) {
        // Ensure the socket directory exists before binding.
        if let Some(parent) = self.socket_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                log::error!("capilot socket dir {} create failed: {e}", parent.display());
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
        let _ = std::fs::write(
            socket_path_file(),
            self.socket_path.to_string_lossy().as_bytes(),
        );

        log::info!(
            "capilot orchestrator listening on {}",
            self.socket_path.display()
        );
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
        if let Ok(request) = serde_json::from_str::<SocketRequest>(line) {
            return match request {
                SocketRequest::Dispatch {
                    worker,
                    title,
                    prompt,
                } => self.dispatch(
                    TaskDispatchRequest {
                        worker_reference: worker,
                        title,
                        prompt,
                    },
                    app,
                ),
                SocketRequest::Status => self.status(),
                SocketRequest::Report {
                    task_id,
                    reporter_agent_id,
                    status,
                    result,
                    error,
                } => {
                    let status = match status.parse::<TaskStatus>() {
                        Ok(status) => status,
                        Err(_) => {
                            return error_json(
                                "invalid_report_status",
                                "report status must be `succeeded` or `failed`",
                                None,
                                vec![],
                                vec![],
                                Some(task_id),
                            )
                        }
                    };
                    self.report_task(
                        TaskReportRequest {
                            task_id,
                            reporter_agent_id,
                            status,
                            result,
                            error,
                            artifact: None,
                        },
                        app,
                    )
                }
                SocketRequest::Ping => json_ok(serde_json::json!({ "pong": true })),
            };
        }
        self.handle_legacy_line(line, app)
    }

    fn handle_legacy_line(&self, line: &str, app: &AppHandle) -> String {
        let mut parts = line.splitn(3, ' ');
        let cmd = parts.next().unwrap_or("").trim();
        match cmd {
            "dispatch" => {
                let (worker, prompt) = parse_legacy_dispatch(line).unwrap_or_default();
                self.dispatch(
                    TaskDispatchRequest {
                        worker_reference: worker,
                        title: None,
                        prompt,
                    },
                    app,
                )
            }
            "status" => self.status(),
            "report" => {
                let first = parts.next().unwrap_or("").trim().to_string();
                let rest = parts.next().unwrap_or("").trim().to_string();
                self.legacy_report(&first, &rest, app)
            }
            "ping" => "OK pong".to_string(),
            _ => format!("ERR unknown command: {cmd}"),
        }
    }

    fn dispatch(&self, request: TaskDispatchRequest, app: &AppHandle) -> String {
        self.dispatch_with(
            &request.worker_reference,
            request.title.as_deref(),
            &request.prompt,
            Some(app),
            |id| self.pty.is_alive(id),
            |id, message| self.write_and_submit(id, message),
        )
    }

    fn dispatch_with<IsAlive, Write>(
        &self,
        worker_reference: &str,
        title: Option<&str>,
        prompt: &str,
        app: Option<&AppHandle>,
        is_alive: IsAlive,
        write: Write,
    ) -> String
    where
        IsAlive: FnOnce(&str) -> bool,
        Write: FnOnce(&str, &str) -> Result<(), String>,
    {
        if worker_reference.trim().is_empty() || prompt.trim().is_empty() {
            return error_json(
                "invalid_request",
                "usage: dispatch --worker <name> [--title <title>] --prompt <prompt>",
                None,
                vec![],
                vec![],
                None,
            );
        }

        let _dispatch_guard = self.dispatch_lock.lock().unwrap();
        let Some(master_id) = self.master_id.lock().unwrap().clone() else {
            return error_json(
                "master_not_found",
                "当前没有可用的 Master Agent",
                None,
                vec![],
                vec![],
                None,
            );
        };
        let master = match self.persistence.db().lock().unwrap().get(&master_id) {
            Ok(Some(master)) => master,
            Ok(None) => {
                return error_json(
                    "master_not_found",
                    "Master Agent 的 session 不存在",
                    None,
                    vec![],
                    vec![],
                    None,
                )
            }
            Err(error) => return storage_error_json(error.to_string(), None),
        };

        let worker = match self.resolve_worker_in_project(&master.project, worker_reference.trim())
        {
            Ok(worker) => worker,
            Err(WorkerResolveError::NotFound {
                reference,
                available_workers,
            }) => {
                let available = if available_workers.is_empty() {
                    "（无）".to_string()
                } else {
                    available_workers.join("、")
                };
                return error_json(
                    "worker_not_found",
                    &format!("找不到 Worker“{reference}”。当前项目可用 Worker：{available}。"),
                    Some(reference),
                    available_workers,
                    vec![],
                    None,
                );
            }
            Err(WorkerResolveError::Ambiguous {
                reference,
                candidates,
            }) => {
                return error_json(
                    "worker_ambiguous",
                    &format!(
                        "Worker 引用“{reference}”不明确。候选：{}。",
                        candidates.join("、")
                    ),
                    Some(reference),
                    vec![],
                    candidates,
                    None,
                );
            }
            Err(WorkerResolveError::Storage(error)) => return storage_error_json(error, None),
        };

        if self.worker_is_busy(&worker.id) {
            return error_json(
                "worker_busy",
                &format!("Worker“{}”正在执行其他 Task", worker.title),
                Some(worker_reference.to_string()),
                vec![],
                vec![],
                None,
            );
        }
        if !is_alive(&worker.id) {
            return error_json(
                "worker_offline",
                &format!("Worker“{}”未运行，请先在 IDE 中恢复它", worker.title),
                Some(worker_reference.to_string()),
                vec![],
                vec![],
                None,
            );
        }

        let task_id = format!("task_{}", uuid::Uuid::new_v4().simple());
        let task_title = normalized_task_title(title, prompt);
        let created_at = chrono_now_ms();
        let task = TaskRecord {
            task_id: task_id.clone(),
            project_id: master.project.clone(),
            master_agent_id: master.id,
            worker_agent_id: worker.id.clone(),
            worker_display_name: worker.title.clone(),
            title: task_title.clone(),
            prompt: prompt.trim().to_string(),
            status: TaskStatus::Queued,
            created_at,
            started_at: None,
            finished_at: None,
            result: None,
            error: None,
            artifact: None,
        };
        if let Err(error) = self.persistence.db().lock().unwrap().task_insert(&task) {
            return storage_error_json(error.to_string(), Some(task_id));
        }

        // The dispatch mutex makes this second check deterministic. If another
        // non-dispatch path changed the worker anyway, fail the persisted task
        // rather than leaving it queued forever.
        if !self.try_mark_busy(&worker.id, &task_id, &task_title) {
            let error = format!("Worker“{}”正在执行其他 Task", worker.title);
            let _ =
                self.persistence
                    .db()
                    .lock()
                    .unwrap()
                    .task_fail(&task_id, &error, chrono_now_ms());
            return error_json(
                "worker_busy",
                &error,
                Some(worker_reference.to_string()),
                vec![],
                vec![],
                Some(task_id),
            );
        }

        let started_at = chrono_now_ms();
        match self
            .persistence
            .db()
            .lock()
            .unwrap()
            .task_mark_running(&task_id, started_at)
        {
            Ok(true) => {}
            Ok(false) => {
                self.rollback_dispatch_failure(
                    &worker.id,
                    &task_id,
                    "Task 无法从 queued 更新为 running",
                    app,
                );
                return error_json(
                    "task_transition_failed",
                    "Task 无法从 queued 更新为 running",
                    None,
                    vec![],
                    vec![],
                    Some(task_id),
                );
            }
            Err(error) => {
                let message = format!("Task 状态更新失败：{error}");
                self.rollback_dispatch_failure(&worker.id, &task_id, &message, app);
                return storage_error_json(message, Some(task_id));
            }
        }

        let worker_prompt = build_worker_task_prompt(&task_id, &task_title, prompt.trim());
        if let Err(error) = write(&worker.id, &worker_prompt) {
            let message = format!("向 Worker 写入 Task 失败：{error}");
            self.rollback_dispatch_failure(&worker.id, &task_id, &message, app);
            return error_json(
                "worker_write_failed",
                &message,
                Some(worker_reference.to_string()),
                vec![],
                vec![],
                Some(task_id),
            );
        }

        if let Some(app) = app {
            self.emit_worker_status(&worker.id, app);
        }
        serde_json::to_string(&DispatchResponse {
            ok: true,
            task_id,
            status: TaskStatus::Running,
            worker_id: worker.id,
            worker_display_name: worker.title,
        })
        .unwrap_or_else(|_| "{\"ok\":false,\"code\":\"serialization_failed\"}".to_string())
    }

    fn rollback_dispatch_failure(
        &self,
        worker_id: &str,
        task_id: &str,
        error: &str,
        app: Option<&AppHandle>,
    ) {
        let _ = self
            .persistence
            .db()
            .lock()
            .unwrap()
            .task_fail(task_id, error, chrono_now_ms());
        let changed = self.mark_idle(worker_id);
        if changed {
            if let Some(app) = app {
                self.emit_worker_status(worker_id, app);
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
                last_task: state.and_then(|ws| ws.current_task_title.clone()),
            });
        }
        infos
    }

    fn status(&self) -> String {
        let project = self
            .master_id
            .lock()
            .unwrap()
            .clone()
            .and_then(|master_id| {
                self.persistence
                    .db()
                    .lock()
                    .ok()
                    .and_then(|db| db.get(&master_id).ok().flatten())
                    .map(|master| master.project)
            });
        let infos = match project {
            Some(project) => self.workers_list_in_project(&project),
            None => vec![],
        };
        serde_json::to_string(&infos).unwrap_or_else(|_| "[]".to_string())
    }

    fn workers_list_in_project(&self, project_id: &str) -> Vec<WorkerInfo> {
        self.workers_list()
            .into_iter()
            .filter(|worker| {
                self.persistence
                    .db()
                    .lock()
                    .ok()
                    .and_then(|db| db.get(&worker.id).ok().flatten())
                    .is_some_and(|record| record.project == project_id)
            })
            .collect()
    }

    fn report_task(&self, request: TaskReportRequest, app: &AppHandle) -> String {
        self.report_task_with(request, Some(app), |master_id, message| {
            if self.pty.is_alive(master_id) {
                self.write_and_submit(master_id, message)
            } else {
                Ok(())
            }
        })
    }

    fn report_task_with<Deliver>(
        &self,
        request: TaskReportRequest,
        app: Option<&AppHandle>,
        deliver_to_master: Deliver,
    ) -> String
    where
        Deliver: FnOnce(&str, &str) -> Result<(), String>,
    {
        match self.apply_task_report(request) {
            Ok(completion) => self.publish_task_completion(completion, app, deliver_to_master),
            Err(response) => response,
        }
    }

    fn apply_task_report(&self, request: TaskReportRequest) -> Result<TaskCompletion, String> {
        if request.reporter_agent_id.trim().is_empty() {
            return Err(error_json(
                "reporter_missing",
                "缺少 CAPILOT_AGENT_ID，Task report 必须由 CaPilot Worker 发出",
                None,
                vec![],
                vec![],
                Some(request.task_id),
            ));
        }
        let content = match request.status {
            TaskStatus::Succeeded => match (&request.result, &request.error) {
                (Some(result), None) if !result.trim().is_empty() => result.clone(),
                _ => {
                    return Err(error_json(
                        "invalid_report_content",
                        "succeeded report 必须且只能包含非空 result",
                        None,
                        vec![],
                        vec![],
                        Some(request.task_id),
                    ))
                }
            },
            TaskStatus::Failed => match (&request.result, &request.error) {
                (None, Some(error)) if !error.trim().is_empty() => error.clone(),
                _ => {
                    return Err(error_json(
                        "invalid_report_content",
                        "failed report 必须且只能包含非空 error",
                        None,
                        vec![],
                        vec![],
                        Some(request.task_id),
                    ))
                }
            },
            _ => {
                return Err(error_json(
                    "invalid_report_status",
                    "report status must be `succeeded` or `failed`",
                    None,
                    vec![],
                    vec![],
                    Some(request.task_id),
                ))
            }
        };

        let task = match self
            .persistence
            .db()
            .lock()
            .unwrap()
            .task_get(&request.task_id)
        {
            Ok(Some(task)) => task,
            Ok(None) => {
                return Err(error_json(
                    "task_not_found",
                    &format!("Task 不存在：{}", request.task_id),
                    None,
                    vec![],
                    vec![],
                    Some(request.task_id),
                ))
            }
            Err(error) => return Err(storage_error_json(error.to_string(), Some(request.task_id))),
        };
        if task.status != TaskStatus::Running || !task.status.can_transition_to(request.status) {
            return Err(error_json(
                "task_not_running",
                &format!(
                    "Task {} 当前状态为 {}，不能再次 report",
                    task.task_id, task.status
                ),
                None,
                vec![],
                vec![],
                Some(task.task_id),
            ));
        }
        if request.reporter_agent_id != task.worker_agent_id {
            return Err(error_json(
                "reporter_mismatch",
                &format!("当前 Agent 不是 Task {} 的 Worker", task.task_id),
                Some(request.reporter_agent_id),
                vec![],
                vec![],
                Some(task.task_id),
            ));
        }

        let finished_at = chrono_now_ms();
        let changed = {
            let db = self.persistence.db().lock().unwrap();
            match request.status {
                TaskStatus::Succeeded => db.task_complete(
                    &task.task_id,
                    &content,
                    request.artifact.as_ref(),
                    finished_at,
                ),
                TaskStatus::Failed => db.task_fail(&task.task_id, &content, finished_at),
                _ => unreachable!(),
            }
        };
        match changed {
            Ok(true) => {}
            Ok(false) => {
                return Err(error_json(
                    "task_transition_rejected",
                    "Task 已被其他 report 完成或当前状态不允许转换",
                    None,
                    vec![],
                    vec![],
                    Some(task.task_id),
                ))
            }
            Err(error) => return Err(storage_error_json(error.to_string(), Some(task.task_id))),
        }

        let worker_released = self.release_worker_task(&task.worker_agent_id, &task.task_id);
        let master_message = task_result_message(&task, request.status, &content);
        Ok(TaskCompletion {
            response: TaskReportResponse {
                ok: true,
                task_id: task.task_id.clone(),
                status: request.status,
                worker_id: task.worker_agent_id.clone(),
                worker_display_name: task.worker_display_name.clone(),
            },
            report: WorkerReport {
                worker: task.worker_display_name.clone(),
                summary: content,
                level: if request.status == TaskStatus::Failed {
                    "failure".to_string()
                } else {
                    "full".to_string()
                },
                ts: finished_at,
                task_id: Some(task.task_id.clone()),
                status: Some(request.status),
            },
            master_agent_id: task.master_agent_id,
            master_message,
            worker_released,
        })
    }

    fn publish_task_completion<Deliver>(
        &self,
        completion: TaskCompletion,
        app: Option<&AppHandle>,
        deliver_to_master: Deliver,
    ) -> String
    where
        Deliver: FnOnce(&str, &str) -> Result<(), String>,
    {
        self.reports.lock().unwrap().push(completion.report.clone());
        if let Some(app) = app {
            let _ = app.emit("orchestration://report", completion.report.clone());
            if completion.worker_released {
                self.emit_worker_status(&completion.response.worker_id, app);
            }
        }
        if let Err(error) =
            deliver_to_master(&completion.master_agent_id, &completion.master_message)
        {
            log::warn!(
                "failed to submit Task {} result to Master {}: {error}",
                completion.response.task_id,
                completion.master_agent_id
            );
        }
        serde_json::to_string(&completion.response)
            .unwrap_or_else(|_| "{\"ok\":false,\"code\":\"serialization_failed\"}".to_string())
    }

    /// Deprecated name-based report. It may still surface a legacy message,
    /// but never mutates Task state or releases a Worker.
    fn legacy_report(&self, first: &str, rest: &str, app: &AppHandle) -> String {
        let worker_id = self.resolve_worker_for_legacy_report(first);
        let (worker, summary) = if worker_id.is_some() {
            (first.to_string(), rest.to_string())
        } else if first.is_empty() {
            ("unknown".to_string(), String::new())
        } else {
            // No known worker named `first` — treat the whole line as summary.
            (
                "unknown".to_string(),
                format!("{first} {rest}").trim().to_string(),
            )
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
            task_id: None,
            status: None,
        };
        self.publish_report(report, app);
        format!("OK legacy report registered ({level}); deprecated: Task state unchanged")
    }

    /// Resolve a worker inside one project. Display names require an exact
    /// match; id prefixes are accepted only when they identify exactly one row.
    fn resolve_worker_in_project(
        &self,
        project_id: &str,
        reference: &str,
    ) -> Result<AgentSessionRecord, WorkerResolveError> {
        if reference.is_empty() {
            return Err(WorkerResolveError::NotFound {
                reference: reference.to_string(),
                available_workers: vec![],
            });
        }
        let db = self.persistence.db().lock().unwrap();
        let sessions = db
            .list_all()
            .map_err(|error| WorkerResolveError::Storage(error.to_string()))?;
        let workers: Vec<AgentSessionRecord> = sessions
            .into_iter()
            .filter(|session| session.project == project_id && session.role == "worker")
            .collect();
        let mut available_workers: Vec<String> =
            workers.iter().map(|worker| worker.title.clone()).collect();
        available_workers.sort();
        available_workers.dedup();

        let exact: Vec<&AgentSessionRecord> = workers
            .iter()
            .filter(|worker| worker.id == reference || worker.title == reference)
            .collect();
        if exact.len() == 1 {
            return Ok(exact[0].clone());
        }
        if exact.len() > 1 {
            return Err(WorkerResolveError::Ambiguous {
                reference: reference.to_string(),
                candidates: exact
                    .iter()
                    .map(|worker| format!("{} ({})", worker.title, worker.id))
                    .collect(),
            });
        }

        let prefix: Vec<&AgentSessionRecord> = workers
            .iter()
            .filter(|worker| worker.id.starts_with(reference))
            .collect();
        if prefix.len() == 1 {
            return Ok(prefix[0].clone());
        }
        if prefix.len() > 1 {
            return Err(WorkerResolveError::Ambiguous {
                reference: reference.to_string(),
                candidates: prefix
                    .iter()
                    .map(|worker| format!("{} ({})", worker.title, worker.id))
                    .collect(),
            });
        }
        Err(WorkerResolveError::NotFound {
            reference: reference.to_string(),
            available_workers,
        })
    }

    /// Temporary Step 2 compatibility for the old name-based report command.
    /// Unlike the previous implementation, ambiguous global matches are
    /// rejected. Step 3 replaces this with task-id lookup.
    fn resolve_worker_for_legacy_report(&self, reference: &str) -> Option<String> {
        if reference.is_empty() {
            return None;
        }
        let sessions = self.persistence.db().lock().ok()?.list_all().ok()?;
        let matches: Vec<&AgentSessionRecord> = sessions
            .iter()
            .filter(|session| {
                session.role == "worker"
                    && (session.id == reference
                        || session.title == reference
                        || session.id.starts_with(reference))
            })
            .collect();
        (matches.len() == 1).then(|| matches[0].id.clone())
    }
}

fn normalized_task_title(title: Option<&str>, prompt: &str) -> String {
    let source = title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .or_else(|| prompt.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or("未命名 Task");
    source.chars().take(80).collect()
}

fn parse_legacy_dispatch(line: &str) -> Option<(String, String)> {
    let mut parts = line.splitn(3, ' ');
    if parts.next()?.trim() != "dispatch" {
        return None;
    }
    let worker = parts.next()?.trim();
    let prompt = parts.next()?.trim();
    if worker.is_empty() || prompt.is_empty() {
        return None;
    }
    Some((worker.to_string(), prompt.to_string()))
}

fn build_worker_task_prompt(task_id: &str, title: &str, prompt: &str) -> String {
    format!(
        "[CaPilot Task]\n\nTask ID: {task_id}\nTitle: {title}\n\n任务：\n{prompt}\n\n完成要求：\n- 这是 CaPilot Task：{task_id}\n- 成功后执行：capilot report {task_id} succeeded \"<结果摘要>\"\n- 失败后执行：capilot report {task_id} failed \"<错误原因>\"\n- 不要使用其他 task_id"
    )
}

fn task_result_message(task: &TaskRecord, status: TaskStatus, content: &str) -> String {
    let body_label = if status == TaskStatus::Succeeded {
        "Result"
    } else {
        "Error"
    };
    let (presented, truncated) = truncate_for_master(content);
    let suffix = if truncated {
        "\n\n[Result truncated for Master context; full report remains in Task storage.]"
    } else {
        ""
    };
    format!(
        "[CaPilot Task Result]\n\nTask ID: {}\nWorker: {}\nStatus: {}\n\n{}:\n{}",
        task.task_id,
        task.worker_display_name,
        status,
        body_label,
        format_args!("{presented}{suffix}")
    )
}

fn truncate_for_master(content: &str) -> (String, bool) {
    if content.chars().count() <= MASTER_RESULT_MAX_CHARS {
        return (content.to_string(), false);
    }
    (
        content.chars().take(MASTER_RESULT_MAX_CHARS).collect(),
        true,
    )
}

fn json_ok(value: serde_json::Value) -> String {
    serde_json::to_string(&serde_json::json!({ "ok": true, "result": value }))
        .unwrap_or_else(|_| "{\"ok\":false,\"code\":\"serialization_failed\"}".to_string())
}

fn storage_error_json(message: String, task_id: Option<String>) -> String {
    error_json("storage_error", &message, None, vec![], vec![], task_id)
}

fn error_json(
    code: &str,
    message: &str,
    reference: Option<String>,
    available_workers: Vec<String>,
    candidates: Vec<String>,
    task_id: Option<String>,
) -> String {
    serde_json::to_string(&ErrorResponse {
        ok: false,
        code: code.to_string(),
        message: message.to_string(),
        reference,
        available_workers,
        candidates,
        task_id,
    })
    .unwrap_or_else(|_| "{\"ok\":false,\"code\":\"serialization_failed\"}".to_string())
}

fn chrono_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn session(id: &str, project: &str, role: &str, title: &str) -> AgentSessionRecord {
        AgentSessionRecord {
            id: id.to_string(),
            workspace_id: None,
            requires_attention: false,
            attention_reason: None,
            project: project.to_string(),
            role: role.to_string(),
            runtime: "codex".to_string(),
            resume_key: None,
            cwd: PathBuf::from("/tmp"),
            title: title.to_string(),
            status: "running".to_string(),
            mode: "ask".to_string(),
            speed: "auto".to_string(),
            model: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    fn fixture(records: &[AgentSessionRecord]) -> (Dispatcher, Arc<Persistence>, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "capilot-dispatcher-{}.db",
            uuid::Uuid::new_v4().simple()
        ));
        let persistence = Arc::new(Persistence::open_test(&path).unwrap());
        {
            let db = persistence.db().lock().unwrap();
            for record in records {
                db.insert(record).unwrap();
            }
        }
        let dispatcher = Dispatcher::new(Arc::new(PtyManager::new()), persistence.clone());
        dispatcher.set_master(Some("master-a".to_string()));
        dispatcher.refresh_workers();
        (dispatcher, persistence, path)
    }

    fn response_json(response: &str) -> Value {
        serde_json::from_str(response)
            .unwrap_or_else(|error| panic!("invalid response JSON ({error}): {response}"))
    }

    fn dispatch_running_task(dispatcher: &Dispatcher, prompt: &str) -> String {
        let response = dispatcher.dispatch_with(
            "阿比西尼亚",
            Some("测试 Task"),
            prompt,
            None,
            |_| true,
            |_, _| Ok(()),
        );
        let json = response_json(&response);
        assert_eq!(json["ok"], true, "dispatch failed: {response}");
        json["task_id"].as_str().unwrap().to_string()
    }

    fn task_report(
        task_id: &str,
        reporter_agent_id: &str,
        status: TaskStatus,
        content: &str,
    ) -> TaskReportRequest {
        TaskReportRequest {
            task_id: task_id.to_string(),
            reporter_agent_id: reporter_agent_id.to_string(),
            status,
            result: (status == TaskStatus::Succeeded).then(|| content.to_string()),
            error: (status == TaskStatus::Failed).then(|| content.to_string()),
            artifact: None,
        }
    }

    #[test]
    fn worker_resolution_is_exact_and_project_scoped() {
        let records = [
            session("master-a", "project-a", "master", "Master"),
            session("agent-a8217c", "project-a", "worker", "阿比西尼亚"),
            session("agent-b9000", "project-b", "worker", "阿比西尼亚"),
            session("agent-a9000", "project-a", "worker", "布偶猫"),
        ];
        let (dispatcher, _, path) = fixture(&records);

        assert_eq!(
            dispatcher
                .resolve_worker_in_project("project-a", "阿比西尼亚")
                .unwrap()
                .id,
            "agent-a8217c"
        );
        assert_eq!(
            dispatcher
                .resolve_worker_in_project("project-a", "agent-a9000")
                .unwrap()
                .title,
            "布偶猫"
        );
        assert_eq!(
            dispatcher
                .resolve_worker_in_project("project-b", "阿比西尼亚")
                .unwrap()
                .id,
            "agent-b9000"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn missing_worker_lists_only_current_project_workers() {
        let records = [
            session("master-a", "project-a", "master", "Master"),
            session("agent-a1", "project-a", "worker", "阿比西尼亚"),
            session("agent-b1", "project-b", "worker", "加菲猫"),
        ];
        let (dispatcher, _, path) = fixture(&records);

        let response =
            dispatcher.dispatch_with("加菲猫", None, "检查 README", None, |_| true, |_, _| Ok(()));
        let json = response_json(&response);
        assert_eq!(json["code"], "worker_not_found");
        assert_eq!(json["reference"], "加菲猫");
        assert_eq!(json["available_workers"], serde_json::json!(["阿比西尼亚"]));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn unique_id_prefix_resolves_and_ambiguous_prefix_or_name_is_rejected() {
        let records = [
            session("master-a", "project-a", "master", "Master"),
            session("agent-alpha-1", "project-a", "worker", "暹罗猫"),
            session("agent-alpha-2", "project-a", "worker", "布偶猫"),
            session("agent-beta-1", "project-a", "worker", "布偶猫"),
        ];
        let (dispatcher, _, path) = fixture(&records);

        assert_eq!(
            dispatcher
                .resolve_worker_in_project("project-a", "agent-beta")
                .unwrap()
                .id,
            "agent-beta-1"
        );
        assert!(matches!(
            dispatcher.resolve_worker_in_project("project-a", "agent-alpha"),
            Err(WorkerResolveError::Ambiguous { .. })
        ));
        assert!(matches!(
            dispatcher.resolve_worker_in_project("project-a", "布偶猫"),
            Err(WorkerResolveError::Ambiguous { .. })
        ));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn successful_dispatch_persists_running_task_binds_worker_and_returns_task_id() {
        let records = [
            session("master-a", "project-a", "master", "Master"),
            session("agent-a8217c", "project-a", "worker", "阿比西尼亚"),
        ];
        let (dispatcher, persistence, path) = fixture(&records);
        let delivered = Arc::new(Mutex::new(None::<(String, String)>));
        let delivered_for_write = delivered.clone();

        let response = dispatcher.dispatch_with(
            "阿比西尼亚",
            Some("检查 README"),
            "检查 README 是否有明显错误",
            None,
            |id| id == "agent-a8217c",
            move |id, prompt| {
                *delivered_for_write.lock().unwrap() = Some((id.to_string(), prompt.to_string()));
                Ok(())
            },
        );
        let parsed: DispatchResponse = serde_json::from_str(&response).unwrap();
        assert!(parsed.ok);
        assert!(parsed.task_id.starts_with("task_"));
        assert_eq!(parsed.status, TaskStatus::Running);
        assert_eq!(parsed.worker_id, "agent-a8217c");
        assert_eq!(parsed.worker_display_name, "阿比西尼亚");

        let task = persistence
            .db()
            .lock()
            .unwrap()
            .task_get(&parsed.task_id)
            .unwrap()
            .unwrap();
        assert_eq!(task.project_id, "project-a");
        assert_eq!(task.master_agent_id, "master-a");
        assert_eq!(task.worker_agent_id, "agent-a8217c");
        assert_eq!(task.status, TaskStatus::Running);
        assert!(task.started_at.is_some());

        let state = dispatcher.workers.lock().unwrap();
        let worker = state.get("agent-a8217c").unwrap();
        assert_eq!(worker.status, WorkerStatus::Busy);
        assert_eq!(
            worker.active_task_id.as_deref(),
            Some(parsed.task_id.as_str())
        );
        assert_eq!(worker.current_task_title.as_deref(), Some("检查 README"));
        drop(state);

        let (worker_id, prompt) = delivered.lock().unwrap().clone().unwrap();
        assert_eq!(worker_id, "agent-a8217c");
        assert!(prompt.contains(&format!("Task ID: {}", parsed.task_id)));
        assert!(prompt.contains(&format!("capilot report {} succeeded", parsed.task_id)));
        assert!(prompt.contains("检查 README 是否有明显错误"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn busy_worker_rejects_second_dispatch_without_creating_another_task() {
        let records = [
            session("master-a", "project-a", "master", "Master"),
            session("agent-a1", "project-a", "worker", "阿比西尼亚"),
        ];
        let (dispatcher, persistence, path) = fixture(&records);
        let first = dispatcher.dispatch_with(
            "阿比西尼亚",
            None,
            "第一个任务",
            None,
            |_| true,
            |_, _| Ok(()),
        );
        assert_eq!(response_json(&first)["ok"], true);

        let second = dispatcher.dispatch_with(
            "阿比西尼亚",
            None,
            "第二个任务",
            None,
            |_| true,
            |_, _| Ok(()),
        );
        assert_eq!(response_json(&second)["code"], "worker_busy");
        assert_eq!(
            persistence
                .db()
                .lock()
                .unwrap()
                .task_list_by_project("project-a", 10)
                .unwrap()
                .len(),
            1
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn write_failure_fails_task_and_releases_worker() {
        let records = [
            session("master-a", "project-a", "master", "Master"),
            session("agent-a1", "project-a", "worker", "阿比西尼亚"),
        ];
        let (dispatcher, persistence, path) = fixture(&records);

        let response = dispatcher.dispatch_with(
            "阿比西尼亚",
            Some("失败任务"),
            "无法写入的任务",
            None,
            |_| true,
            |_, _| Err("PTY closed".to_string()),
        );
        let json = response_json(&response);
        assert_eq!(json["code"], "worker_write_failed");
        let task_id = json["task_id"].as_str().unwrap();
        let task = persistence
            .db()
            .lock()
            .unwrap()
            .task_get(task_id)
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert!(task.error.unwrap().contains("PTY closed"));

        let state = dispatcher.workers.lock().unwrap();
        let worker = state.get("agent-a1").unwrap();
        assert_eq!(worker.status, WorkerStatus::Idle);
        assert_eq!(worker.active_task_id, None);
        assert_eq!(worker.current_task_title, None);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn succeeded_report_persists_result_releases_worker_and_notifies_correct_master() {
        let records = [
            session("master-a", "project-a", "master", "Master"),
            session("agent-a1", "project-a", "worker", "阿比西尼亚"),
        ];
        let (dispatcher, persistence, path) = fixture(&records);
        let task_id = dispatch_running_task(&dispatcher, "检查 README");
        let result = format!("中文结果\n第二行含 \"引号\"\n{}", "较长内容".repeat(300));
        let delivered = Arc::new(Mutex::new(None::<(String, String)>));
        let delivered_copy = delivered.clone();

        let response = dispatcher.report_task_with(
            task_report(&task_id, "agent-a1", TaskStatus::Succeeded, &result),
            None,
            move |master_id, message| {
                *delivered_copy.lock().unwrap() =
                    Some((master_id.to_string(), message.to_string()));
                Ok(())
            },
        );
        let json = response_json(&response);
        assert_eq!(json["ok"], true);
        assert_eq!(json["task_id"], task_id);
        assert_eq!(json["status"], "succeeded");

        let task = persistence
            .db()
            .lock()
            .unwrap()
            .task_get(&task_id)
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Succeeded);
        assert_eq!(task.result.as_deref(), Some(result.as_str()));
        assert!(task.finished_at.is_some());
        assert_eq!(task.error, None);

        let workers = dispatcher.workers.lock().unwrap();
        let worker = workers.get("agent-a1").unwrap();
        assert_eq!(worker.status, WorkerStatus::Idle);
        assert_eq!(worker.active_task_id, None);
        assert_eq!(worker.current_task_title, None);
        drop(workers);

        let (master_id, message) = delivered.lock().unwrap().clone().unwrap();
        assert_eq!(master_id, "master-a");
        assert!(message.contains("[CaPilot Task Result]"));
        assert!(message.contains(&format!("Task ID: {task_id}")));
        assert!(message.contains("Worker: 阿比西尼亚"));
        assert!(message.contains("Status: succeeded"));
        assert!(message.contains(&result));
        let report = dispatcher.reports.lock().unwrap().last().cloned().unwrap();
        assert_eq!(report.task_id.as_deref(), Some(task_id.as_str()));
        assert_eq!(report.status, Some(TaskStatus::Succeeded));
        assert_eq!(report.summary, result);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn failed_report_persists_error_releases_worker_and_marks_master_result_failed() {
        let records = [
            session("master-a", "project-a", "master", "Master"),
            session("agent-a1", "project-a", "worker", "阿比西尼亚"),
        ];
        let (dispatcher, persistence, path) = fixture(&records);
        let task_id = dispatch_running_task(&dispatcher, "运行测试");
        let delivered = Arc::new(Mutex::new(String::new()));
        let delivered_copy = delivered.clone();
        let error = "测试命令失败\nexit code: 1";

        let response = dispatcher.report_task_with(
            task_report(&task_id, "agent-a1", TaskStatus::Failed, error),
            None,
            move |_, message| {
                *delivered_copy.lock().unwrap() = message.to_string();
                Ok(())
            },
        );
        assert_eq!(response_json(&response)["status"], "failed");
        let task = persistence
            .db()
            .lock()
            .unwrap()
            .task_get(&task_id)
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert_eq!(task.error.as_deref(), Some(error));
        assert!(task.finished_at.is_some());
        let worker = dispatcher.workers.lock().unwrap()["agent-a1"].clone();
        assert_eq!(worker.status, WorkerStatus::Idle);
        assert_eq!(worker.active_task_id, None);
        let message = delivered.lock().unwrap();
        assert!(message.contains("Status: failed"));
        assert!(message.contains("Error:\n测试命令失败"));
        assert_eq!(
            dispatcher.reports.lock().unwrap().last().unwrap().level,
            "failure"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn report_rejects_wrong_missing_or_display_name_identity_without_mutation() {
        let records = [
            session("master-a", "project-a", "master", "Master"),
            session("agent-a1", "project-a", "worker", "阿比西尼亚"),
            session("agent-b1", "project-a", "worker", "布偶猫"),
        ];
        let (dispatcher, persistence, path) = fixture(&records);
        let task_id = dispatch_running_task(&dispatcher, "检查身份");

        for (identity, code) in [
            ("agent-b1", "reporter_mismatch"),
            ("阿比西尼亚", "reporter_mismatch"),
            ("", "reporter_missing"),
        ] {
            let response = dispatcher.report_task_with(
                task_report(&task_id, identity, TaskStatus::Succeeded, "伪造结果"),
                None,
                |_, _| Ok(()),
            );
            assert_eq!(response_json(&response)["code"], code);
        }
        assert_eq!(
            persistence
                .db()
                .lock()
                .unwrap()
                .task_get(&task_id)
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Running
        );
        let worker = dispatcher.workers.lock().unwrap()["agent-a1"].clone();
        assert_eq!(worker.status, WorkerStatus::Busy);
        assert_eq!(worker.active_task_id.as_deref(), Some(task_id.as_str()));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn reports_reject_missing_terminal_cancelled_and_invalid_lifecycle_states() {
        let records = [
            session("master-a", "project-a", "master", "Master"),
            session("agent-a1", "project-a", "worker", "阿比西尼亚"),
        ];
        let (dispatcher, persistence, path) = fixture(&records);

        let missing = dispatcher.report_task_with(
            task_report("task_missing", "agent-a1", TaskStatus::Succeeded, "结果"),
            None,
            |_, _| Ok(()),
        );
        assert_eq!(response_json(&missing)["code"], "task_not_found");

        let succeeded_id = dispatch_running_task(&dispatcher, "成功一次");
        let first = dispatcher.report_task_with(
            task_report(&succeeded_id, "agent-a1", TaskStatus::Succeeded, "完成"),
            None,
            |_, _| Ok(()),
        );
        assert_eq!(response_json(&first)["ok"], true);
        for status in [TaskStatus::Succeeded, TaskStatus::Failed] {
            let repeated = dispatcher.report_task_with(
                task_report(&succeeded_id, "agent-a1", status, "迟到结果"),
                None,
                |_, _| Ok(()),
            );
            assert_eq!(response_json(&repeated)["code"], "task_not_running");
        }

        let cancelled_id = dispatch_running_task(&dispatcher, "取消任务");
        assert!(persistence
            .db()
            .lock()
            .unwrap()
            .task_cancel(&cancelled_id, chrono_now_ms())
            .unwrap());
        let late = dispatcher.report_task_with(
            task_report(&cancelled_id, "agent-a1", TaskStatus::Succeeded, "迟到成功"),
            None,
            |_, _| Ok(()),
        );
        assert_eq!(response_json(&late)["code"], "task_not_running");

        let invalid = dispatcher.report_task_with(
            TaskReportRequest {
                task_id: cancelled_id.clone(),
                reporter_agent_id: "agent-a1".to_string(),
                status: TaskStatus::Cancelled,
                result: None,
                error: None,
                artifact: None,
            },
            None,
            |_, _| Ok(()),
        );
        assert_eq!(response_json(&invalid)["code"], "invalid_report_status");

        // A Task that originally failed is terminal too; a repeated failed
        // report must not overwrite its first error.
        dispatcher.release_worker_task("agent-a1", &cancelled_id);
        let failed_id = dispatch_running_task(&dispatcher, "失败一次");
        let first_failed = dispatcher.report_task_with(
            task_report(&failed_id, "agent-a1", TaskStatus::Failed, "第一个错误"),
            None,
            |_, _| Ok(()),
        );
        assert_eq!(response_json(&first_failed)["ok"], true);
        let repeated_failed = dispatcher.report_task_with(
            task_report(&failed_id, "agent-a1", TaskStatus::Failed, "第二个错误"),
            None,
            |_, _| Ok(()),
        );
        assert_eq!(response_json(&repeated_failed)["code"], "task_not_running");
        assert_eq!(
            persistence
                .db()
                .lock()
                .unwrap()
                .task_get(&failed_id)
                .unwrap()
                .unwrap()
                .error
                .as_deref(),
            Some("第一个错误")
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn old_task_report_cannot_release_worker_bound_to_new_task() {
        let records = [
            session("master-a", "project-a", "master", "Master"),
            session("agent-a1", "project-a", "worker", "阿比西尼亚"),
        ];
        let (dispatcher, persistence, path) = fixture(&records);
        let old_task_id = dispatch_running_task(&dispatcher, "旧任务");
        {
            let mut workers = dispatcher.workers.lock().unwrap();
            let worker = workers.get_mut("agent-a1").unwrap();
            worker.active_task_id = Some("task_newer".to_string());
            worker.current_task_title = Some("新任务".to_string());
            worker.status = WorkerStatus::Busy;
        }

        let response = dispatcher.report_task_with(
            task_report(
                &old_task_id,
                "agent-a1",
                TaskStatus::Succeeded,
                "旧任务完成",
            ),
            None,
            |_, _| Ok(()),
        );
        assert_eq!(response_json(&response)["ok"], true);
        assert_eq!(
            persistence
                .db()
                .lock()
                .unwrap()
                .task_get(&old_task_id)
                .unwrap()
                .unwrap()
                .status,
            TaskStatus::Succeeded
        );
        let worker = dispatcher.workers.lock().unwrap()["agent-a1"].clone();
        assert_eq!(worker.status, WorkerStatus::Busy);
        assert_eq!(worker.active_task_id.as_deref(), Some("task_newer"));
        assert_eq!(worker.current_task_title.as_deref(), Some("新任务"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn natural_worker_exit_fails_active_task_and_builds_failed_master_result() {
        let records = [
            session("master-a", "project-a", "master", "Master"),
            session("agent-a1", "project-a", "worker", "阿比西尼亚"),
        ];
        let (dispatcher, persistence, path) = fixture(&records);
        let task_id = dispatch_running_task(&dispatcher, "会异常退出");

        let completion = dispatcher
            .apply_worker_exit("agent-a1", 137)
            .unwrap()
            .unwrap();
        assert_eq!(completion.response.status, TaskStatus::Failed);
        assert!(completion.master_message.contains("Status: failed"));
        assert!(completion.master_message.contains("exit=137"));
        let task = persistence
            .db()
            .lock()
            .unwrap()
            .task_get(&task_id)
            .unwrap()
            .unwrap();
        assert_eq!(task.status, TaskStatus::Failed);
        assert!(task.error.unwrap().contains("exited before reporting"));
        let worker = dispatcher.workers.lock().unwrap()["agent-a1"].clone();
        assert_eq!(worker.status, WorkerStatus::Idle);
        assert_eq!(worker.active_task_id, None);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn offline_worker_is_rejected_before_task_creation() {
        let records = [
            session("master-a", "project-a", "master", "Master"),
            session("agent-a1", "project-a", "worker", "阿比西尼亚"),
        ];
        let (dispatcher, persistence, path) = fixture(&records);
        let response = dispatcher.dispatch_with(
            "阿比西尼亚",
            None,
            "检查 README",
            None,
            |_| false,
            |_, _| Ok(()),
        );
        assert_eq!(response_json(&response)["code"], "worker_offline");
        assert!(persistence
            .db()
            .lock()
            .unwrap()
            .task_list_by_project("project-a", 10)
            .unwrap()
            .is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn legacy_dispatch_and_structured_socket_requests_parse() {
        assert_eq!(
            parse_legacy_dispatch("dispatch 阿比西尼亚 检查 README 是否正确"),
            Some(("阿比西尼亚".to_string(), "检查 README 是否正确".to_string()))
        );
        assert_eq!(parse_legacy_dispatch("dispatch 阿比西尼亚"), None);

        let request: SocketRequest = serde_json::from_str(
            r#"{"type":"dispatch","worker":"阿比西尼亚","title":"检查 README","prompt":"检查内容"}"#,
        )
        .unwrap();
        assert_eq!(
            request,
            SocketRequest::Dispatch {
                worker: "阿比西尼亚".to_string(),
                title: Some("检查 README".to_string()),
                prompt: "检查内容".to_string(),
            }
        );

        let report: SocketRequest = serde_json::from_str(
            r#"{"type":"report","task_id":"task_123","reporter_agent_id":"agent-a1","status":"failed","result":null,"error":"测试失败"}"#,
        )
        .unwrap();
        assert_eq!(
            report,
            SocketRequest::Report {
                task_id: "task_123".to_string(),
                reporter_agent_id: "agent-a1".to_string(),
                status: "failed".to_string(),
                result: None,
                error: Some("测试失败".to_string()),
            }
        );
    }

    #[test]
    fn generated_title_uses_first_nonempty_line_and_caps_at_eighty_characters() {
        assert_eq!(
            normalized_task_title(None, "\n 检查 README\n更多内容"),
            "检查 README"
        );
        let long = "猫".repeat(100);
        assert_eq!(normalized_task_title(None, &long).chars().count(), 80);
        assert_eq!(
            normalized_task_title(Some(" 显式标题 "), "ignored"),
            "显式标题"
        );
    }

    #[test]
    fn master_result_is_bounded_but_task_storage_content_can_remain_full() {
        let content = "长".repeat(MASTER_RESULT_MAX_CHARS + 50);
        let (presented, truncated) = truncate_for_master(&content);
        assert!(truncated);
        assert_eq!(presented.chars().count(), MASTER_RESULT_MAX_CHARS);
        assert_eq!(content.chars().count(), MASTER_RESULT_MAX_CHARS + 50);
    }
}
