//! Single-session ACP host: ordinary process pipes + NDJSON JSON-RPC 2.0.
//!
//! Does **not** use portable-pty. Cancel is sent as a **notification** (no `id`)
//! — OpenCode rejects request-shaped `session/cancel` with -32601 (DEF-002).

use super::descriptor::AcpAgentDescriptor;
use super::events::{AcpEvent, AcpEventSink};
use super::fs_sandbox::{self, FsSandboxError};
use super::permission::{PermissionBoard, PermissionOutcome, PendingPermission};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum AcpHostError {
    #[error("{0}")]
    Message(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("timeout waiting for ACP response")]
    Timeout,
    #[error("agent process exited")]
    ProcessExited,
    #[error("session not ready")]
    NotReady,
    #[error("channel closed")]
    ChannelClosed,
}

impl From<AcpHostError> for String {
    fn from(e: AcpHostError) -> Self {
        e.to_string()
    }
}

/// Lifecycle status mirrored to UI via events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpSessionStatus {
    Connecting,
    Ready,
    Running,
    WaitingPermission,
    Failed,
    Done,
}

impl AcpSessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connecting => "running",
            Self::Ready => "idle",
            Self::Running => "running",
            Self::WaitingPermission => "waiting_input",
            Self::Failed => "failed",
            Self::Done => "done",
        }
    }
}

enum HostCmd {
    /// JSON-RPC request; response delivered on oneshot-like channel.
    Request {
        method: String,
        params: Value,
        reply: Sender<Result<Value, AcpHostError>>,
    },
    /// JSON-RPC notification (no id) — used for `session/cancel` (DEF-002).
    Notify { method: String, params: Value },
    /// Respond to an agent-originated request (permission, etc.).
    Respond { id: Value, result: Value },
    Kill,
}

