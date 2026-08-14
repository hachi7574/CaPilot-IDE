//! Shared JSON-RPC 2.0 over NDJSON stdio transport (architecture §8).
//!
//! Every provider adapter that speaks JSON-RPC over a child process's stdio
//! (ACP v1 and the Codex Direct adapter, at minimum) uses the same transport
//! shape: one spawned subprocess, a reader thread that parses one JSON object
//! per line, and a demux that routes responses to the pending callback
//! registered for their request id while forwarding agent→client requests
//! (permissions) and notifications to the session's inbound channel.
//!
//! The transport is protocol-agnostic: it knows nothing about ACP or Codex
//! method names. Adapters own the mapping from these wire primitives to the
//! provider-neutral domain model.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A JSON-RPC error object (agent→client in a response, or our own).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Agent→client traffic that is not a response to one of our requests.
#[derive(Debug)]
pub enum Inbound {
    Notification {
        method: String,
        params: Value,
    },
    Request {
        id: u64,
        method: String,
        params: Value,
    },
    /// The agent's stdout closed (process exited) — terminate the session.
    ConnectionClosed,
}

pub(crate) type Pending = Box<dyn FnOnce(Result<Value, RpcError>) + Send>;

/// Default timeout for a blocking request (handshake). Turns use the async path
/// and are bounded by the session, not this.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(90);
/// Graceful-close timeout: an agent that won't answer the close request is
/// killed after this.
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// One spawned protocol process + its pending-response table.
pub struct RpcConnection {
    stdin: Mutex<ChildStdin>,
    pending: Mutex<HashMap<u64, Pending>>,
    next_id: AtomicU64,
    child: Mutex<Option<Child>>,
    request_timeout: Duration,
    shutdown: AtomicBool,
}

impl RpcConnection {
    /// Spawn the configured command and start the reader/drainer threads.
    /// Returns the connection plus the channel the session must consume.
    pub fn spawn(
        command: &[String],
        env: &[(String, String)],
        cwd: &Path,
    ) -> Result<(Arc<Self>, Receiver<Inbound>), crate::agent_provider::types::AgentError> {
        if command.is_empty() {
            return Err(crate::agent_provider::types::AgentError::InvalidArgument(
                "empty protocol command".into(),
            ));
        }
        let mut cmd = Command::new(&command[0]);
        cmd.args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(cwd);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| {
            crate::agent_provider::types::AgentError::Provider(format!(
                "failed to spawn {}: {e}",
                command[0]
            ))
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            crate::agent_provider::types::AgentError::Provider("protocol child has no stdin".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            crate::agent_provider::types::AgentError::Provider(
                "protocol child has no stdout".into(),
            )
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            crate::agent_provider::types::AgentError::Provider(
                "protocol child has no stderr".into(),
            )
        })?;

        let (in_tx, in_rx) = mpsc::channel::<Inbound>();
        let conn = Arc::new(Self {
            stdin: Mutex::new(stdin),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            child: Mutex::new(Some(child)),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            shutdown: AtomicBool::new(false),
        });

        spawn_stderr_drainer(stderr, &command[0]);
        spawn_reader(stdout, conn.clone(), in_tx);

        Ok((conn, in_rx))
    }

    /// Send a request and block until its response arrives (handshake only).
    pub fn send_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, crate::agent_provider::types::AgentError> {
        self.send_request_timeout(method, params, self.request_timeout)
    }

