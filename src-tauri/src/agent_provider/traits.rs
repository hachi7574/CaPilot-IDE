//! Provider adapter contracts (architecture §5.3, §5.4).
//!
//! A provider adapter owns one native protocol (ACP stdio, Direct SDK/RPC/...)
//! and exposes it as [`AgentClient`] + [`AgentSession`]. It converts native
//! events to [`AgentEvent`]s and native config/permission/cancel back to native
//! requests. It never touches the frontend store, the canonical timeline, or
//! the daemon wire types.

use crate::agent_provider::types::*;
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

/// Sink a provider session pushes [`AgentEvent`]s into. Synchronous on purpose:
/// the provider's background task emits events from any thread, and the manager
/// applies them immediately under its own lock (no queue, no reordering).
pub trait AgentEventSink: Send + Sync {
    fn on_event(&self, agent_id: &str, event: AgentEvent);
}

/// A provider's session factory. One client per provider profile.
#[async_trait]
pub trait AgentClient: Send + Sync {
    fn provider_id(&self) -> &str;

    /// Backend kind this adapter implements: `"acp"` (generic ACP stdio) or
    /// `"direct"` (native protocol). The daemon exposes it via `provider_list`
    /// so the frontend creates agents with the real backend kind instead of a
    /// hardcoded value (handoff §6 / architecture §18.2).
    fn backend_kind(&self) -> &str;

    async fn is_available(&self) -> Result<bool, AgentError>;
    async fn diagnostic(&self) -> Result<ProviderDiagnostic, AgentError>;
    async fn fetch_catalog(&self, cwd: &Path) -> Result<ProviderCatalog, AgentError>;

    /// Spawn a brand-new native session. The returned session must already emit
    /// `SessionReady` through `sink` once its native `initialize`/`session/new`
    /// completes.
    async fn create_session(
        &self,
        config: AgentSessionConfig,
        sink: Arc<dyn AgentEventSink>,
    ) -> Result<Arc<dyn AgentSession>, AgentError>;

    /// Continue a previously-persisted native session. The adapter must reject a
    /// handle whose `provider_id` does not match its own (a handle from another
    /// provider is not "resume", it is a handoff).
    async fn resume_session(
        &self,
        handle: PersistenceHandle,
        overrides: AgentSessionConfig,
        sink: Arc<dyn AgentEventSink>,
    ) -> Result<Arc<dyn AgentSession>, AgentError>;
}

/// One live provider session.
#[async_trait]
pub trait AgentSession: Send + Sync {
    fn provider_id(&self) -> &str;
    fn runtime_session_id(&self) -> Option<&str>;
    fn capabilities(&self) -> &ProviderCapabilities;

    /// Begin a foreground turn. Returns the provider turn id; the turn's
    /// timeline/status arrives asynchronously through the event sink.
    async fn start_turn(&self, prompt: AgentPrompt) -> Result<TurnId, AgentError>;

    /// Cancel the in-flight turn (Esc/stop in the UI).
    async fn interrupt(&self) -> Result<(), AgentError>;

    async fn set_config_option(
        &self,
        config_id: &str,
        value: ConfigValue,
    ) -> Result<Vec<ConfigOption>, AgentError>;

    /// Resolve a pending permission request. `request_id`/`action_id` must be
    /// values the provider itself emitted; the adapter must not invent actions.
    async fn respond_to_permission(
        &self,
        request_id: &str,
        action_id: &str,
    ) -> Result<(), AgentError>;

    /// Provider-native pointer for later resume. `None` when the provider has no
    /// durable session concept.
    fn describe_persistence(&self) -> Option<PersistenceHandle>;

    /// Release live resources only — never deletes the native session and never
    /// archives the CaPilot agent record.
    async fn close(&self) -> Result<(), AgentError>;
}