/// Live handle for one ACP child process / JSON-RPC session.
pub struct AcpSessionHandle {
    pub agent_id: String,
    pub runtime: String,
    pub cwd: PathBuf,
    pub acp_session_id: Arc<Mutex<Option<String>>>,
    pub capabilities: Arc<Mutex<Value>>,
    pub status: Arc<Mutex<AcpSessionStatus>>,
    /// Last known model id (bootstrap or fallback).
    pub last_model: Arc<Mutex<Option<String>>>,
    cmd_tx: Sender<HostCmd>,
    /// Set when the IO thread exits.
    dead: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl AcpSessionHandle {
    pub fn is_alive(&self) -> bool {
        !self.dead.load(Ordering::SeqCst)
    }

    pub fn status(&self) -> AcpSessionStatus {
        *self
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub fn acp_session_id(&self) -> Option<String> {
        self.acp_session_id
            .lock()
            .ok()
            .and_then(|g| g.clone())
    }

    pub fn last_model(&self) -> Option<String> {
        self.last_model.lock().ok().and_then(|g| g.clone())
    }

    pub fn note_last_model(&self, model: &str) {
        if let Ok(mut g) = self.last_model.lock() {
            *g = Some(model.to_string());
        }
    }

    /// Send `session/prompt` and wait for the final result (`stopReason`).
    ///
    /// On OpenCode provider rate-limit, one automatic fallback is attempted:
    /// switch to `opencode-go/deepseek-v4-flash` (or the next zen-free id) and
    /// retry the same prompt once so the first user message is not a hard fail
    /// when Console/Zen quota is exhausted.
    pub fn prompt(&self, text: &str, timeout: Duration) -> Result<String, AcpHostError> {
        let sid = self
            .acp_session_id()
            .ok_or(AcpHostError::NotReady)?;
        {
            let mut st = self.status.lock().unwrap_or_else(|e| e.into_inner());
            *st = AcpSessionStatus::Running;
        }
        let params = json!({
            "sessionId": sid,
            "prompt": [{ "type": "text", "text": text }],
        });
        let result = match self.request("session/prompt", params.clone(), timeout) {
            Ok(v) => v,
            Err(e) => {
                let msg = e.to_string();
                let lower = msg.to_ascii_lowercase();
                if lower.contains("rate limit") {
                    // One-shot fallback chain for product usability.
                    const FALLBACKS: &[&str] = &[
                        "opencode-go/deepseek-v4-flash",
                        "opencode/nemotron-3.5-lightning-free",
                        "opencode/hy3-free",
                        "opencode-go/deepseek-v4-pro",
                    ];
                    // Keep fallback prompts shorter so a dead provider does not
                    // block the UI thread for the full 300s × N window.
                    let fb_timeout = timeout.min(Duration::from_secs(90));
                    let mut last_err = e;
                    for fb in FALLBACKS {
                        log::warn!("acp prompt rate-limited; trying fallback model {fb}");
                        match self.set_model(fb, Duration::from_secs(15)) {
                            Ok(_) => {
                                match self.request(
                                    "session/prompt",
                                    json!({
                                        "sessionId": sid,
                                        "prompt": [{ "type": "text", "text": text }],
                                    }),
                                    fb_timeout,
                                ) {
                                    Ok(v) => {
                                        let stop = v
                                            .get("stopReason")
                                            .and_then(|s| s.as_str())
                                            .unwrap_or("end_turn")
                                            .to_string();
                                        {
                                            let mut st = self
                                                .status
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner());
                                            *st = AcpSessionStatus::Ready;
                                        }
                                        self.note_last_model(fb);
                                        return Ok(stop);
                                    }
                                    Err(e2) => {
                                        last_err = e2;
                                        if !last_err
                                            .to_string()
                                            .to_ascii_lowercase()
                                            .contains("rate limit")
                                        {
                                            break;
                                        }
                                    }
                                }
                            }
                            Err(e2) => {
                                last_err = e2;
                            }
                        }
                    }
                    {
                        let mut st = self.status.lock().unwrap_or_else(|e| e.into_inner());
                        *st = AcpSessionStatus::Ready;
                    }
                    return Err(last_err);
                }
                {
                    let mut st = self.status.lock().unwrap_or_else(|e| e.into_inner());
                    *st = AcpSessionStatus::Ready;
                }
                return Err(e);
            }
        };
        let stop = result
            .get("stopReason")
            .and_then(|v| v.as_str())
            .unwrap_or("end_turn")
            .to_string();
        {
            let mut st = self.status.lock().unwrap_or_else(|e| e.into_inner());
            *st = AcpSessionStatus::Ready;
        }
        Ok(stop)
    }

    /// Cancel the in-flight turn as a **notification** (no JSON-RPC id).
    /// OpenCode only accepts this shape (DEF-002).
    pub fn cancel(&self) -> Result<(), AcpHostError> {
        let sid = self
            .acp_session_id()
            .ok_or(AcpHostError::NotReady)?;
        self.cmd_tx
            .send(HostCmd::Notify {
                method: "session/cancel".to_string(),
                params: json!({ "sessionId": sid }),
            })
            .map_err(|_| AcpHostError::ChannelClosed)?;
        Ok(())
    }

    /// Live `session/set_config_option` (OpenCode: model / effort / mode).
    pub fn set_config_option(
        &self,
        config_id: &str,
        value: &str,
        timeout: Duration,
    ) -> Result<Value, AcpHostError> {
        let sid = self.acp_session_id().ok_or(AcpHostError::NotReady)?;
        let params = json!({
            "sessionId": sid,
            "configId": config_id,
            "value": value,
        });
        self.request("session/set_config_option", params, timeout)
    }

    /// Convenience: switch model via config option id `"model"`.
    pub fn set_model(&self, model: &str, timeout: Duration) -> Result<Value, AcpHostError> {
        let v = self.set_config_option("model", model, timeout)?;
        self.note_last_model(model);
        Ok(v)
    }

    /// Answer a pending `session/request_permission`.
    pub fn respond_permission(
        &self,
        request_id: &str,
        outcome: PermissionOutcome,
    ) -> Result<(), AcpHostError> {
        let id_val = parse_rpc_id(request_id);
        let result = match outcome {
            PermissionOutcome::Allow { option_id } => json!({
                "outcome": { "outcome": "selected", "optionId": option_id }
            }),
            PermissionOutcome::Reject { option_id } => {
                if let Some(oid) = option_id {
                    json!({
                        "outcome": { "outcome": "selected", "optionId": oid }
                    })
                } else {
                    json!({
                        "outcome": { "outcome": "cancelled" }
                    })
                }
            }
            PermissionOutcome::Cancelled => json!({
                "outcome": { "outcome": "cancelled" }
            }),
        };
        self.cmd_tx
            .send(HostCmd::Respond {
                id: id_val,
                result,
            })
            .map_err(|_| AcpHostError::ChannelClosed)?;
        {
            let mut st = self.status.lock().unwrap_or_else(|e| e.into_inner());
            if *st == AcpSessionStatus::WaitingPermission {
                *st = AcpSessionStatus::Running;
            }
        }
        Ok(())
    }

    pub fn kill(&self) -> Result<(), AcpHostError> {
        let _ = self.cmd_tx.send(HostCmd::Kill);
        if let Ok(mut g) = self.child.lock() {
            if let Some(mut child) = g.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        self.dead.store(true, Ordering::SeqCst);
        {
            let mut st = self.status.lock().unwrap_or_else(|e| e.into_inner());
            *st = AcpSessionStatus::Done;
        }
        Ok(())
    }

    fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, AcpHostError> {
        if self.dead.load(Ordering::SeqCst) {
            return Err(AcpHostError::ProcessExited);
        }
        let (tx, rx) = mpsc::channel();
        self.cmd_tx
            .send(HostCmd::Request {
                method: method.to_string(),
                params,
                reply: tx,
            })
            .map_err(|_| AcpHostError::ChannelClosed)?;
        match rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(AcpHostError::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(AcpHostError::ChannelClosed),
        }
    }
}

fn parse_rpc_id(s: &str) -> Value {
    if let Ok(n) = s.parse::<i64>() {
        json!(n)
    } else {
        json!(s)
    }
}

fn rpc_id_key(id: &Value) -> String {
    match id {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Prefer OpenCode Zen free models (especially deepseek-v4-flash-free) so
/// product smoke does not die on the paid/Console default (`big-pickle`).
pub fn pick_bootstrap_model(
    available: &[String],
    preferred: Option<&str>,
) -> Option<String> {
    if available.is_empty() {
        return preferred.map(|s| s.to_string());
    }
    let has = |id: &str| available.iter().any(|a| a == id);
    if let Some(p) = preferred {
        if has(p) {
            return Some(p.to_string());
        }
    }
    const PREFERRED_FREE: &[&str] = &[
        "opencode/deepseek-v4-flash-free",
        "opencode/nemotron-3.5-lightning-free",
        "opencode/hy3-free",
        "opencode/mimo-v2.5-free",
        "opencode/laguna-s-2.1-free",
        "opencode/nemotron-3-ultra-free",
    ];
    for id in PREFERRED_FREE {
        if has(id) {
            return Some((*id).to_string());
        }
    }
    // Any remaining free zen id, then go/deepseek flash, else first catalog entry.
    if let Some(id) = available.iter().find(|a| a.contains("-free") || a.ends_with("/free")) {
        return Some(id.clone());
    }
    if has("opencode-go/deepseek-v4-flash") {
        return Some("opencode-go/deepseek-v4-flash".into());
    }
    available.first().cloned()
}

fn model_values_from_config_options(opts: &Value) -> (Vec<String>, Option<String>) {
    let mut values = Vec::new();
    let mut current = None;
    let Some(arr) = opts.as_array() else {
        return (values, current);
    };
    for opt in arr {
        if opt.get("id").and_then(|v| v.as_str()) != Some("model") {
            continue;
        }
        current = opt
            .get("currentValue")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(options) = opt.get("options").and_then(|v| v.as_array()) {
            for o in options {
                if let Some(v) = o.get("value").and_then(|v| v.as_str()) {
                    values.push(v.to_string());
                }
            }
        }
    }
    (values, current)
}

/// Human-facing message for JSON-RPC / provider failures (rate limit, etc.).
pub fn humanize_acp_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.contains("rate limit") {
        return format!(
            "模型额度已用尽或触发限流。请在 Composer 切换到 OpenCode Zen 免费模型（优先 deepseek-v4-flash-free）或 OpenCode Go。原始错误：{raw}"
        );
    }
    if lower.contains("insufficient") && (lower.contains("quota") || lower.contains("credit")) {
        return format!("账户额度不足。请切换模型或检查 OpenCode 账单。原始错误：{raw}");
    }
    raw.to_string()
}

/// Spawn the agent process, run initialize + session/new|load, return handle.
///
/// When `preferred_model` is set (or the agent advertises a model catalog),
/// bootstrap applies `session/set_config_option` so the first prompt is not
/// stuck on a rate-limited default.
pub fn start_session(
    agent_id: &str,
    runtime: &str,
    desc: &AcpAgentDescriptor,
    cwd: &Path,
    resume_key: Option<&str>,
    preferred_model: Option<&str>,
    sink: Arc<dyn AcpEventSink>,
    permissions: PermissionBoard,
) -> Result<AcpSessionHandle, AcpHostError> {
    let mut cmd = Command::new(&desc.command);
    cmd.args(&desc.args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Never inject OPENCODE_TUI_CONFIG / status plugin dirs.
    for (k, v) in &desc.env {
        cmd.env(k, v);
    }
    // Clear any inherited OpenCode TUI config that would pollute ACP.
    cmd.env_remove("OPENCODE_TUI_CONFIG");

    let mut child = cmd.spawn().map_err(|e| {
        AcpHostError::Message(format!(
            "failed to spawn ACP agent '{}': {e}",
            desc.command
        ))
    })?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| AcpHostError::Message("no stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AcpHostError::Message("no stdout".into()))?;
    let stderr = child.stderr.take();

    let (cmd_tx, cmd_rx) = mpsc::channel::<HostCmd>();
    let dead = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new(AcpSessionStatus::Connecting));
    let acp_session_id = Arc::new(Mutex::new(None::<String>));
    let last_model = Arc::new(Mutex::new(None::<String>));
    let capabilities = Arc::new(Mutex::new(Value::Null));
    let child_arc = Arc::new(Mutex::new(Some(child)));

    // stderr reader
    if let Some(stderr) = stderr {
        let sink_err = Arc::clone(&sink);
        let aid = agent_id.to_string();
        thread::Builder::new()
            .name(format!("acp-stderr-{aid}"))
            .spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().flatten() {
                    sink_err.emit(&aid, AcpEvent::Stderr { line });
                }
            })
            .ok();
    }

    let agent_id_owned = agent_id.to_string();
    let dead_io = Arc::clone(&dead);
    let status_io = Arc::clone(&status);
    let sid_io = Arc::clone(&acp_session_id);
    let caps_io = Arc::clone(&capabilities);
    let sink_io = Arc::clone(&sink);
    let sink_err = Arc::clone(&sink);
    let perms_io = permissions.clone();
    let child_io = Arc::clone(&child_arc);
    let resume_owned = resume_key.map(|s| s.to_string());
    let preferred_owned = preferred_model.map(|s| s.to_string());
    let cwd_owned = cwd.to_path_buf();

    // Bootstrap channel: IO thread signals initialize+session ready (or err).
    // Also returns effective model id after set_config_option (if any).
    let (boot_tx, boot_rx) = mpsc::channel::<Result<Option<String>, AcpHostError>>();

    thread::Builder::new()
        .name(format!("acp-io-{agent_id_owned}"))
        .spawn(move || {
            let result = io_loop(
                agent_id_owned.clone(),
                stdin,
                stdout,
                cmd_rx,
                sink_io,
                perms_io,
                dead_io.clone(),
                status_io.clone(),
                sid_io.clone(),
                caps_io.clone(),
                child_io,
                resume_owned,
                preferred_owned,
                cwd_owned,
                boot_tx,
            );
            if let Err(e) = result {
                log::warn!("acp io loop ended with error for {agent_id_owned}: {e}");
                sink_err.emit(
                    &agent_id_owned,
                    AcpEvent::Error {
                        message: e.to_string(),
                    },
                );
                if let Ok(mut st) = status_io.lock() {
                    *st = AcpSessionStatus::Failed;
                }
            }
            dead_io.store(true, Ordering::SeqCst);
        })
        .map_err(|e| AcpHostError::Message(format!("spawn io thread: {e}")))?;

    // Wait for bootstrap (initialize + session/new|load + optional model set).
    match boot_rx.recv_timeout(Duration::from_secs(45)) {
        Ok(Ok(effective_model)) => {
            if let Some(m) = effective_model {
                if let Ok(mut g) = last_model.lock() {
                    *g = Some(m);
                }
            }
        }
        Ok(Err(e)) => {
            let _ = cmd_tx.send(HostCmd::Kill);
            return Err(e);
        }
        Err(_) => {
            let _ = cmd_tx.send(HostCmd::Kill);
            return Err(AcpHostError::Timeout);
        }
    }

    Ok(AcpSessionHandle {
        agent_id: agent_id.to_string(),
        runtime: runtime.to_string(),
        cwd: cwd.to_path_buf(),
        acp_session_id,
        capabilities,
        status,
        last_model,
        cmd_tx,
        dead,
        child: child_arc,
    })
}

fn write_line(stdin: &mut ChildStdin, obj: &Value) -> Result<(), AcpHostError> {
    let mut line = serde_json::to_string(obj)?;
    line.push('\n');
    stdin.write_all(line.as_bytes())?;
    stdin.flush()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn io_loop(
    agent_id: String,
    mut stdin: ChildStdin,
    stdout: impl std::io::Read + Send + 'static,
    cmd_rx: Receiver<HostCmd>,
    sink: Arc<dyn AcpEventSink>,
    permissions: PermissionBoard,
    dead: Arc<AtomicBool>,
    status: Arc<Mutex<AcpSessionStatus>>,
    acp_session_id: Arc<Mutex<Option<String>>>,
    capabilities: Arc<Mutex<Value>>,
    child: Arc<Mutex<Option<Child>>>,
    resume_key: Option<String>,
    preferred_model: Option<String>,
    cwd: PathBuf,
    boot_tx: Sender<Result<Option<String>, AcpHostError>>,
) -> Result<(), AcpHostError> {
    let next_id = AtomicU64::new(1);
    let mut pending: HashMap<String, Sender<Result<Value, AcpHostError>>> = HashMap::new();
    let reader = BufReader::new(stdout);
    let (line_tx, line_rx) = mpsc::channel::<Result<String, AcpHostError>>();

    // Dedicated reader thread → lines channel (so we can select with cmds).
    thread::Builder::new()
        .name(format!("acp-stdout-{agent_id}"))
        .spawn(move || {
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if line_tx.send(Ok(l)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = line_tx.send(Err(AcpHostError::Io(e)));
                        break;
                    }
                }
            }
        })
        .ok();

    // ── initialize ──────────────────────────────────────────────
    let init_id = next_id.fetch_add(1, Ordering::SeqCst);
    let init_req = json!({
        "jsonrpc": "2.0",
        "id": init_id,
        "method": "initialize",
        "params": {
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": { "readTextFile": true, "writeTextFile": false },
                "terminal": false
            },
            "clientInfo": {
                "name": "capilot",
                "title": "CaPilot IDE",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    });
    write_line(&mut stdin, &init_req)?;
    let init_key = init_id.to_string();
    let (init_tx, init_rx) = mpsc::channel();
    pending.insert(init_key, init_tx);

    // Pump until initialize returns.
    let init_result = pump_until(
        &agent_id,
        &mut stdin,
        &line_rx,
        &cmd_rx,
        &mut pending,
        &sink,
        &permissions,
        &status,
        &next_id,
        &init_rx,
        Duration::from_secs(20),
        &cwd,
    );
    let init_val = match init_result {
        Ok(v) => v,
        Err(e) => {
            let _ = boot_tx.send(Err(e));
            return Ok(());
        }
    };
    if let Some(caps) = init_val.get("agentCapabilities").cloned() {
        if let Ok(mut g) = capabilities.lock() {
            *g = caps;
        }
    }

    // ── session/new or session/load ─────────────────────────────
    let load_session = init_val
        .pointer("/agentCapabilities/loadSession")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let (sess_method, sess_params) = if load_session {
        if let Some(key) = resume_key.as_deref() {
            (
                "session/load",
                json!({
                    "sessionId": key,
                    "cwd": cwd.to_string_lossy(),
                    "mcpServers": []
                }),
            )
        } else {
            (
                "session/new",
                json!({
                    "cwd": cwd.to_string_lossy(),
                    "mcpServers": []
                }),
            )
        }
    } else {
        (
            "session/new",
            json!({
                "cwd": cwd.to_string_lossy(),
                "mcpServers": []
            }),
        )
    };

    let sess_id = next_id.fetch_add(1, Ordering::SeqCst);
    let sess_req = json!({
        "jsonrpc": "2.0",
        "id": sess_id,
        "method": sess_method,
        "params": sess_params
    });
    write_line(&mut stdin, &sess_req)?;
    let (sess_tx, sess_rx) = mpsc::channel();
    pending.insert(sess_id.to_string(), sess_tx);

    let sess_val = match pump_until(
        &agent_id,
        &mut stdin,
        &line_rx,
        &cmd_rx,
        &mut pending,
        &sink,
        &permissions,
        &status,
        &next_id,
        &sess_rx,
        Duration::from_secs(20),
        &cwd,
    ) {
        Ok(v) => v,
        Err(e) => {
            let _ = boot_tx.send(Err(e));
            return Ok(());
        }
    };

    let sid = sess_val
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| resume_key.clone())
        .unwrap_or_else(|| "unknown".to_string());

    if let Ok(mut g) = acp_session_id.lock() {
        *g = Some(sid.clone());
    }

    // Apply model from preferred / zen-free defaults when the agent exposes a catalog.
    let mut config_options = sess_val.get("configOptions").cloned();
    let (catalog, current_model) = config_options
        .as_ref()
        .map(model_values_from_config_options)
        .unwrap_or_default();
    let target_model = pick_bootstrap_model(&catalog, preferred_model.as_deref());
    let mut effective_model = current_model.clone();

    if let Some(target) = target_model.as_ref() {
        let needs_switch = current_model.as_deref() != Some(target.as_str());
        if needs_switch && !catalog.is_empty() {
            let set_id = next_id.fetch_add(1, Ordering::SeqCst);
            let set_req = json!({
                "jsonrpc": "2.0",
                "id": set_id,
                "method": "session/set_config_option",
                "params": {
                    "sessionId": sid,
                    "configId": "model",
                    "value": target,
                }
            });
            if let Err(e) = write_line(&mut stdin, &set_req) {
                let _ = boot_tx.send(Err(e));
                return Ok(());
            }
            let (set_tx, set_rx) = mpsc::channel();
            pending.insert(set_id.to_string(), set_tx);
            match pump_until(
                &agent_id,
                &mut stdin,
                &line_rx,
                &cmd_rx,
                &mut pending,
                &sink,
                &permissions,
                &status,
                &next_id,
                &set_rx,
                Duration::from_secs(20),
                &cwd,
            ) {
                Ok(v) => {
                    if let Some(opts) = v.get("configOptions").cloned() {
                        config_options = Some(opts.clone());
                        let (_, cur) = model_values_from_config_options(&opts);
                        effective_model = cur.or_else(|| Some(target.clone()));
                    } else {
                        effective_model = Some(target.clone());
                    }
                }
                Err(e) => {
                    // Non-fatal: keep session with agent default; surface warning.
                    log::warn!("acp bootstrap set_config_option(model) failed: {e}");
                    sink.emit(
                        &agent_id,
                        AcpEvent::Error {
                            message: format!(
                                "未能切换到模型 {target}：{}。将使用 agent 默认模型。",
                                humanize_acp_error(&e.to_string())
                            ),
                        },
                    );
                }
            }
        } else if !needs_switch {
            effective_model = Some(target.clone());
        }
    }

    if let Ok(mut st) = status.lock() {
        *st = AcpSessionStatus::Ready;
    }

    let caps_snapshot = capabilities
        .lock()
        .map(|g| g.clone())
        .unwrap_or(Value::Null);
    sink.emit(
        &agent_id,
        AcpEvent::SessionStarted {
            session_id: sid,
            capabilities: caps_snapshot,
            config_options,
            model: effective_model.clone(),
        },
    );
    sink.emit(
        &agent_id,
        AcpEvent::Status {
            status: "idle".into(),
        },
    );
    let _ = boot_tx.send(Ok(effective_model));

    // ── steady state ────────────────────────────────────────────
    loop {
        // Prefer draining stdout; also accept cmds.
        // Use recv_timeout on a combined approach: try_recv lines, then cmds with short timeout.
        let mut progressed = false;

        while let Ok(line_res) = line_rx.try_recv() {
            progressed = true;
            match line_res {
                Ok(line) => {
                    if let Err(e) = handle_line(
                        &agent_id,
                        &line,
                        &mut pending,
                        &sink,
                        &permissions,
                        &status,
                        &mut stdin,
                        &cwd,
                    ) {
                        log::warn!("acp handle_line: {e}");
                    }
                }
                Err(e) => {
                    dead.store(true, Ordering::SeqCst);
                    return Err(e);
                }
            }
        }

        match cmd_rx.recv_timeout(Duration::from_millis(if progressed { 0 } else { 50 })) {
            Ok(HostCmd::Request {
                method,
                params,
                reply,
            }) => {
                let id = next_id.fetch_add(1, Ordering::SeqCst);
                let req = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": method,
                    "params": params
                });
                if let Err(e) = write_line(&mut stdin, &req) {
                    let _ = reply.send(Err(e));
                } else {
                    pending.insert(id.to_string(), reply);
                }
            }
            Ok(HostCmd::Notify { method, params }) => {
                // DEF-002: notification — **no id field**.
                let note = json!({
                    "jsonrpc": "2.0",
                    "method": method,
                    "params": params
                });
                if let Err(e) = write_line(&mut stdin, &note) {
                    log::warn!("acp notify write failed: {e}");
                }
            }
            Ok(HostCmd::Respond { id, result }) => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result
                });
                if let Err(e) = write_line(&mut stdin, &resp) {
                    log::warn!("acp respond write failed: {e}");
                }
            }
            Ok(HostCmd::Kill) => {
                if let Ok(mut g) = child.lock() {
                    if let Some(mut c) = g.take() {
                        let _ = c.kill();
                        let _ = c.wait();
                    }
                }
                dead.store(true, Ordering::SeqCst);
                return Ok(());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if dead.load(Ordering::SeqCst) {
                    return Ok(());
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // All handles dropped — kill child.
                if let Ok(mut g) = child.lock() {
                    if let Some(mut c) = g.take() {
                        let _ = c.kill();
                        let _ = c.wait();
                    }
                }
                dead.store(true, Ordering::SeqCst);
                return Ok(());
            }
        }
    }
}

