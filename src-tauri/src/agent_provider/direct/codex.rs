//! Codex `app-server` Direct adapter (architecture §8.2).
//!
//! Codex speaks JSON-RPC 2.0 over NDJSON stdio via `codex app-server --listen
//! stdio://`. This adapter implements the provider-neutral [`AgentClient`] /
//! [`AgentSession`] contracts over that protocol, mirroring the ACP adapter's
//! structure: a lazy factory ([`CodexClient`]) spawns the app-server only when
//! a session or catalog is requested, and [`CodexSession`] owns one spawned
//! server, mapping its notifications (items, deltas, token usage, turn status)
//! and permission requests to canonical timeline events.
//!
//! Permission decisions are normalized to the same domain vocabulary as ACP
//! (`allow_once`/`allow_always`/`reject_once`/`reject_always`) so both adapters
//! pass the identical contract test. The mapping back to native decisions is
//! one-to-one: `accept`/`acceptForSession`/`decline`/`cancel`.

use crate::agent_provider::rpc_stdio::{Inbound, RpcConnection, CLOSE_TIMEOUT};
use crate::agent_provider::traits::{AgentClient, AgentEventSink, AgentSession};
use crate::agent_provider::types::*;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, Weak};

/// Static configuration for the Codex provider (architecture §7.2).
#[derive(Debug, Clone)]
pub struct CodexProfile {
    pub provider_id: String,
    /// argv array (never a whitespace-split string).
    pub command: Vec<String>,
    /// Extra environment injected only into this provider's process.
    pub env: Vec<(String, String)>,
}

/// The default Codex profile (`codex app-server --listen stdio://`).
pub fn codex_profile() -> CodexProfile {
    CodexProfile {
        provider_id: "codex".into(),
        command: vec![
            "codex".into(),
            "app-server".into(),
            "--listen".into(),
            "stdio://".into(),
        ],
        env: vec![],
    }
}

/// The Codex `app-server` Direct provider client.
pub struct CodexClient {
    profile: CodexProfile,
}

impl CodexClient {
    pub fn new(profile: CodexProfile) -> Self {
        Self { profile }
    }

    pub fn profile(&self) -> &CodexProfile {
        &self.profile
    }

    async fn handshake_session(
        &self,
        config: &AgentSessionConfig,
        sink: Arc<dyn AgentEventSink>,
    ) -> Result<Arc<CodexSession>, AgentError> {
        let (conn, rx) =
            RpcConnection::spawn(&self.profile.command, &self.profile.env, &config.cwd)?;
        initialize(&conn)?;
        // Start a fresh thread; its id is the runtime session id. The server
        // re-emits `thread/started`; the client's own SessionReady is the
        // authoritative signal, so that notification is ignored.
        let raw = conn.send_request("thread/start", json!({ "cwd": config.cwd }))?;
        let thread_id = extract_thread_id(&raw)?;
        let model_options = fetch_model_options(&conn);
        let capabilities = codex_capabilities();
        let handle = PersistenceHandle {
            provider_id: self.profile.provider_id.clone(),
            runtime_session_id: thread_id.clone(),
            native_handle: Some(json!({ "protocol": "codex-app-server" })),
            metadata: None,
        };
        let session = Arc::new(CodexSession::new(CodexSessionInit {
            provider_id: self.profile.provider_id.clone(),
            agent_id: config.agent_id.clone(),
            runtime_session_id: thread_id.clone(),
            capabilities,
            persistence: handle.clone(),
            model_options,
            conn,
            sink,
        }));
        session.set_self_weak(Arc::downgrade(&session));
        session.spawn_loop(rx);
        session.emit(AgentEvent::SessionReady(SessionReady {
            provider_id: self.profile.provider_id.clone(),
            runtime_session_id: Some(thread_id),
            capabilities: session.capabilities().clone(),
            persistence: Some(handle),
        }));
        Ok(session)
    }

    async fn handshake_resume(
        &self,
        handle: PersistenceHandle,
        overrides: &AgentSessionConfig,
        sink: Arc<dyn AgentEventSink>,
    ) -> Result<Arc<CodexSession>, AgentError> {
        let (conn, rx) =
            RpcConnection::spawn(&self.profile.command, &self.profile.env, &overrides.cwd)?;
        initialize(&conn)?;
        let session_id = handle.runtime_session_id.clone();
        // `thread/resume` reopens an existing native thread in this (new)
        // process; the server restores its own history server-side.
        conn.send_request(
            "thread/resume",
            json!({ "threadId": session_id, "cwd": overrides.cwd }),
        )?;
        let model_options = fetch_model_options(&conn);
        let session = Arc::new(CodexSession::new(CodexSessionInit {
            provider_id: self.profile.provider_id.clone(),
            agent_id: overrides.agent_id.clone(),
            runtime_session_id: session_id.clone(),
            capabilities: codex_capabilities(),
            persistence: handle.clone(),
            model_options,
            conn,
            sink,
        }));
        session.set_self_weak(Arc::downgrade(&session));
        session.spawn_loop(rx);
        session.emit(AgentEvent::SessionReady(SessionReady {
            provider_id: self.profile.provider_id.clone(),
            runtime_session_id: Some(session_id),
            capabilities: session.capabilities().clone(),
            persistence: Some(handle),
        }));
        Ok(session)
    }
}

