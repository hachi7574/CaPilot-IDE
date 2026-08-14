//! Deterministic fake provider (Phase 1 acceptance).
//!
//! Lets tests drive the full AgentManager lifecycle without a real CLI: create a
//! session, stream timeline events, request+resolve a permission, cancel a turn,
//! and resume from a persisted handle (which replays the recorded events). The
//! fake is `cfg(test)`-only — the real ACP/Direct adapters arrive in later
//! phases and must pass the same contract.

#![cfg(test)]

use crate::agent_provider::traits::{AgentClient, AgentEventSink, AgentSession};
use crate::agent_provider::types::*;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

static FAKE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_id(prefix: &str) -> String {
    let n = FAKE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{n}")
}

/// A controllable fake session. `emit` records the event for resume-replay AND
/// forwards it to the manager sink, exactly like a real adapter's background
/// task would.
pub struct FakeSession {
    pub provider_id: String,
    pub runtime_session_id: String,
    pub agent_id: String,
    pub capabilities: ProviderCapabilities,
    sink: Mutex<Option<Arc<dyn AgentEventSink>>>,
    persistence: Mutex<Option<PersistenceHandle>>,
    /// Events emitted so far, for resume replay (shared with the provider).
    replay: Arc<Mutex<Vec<AgentEvent>>>,
    closed: AtomicBool,
}

impl FakeSession {
    pub fn emit(&self, event: AgentEvent) {
        // `SessionClosed` is a live-runtime signal, not durable state — never
        // replay it into a freshly resumed session.
        let durable = !matches!(event, AgentEvent::SessionClosed);
        if durable {
            if let Ok(mut replay) = self.replay.lock() {
                replay.push(event.clone());
            }
        }
        if let Some(sink) = self.sink.lock().unwrap_or_else(|p| p.into_inner()).clone() {
            sink.on_event(&self.agent_id, event);
        }
    }

    pub fn emit_session_ready(&self) {
        self.emit(AgentEvent::SessionReady(SessionReady {
            provider_id: self.provider_id.clone(),
            runtime_session_id: Some(self.runtime_session_id.clone()),
            capabilities: self.capabilities.clone(),
            persistence: self
                .persistence
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
        }));
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl AgentSession for FakeSession {
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
        let turn_id = next_id("turn");
        self.emit(AgentEvent::TurnStarted(TurnStarted {
            turn_id: turn_id.clone(),
            client_message_id: prompt.client_message_id.clone(),
        }));
        Ok(turn_id)
    }

    async fn interrupt(&self) -> Result<(), AgentError> {
        self.emit(AgentEvent::TurnCancelled(TurnCancelled {
            turn_id: next_id("turn"),
        }));
        Ok(())
    }

    async fn set_config_option(
        &self,
        config_id: &str,
        value: ConfigValue,
    ) -> Result<Vec<ConfigOption>, AgentError> {
        let mut options = vec![];
        if let ConfigValue::String(s) = &value {
            options.push(ConfigOption::Select {
                id: config_id.to_string(),
                label: config_id.to_string(),
                category: None,
                current: s.clone(),
                options: vec![],
            });
        } else if let ConfigValue::Bool(b) = value {
            options.push(ConfigOption::Boolean {
                id: config_id.to_string(),
                label: config_id.to_string(),
                category: None,
                current: b,
            });
        }
        self.emit(AgentEvent::ConfigUpdated(options.clone()));
        Ok(options)
    }

    async fn respond_to_permission(
        &self,
        request_id: &str,
        action_id: &str,
    ) -> Result<(), AgentError> {
        self.emit(AgentEvent::PermissionResolved(PermissionResolution {
            request_id: request_id.to_string(),
            action_id: action_id.to_string(),
            resolved_at: now_ms(),
        }));
        Ok(())
    }

    fn describe_persistence(&self) -> Option<PersistenceHandle> {
        self.persistence
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    async fn close(&self) -> Result<(), AgentError> {
        self.emit(AgentEvent::SessionClosed);
        self.closed.store(true, Ordering::Relaxed);
        Ok(())
    }
}

/// Fake provider that keeps per-session replay logs for resume tests.
pub struct FakeProvider {
    provider_id: String,
    capabilities: ProviderCapabilities,
    catalog: ProviderCatalog,
    sessions: Mutex<HashMap<String, Arc<FakeSession>>>,
    /// runtime_session_id → shared event log (the session appends, resume
    /// replays the same `Arc`).
    replay: Mutex<HashMap<String, Arc<Mutex<Vec<AgentEvent>>>>>,
    /// Per-provider session counter so `rsession-0` is deterministic within one
    /// provider even when tests run in parallel.
    counter: AtomicU64,
}

impl FakeProvider {
    pub fn new(provider_id: &str, capabilities: ProviderCapabilities) -> Arc<Self> {
        let catalog = ProviderCatalog {
            models: vec![ModelDefinition {
                id: "fake-model".into(),
                label: "Fake Model".into(),
                context_window: Some(200_000),
                reasoning_efforts: vec![],
                is_default: true,
            }],
            config_options: vec![],
            capabilities: capabilities.clone(),
        };
        Arc::new(Self {
            provider_id: provider_id.to_string(),
            capabilities,
            catalog,
            sessions: Mutex::new(HashMap::new()),
            replay: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(0),
        })
    }

    fn next_session_id(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("rsession-{n}")
    }

    /// Grab the live fake session for direct event driving in tests.
    pub fn session(&self, runtime_session_id: &str) -> Option<Arc<FakeSession>> {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(runtime_session_id)
            .cloned()
    }

    pub fn session_count(&self) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .len()
    }
}

#[async_trait]
impl AgentClient for FakeProvider {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    fn backend_kind(&self) -> &str {
        "acp"
    }