/// Drain lines (and ignore cmds except Kill) until `reply_rx` yields or timeout.
#[allow(clippy::too_many_arguments)]
fn pump_until(
    agent_id: &str,
    stdin: &mut ChildStdin,
    line_rx: &Receiver<Result<String, AcpHostError>>,
    cmd_rx: &Receiver<HostCmd>,
    pending: &mut HashMap<String, Sender<Result<Value, AcpHostError>>>,
    sink: &Arc<dyn AcpEventSink>,
    permissions: &PermissionBoard,
    status: &Arc<Mutex<AcpSessionStatus>>,
    _next_id: &AtomicU64,
    reply_rx: &Receiver<Result<Value, AcpHostError>>,
    timeout: Duration,
    cwd: &Path,
) -> Result<Value, AcpHostError> {
    let start = std::time::Instant::now();
    loop {
        if let Ok(r) = reply_rx.try_recv() {
            return r;
        }
        // Drain cmds lightly during bootstrap (only Kill/Notify matter).
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                HostCmd::Kill => return Err(AcpHostError::ProcessExited),
                HostCmd::Notify { method, params } => {
                    let note = json!({
                        "jsonrpc": "2.0",
                        "method": method,
                        "params": params
                    });
                    write_line(stdin, &note)?;
                }
                HostCmd::Respond { id, result } => {
                    let resp = json!({ "jsonrpc": "2.0", "id": id, "result": result });
                    write_line(stdin, &resp)?;
                }
                HostCmd::Request { reply, .. } => {
                    let _ = reply.send(Err(AcpHostError::Message(
                        "request during bootstrap not supported".into(),
                    )));
                }
            }
        }
        match line_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Ok(line)) => {
                handle_line(
                    agent_id,
                    &line,
                    pending,
                    sink,
                    permissions,
                    status,
                    stdin,
                    cwd,
                )?;
                if let Ok(r) = reply_rx.try_recv() {
                    return r;
                }
            }
            Ok(Err(e)) => return Err(e),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if start.elapsed() > timeout {
                    return Err(AcpHostError::Timeout);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(AcpHostError::ProcessExited);
            }
        }
    }
}