#[async_trait]
impl AgentClient for CodexClient {
    fn provider_id(&self) -> &str {
        &self.profile.provider_id
    }

    fn backend_kind(&self) -> &str {
        "direct"
    }

    async fn is_available(&self) -> Result<bool, AgentError> {
        Ok(which_binary(
            self.profile
                .command
                .first()
                .map(String::as_str)
                .unwrap_or_default(),
        )
        .is_some())
    }

    async fn diagnostic(&self) -> Result<ProviderDiagnostic, AgentError> {
        let cmd = self
            .profile
            .command
            .first()
            .map(String::as_str)
            .unwrap_or_default();
        match which_binary(cmd) {
            Some(path) => Ok(ProviderDiagnostic {
                available: true,
                authenticated: true,
                version: None,
                message: Some(format!("command: {}", path.display())),
            }),
            None => Ok(ProviderDiagnostic {
                available: false,
                authenticated: false,
                version: None,
                message: Some(format!("command not found: {cmd}")),
            }),
        }
    }

    async fn fetch_catalog(&self, cwd: &Path) -> Result<ProviderCatalog, AgentError> {
        let (conn, rx) = RpcConnection::spawn(&self.profile.command, &self.profile.env, cwd)?;
        initialize(&conn)?;
        // `model/list` is served on a loaded thread; probe with one so the
        // catalog is deterministic across server builds.
        let raw = conn.send_request("thread/start", json!({ "cwd": cwd }))?;
        let thread_id = extract_thread_id(&raw)?;
        let models = match conn.send_request("model/list", json!({})) {
            Ok(raw) => extract_models(&raw),
            Err(_) => vec![],
        };
        // Close the probe thread and reap the child (best effort).
        let _ = conn.send_request("thread/unsubscribe", json!({ "threadId": thread_id }));
        conn.shutdown();
        drop(rx);
        Ok(catalog_from_models(models))
    }

    async fn create_session(
        &self,
        config: AgentSessionConfig,
        sink: Arc<dyn AgentEventSink>,
    ) -> Result<Arc<dyn AgentSession>, AgentError> {
        Ok(self.handshake_session(&config, sink).await?)
    }

    async fn resume_session(
        &self,
        handle: PersistenceHandle,
        overrides: AgentSessionConfig,
        sink: Arc<dyn AgentEventSink>,
    ) -> Result<Arc<dyn AgentSession>, AgentError> {
        if handle.provider_id != self.profile.provider_id {
            return Err(AgentError::InvalidArgument(format!(
                "handle from provider {} cannot resume {}",
                handle.provider_id, self.profile.provider_id
            )));
        }
        Ok(self.handshake_resume(handle, &overrides, sink).await?)
    }
}

// ── CodexSession ────────────────────────────────────────────────

/// Everything a [`CodexSession`] needs at construction.
pub struct CodexSessionInit {
    pub provider_id: String,
    pub agent_id: String,
    pub runtime_session_id: String,
    pub capabilities: ProviderCapabilities,
    pub persistence: PersistenceHandle,
    /// Model options from `model/list`, cached for `ConfigUpdated` payloads.
    pub model_options: Vec<SelectOption>,
    pub conn: Arc<RpcConnection>,
    pub sink: Arc<dyn AgentEventSink>,
}

/// Which wire shape answers a pending permission request.
enum PermissionResponseShape {
    /// `{ decision: "..." }` — command/file-change approvals.
    Decision,
    /// `{ permissions: <echo>, scope }` — `item/permissions/requestApproval`.
    Grant(Value),
}

/// Bookkeeping for an unresolved permission: the JSON-RPC id to answer, the
/// declared actions (mirror of the request), and the response shape.
struct PermissionMeta {
    rpc_id: u64,
    actions: Vec<PermissionAction>,
    response: PermissionResponseShape,
}

/// One in-flight foreground turn.
struct ActiveTurn {
    /// Client-local turn id returned by `start_turn`.
    turn_id: String,
    /// Server turn id (from `turn/started`), bound when it arrives. Used by
    /// `turn/interrupt` and to correlate `turn/completed`.
    server_turn_id: Mutex<Option<String>>,
    terminal_emitted: AtomicBool,
}

pub struct CodexSession {
    provider_id: String,
    agent_id: String,
    runtime_session_id: String,
    capabilities: ProviderCapabilities,
    persistence: PersistenceHandle,
    model_options: Vec<SelectOption>,
    conn: Arc<RpcConnection>,
    sink: Arc<dyn AgentEventSink>,
    /// Set after construction so `start_turn` can hand a `Weak<Self>` to the
    /// response callback (the callback runs on the reader thread).
    self_weak: Mutex<Option<Weak<CodexSession>>>,
    /// item_id → whether a Started event was already emitted (append vs new).
    messages: Mutex<HashMap<String, bool>>,
    tool_calls: Mutex<HashMap<String, ToolCallItem>>,
    /// Accumulated command-output text by item id (`outputDelta` streams).
    tool_output_text: Mutex<HashMap<String, String>>,
    permissions: Mutex<HashMap<String, PermissionMeta>>,
    active_turn: Mutex<Option<ActiveTurn>>,
    /// Set by `interrupt()` and cleared by the next `start_turn`. A permission
    /// request that surfaces after the cancel is auto-cancelled instead of
    /// hanging the server on an unanswered approval.
    cancel_requested: AtomicBool,
    closed: AtomicBool,
}

