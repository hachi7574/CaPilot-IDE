//! ACP session adapter (architecture §8.1).
//!
//! [`AcpSession`] implements the provider-neutral [`AgentSession`] contract over
//! one ACP connection. Its message-loop thread consumes the connection's inbound
//! channel and maps ACP `session/update` notifications to canonical timeline
//! events, answers `session/request_permission` requests, and turns the
//! `session/prompt` response (stop reason) into a `TurnCompleted`/`TurnCancelled`/
//! `TurnFailed` event.

use crate::agent_provider::acp::client::{AcpConnection, Inbound, CLOSE_TIMEOUT};
use crate::agent_provider::acp::protocol::{
    ContentBlockEnvelope, RequestPermissionParams, SessionUpdate,
};
use crate::agent_provider::traits::{AgentEventSink, AgentSession};
use crate::agent_provider::types::*;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, Weak};

/// Everything an [`AcpSession`] needs at construction (kept out of the struct so
/// the fields map 1:1 to immutable session identity).
pub struct AcpSessionInit {
    pub provider_id: String,
    pub agent_id: String,
    pub runtime_session_id: String,
    pub capabilities: ProviderCapabilities,
    pub persistence: PersistenceHandle,
    pub conn: Arc<AcpConnection>,
    pub sink: Arc<dyn AgentEventSink>,
}

/// Bookkeeping for an unresolved permission: the JSON-RPC id to answer, the ACP
/// session it belongs to, and the declared actions (mirror of the request).
struct PermissionMeta {
    rpc_id: u64,
    actions: Vec<PermissionAction>,
}

/// One in-flight foreground turn.
struct ActiveTurn {
    turn_id: String,
    terminal_emitted: AtomicBool,
}

pub struct AcpSession {
    provider_id: String,
    agent_id: String,
    runtime_session_id: String,
    capabilities: ProviderCapabilities,
    persistence: PersistenceHandle,
    conn: Arc<AcpConnection>,
    sink: Arc<dyn AgentEventSink>,
    /// Set after construction so `start_turn` can hand a `Weak<Self>` to the
    /// response callback (the callback runs on the reader thread).
    self_weak: Mutex<Option<Weak<AcpSession>>>,
    /// message_id → whether a Started event was already emitted (append vs new).
    messages: Mutex<HashMap<String, bool>>,
    tool_calls: Mutex<HashMap<String, ToolCallItem>>,
    permissions: Mutex<HashMap<String, PermissionMeta>>,
    active_turn: Mutex<Option<ActiveTurn>>,
    closed: AtomicBool,
}