fn handle_line(
    agent_id: &str,
    line: &str,
    pending: &mut HashMap<String, Sender<Result<Value, AcpHostError>>>,
    sink: &Arc<dyn AcpEventSink>,
    permissions: &PermissionBoard,
    status: &Arc<Mutex<AcpSessionStatus>>,
    stdin: &mut ChildStdin,
    cwd: &Path,
) -> Result<(), AcpHostError> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("acp non-json stdout: {line:?} ({e})");
            return Ok(());
        }
    };

    // Response to our request?
    if let Some(id) = msg.get("id") {
        if msg.get("method").is_none() {
            let key = rpc_id_key(id);
            if let Some(tx) = pending.remove(&key) {
                if let Some(err) = msg.get("error") {
                    let raw = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("ACP error")
                        .to_string();
                    let message = humanize_acp_error(&raw);
                    // Also push to the panel so the user sees rate-limit etc.
                    sink.emit(
                        agent_id,
                        AcpEvent::Error {
                            message: message.clone(),
                        },
                    );
                    let _ = tx.send(Err(AcpHostError::Message(message)));
                } else {
                    let result = msg.get("result").cloned().unwrap_or(Value::Null);
                    // Turn done?
                    if let Some(stop) = result.get("stopReason").and_then(|s| s.as_str()) {
                        sink.emit(
                            agent_id,
                            AcpEvent::TurnDone {
                                stop_reason: stop.to_string(),
                            },
                        );
                        sink.emit(
                            agent_id,
                            AcpEvent::Status {
                                status: "idle".into(),
                            },
                        );
                    }
                    let _ = tx.send(Ok(result));
                }
                return Ok(());
            }
            // Agent-originated request (has id + method) falls through.
        }
    }

    // Notification or agent request.
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "session/update" => {
            dispatch_update(agent_id, &params, sink);
        }
        "session/request_permission" => {
            // MVP policy = ask: never auto-allow; surface to UI and wait for
            // acp_respond_permission (HostCmd::Respond).
            let req_id = msg
                .get("id")
                .map(rpc_id_key)
                .unwrap_or_else(|| "unknown".into());
            let tool = params.get("toolCall").cloned().unwrap_or(Value::Null);
            let tool_call_id = tool
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let title = tool
                .get("title")
                .and_then(|v| v.as_str())
                .or_else(|| params.get("title").and_then(|v| v.as_str()))
                .unwrap_or("Permission required")
                .to_string();
            permissions.insert(PendingPermission {
                agent_id: agent_id.to_string(),
                request_id: req_id.clone(),
                tool_call_id: tool_call_id.clone(),
                summary: title.clone(),
            });
            if let Ok(mut st) = status.lock() {
                *st = AcpSessionStatus::WaitingPermission;
            }
            sink.emit(
                agent_id,
                AcpEvent::PermissionRequest {
                    request_id: req_id,
                    tool_call_id,
                    summary: title,
                    raw: Some(params),
                },
            );
            sink.emit(
                agent_id,
                AcpEvent::Status {
                    status: "waiting_input".into(),
                },
            );
        }
        "fs/read_text_file" => {
            respond_fs_read(stdin, &msg, cwd)?;
        }
        "fs/write_text_file" => {
            respond_fs_write(stdin, &msg)?;
        }
        "" => {
            // stray response already handled
        }
        other => {
            // Unknown agent→client request with id: reject so the agent does not hang.
            if let Some(id) = msg.get("id") {
                if !method.is_empty() {
                    let resp = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {
                            "code": -32601,
                            "message": format!("Method not found: {other}")
                        }
                    });
                    write_line(stdin, &resp)?;
                }
            } else {
                log::debug!("acp ignoring method {other}");
            }
        }
    }
    Ok(())
}