impl CodexSession {
    pub fn new(init: CodexSessionInit) -> Self {
        Self {
            provider_id: init.provider_id,
            agent_id: init.agent_id,
            runtime_session_id: init.runtime_session_id,
            capabilities: init.capabilities,
            persistence: init.persistence,
            model_options: init.model_options,
            conn: init.conn,
            sink: init.sink,
            self_weak: Mutex::new(None),
            messages: Mutex::new(HashMap::new()),
            tool_calls: Mutex::new(HashMap::new()),
            tool_output_text: Mutex::new(HashMap::new()),
            permissions: Mutex::new(HashMap::new()),
            active_turn: Mutex::new(None),
            cancel_requested: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }
    }

    /// Run the message loop on its own thread. Call once after construction.
    pub fn spawn_loop(self: &Arc<Self>, rx: Receiver<Inbound>) {
        let weak = Arc::downgrade(self);
        std::thread::spawn(move || {
            while let Ok(inbound) = rx.recv() {
                let Some(session) = weak.upgrade() else { break };
                match inbound {
                    Inbound::Notification { method, params } => {
                        session.handle_notification(&method, &params)
                    }
                    Inbound::Request { id, method, params } => {
                        session.handle_request(id, &method, &params)
                    }
                    Inbound::ConnectionClosed => session.handle_connection_closed(),
                }
            }
        });
    }

    pub fn set_self_weak(&self, weak: Weak<CodexSession>) {
        *self.self_weak.lock().unwrap_or_else(|p| p.into_inner()) = Some(weak);
    }

    fn upgrade_self(&self) -> Option<Arc<CodexSession>> {
        self.self_weak
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .and_then(Weak::upgrade)
    }

    pub(crate) fn emit(&self, event: AgentEvent) {
        self.sink.on_event(&self.agent_id, event);
    }

    // ── Inbound dispatch ─────────────────────────────────────────

    fn handle_notification(&self, method: &str, params: &Value) {
        match method {
            // The client's SessionReady is authoritative; the server echo of a
            // started thread carries no additional state we render.
            "thread/started" => {}
            "turn/started" => self.handle_turn_started(params),
            "turn/completed" => self.handle_turn_completed(params),
            "item/started" => self.handle_item_started(params),
            "item/completed" => self.handle_item_completed(params),
            "item/agentMessage/delta" => self.handle_message_delta(params, false),
            "item/reasoning/textDelta" => self.handle_message_delta(params, true),
            // The summary is consolidated on `item/completed`; ignore the
            // incremental summary stream to avoid double-appending.
            "item/reasoning/summaryTextDelta" => {}
            "item/commandExecution/outputDelta" => self.handle_output_delta(params),
            "thread/tokenUsage/updated" => self.handle_token_usage(params),
            // Noise notifications the server emits with no timeline content.
            "thread/statusChanged"
            | "thread/settings/updated"
            | "mcpServer/startupStatus/updated"
            | "remoteControl/status/changed" => {}
            other => log::debug!("codex: unhandled notification {other}"),
        }
    }

    fn handle_request(&self, id: u64, method: &str, params: &Value) {
        match method {
            "item/commandExecution/requestApproval" => self.handle_command_approval(id, params),
            "item/fileChange/requestApproval" => self.handle_file_change_approval(id, params),
            "item/permissions/requestApproval" => self.handle_permissions_approval(id, params),
            // Unknown agent request: answer with an error so the agent unblocks
            // instead of waiting on us forever.
            other => {
                let _ = self
                    .conn
                    .respond_error(id, -32601, &format!("method not found: {other}"));
            }
        }
    }

    fn handle_connection_closed(&self) {
        if self.closed.load(Ordering::Relaxed) {
            return;
        }
        // Fail an active turn, then close the session record.
        let mut active = self.active_turn.lock().unwrap_or_else(|p| p.into_inner());
        let turn_id = active.as_ref().map(|a| a.turn_id.clone());
        let should_fail = active
            .as_ref()
            .map(|a| !a.terminal_emitted.load(Ordering::SeqCst))
            .unwrap_or(false);
        if let Some(a) = active.as_mut() {
            a.terminal_emitted.store(true, Ordering::SeqCst);
        }
        drop(active);
        if let (Some(turn_id), true) = (turn_id, should_fail) {
            self.emit(AgentEvent::TurnFailed(TurnFailed {
                turn_id,
                message: "codex app-server process exited".into(),
            }));
        }
        self.emit(AgentEvent::SessionClosed);
    }

    // ── Notifications ────────────────────────────────────────────

    fn handle_turn_started(&self, params: &Value) {
        let Some(server_turn_id) = params
            .get("turn")
            .and_then(|t| t.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return;
        };
        let mut active = self.active_turn.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(at) = active.as_mut() {
            let mut sid = at.server_turn_id.lock().unwrap_or_else(|p| p.into_inner());
            if sid.is_none() {
                *sid = Some(server_turn_id);
            }
        }
    }