impl AcpSession {
    pub fn new(init: AcpSessionInit) -> Self {
        Self {
            provider_id: init.provider_id,
            agent_id: init.agent_id,
            runtime_session_id: init.runtime_session_id,
            capabilities: init.capabilities,
            persistence: init.persistence,
            conn: init.conn,
            sink: init.sink,
            self_weak: Mutex::new(None),
            messages: Mutex::new(HashMap::new()),
            tool_calls: Mutex::new(HashMap::new()),
            permissions: Mutex::new(HashMap::new()),
            active_turn: Mutex::new(None),
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

    pub fn set_self_weak(&self, weak: Weak<AcpSession>) {
        *self.self_weak.lock().unwrap_or_else(|p| p.into_inner()) = Some(weak);
    }

    fn upgrade_self(&self) -> Option<Arc<AcpSession>> {
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
        if method != "session/update" {
            return;
        }
        let update = params
            .get("update")
            .cloned()
            .and_then(|u| serde_json::from_value::<SessionUpdate>(u).ok());
        if let Some(update) = update {
            self.apply_update(update);
        }
    }

    fn handle_request(&self, id: u64, method: &str, params: &Value) {
        match method {
            "session/request_permission" => self.handle_permission_request(id, params),
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
                message: "agent process exited".into(),
            }));
        }
        self.emit(AgentEvent::SessionClosed);
    }

    // ── session/update mapping ───────────────────────────────────

    fn apply_update(&self, update: SessionUpdate) {
        match update {
            SessionUpdate::UserMessageChunk { .. } => {
                // The client renders the prompt it submitted itself; the agent's
                // echo of it would duplicate the user item, so we skip it.
            }
            SessionUpdate::AgentMessageChunk {
                message_id,
                content,
            } => {
                if let Some(text) = content.text() {
                    let id = message_id.unwrap_or_else(|| format!("assistant-{}", now_ms()));
                    self.append_message(&id, &text, MessageRole::Assistant, false);
                }
            }
            SessionUpdate::ThoughtChunk {
                message_id,
                content,
            } => {
                if let Some(text) = content.text() {
                    let id = message_id.unwrap_or_else(|| format!("thought-{}", now_ms()));
                    self.append_message(&id, &text, MessageRole::Assistant, true);
                }
            }
            SessionUpdate::ToolCall {
                tool_call_id,
                title,
                kind,
                status,
                raw_input,
            } => {
                let title = title.unwrap_or_else(|| "tool".into());
                let metadata = serde_json::json!({ "kind": kind, "title": title });
                let item = ToolCallItem {
                    item_id: tool_call_id.clone(),
                    tool_name: title.clone(),
                    tool_input: raw_input,
                    tool_output: None,
                    status: map_tool_status(status.as_deref()),
                    created_at: now_ms(),
                    metadata: Some(metadata),
                };
                self.tool_calls
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(tool_call_id.clone(), item.clone());
                self.emit(AgentEvent::Timeline(TimelineEvent::Started {
                    item: TimelineItem::ToolCall(item),
                }));
            }
            SessionUpdate::ToolCallUpdate {
                tool_call_id,
                title,
                kind,
                status,
                content,
                raw_input,
            } => {
                self.apply_tool_call_update(&tool_call_id, title, kind, status, content, raw_input)
            }
            SessionUpdate::Plan { entries } => {
                let title = entries
                    .first()
                    .map(|e| e.content.clone())
                    .unwrap_or_else(|| "Plan".into());
                let body = entries
                    .iter()
                    .map(|e| e.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                self.emit(AgentEvent::Timeline(TimelineEvent::Started {
                    item: TimelineItem::Plan(PlanItem {
                        item_id: format!("plan-{}", now_ms()),
                        title,
                        content: body,
                        created_at: now_ms(),
                    }),
                }));
            }
            SessionUpdate::UsageUpdate { used, size, .. } => {
                self.emit(AgentEvent::ContextUsageUpdated(ContextUsage {
                    context_window_used_tokens: Some(used),
                    context_window_max_tokens: Some(size),
                }));
            }
            // available_commands / mode_changed / unknown: not surfaced in Phase 2.
            SessionUpdate::AvailableCommandsUpdate { .. }
            | SessionUpdate::ModeChanged { .. }
            | SessionUpdate::Unknown => {}
        }
    }

    fn append_message(&self, id: &str, delta: &str, role: MessageRole, reasoning: bool) {
        let mut messages = self.messages.lock().unwrap_or_else(|p| p.into_inner());
        let started = messages.get(id).copied().unwrap_or(false);
        messages.insert(id.to_string(), true);
        drop(messages);
        if !started {
            let item = if reasoning {
                TimelineItem::Reasoning(MessageItem {
                    item_id: id.into(),
                    role,
                    text: delta.into(),
                    created_at: now_ms(),
                    metadata: None,
                })
            } else {
                TimelineItem::AssistantMessage(MessageItem {
                    item_id: id.into(),
                    role,
                    text: delta.into(),
                    created_at: now_ms(),
                    metadata: None,
                })
            };
            self.emit(AgentEvent::Timeline(TimelineEvent::Started { item }));
        } else {
            self.emit(AgentEvent::Timeline(TimelineEvent::Appended {
                item_id: id.into(),
                text_delta: delta.into(),
            }));
        }
    }

    fn apply_tool_call_update(
        &self,
        tool_call_id: &str,
        title: Option<String>,
        kind: Option<String>,
        status: Option<String>,
        content: Option<Vec<ContentBlockEnvelope>>,
        raw_input: Option<Value>,
    ) {
        let status = map_tool_status(status.as_deref());
        let mut map = self.tool_calls.lock().unwrap_or_else(|p| p.into_inner());
        let item = match map.get_mut(tool_call_id) {
            Some(existing) => {
                if let Some(t) = title {
                    existing.tool_name = t;
                }
                if let Some(raw) = raw_input {
                    existing.tool_input = Some(raw);
                }
                if let Some(blocks) = content {
                    let text = blocks
                        .iter()
                        .filter_map(|e| e.content.text())
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !text.is_empty() {
                        existing.tool_output = Some(serde_json::json!({ "text": text }));
                    }
                }
                existing.status = status.clone();
                existing.clone()
            }
            None => {
                // Update arrived without a preceding `tool_call` (some agents
                // only send updates): synthesize the item from what we have.
                let title = title.clone().unwrap_or_else(|| "tool".into());
                let new = ToolCallItem {
                    item_id: tool_call_id.into(),
                    tool_name: title,
                    tool_input: raw_input.clone(),
                    tool_output: None,
                    status: status.clone(),
                    created_at: now_ms(),
                    metadata: kind.map(|k| serde_json::json!({ "kind": k })),
                };
                map.insert(tool_call_id.into(), new.clone());
                new
            }
        };
        drop(map);

        let terminal = matches!(
            status,
            ToolCallStatus::Completed | ToolCallStatus::Failed | ToolCallStatus::Cancelled
        );
        // Always replace so output/input merge lands; then finish terminal calls.
        self.emit(AgentEvent::Timeline(TimelineEvent::Replaced {
            item: TimelineItem::ToolCall(item),
        }));
        if terminal {
            let item_status = match &status {
                ToolCallStatus::Completed => ItemStatus::Complete,
                ToolCallStatus::Failed => ItemStatus::Failed,
                ToolCallStatus::Cancelled => ItemStatus::Cancelled,
                _ => ItemStatus::Complete,
            };
            self.emit(AgentEvent::Timeline(TimelineEvent::Finished {
                item_id: tool_call_id.into(),
                status: item_status,
            }));
        }
    }

    // ── Permission ───────────────────────────────────────────────

    fn handle_permission_request(&self, id: u64, params: &Value) {
        match serde_json::from_value::<RequestPermissionParams>(params.clone()) {
            Ok(p) => {
                let title = p
                    .tool_call
                    .tool_title
                    .clone()
                    .unwrap_or_else(|| "Tool".into());
                let kind = map_permission_kind(p.tool_call.tool_kind.as_deref());
                let actions: Vec<PermissionAction> = p
                    .options
                    .iter()
                    .map(|o| PermissionAction {
                        id: o.option_id.clone(),
                        label: o.name.clone().unwrap_or_else(|| o.option_id.clone()),
                        behavior: map_permission_behavior(o.kind.as_deref()),
                    })
                    .collect();
                let request_id = id.to_string();
                self.permissions
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(
                        request_id.clone(),
                        PermissionMeta {
                            rpc_id: id,
                            actions: actions.clone(),
                        },
                    );
                self.emit(AgentEvent::PermissionRequested(PermissionRequest {
                    id: request_id,
                    agent_id: self.agent_id.clone(),
                    kind: kind.clone(),
                    title: title.clone(),
                    description: p.reason.clone(),
                    subject: PermissionSubject {
                        kind,
                        title,
                        description: p.reason,
                        icon: None,
                    },
                    actions,
                }));
            }
            Err(e) => {
                let _ = self.conn.respond_error(
                    id,
                    -32602,
                    &format!("invalid request_permission: {e}"),
                );
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
            let _ = self.conn.respond(
                meta.rpc_id,
                serde_json::json!({ "outcome": { "outcome": "cancelled" } }),
            );
            self.emit(AgentEvent::PermissionResolved(PermissionResolution {
                request_id,
                action_id: "cancelled".into(),
                resolved_at: now_ms(),
            }));
        }
    }

    fn cancel_pending_tool_calls(&self) {
        let ids: Vec<String> = {
            let map = self.tool_calls.lock().unwrap_or_else(|p| p.into_inner());
            map.iter()
                .filter(|(_, t)| {
                    matches!(t.status, ToolCallStatus::Pending | ToolCallStatus::Running)
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in ids {
            self.emit(AgentEvent::Timeline(TimelineEvent::Finished {
                item_id: id,
                status: ItemStatus::Cancelled,
            }));
        }
    }

    // ── Turn finalization ────────────────────────────────────────

    /// Runs on the reader thread when the `session/prompt` response arrives.
    fn finalize_turn(&self, turn_id: &str, result: Result<Value, AgentError>) {
        let active = self.active_turn.lock().unwrap_or_else(|p| p.into_inner());
        let Some(at) = active.as_ref() else { return };
        if at.turn_id != turn_id {
            return;
        }
        if at.terminal_emitted.swap(true, Ordering::SeqCst) {
            return;
        }
        let turn_id = at.turn_id.clone();
        drop(active);

        match result {
            Ok(v) => match v.get("stopReason").and_then(Value::as_str) {
                Some("cancelled") => {
                    self.emit(AgentEvent::TurnCancelled(TurnCancelled { turn_id }))
                }
                Some("refusal") => self.emit(AgentEvent::TurnFailed(TurnFailed {
                    turn_id,
                    message: "the agent refused the prompt".into(),
                })),
                // end_turn / max_tokens / max_turn_requests — all a completed turn.
                _ => self.emit(AgentEvent::TurnCompleted(TurnCompleted { turn_id })),
            },
            Err(e) => self.emit(AgentEvent::TurnFailed(TurnFailed {
                turn_id,
                message: e.to_string(),
            })),
        }
    }
}

#[async_trait]
impl AgentSession for AcpSession {
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
        // Render the user message from the client side immediately (the agent's
        // `user_message_chunk` echo would duplicate it).
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
        let params = serde_json::json!({ "sessionId": self.runtime_session_id, "prompt": content });
        let turn_id = format!("turn-{}", now_ms());
        // Register the turn before writing the prompt so a fast agent reply can
        // never find `active_turn` missing when `finalize_turn` runs.
        *self.active_turn.lock().unwrap_or_else(|p| p.into_inner()) = Some(ActiveTurn {
            turn_id: turn_id.clone(),
            terminal_emitted: AtomicBool::new(false),
        });
        let weak = self.upgrade_self();
        let turn_id_for_cb = turn_id.clone();
        if let Err(e) = self.conn.send_request_async(
            "session/prompt",
            params,
            Box::new(move |res| {
                if let Some(session) = weak {
                    let err = res.map_err(|e| AgentError::Protocol(e.message));
                    session.finalize_turn(&turn_id_for_cb, err);
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
        // Per ACP: notify cancel, resolve any pending permission with `cancelled`,
        // mark in-flight tool calls cancelled, then emit the turn cancel.
        let _ = self.conn.send_notification(
            "session/cancel",
            json!({ "sessionId": self.runtime_session_id }),
        );
        self.cancel_pending_permissions();
        self.cancel_pending_tool_calls();

        let mut active = self.active_turn.lock().unwrap_or_else(|p| p.into_inner());
        let turn_id = active
            .as_ref()
            .map(|a| a.turn_id.clone())
            .unwrap_or_else(|| format!("turn-{}", now_ms()));
        // Same atomic swap-guard as `finalize_turn`: whoever claims the terminal
        // state first emits, so a fast agent reply racing our cancel can never
        // double-emit TurnCancelled.
        let won = match active.as_mut() {
            Some(a) => !a.terminal_emitted.swap(true, Ordering::SeqCst),
            None => true,
        };
        drop(active);

        if won {
            self.emit(AgentEvent::TurnCancelled(TurnCancelled { turn_id }));
        }
        Ok(())
    }

    async fn set_config_option(
        &self,
        config_id: &str,
        _value: ConfigValue,
    ) -> Result<Vec<ConfigOption>, AgentError> {
        // ACP v1 exposes no generic dynamic-config method; the catalog is
        // display-only. `config_options` capability is false, so the UI never
        // offers this path.
        Err(AgentError::Provider(format!(
            "ACP v1 has no dynamic config option '{config_id}'"
        )))
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
        self.conn.respond(
            meta.rpc_id,
            serde_json::json!({ "outcome": { "outcome": "selected", "optionId": action_id } }),
        )?;
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
        // Graceful close, then reap the child regardless of whether the agent
        // answered (a hung agent must not block the caller for long).
        let _ = self.conn.send_request_timeout(
            "session/close",
            json!({ "sessionId": self.runtime_session_id }),
            CLOSE_TIMEOUT,
        );
        self.conn.shutdown();
        self.emit(AgentEvent::SessionClosed);
        Ok(())
    }
}

// ── Mapping helpers ─────────────────────────────────────────────

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

fn map_prompt_content(prompt: &AgentPrompt) -> Result<Vec<serde_json::Value>, AgentError> {
    prompt
        .content
        .iter()
        .map(|c| match c {
            PromptContent::Text { text } => Ok(serde_json::json!({ "type": "text", "text": text })),
            PromptContent::Image { .. } => Err(AgentError::UnsupportedContent),
            PromptContent::Resource { uri, text } => Ok(serde_json::json!({
                "type": "resource",
                "resource": { "uri": uri, "text": text }
            })),
        })
        .collect()
}

fn map_tool_status(status: Option<&str>) -> ToolCallStatus {
    match status {
        Some("in_progress") | Some("running") => ToolCallStatus::Running,
        Some("completed") => ToolCallStatus::Completed,
        Some("failed") => ToolCallStatus::Failed,
        Some("cancelled") => ToolCallStatus::Cancelled,
        _ => ToolCallStatus::Pending,
    }
}

fn map_permission_kind(kind: Option<&str>) -> PermissionKind {
    match kind {
        Some("write" | "edit" | "create" | "delete" | "move") => PermissionKind::FileChange,
        Some("execute" | "run" | "terminal") => PermissionKind::TerminalCommand,
        Some("think") => PermissionKind::Other,
        _ => PermissionKind::ToolCall,
    }
}

fn map_permission_behavior(kind: Option<&str>) -> PermissionBehavior {
    match kind {
        Some("allow_once") | Some("allow_always") => PermissionBehavior::Allow,
        Some("reject_once") | Some("reject_always") => PermissionBehavior::Deny,
        _ => PermissionBehavior::Ask,
    }
}