/// Handle agent→client `fs/read_text_file` under session cwd sandbox.
fn respond_fs_read(
    stdin: &mut ChildStdin,
    msg: &Value,
    cwd: &Path,
) -> Result<(), AcpHostError> {
    let id = match msg.get("id") {
        Some(id) => id.clone(),
        None => return Ok(()), // notification — ignore
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let path = params
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let line = params
        .get("line")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);

    if cwd.as_os_str().is_empty() {
        let resp = json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32000, "message": "fs/read_text_file: session cwd not ready" }
        });
        return write_line(stdin, &resp);
    }

    match fs_sandbox::read_text_file(cwd, path, line, limit) {
        Ok(content) => {
            let resp = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "content": content }
            });
            write_line(stdin, &resp)
        }
        Err(e) => {
            let code = match &e {
                FsSandboxError::OutsideRoot(_) | FsSandboxError::NotAbsolute(_) => -32001,
                FsSandboxError::NotFound(_) => -32002,
                FsSandboxError::WriteDisabled => -32003,
                FsSandboxError::TooLarge(_) => -32004,
                _ => -32000,
            };
            let resp = json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": code, "message": e.to_string() }
            });
            write_line(stdin, &resp)
        }
    }
}

/// MVP: always reject writes (clientCapabilities.writeTextFile=false, belt+suspenders).
fn respond_fs_write(stdin: &mut ChildStdin, msg: &Value) -> Result<(), AcpHostError> {
    let id = match msg.get("id") {
        Some(id) => id.clone(),
        None => return Ok(()),
    };
    let resp = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32003,
            "message": FsSandboxError::WriteDisabled.to_string()
        }
    });
    write_line(stdin, &resp)
}