    fn handle_turn_completed(&self, params: &Value) {
        let status = params
            .get("turn")
            .and_then(|t| t.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let server_turn_id = params
            .get("turn")
            .and_then(|t| t.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let error_message = params
            .get("turn")
            .and_then(|t| t.get("error"))
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        self.finalize_turn(status, server_turn_id, error_message);
    }

    /// Terminal turn event, guarded by the same atomic swap as `interrupt()`:
    /// whoever claims the terminal state first emits, so a `turn/completed`
    /// racing our cancel can never double-emit.
    fn finalize_turn(&self, status: &str, server_turn_id: &str, error_message: String) {
        if !matches!(status, "completed" | "interrupted" | "failed") {
            return;
        }
        let active = self.active_turn.lock().unwrap_or_else(|p| p.into_inner());
        let Some(at) = active.as_ref() else { return };
        // Only accept the completion of the turn we bound, if we bound one.
        let bound = at
            .server_turn_id
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(bound) = bound {
            if !server_turn_id.is_empty() && bound != server_turn_id {
                return;
            }
        }
        if at.terminal_emitted.swap(true, Ordering::SeqCst) {
            return;
        }
        let turn_id = at.turn_id.clone();
        drop(active);
        match status {
            "completed" => self.emit(AgentEvent::TurnCompleted(TurnCompleted { turn_id })),
            "interrupted" => self.emit(AgentEvent::TurnCancelled(TurnCancelled { turn_id })),
            _ => self.emit(AgentEvent::TurnFailed(TurnFailed {
                turn_id,
                message: if error_message.is_empty() {
                    "codex turn failed".into()
                } else {
                    error_message
                },
            })),
        }
    }

    fn handle_item_started(&self, params: &Value) {
        let item = params.get("item").cloned().unwrap_or(Value::Null);
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        match item_type {
            "agentMessage" => {
                let text = item
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                // Mark as started immediately so later deltas append rather than
                // re-starting the item.
                self.messages
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id.clone(), true);
                self.emit(AgentEvent::Timeline(TimelineEvent::Started {
                    item: TimelineItem::AssistantMessage(MessageItem {
                        item_id: id,
                        role: MessageRole::Assistant,
                        text,
                        created_at: now_ms(),
                        metadata: None,
                    }),
                }));
            }
            "reasoning" => {
                let text = item
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                self.messages
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id.clone(), true);
                self.emit(AgentEvent::Timeline(TimelineEvent::Started {
                    item: TimelineItem::Reasoning(MessageItem {
                        item_id: id,
                        role: MessageRole::Assistant,
                        text,
                        created_at: now_ms(),
                        metadata: None,
                    }),
                }));
            }
            "commandExecution" => {
                let command = item
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let cwd = item
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let tool = ToolCallItem {
                    item_id: id.clone(),
                    tool_name: "shell".into(),
                    tool_input: Some(json!({ "command": command, "cwd": cwd })),
                    tool_output: None,
                    status: ToolCallStatus::Running,
                    created_at: now_ms(),
                    metadata: Some(json!({ "command": command })),
                };
                self.tool_calls
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id.clone(), tool.clone());
                // Preserve any output that streamed before this started.
                self.tool_output_text
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .entry(id.clone())
                    .or_default();
                self.emit(AgentEvent::Timeline(TimelineEvent::Started {
                    item: TimelineItem::ToolCall(tool),
                }));
            }
            "fileChange" => {
                let tool = ToolCallItem {
                    item_id: id.clone(),
                    tool_name: "file_change".into(),
                    tool_input: item.get("changes").cloned(),
                    tool_output: None,
                    status: map_patch_status(item.get("status").and_then(Value::as_str)),
                    created_at: now_ms(),
                    metadata: None,
                };
                self.tool_calls
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id.clone(), tool.clone());
                self.emit(AgentEvent::Timeline(TimelineEvent::Started {
                    item: TimelineItem::ToolCall(tool),
                }));
            }
            "plan" => {
                let text = item
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                self.emit(AgentEvent::Timeline(TimelineEvent::Started {
                    item: TimelineItem::Plan(PlanItem {
                        item_id: id,
                        title: "计划".into(),
                        content: text,
                        created_at: now_ms(),
                    }),
                }));
            }
            // userMessage echo, hookPrompt, subAgentActivity, webSearch, image,
            // mcpToolCall, etc. — not rendered in the Phase 4 timeline.
            _ => {}
        }
    }

    fn handle_message_delta(&self, params: &Value, reasoning: bool) {
        let id = params
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let delta = params
            .get("delta")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if delta.is_empty() || id.is_empty() {
            return;
        }
        let mut messages = self.messages.lock().unwrap_or_else(|p| p.into_inner());
        let started = messages.get(&id).copied().unwrap_or(false);
        messages.insert(id.clone(), true);
        drop(messages);
        if !started {
            // A delta arrived without its item/started — synthesize the item.
            let item = if reasoning {
                TimelineItem::Reasoning(MessageItem {
                    item_id: id,
                    role: MessageRole::Assistant,
                    text: delta,
                    created_at: now_ms(),
                    metadata: None,
                })
            } else {
                TimelineItem::AssistantMessage(MessageItem {
                    item_id: id,
                    role: MessageRole::Assistant,
                    text: delta,
                    created_at: now_ms(),
                    metadata: None,
                })
            };
            self.emit(AgentEvent::Timeline(TimelineEvent::Started { item }));
        } else {
            self.emit(AgentEvent::Timeline(TimelineEvent::Appended {
                item_id: id,
                text_delta: delta,
            }));
        }
    }

    fn handle_output_delta(&self, params: &Value) {
        let id = params
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let delta = params
            .get("delta")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            return;
        }
        self.tool_output_text
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(id)
            .or_default()
            .push_str(&delta);
    }

    fn handle_token_usage(&self, params: &Value) {
        // ThreadTokenUsage: { total: TokenUsageBreakdown, modelContextWindow }.
        let used = params
            .get("tokenUsage")
            .and_then(|u| u.get("total"))
            .and_then(|t| t.get("totalTokens"))
            .and_then(Value::as_u64);
        let max = params
            .get("tokenUsage")
            .and_then(|u| u.get("modelContextWindow"))
            .and_then(Value::as_u64);
        if let Some(used) = used {
            self.emit(AgentEvent::ContextUsageUpdated(ContextUsage {
                context_window_used_tokens: Some(used),
                context_window_max_tokens: max,
            }));
        }
    }

    fn handle_item_completed(&self, params: &Value) {
        let item = params.get("item").cloned().unwrap_or(Value::Null);
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        match item_type {
            "commandExecution" => {
                let status = map_patch_status(item.get("status").and_then(Value::as_str));
                let out = item
                    .get("aggregatedOutput")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let acc = self
                    .tool_output_text
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&id);
                let output = out.or(acc).filter(|s| !s.is_empty());
                self.finish_tool_call(&id, status, output);
            }
            "fileChange" => {
                let status = map_patch_status(item.get("status").and_then(Value::as_str));
                self.finish_tool_call(&id, status, None);
            }
            // The completed agentMessage carries its final text, but deltas
            // already appended it; nothing more to emit.
            "agentMessage" => {}
            "reasoning" => {
                // Consolidate the streamed reasoning into its final summary.
                let summary = item
                    .get("summary")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                if !summary.is_empty() {
                    self.emit(AgentEvent::Timeline(TimelineEvent::Replaced {
                        item: TimelineItem::Reasoning(MessageItem {
                            item_id: id,
                            role: MessageRole::Assistant,
                            text: summary,
                            created_at: now_ms(),
                            metadata: None,
                        }),
                    }));
                }
            }
            _ => {}
        }
    }

    /// Merge the terminal status (+ optional output) into a tool item, emit the
    /// replacement, then finish it.
    fn finish_tool_call(&self, id: &str, status: ToolCallStatus, output: Option<String>) {
        let mut map = self.tool_calls.lock().unwrap_or_else(|p| p.into_inner());
        let Some(t) = map.get_mut(id) else { return };
        if let Some(text) = output {
            t.tool_output = Some(json!({ "text": text }));
        }
        t.status = status.clone();
        let item = t.clone();
        drop(map);
        self.emit(AgentEvent::Timeline(TimelineEvent::Replaced {
            item: TimelineItem::ToolCall(item),
        }));
        let item_status = match status {
            ToolCallStatus::Completed => ItemStatus::Complete,
            ToolCallStatus::Failed => ItemStatus::Failed,
            ToolCallStatus::Cancelled => ItemStatus::Cancelled,
            _ => ItemStatus::Complete,
        };
        self.emit(AgentEvent::Timeline(TimelineEvent::Finished {
            item_id: id.to_string(),
            status: item_status,
        }));
    }

    // ── Permission ───────────────────────────────────────────────

    fn handle_command_approval(&self, id: u64, params: &Value) {
        let command = params
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // For regular shell approvals `approvalId` is null; the request id is
        // then the JSON-RPC id itself.
        let approval_id = params
            .get("approvalId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let request_id = if approval_id.is_empty() {
            id.to_string()
        } else {
            approval_id
        };
        let title = if command.is_empty() {
            "运行命令".into()
        } else {
            format!("运行命令：{command}")
        };
        let reason = params
            .get("availableDecisions")
            .and_then(Value::as_array)
            .and_then(|a| {
                a.iter()
                    .find_map(|d| d.get("reason").and_then(Value::as_str).map(str::to_string))
            });
        self.register_permission(
            request_id,
            PermissionMeta {
                rpc_id: id,
                actions: codex_actions(),
                response: PermissionResponseShape::Decision,
            },
            title,
            reason,
            PermissionKind::TerminalCommand,
        );
    }

    fn handle_file_change_approval(&self, id: u64, params: &Value) {
        let item_id = params
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let request_id = if item_id.is_empty() {
            id.to_string()
        } else {
            item_id
        };
        let reason = params
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_string);
        self.register_permission(
            request_id,
            PermissionMeta {
                rpc_id: id,
                actions: codex_actions(),
                response: PermissionResponseShape::Decision,
            },
            "文件变更审批".into(),
            reason,
            PermissionKind::FileChange,
        );
    }

    fn handle_permissions_approval(&self, id: u64, params: &Value) {
        let item_id = params
            .get("itemId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let request_id = if item_id.is_empty() {
            id.to_string()
        } else {
            item_id
        };
        let reason = params
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_string);
        let echo = params.get("permissions").cloned().unwrap_or(Value::Null);
        self.register_permission(
            request_id,
            PermissionMeta {
                rpc_id: id,
                actions: codex_actions(),
                response: PermissionResponseShape::Grant(echo),
            },
            "权限请求".into(),
            reason,
            PermissionKind::Other,
        );
    }

    /// Record a permission request and surface it to the UI — unless the turn
    /// was already cancelled, in which case answer `cancel` at once so the
    /// server never hangs on an approval that will never be answered.
    fn register_permission(
        &self,
        request_id: String,
        meta: PermissionMeta,
        title: String,
        description: Option<String>,
        kind: PermissionKind,
    ) {
        if self.cancel_requested.load(Ordering::SeqCst) {
            let _ = self.write_approval_response(&meta, "cancel");
            self.emit(AgentEvent::PermissionResolved(PermissionResolution {
                request_id,
                action_id: "reject_always".into(),
                resolved_at: now_ms(),
            }));
            return;
        }
        let actions = meta.actions.clone();
        self.permissions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(request_id.clone(), meta);
        self.emit(AgentEvent::PermissionRequested(PermissionRequest {
            id: request_id,
            agent_id: self.agent_id.clone(),
            kind: kind.clone(),
            title: title.clone(),
            description: description.clone(),
            subject: PermissionSubject {
                kind,
                title,
                description,
                icon: None,
            },
            actions,
        }));
    }

    /// Write the native approval response for a meta's wire shape.
    fn write_approval_response(
        &self,
        meta: &PermissionMeta,
        decision: &str,
    ) -> Result<(), AgentError> {
        match &meta.response {
            PermissionResponseShape::Decision => self
                .conn
                .respond(meta.rpc_id, json!({ "decision": decision })),
            PermissionResponseShape::Grant(echo) => {
                let mut granted = echo.clone();
                if let Some(arr) = granted.as_array_mut() {
                    for item in arr.iter_mut() {
                        if let Some(result) = item.get_mut("result") {
                            result["decision"] =
                                json!(if decision == "cancel" || decision == "decline" {
                                    "deny"
                                } else {
                                    "approve"
                                });
                        }
                    }
                }
                self.conn.respond(
                    meta.rpc_id,
                    json!({ "permissions": granted, "scope": "turn" }),
                )
            }
        }
    }

    fn cancel_pending_permissions(&self) {
        let metas: Vec<(String, PermissionMeta)> = self
            .permissions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain()
            .collect();
        for (request_id, meta) in metas {
            // codex `cancel` = deny + interrupt the turn.
            let _ = self.write_approval_response(&meta, "cancel");
            self.emit(AgentEvent::PermissionResolved(PermissionResolution {
                request_id,
                action_id: "reject_always".into(),
                resolved_at: now_ms(),
            }));
        }
    }

    fn cancel_pending_tool_calls(&self) {
        let ids: Vec<String> = {
            let mut map = self.tool_calls.lock().unwrap_or_else(|p| p.into_inner());
            map.iter_mut()
                .filter(|(_, t)| {
                    matches!(t.status, ToolCallStatus::Pending | ToolCallStatus::Running)
                })
                .map(|(id, t)| {
                    t.status = ToolCallStatus::Cancelled;
                    id.clone()
                })
                .collect()
        };
        for id in ids {
            self.emit(AgentEvent::Timeline(TimelineEvent::Finished {
                item_id: id,
                status: ItemStatus::Cancelled,
            }));
        }
    }
}