    async fn is_available(&self) -> Result<bool, AgentError> {
        Ok(true)
    }

    async fn diagnostic(&self) -> Result<ProviderDiagnostic, AgentError> {
        Ok(ProviderDiagnostic {
            available: true,
            authenticated: true,
            version: Some("fake-1.0".into()),
            message: None,
        })
    }

    async fn fetch_catalog(&self, _cwd: &Path) -> Result<ProviderCatalog, AgentError> {
        Ok(self.catalog.clone())
    }

    async fn create_session(
        &self,
        config: AgentSessionConfig,
        sink: Arc<dyn AgentEventSink>,
    ) -> Result<Arc<dyn AgentSession>, AgentError> {
        let runtime_session_id = self.next_session_id();
        let replay = Arc::new(Mutex::new(Vec::new()));
        let session = Arc::new(FakeSession {
            provider_id: self.provider_id.clone(),
            runtime_session_id: runtime_session_id.clone(),
            agent_id: config.agent_id.clone(),
            capabilities: self.capabilities.clone(),
            sink: Mutex::new(Some(sink)),
            persistence: Mutex::new(Some(PersistenceHandle {
                provider_id: self.provider_id.clone(),
                runtime_session_id: runtime_session_id.clone(),
                native_handle: Some(serde_json::json!({ "kind": "fake" })),
                metadata: None,
            })),
            replay: replay.clone(),
            closed: AtomicBool::new(false),
        });
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(runtime_session_id.clone(), session.clone());
        self.replay
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(runtime_session_id.clone(), replay);
        session.emit_session_ready();
        Ok(session)
    }

    async fn resume_session(
        &self,
        handle: PersistenceHandle,
        overrides: AgentSessionConfig,
        sink: Arc<dyn AgentEventSink>,
    ) -> Result<Arc<dyn AgentSession>, AgentError> {
        if handle.provider_id != self.provider_id {
            return Err(AgentError::InvalidArgument(format!(
                "handle from provider {} cannot resume {}",
                handle.provider_id, self.provider_id
            )));
        }
        let runtime_session_id = handle.runtime_session_id.clone();
        let replay = self
            .replay
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&runtime_session_id)
            .cloned()
            .ok_or_else(|| AgentError::Provider("unknown runtime session for resume".into()))?;
        let session = Arc::new(FakeSession {
            provider_id: self.provider_id.clone(),
            runtime_session_id: runtime_session_id.clone(),
            agent_id: overrides.agent_id.clone(),
            capabilities: self.capabilities.clone(),
            sink: Mutex::new(Some(sink)),
            persistence: Mutex::new(Some(handle)),
            replay: replay.clone(),
            closed: AtomicBool::new(false),
        });
        self.sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(runtime_session_id.clone(), session.clone());
        // Replay the recorded events so the manager rebuilds timeline/status.
        let recorded = replay.lock().unwrap_or_else(|p| p.into_inner()).clone();
        for event in recorded {
            session.emit(event);
        }
        Ok(session)
    }
}

/// Convenience: a provider with every capability enabled.
pub fn full_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        session_resume: true,
        session_list: true,
        structured_tools: true,
        reasoning_stream: true,
        permissions: true,
        config_options: true,
        slash_commands: true,
        mcp_servers: false,
        images: true,
        context_usage: true,
    }
}

/// A default provider with the standard fake model catalog.
pub fn default_provider() -> Arc<FakeProvider> {
    FakeProvider::new("fake", full_capabilities())
}
