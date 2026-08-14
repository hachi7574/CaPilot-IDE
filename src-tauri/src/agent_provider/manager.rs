//! AgentManager (architecture §3.4, §6.3, §10.2).
//!
//! The manager is the single authority for agent/session lifecycle, the
//! canonical timeline, pending permissions, provider runtime/config snapshot,
//! persistence handle, status, and the monotonic event sequence + reconnect
//! snapshot. The frontend consumes snapshots and events — it never derives
//! provider lifecycle itself.
//!
//! Providers plug in as [`AgentClient`]s registered by id. The manager is
//! Tauri-independent: the daemon owns one and forwards structured requests to
//! it; the GUI bridge reads snapshots from it.

use crate::agent_provider::timeline::TimelineStore;
use crate::agent_provider::traits::{AgentClient, AgentEventSink, AgentSession};
use crate::agent_provider::types::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Stable persisted identity of one agent (architecture §10.2). `config_json` /
/// `capabilities_json` / `persistence_json` are materialized as typed fields
/// here; a later phase may serialize them into the SQLite row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRecord {
    pub agent_id: String,
    pub provider_id: String,
    /// `acp` | `direct` | `legacy_pty`.
    pub backend_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub cwd: PathBuf,
    pub status: AgentStatus,
    #[serde(default)]
    pub config: Vec<(String, ConfigValue)>,
    pub capabilities: ProviderCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persistence: Option<PersistenceHandle>,
    /// High-water mark of the per-agent event sequence (for reconnect replay).
    pub last_event_seq: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Everything a reconnecting client needs to render an agent without missing
/// state: the record, the canonical timeline, and unresolved permissions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSnapshot {
    pub agent: AgentRecord,
    pub timeline: Vec<TimelineItem>,
    pub pending_permissions: Vec<PermissionRequest>,
    pub last_seq: u64,
}

/// A subscriber notified of every sequenced event (daemon broadcast).
pub trait AgentEventObserver: Send + Sync {
    fn on_agent_event(&self, agent_id: &str, seq: u64, event: &AgentEvent);
}

/// Request to create a new agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewAgentRequest {
    pub agent_id: String,
    pub provider_id: String,
    /// `acp` | `direct` | `legacy_pty`.
    pub backend_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub cwd: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub config: Vec<(String, ConfigValue)>,
}

/// Request to resume a previously-persisted agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeAgentRequest {
    pub agent_id: String,
    pub handle: PersistenceHandle,
    pub cwd: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub config: Vec<(String, ConfigValue)>,
}

struct ManagerInner {
    records: HashMap<String, AgentRecord>,
    timelines: HashMap<String, TimelineStore>,
    sessions: HashMap<String, Arc<dyn AgentSession>>,
    pending: HashMap<String, Vec<PermissionRequest>>,
    clients: HashMap<String, Arc<dyn AgentClient>>,
    observers: Vec<Arc<dyn AgentEventObserver>>,
}

/// Owns agent state. Cheap to clone (an `Arc`); share one instance across the
/// daemon and the GUI bridge.
#[derive(Clone)]
pub struct AgentManager {
    inner: Arc<Mutex<ManagerInner>>,
    /// Global monotonic event sequence (per-agent, across all agents within one
    /// daemon run). Each `process_event` stamps the next value.
    event_seq: Arc<AtomicU64>,
}

/// Sink handed to provider sessions; forwards events into the manager's state.
struct ManagerSink {
    manager: AgentManager,
}

impl AgentEventSink for ManagerSink {
    fn on_event(&self, agent_id: &str, event: AgentEvent) {
        self.manager.process_event(agent_id, event);
    }
}