#[async_trait]
impl AgentSession for CodexSession {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    fn runtime_session_id(&self) -> Option<&str> {
        Some(&self.runtime_session_id)
    }

    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    async fn start_turn(&self, prompt: AgentPrompt) -> Result<TurnId, AgentError> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(AgentError::SessionClosed);
        }
        let text = prompt_text(&prompt);
        // Render the user message from the client side immediately (the server's
        // `userMessage` echo would duplicate it).
        self.emit(AgentEvent::Timeline(TimelineEvent::Started {
            item: TimelineItem::UserMessage(MessageItem {
                item_id: prompt.client_message_id.clone(),
                role: MessageRole::User,
                text,
                created_at: now_ms(),
                metadata: None,
            }),
        }));
        let content = map_prompt_content(&prompt)?;
        let turn_id = format!("turn-{}", now_ms());
        // A new turn clears any pending cancel from a previous interrupt.
        self.cancel_requested.store(false, Ordering::SeqCst);
        // Register the turn before writing the request so a fast server reply
        // can never find `active_turn` missing when `turn/completed` runs.
        *self.active_turn.lock().unwrap_or_else(|p| p.into_inner()) = Some(ActiveTurn {
            turn_id: turn_id.clone(),
            server_turn_id: Mutex::new(None),
            terminal_emitted: AtomicBool::new(false),
        });
        let weak = self.upgrade_self();
        let params = json!({
            "threadId": self.runtime_session_id,
            "clientUserMessageId": prompt.client_message_id,
            "input": content,
        });
        if let Err(e) = self.conn.send_request_async(
            "turn/start",
            params,
            Box::new(move |res| {
                // A successful start is finalized by `turn/completed`; only an
                // error needs the local turn failed so it cannot hang.
                if let Err(err) = res {
                    if let Some(session) = weak {
                        session.finalize_turn("failed", "", err.message);
                    }
                }
            }),
        ) {
            // The write failed; leave no dangling turn behind.
            *self.active_turn.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return Err(e);
        }
        self.emit(AgentEvent::TurnStarted(TurnStarted {
            turn_id: turn_id.clone(),
            client_message_id: prompt.client_message_id,
        }));
        Ok(turn_id)
    }

    async fn interrupt(&self) -> Result<(), AgentError> {
        if self.closed.load(Ordering::Relaxed) {
            return Ok(());
        }
        // Ask the server to interrupt the current turn (fire-and-forget; the
        // outcome arrives as `turn/completed`). Only possible once we know the
        // server-side turn id.
        let server_turn_id = {
            let active = self.active_turn.lock().unwrap_or_else(|p| p.into_inner());
            active.as_ref().and_then(|a| {
                a.server_turn_id
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone()
            })
        };
        if let Some(sid) = server_turn_id {
            let _ = self.conn.send_request_async(
                "turn/interrupt",
                json!({ "threadId": self.runtime_session_id, "turnId": sid }),
                Box::new(|_| {}),
            );
        }
        // Any approval that surfaces after this point (before the next turn)
        // belongs to the cancelled turn — answer `cancel` immediately.
        self.cancel_requested.store(true, Ordering::SeqCst);
        // Resolve any pending permission with `cancel` and mark in-flight tool
        // calls cancelled, mirroring ACP's interrupt semantics.
        self.cancel_pending_permissions();
        self.cancel_pending_tool_calls();

        // Same atomic swap-guard as `finalize_turn`: whoever claims the terminal
        // state first emits, so a racing `turn/completed` can never double-emit.
        // With no turn in flight there is nothing to cancel at the turn level —
        // pending permissions were already resolved above.
        let mut active = self.active_turn.lock().unwrap_or_else(|p| p.into_inner());
        let turn_id = active.as_ref().map(|a| a.turn_id.clone());
        let won = match active.as_mut() {
            Some(a) => !a.terminal_emitted.swap(true, Ordering::SeqCst),
            None => false,
        };
        drop(active);

        if let (Some(turn_id), true) = (turn_id, won) {
            self.emit(AgentEvent::TurnCancelled(TurnCancelled { turn_id }));
        }
        Ok(())
    }

    async fn set_config_option(
        &self,
        config_id: &str,
        value: ConfigValue,
    ) -> Result<Vec<ConfigOption>, AgentError> {
        match config_id {
            "model" => {
                let model = match value {
                    ConfigValue::String(s) => s,
                    _ => {
                        return Err(AgentError::InvalidArgument(
                            "codex model must be a string".into(),
                        ))
                    }
                };
                self.conn.send_request(
                    "thread/settings/update",
                    json!({ "threadId": self.runtime_session_id, "model": model }),
                )?;
                let options = vec![ConfigOption::Select {
                    id: "model".into(),
                    label: "模型".into(),
                    category: Some("model".into()),
                    current: model.clone(),
                    options: self.model_options.clone(),
                }];
                self.emit(AgentEvent::ConfigUpdated(options.clone()));
                Ok(options)
            }
            other => Err(AgentError::Provider(format!(
                "codex has no config option '{other}'"
            ))),
        }
    }

    async fn respond_to_permission(
        &self,
        request_id: &str,
        action_id: &str,
    ) -> Result<(), AgentError> {
        let meta = self
            .permissions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(request_id)
            .ok_or_else(|| AgentError::PermissionRequestNotFound(request_id.to_string()))?;
        // The manager pre-validates, but never invent an option the agent did
        // not offer.
        if !meta.actions.iter().any(|a| a.id == action_id) {
            return Err(AgentError::InvalidArgument(format!(
                "action {action_id} not declared by request {request_id}"
            )));
        }
        let decision = map_domain_to_decision(action_id);
        self.write_approval_response(&meta, decision)?;
        self.emit(AgentEvent::PermissionResolved(PermissionResolution {
            request_id: request_id.to_string(),
            action_id: action_id.to_string(),
            resolved_at: now_ms(),
        }));
        Ok(())
    }

    fn describe_persistence(&self) -> Option<PersistenceHandle> {
        Some(self.persistence.clone())
    }

    async fn close(&self) -> Result<(), AgentError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        // Graceful unsubscribe, then reap the child regardless of whether the
        // server answered (a hung server must not block the caller for long).
        let _ = self.conn.send_request_timeout(
            "thread/unsubscribe",
            json!({ "threadId": self.runtime_session_id }),
            CLOSE_TIMEOUT,
        );
        self.conn.shutdown();
        self.emit(AgentEvent::SessionClosed);
        Ok(())
    }
}