    /// Send a request with a caller-chosen timeout (used by `close`).
    pub fn send_request_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, crate::agent_provider::types::AgentError> {
        let id = self.allocate_id();
        let (tx, rx) = mpsc::channel::<Result<Value, RpcError>>();
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id, Box::new(move |r| drop(tx.send(r))));
        if let Err(e) = self.write_line(&rpc_request(id, method, &params)) {
            self.pending
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&id);
            return Err(e);
        }
        rx.recv_timeout(timeout)
            .map_err(|_| {
                crate::agent_provider::types::AgentError::Protocol(format!(
                    "timeout waiting for {method} response"
                ))
            })?
            .map_err(|e| {
                crate::agent_provider::types::AgentError::Protocol(format!(
                    "{method} failed: {}",
                    e.message
                ))
            })
    }

    /// Send a request whose response is handled later by `cb` (runs on the
    /// reader thread). Used for turn starts so `start_turn` returns at once.
    pub fn send_request_async(
        &self,
        method: &str,
        params: Value,
        cb: Pending,
    ) -> Result<u64, crate::agent_provider::types::AgentError> {
        let id = self.allocate_id();
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id, cb);
        if let Err(e) = self.write_line(&rpc_request(id, method, &params)) {
            self.pending
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&id);
            return Err(e);
        }
        Ok(id)
    }

    /// Fire a notification (no response expected).
    pub fn send_notification(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(), crate::agent_provider::types::AgentError> {
        let line = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        self.write_line(&serde_json::to_string(&line).unwrap_or_default())
    }

    /// Answer an agent→client request (e.g. a permission approval).
    pub fn respond(
        &self,
        id: u64,
        result: Value,
    ) -> Result<(), crate::agent_provider::types::AgentError> {
        let line = json!({ "jsonrpc": "2.0", "id": id, "result": result });
        self.write_line(&serde_json::to_string(&line).unwrap_or_default())
    }

    /// Answer an agent→client request with a JSON-RPC error.
    pub fn respond_error(
        &self,
        id: u64,
        code: i64,
        message: &str,
    ) -> Result<(), crate::agent_provider::types::AgentError> {
        let line =
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } });
        self.write_line(&serde_json::to_string(&line).unwrap_or_default())
    }

    fn allocate_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn write_line(&self, line: &str) -> Result<(), crate::agent_provider::types::AgentError> {
        let mut stdin = self.stdin.lock().unwrap_or_else(|p| p.into_inner());
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;
        Ok(())
    }

    /// Close stdin and kill/reap the child. Pending requests fail with
    /// "connection closed"; callers that care (the `close()` handshake) wait for
    /// the response before shutting down.
    pub fn shutdown(&self) {
        if self.shutdown.swap(true, Ordering::SeqCst) {
            return;
        }
        // Dropping the ChildStdin closes the pipe so the agent sees EOF even if
        // kill is too late.
        {
            let mut stdin = self.stdin.lock().unwrap_or_else(|p| p.into_inner());
            let _ = stdin.flush();
        }
        if let Some(mut child) = self.child.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn rpc_request(id: u64, method: &str, params: &Value) -> String {
    serde_json::to_string(
        &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
    )
    .unwrap_or_else(|_| "{}".into())
}

fn spawn_stderr_drainer(stderr: ChildStderr, label: &str) {
    // stderr is piped to keep the agent from blocking on a full pipe; lines are
    // logged at debug level so `--print-logs` traces stay greppable.
    let label = label.to_string();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            match line {
                Ok(l) => log::debug!("[{label}] {l}"),
                Err(_) => break,
            }
        }
    });
}

fn spawn_reader(stdout: ChildStdout, conn: Arc<RpcConnection>, inbound: Sender<Inbound>) {
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let msg: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                // Some agents log non-JSON to stdout; ignore those lines.
                Err(_) => continue,
            };

            let id = msg.get("id").and_then(Value::as_u64);
            let method = msg
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_string);

            if let Some(id) = id {
                if let Some(method) = method {
                    // Agent→client request (permission).
                    let params = msg.get("params").cloned().unwrap_or(Value::Null);
                    if inbound
                        .send(Inbound::Request { id, method, params })
                        .is_err()
                    {
                        break; // session gone
                    }
                } else {
                    // Response to one of our requests.
                    let payload = match msg.get("error") {
                        Some(err) => Err(serde_json::from_value(err.clone()).unwrap_or(RpcError {
                            code: -32000,
                            message: "unknown rpc error".into(),
                            data: None,
                        })),
                        None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let cb = conn
                        .pending
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&id);
                    if let Some(cb) = cb {
                        cb(payload);
                    }
                }
            } else if let Some(method) = method {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                if inbound
                    .send(Inbound::Notification { method, params })
                    .is_err()
                {
                    break;
                }
            }
            // else: a response with a non-u64 id (protocol violation) — ignore.
        }

        // EOF: the child is gone. Fail every pending request so nobody blocks,
        // then tell the session to tear down.
        let drained: Vec<(u64, Pending)> = conn
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain()
            .collect();
        for (_, cb) in drained {
            cb(Err(RpcError {
                code: -32000,
                message: "protocol connection closed".into(),
                data: None,
            }));
        }
        let _ = inbound.send(Inbound::ConnectionClosed);
    });
}