impl AgentManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ManagerInner {
                records: HashMap::new(),
                timelines: HashMap::new(),
                sessions: HashMap::new(),
                pending: HashMap::new(),
                clients: HashMap::new(),
                observers: Vec::new(),
            })),
            event_seq: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Register (or replace) a provider client.
    pub fn register_provider(&self, client: Arc<dyn AgentClient>) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner
            .clients
            .insert(client.provider_id().to_string(), client);
    }

    pub fn has_provider(&self, provider_id: &str) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.clients.contains_key(provider_id)
    }

    pub fn provider_ids(&self) -> Vec<String> {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let mut ids: Vec<String> = inner.clients.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Registered providers with their backend kind (acp | direct), sorted by
    /// id. The daemon exposes this so the frontend creates agents with the
    /// provider's real backend kind instead of a hardcoded value (§18.2).
    pub fn provider_info(&self) -> Vec<ProviderInfo> {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let mut out: Vec<ProviderInfo> = inner
            .clients
            .values()
            .map(|c| ProviderInfo {
                provider_id: c.provider_id().to_string(),
                backend_kind: c.backend_kind().to_string(),
            })
            .collect();
        out.sort_by(|a, b| a.provider_id.cmp(&b.provider_id));
        out
    }

    /// Availability + auth state for a registered provider (architecture §5.3).
    pub async fn provider_diagnostic(
        &self,
        provider_id: &str,
    ) -> Result<ProviderDiagnostic, AgentError> {
        let client = self.client_for(provider_id)?;
        client.diagnostic().await
    }

    /// Fetch the provider's runtime-discovered catalog (models, config knobs,
    /// capabilities) for a given working directory (§5.2).
    pub async fn provider_catalog(
        &self,
        provider_id: &str,
        cwd: &Path,
    ) -> Result<ProviderCatalog, AgentError> {
        let client = self.client_for(provider_id)?;
        client.fetch_catalog(cwd).await
    }

    /// Subscribe to the sequenced event stream (daemon broadcast, reconnect
    /// replay). Returns nothing; the observer receives events as they occur.
    pub fn subscribe(&self, observer: Arc<dyn AgentEventObserver>) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.observers.push(observer);
    }

    // ── Lifecycle ────────────────────────────────────────────────

    /// Create a fresh agent through the registered provider.
    pub async fn create_agent(
        self: &Arc<Self>,
        request: NewAgentRequest,
    ) -> Result<AgentSnapshot, AgentError> {
        let client = self.client_for(&request.provider_id)?;
        // Reserve the record BEFORE the provider session starts emitting, so a
        // synchronous `SessionReady` (or any later async event) always finds it.
        {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if inner.records.contains_key(&request.agent_id) {
                return Err(AgentError::InvalidArgument(format!(
                    "agent already exists: {}",
                    request.agent_id
                )));
            }
            let now = now_ms();
            let record = AgentRecord {
                agent_id: request.agent_id.clone(),
                provider_id: request.provider_id.clone(),
                backend_kind: request.backend_kind.clone(),
                workspace_id: request.workspace_id.clone(),
                cwd: request.cwd.clone(),
                status: AgentStatus::Initializing,
                config: request.config.clone(),
                capabilities: ProviderCapabilities::default(),
                persistence: None,
                last_event_seq: 0,
                created_at: now,
                updated_at: now,
            };
            inner.records.insert(record.agent_id.clone(), record);
            inner
                .timelines
                .insert(request.agent_id.clone(), TimelineStore::new());
        }

        let sink = Arc::new(ManagerSink {
            manager: self.as_ref().clone(),
        });
        let session = match client
            .create_session(
                AgentSessionConfig {
                    agent_id: request.agent_id.clone(),
                    cwd: request.cwd.clone(),
                    model: request.model.clone(),
                    config: request.config.clone(),
                },
                sink,
            )
            .await
        {
            Ok(s) => s,
            Err(e) => {
                // Roll the reserved record back; the provider never started.
                let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
                inner.records.remove(&request.agent_id);
                inner.timelines.remove(&request.agent_id);
                return Err(e);
            }
        };

        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.sessions.insert(request.agent_id.clone(), session);
        self.snapshot_locked(&mut inner, &request.agent_id)
    }

    /// Resume an existing agent from a persisted handle. The provider client is
    /// resolved by `handle.provider_id` — a handle from another provider is
    /// rejected (a different provider is a handoff, not a resume, §10.3).
    pub async fn resume_agent(
        self: &Arc<Self>,
        request: ResumeAgentRequest,
    ) -> Result<AgentSnapshot, AgentError> {
        let client = self.client_for(&request.handle.provider_id)?;
        // Same-provider guard: switching providers is a new session, never a
        // silent "continue" of a foreign native session (§10.3).
        {
            let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(existing) = inner.records.get(&request.agent_id) {
                if existing.provider_id != request.handle.provider_id {
                    return Err(AgentError::InvalidArgument(format!(
                        "agent {} was created with provider {}, cannot resume with {} — create a new session to switch providers",
                        request.agent_id, existing.provider_id, request.handle.provider_id
                    )));
                }
            }
        }

        // Ensure a record exists before the provider replays its events.
        {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            inner
                .records
                .entry(request.agent_id.clone())
                .or_insert_with(|| AgentRecord {
                    agent_id: request.agent_id.clone(),
                    provider_id: request.handle.provider_id.clone(),
                    backend_kind: "direct".to_string(),
                    workspace_id: None,
                    cwd: request.cwd.clone(),
                    status: AgentStatus::Initializing,
                    config: request.config.clone(),
                    capabilities: ProviderCapabilities::default(),
                    persistence: Some(request.handle.clone()),
                    last_event_seq: 0,
                    created_at: now_ms(),
                    updated_at: now_ms(),
                });
            inner.timelines.entry(request.agent_id.clone()).or_default();
        }

        let sink = Arc::new(ManagerSink {
            manager: self.as_ref().clone(),
        });
        let session = client
            .resume_session(
                request.handle.clone(),
                AgentSessionConfig {
                    agent_id: request.agent_id.clone(),
                    cwd: request.cwd.clone(),
                    model: request.model.clone(),
                    config: request.config.clone(),
                },
                sink,
            )
            .await?;

        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.sessions.insert(request.agent_id.clone(), session);
        self.snapshot_locked(&mut inner, &request.agent_id)
    }

    /// Point-in-time snapshot for reconnect: record + timeline + pending
    /// permissions + the last applied sequence. The client then subscribes and
    /// applies only `seq > snapshot.last_seq`.
    pub fn snapshot(&self, agent_id: &str) -> Result<AgentSnapshot, AgentError> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        self.snapshot_locked(&mut inner, agent_id)
    }

    pub fn list_agents(&self) -> Vec<AgentRecord> {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let mut records: Vec<AgentRecord> = inner.records.values().cloned().collect();
        records.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        records
    }

    pub fn get_record(&self, agent_id: &str) -> Option<AgentRecord> {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.records.get(agent_id).cloned()
    }

    // ── Foreground turn / config / permission ────────────────────

    pub async fn start_turn(
        &self,
        agent_id: &str,
        prompt: AgentPrompt,
    ) -> Result<TurnId, AgentError> {
        let session = self.session_for(agent_id)?;
        session.start_turn(prompt).await
    }

    pub async fn interrupt(&self, agent_id: &str) -> Result<(), AgentError> {
        let session = self.session_for(agent_id)?;
        session.interrupt().await
    }

    pub async fn set_config_option(
        &self,
        agent_id: &str,
        config_id: &str,
        value: ConfigValue,
    ) -> Result<Vec<ConfigOption>, AgentError> {
        let session = self.session_for(agent_id)?;
        session.set_config_option(config_id, value).await
    }

    pub async fn respond_to_permission(
        &self,
        agent_id: &str,
        request_id: &str,
        action_id: &str,
    ) -> Result<(), AgentError> {
        {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let pending = inner
                .pending
                .get_mut(agent_id)
                .ok_or_else(|| AgentError::PermissionRequestNotFound(request_id.to_string()))?;
            let idx = pending
                .iter()
                .position(|r| r.id == request_id)
                .ok_or_else(|| AgentError::PermissionRequestNotFound(request_id.to_string()))?;
            // The chosen action must be one the request declared (§9.2.5).
            let declared = pending[idx].actions.iter().any(|a| a.id == action_id);
            if !declared {
                return Err(AgentError::InvalidArgument(format!(
                    "action {action_id} not declared by request {request_id}"
                )));
            }
            // Resolve only once.
            pending.remove(idx);
        }
        let session = self.session_for(agent_id)?;
        session.respond_to_permission(request_id, action_id).await
    }

    /// Release live resources. The native session and CaPilot record survive
    /// (close ≠ archive ≠ delete).
    pub async fn close_agent(&self, agent_id: &str) -> Result<(), AgentError> {
        let session = self.session_for(agent_id)?;
        session.close().await?;
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.sessions.remove(agent_id);
        if let Some(rec) = inner.records.get_mut(agent_id) {
            rec.status = AgentStatus::Closed;
            rec.updated_at = now_ms();
        }
        Ok(())
    }

    // ── Event application ─────────────────────────────────────────

    /// Apply one provider event to authoritative state and fan it out to
    /// observers with its sequence number. Called by [`ManagerSink`] from the
    /// provider's background task.
    pub fn process_event(&self, agent_id: &str, event: AgentEvent) {
        let seq = self.event_seq.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            self.apply_event_locked(&mut inner, agent_id, &event);
            if let Some(rec) = inner.records.get_mut(agent_id) {
                rec.last_event_seq = seq;
                rec.updated_at = now_ms();
            }
        }
        for observer in self.observers() {
            observer.on_agent_event(agent_id, seq, &event);
        }
    }

    fn apply_event_locked(&self, inner: &mut ManagerInner, agent_id: &str, event: &AgentEvent) {
        match event {
            AgentEvent::SessionReady(ready) => {
                if let Some(rec) = inner.records.get_mut(agent_id) {
                    rec.capabilities = ready.capabilities.clone();
                    rec.persistence = ready.persistence.clone();
                    rec.status = AgentStatus::Idle;
                }
            }
            AgentEvent::TurnStarted(_) => {
                if let Some(rec) = inner.records.get_mut(agent_id) {
                    rec.status = AgentStatus::Running;
                }
            }
            AgentEvent::Timeline(te) => {
                let store = inner.timelines.entry(agent_id.to_string()).or_default();
                store.apply(te);
            }
            AgentEvent::PermissionRequested(req) => {
                // Upsert by request id so a replayed/re-emitted request never
                // leaves a duplicate pending entry (§10.2 recoverability).
                let pending = inner.pending.entry(agent_id.to_string()).or_default();
                pending.retain(|existing| existing.id != req.id);
                pending.push(req.clone());
                if let Some(rec) = inner.records.get_mut(agent_id) {
                    rec.status = AgentStatus::WaitingPermission;
                }
            }
            AgentEvent::PermissionResolved(res) => {
                // Resolutions may arrive via replay without the local remove
                // path; keep pending authoritative by dropping the resolved id.
                if let Some(pending) = inner.pending.get_mut(agent_id) {
                    pending.retain(|r| r.id != res.request_id);
                }
                if let Some(rec) = inner.records.get_mut(agent_id) {
                    rec.status = AgentStatus::Running;
                }
            }
            AgentEvent::ConfigUpdated(options) => {
                if let Some(rec) = inner.records.get_mut(agent_id) {
                    for opt in options {
                        rec.config.retain(|(id, _)| id != opt.id());
                        let value = match opt {
                            ConfigOption::Select { current, .. } => {
                                ConfigValue::String(current.clone())
                            }
                            ConfigOption::Boolean { current, .. } => ConfigValue::Bool(*current),
                        };
                        rec.config.push((opt.id().to_string(), value));
                    }
                }
            }
            AgentEvent::ContextUsageUpdated(_) => {}
            AgentEvent::TurnCompleted(_) | AgentEvent::TurnCancelled(_) => {
                if let Some(rec) = inner.records.get_mut(agent_id) {
                    rec.status = AgentStatus::Idle;
                }
            }
            AgentEvent::TurnFailed(_) => {
                if let Some(rec) = inner.records.get_mut(agent_id) {
                    rec.status = AgentStatus::Error;
                }
            }
            AgentEvent::SessionClosed => {
                if let Some(rec) = inner.records.get_mut(agent_id) {
                    rec.status = AgentStatus::Closed;
                }
                inner.sessions.remove(agent_id);
            }
        }
    }

    // ── Internals ─────────────────────────────────────────────────

    fn client_for(&self, provider_id: &str) -> Result<Arc<dyn AgentClient>, AgentError> {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner
            .clients
            .get(provider_id)
            .cloned()
            .ok_or_else(|| AgentError::ProviderNotFound(provider_id.to_string()))
    }

    fn session_for(&self, agent_id: &str) -> Result<Arc<dyn AgentSession>, AgentError> {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner
            .sessions
            .get(agent_id)
            .cloned()
            .ok_or_else(|| AgentError::AgentNotFound(agent_id.to_string()))
    }

    fn observers(&self) -> Vec<Arc<dyn AgentEventObserver>> {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.observers.clone()
    }

    fn snapshot_locked(
        &self,
        inner: &mut ManagerInner,
        agent_id: &str,
    ) -> Result<AgentSnapshot, AgentError> {
        let agent = inner
            .records
            .get(agent_id)
            .cloned()
            .ok_or_else(|| AgentError::AgentNotFound(agent_id.to_string()))?;
        let timeline = inner
            .timelines
            .get(agent_id)
            .map(TimelineStore::items)
            .unwrap_or_default();
        let pending_permissions = inner.pending.get(agent_id).cloned().unwrap_or_default();
        let last_seq = self.event_seq.load(Ordering::SeqCst);
        Ok(AgentSnapshot {
            agent,
            timeline,
            pending_permissions,
            last_seq,
        })
    }
}

impl Default for AgentManager {
    fn default() -> Self {
        Self::new()
    }
}