// ── Mapping helpers ─────────────────────────────────────────────

fn codex_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        session_resume: true,
        session_list: false,
        structured_tools: true,
        reasoning_stream: true,
        permissions: true,
        config_options: true,
        slash_commands: false,
        mcp_servers: false,
        images: false,
        context_usage: true,
    }
}

/// The four domain actions every codex approval surfaces (normalized to the ACP
/// vocabulary so both adapters pass the identical contract test).
fn codex_actions() -> Vec<PermissionAction> {
    vec![
        PermissionAction {
            id: "allow_once".into(),
            label: "允许一次".into(),
            behavior: PermissionBehavior::Allow,
        },
        PermissionAction {
            id: "allow_always".into(),
            label: "始终允许".into(),
            behavior: PermissionBehavior::Allow,
        },
        PermissionAction {
            id: "reject_once".into(),
            label: "拒绝一次".into(),
            behavior: PermissionBehavior::Deny,
        },
        PermissionAction {
            id: "reject_always".into(),
            label: "总是拒绝".into(),
            behavior: PermissionBehavior::Deny,
        },
    ]
}

/// Map a domain action id back to the codex approval decision.
fn map_domain_to_decision(action_id: &str) -> &'static str {
    match action_id {
        "allow_once" => "accept",
        "allow_always" => "acceptForSession",
        "reject_always" => "cancel",
        // reject_once (and anything unknown) deny without interrupting.
        _ => "decline",
    }
}

