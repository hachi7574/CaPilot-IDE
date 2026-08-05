//! PATH shim orchestration server (DevPlan §5.2 / §5.3).
//!
//! `capilot dispatch|status|report` → shell shim → Unix socket → this
//! dispatcher → worker PTY.

use crate::agent_runtime::pty::PtyManager;
use crate::orchestration::smart_return;
use crate::persistence::Persistence;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

fn default_socket_path() -> PathBuf {
    std::env::temp_dir().join("capilot-orchestrator.sock")
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

    pub fn mark_busy(&self, id: &str, task: &str) {
        let mut workers = self.workers.lock().unwrap();
        if let Some(ws) = workers.get_mut(id) {
            ws.status = WorkerStatus::Busy;
            ws.last_task = Some(task.to_string());
        }
    }

    #[allow(dead_code)]
    pub fn mark_idle(&self, id: &str) {
        let mut workers = self.workers.lock().unwrap();
        if let Some(ws) = workers.get_mut(id) {
            ws.status = WorkerStatus::Idle;
        }
    }

    /// Start the Unix socket listener in the background.
    pub fn start(self: &Arc<Self>, app: AppHandle) {
        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            this.run_socket(app).await;
        });
    }

    async fn run_socket(self: Arc<Self>, app: AppHandle) {
        let listener = match UnixListener::bind(&self.socket_path) {
            Ok(l) => l,
            Err(e) => {
                log::error!("capilot socket bind failed: {e}");
                return;
            }
        };
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
        {
            let workers = self.workers.lock().unwrap();
            if let Some(ws) = workers.get(&agent_id) {
                if ws.status == WorkerStatus::Busy {
                    return format!("ERR worker busy: {worker}");
                }
            }
        }
        if !self.pty.is_alive(&agent_id) {
            return format!("ERR worker has no live PTY (offline): {worker} — resume it in the IDE first");
        }
        // Inject the instruction into the worker's interactive TUI.
        let payload = format!("{}\r", prompt);
        match self.pty.write(&agent_id, payload.as_bytes()) {
            Ok(()) => {
                self.mark_busy(&agent_id, prompt);
                let _ = app.emit("orchestration://event", WorkerInfo {
                    id: agent_id.clone(),
                    title: agent_id.clone(),
                    runtime: String::new(),
                    status: "busy".to_string(),
                    last_task: Some(prompt.to_string()),
                });
                format!("OK dispatched to {worker}")
            }
            Err(e) => format!("ERR write failed: {e}"),
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
        let (worker, summary) = if self.resolve_worker(first).is_some() {
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
        self.reports.lock().unwrap().push(report.clone());

        let _ = app.emit("orchestration://report", report.clone());

        // Notify the master session PTY (if live) so it sees the aggregated result.
        let master = self.master_id.lock().unwrap().clone();
        if let Some(mid) = master {
            if self.pty.is_alive(&mid) {
                let msg = format!("\r\n[编排] worker {worker} 完成：{}\r\n", report.summary);
                let _ = self.pty.write(&mid, msg.as_bytes());
            }
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