fn dispatch_update(agent_id: &str, params: &Value, sink: &Arc<dyn AcpEventSink>) {
    let update = params.get("update").unwrap_or(params);
    let kind = update
        .get("sessionUpdate")
        .or_else(|| update.get("session_update"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match kind {
        "agent_message_chunk" | "user_message_chunk" | "agent_thought_chunk" => {
            let text = update
                .pointer("/content/text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if text.is_empty() {
                return;
            }
            let role = match kind {
                "user_message_chunk" => "user",
                "agent_thought_chunk" => "thought",
                _ => "agent",
            };
            let message_id = update
                .get("messageId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            sink.emit(
                agent_id,
                AcpEvent::MessageChunk {
                    message_id,
                    text,
                    role: role.to_string(),
                },
            );
        }
        "tool_call" => {
            let tool_call_id = update
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let title = update
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let kind = update
                .get("kind")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let status = update
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending")
                .to_string();
            sink.emit(
                agent_id,
                AcpEvent::ToolCall {
                    tool_call_id,
                    title,
                    kind,
                    status,
                },
            );
        }
        "tool_call_update" => {
            let tool_call_id = update
                .get("toolCallId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let status = update
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            sink.emit(
                agent_id,
                AcpEvent::ToolCallUpdate {
                    tool_call_id,
                    status,
                    detail: None,
                },
            );
        }
        "usage_update" => {
            let used = update.get("used").and_then(|v| v.as_u64()).unwrap_or(0);
            let size = update.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
            sink.emit(agent_id, AcpEvent::Usage { used, size });
        }
        "plan" => {
            let entries = update
                .get("entries")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            sink.emit(agent_id, AcpEvent::Plan { entries });
        }
        // available_commands_update and unknown → ignore (design: do not break session)
        _ => {
            log::debug!("acp ignore sessionUpdate={kind}");
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_rate_limit() {
        let h = humanize_acp_error(
            "Internal error: Error from provider (Console): Rate limit exceeded. Please try again later.",
        );
        assert!(h.contains("限流") || h.contains("额度"), "{h}");
    }

    #[test]
    fn pick_prefers_preferred_when_present() {
        let cat = vec!["a".into(), "opencode/deepseek-v4-flash-free".into()];
        assert_eq!(pick_bootstrap_model(&cat, Some("a")).as_deref(), Some("a"));
    }
}