fn map_patch_status(status: Option<&str>) -> ToolCallStatus {
    match status {
        Some("inProgress") => ToolCallStatus::Running,
        Some("completed") => ToolCallStatus::Completed,
        Some("failed") | Some("declined") => ToolCallStatus::Failed,
        _ => ToolCallStatus::Pending,
    }
}

/// Codex `initialize` handshake. The server answers `{}`; the negotiated
/// protocol version is implicit in the notification stream it sends afterwards.
fn initialize(conn: &RpcConnection) -> Result<(), AgentError> {
    let params = json!({
        "clientInfo": {
            "name": "CaPilot IDE",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "clientCapabilities": {},
        "clientVersion": env!("CARGO_PKG_VERSION"),
        "config": {
            "modelPreferences": {},
            "permissions": {},
        },
    });
    conn.send_request("initialize", params)?;
    Ok(())
}

fn extract_thread_id(raw: &Value) -> Result<String, AgentError> {
    raw.get("thread")
        .and_then(|t| t.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AgentError::Protocol("codex response missing thread.id".into()))
}

/// Best-effort model list for the session's `ConfigUpdated` payloads. A failure
/// (or a server that requires auth) yields an empty list — the model selector
/// then shows only the current value.
fn fetch_model_options(conn: &RpcConnection) -> Vec<SelectOption> {
    match conn.send_request("model/list", json!({})) {
        Ok(raw) => extract_models(&raw)
            .into_iter()
            .map(|m| SelectOption {
                id: m.id,
                label: m.label,
            })
            .collect(),
        Err(_) => vec![],
    }
}

/// `model/list` response → model definitions.
fn extract_models(raw: &Value) -> Vec<ModelDefinition> {
    let mut models = Vec::new();
    if let Some(data) = raw.get("data").and_then(Value::as_array) {
        for m in data {
            let id = m
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if id.is_empty() {
                continue;
            }
            let label = m
                .get("displayName")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| id.clone());
            let is_default = m.get("isDefault").and_then(Value::as_bool).unwrap_or(false);
            let reasoning_efforts = m
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            models.push(ModelDefinition {
                id,
                label,
                context_window: None,
                reasoning_efforts,
                is_default,
            });
        }
    }
    models
}

