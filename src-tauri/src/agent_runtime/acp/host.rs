//! Single-session ACP host: ordinary process pipes + NDJSON JSON-RPC 2.0.
//!
//! Does **not** use portable-pty. Cancel is sent as a **notification** (no `id`)
//! — OpenCode rejects request-shaped `session/cancel` with -32601 (DEF-002).

use super::descriptor::AcpAgentDescriptor;
use super::events::{AcpEvent, AcpEventSink};
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

    /// Send `session/prompt` and wait for the final result (`stopReason`).
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
        let result = self.request("session/prompt", params, timeout)?;
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

/// Spawn the agent process, run initialize + session/new|load, return handle.
pub fn start_session(
    agent_id: &str,
    runtime: &str,
    desc: &AcpAgentDescriptor,
    cwd: &Path,
    resume_key: Option<&str>,
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
    let cwd_owned = cwd.to_path_buf();

    // Bootstrap channel: IO thread signals initialize+session ready (or err).
    let (boot_tx, boot_rx) = mpsc::channel::<Result<(), AcpHostError>>();

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

    // Wait for bootstrap (initialize + session/new|load).
    match boot_rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
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
    cwd: PathBuf,
    boot_tx: Sender<Result<(), AcpHostError>>,
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
        },
    );
    sink.emit(
        &agent_id,
        AcpEvent::Status {
            status: "idle".into(),
        },
    );
    let _ = boot_tx.send(Ok(()));

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
    _stdin: &mut ChildStdin,
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
                    let message = err
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("ACP error")
                        .to_string();
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
            // Phase 3: full sandbox. MVP: reject with error if we get a request.
            if let Some(id) = msg.get("id") {
                // Leave actual response to Phase 3 host path; for now error.
                // We can't easily write here without restructuring — bridge will
                // grow fs handlers. Log and ignore (agent may hang) — Phase 3.
                log::warn!(
                    "acp fs/read_text_file not implemented in MVP (id={id}); Phase 3"
                );
            }
        }
        "" => {
            // stray response already handled
        }
        other => {
            log::debug!("acp ignoring method {other}");
        }
    }
    Ok(())
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