fn catalog_from_models(models: Vec<ModelDefinition>) -> ProviderCatalog {
    let models = if models.is_empty() {
        vec![ModelDefinition {
            id: "default".into(),
            label: "Default".into(),
            context_window: None,
            reasoning_efforts: vec![],
            is_default: true,
        }]
    } else {
        models
    };
    ProviderCatalog {
        models,
        // The model list is rendered from `models` alone; config_options stays
        // empty so the ConfigBar does not duplicate the model selector.
        config_options: vec![],
        capabilities: codex_capabilities(),
    }
}

fn prompt_text(prompt: &AgentPrompt) -> String {
    prompt
        .content
        .iter()
        .filter_map(|c| match c {
            PromptContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn map_prompt_content(prompt: &AgentPrompt) -> Result<Vec<Value>, AgentError> {
    prompt
        .content
        .iter()
        .map(|c| match c {
            PromptContent::Text { text } => Ok(json!({ "type": "text", "text": text })),
            PromptContent::Image { .. } => Err(AgentError::UnsupportedContent),
            PromptContent::Resource { uri, text } => Ok(json!({
                "type": "resource",
                "resource": { "uri": uri, "text": text }
            })),
        })
        .collect()
}

/// Locate a command: absolute/relative paths are checked as-is; bare names are
/// resolved against `PATH`.
fn which_binary(cmd: &str) -> Option<PathBuf> {
    let p = Path::new(cmd);
    if p.components().count() > 1 {
        return p.is_file().then(|| p.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(cmd))
        .find(|c| c.is_file())
}
