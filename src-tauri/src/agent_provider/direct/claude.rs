//! Claude Agent SDK sidecar adapter (architecture §8.1).
//!
//! Claude Code has no ACP support, so the Claude provider bridges the Node-side
//! Claude Agent SDK through a sidecar process (`scripts/claude_agent_server.mjs`)
//! that speaks the *same* JSON-RPC wire schema as the Codex Direct adapter. The
//! session machinery is therefore literally the Codex one — a provider that
//! passes the shared contract test is interchangeable at the UI layer, so this
//! adapter is a thin [`AgentClient`] over a [`CodexClient`] configured with the
//! sidecar command, plus its own availability/diagnostic probes.

use crate::agent_provider::direct::codex::{CodexClient, CodexProfile};
use crate::agent_provider::traits::{AgentClient, AgentEventSink, AgentSession};
use crate::agent_provider::types::*;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Static configuration for the Claude sidecar provider.
#[derive(Debug, Clone)]
pub struct ClaudeProfile {
    pub provider_id: String,
    /// `["node", "<scripts>/claude_agent_server.mjs"]` — never a
    /// whitespace-split string.
    pub command: Vec<String>,
    /// Extra environment injected only into this provider's process.
    pub env: Vec<(String, String)>,
}

/// The default Claude profile (Node sidecar + Agent SDK).
pub fn claude_profile() -> ClaudeProfile {
    ClaudeProfile {
        provider_id: "claude".into(),
        command: vec![
            "node".into(),
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/scripts/claude_agent_server.mjs"
            )
            .into(),
        ],
        env: vec![],
    }
}

/// A Claude Agent SDK provider client (sidecar-backed).
pub struct ClaudeClient {
    profile: ClaudeProfile,
    inner: CodexClient,
}

impl ClaudeClient {
    pub fn new(profile: ClaudeProfile) -> Self {
        let inner = CodexClient::new(CodexProfile {
            provider_id: profile.provider_id.clone(),
            command: profile.command.clone(),
            env: profile.env.clone(),
        });
        Self { profile, inner }
    }

    pub fn profile(&self) -> &ClaudeProfile {
        &self.profile
    }
}

#[async_trait]
impl AgentClient for ClaudeClient {
    fn provider_id(&self) -> &str {
        &self.profile.provider_id
    }

    fn backend_kind(&self) -> &str {
        "direct"
    }

    async fn is_available(&self) -> Result<bool, AgentError> {
        let binary = self
            .profile
            .command
            .first()
            .map(String::as_str)
            .unwrap_or_default();
        let binary_ok = which_binary(binary).is_some();
        // The sidecar script must exist too; a `node` on PATH alone is not a
        // usable Claude provider.
        let script_ok = self
            .profile
            .command
            .get(1)
            .map(|p| Path::new(p).is_file())
            .unwrap_or(true);
        Ok(binary_ok && script_ok)
    }

    async fn diagnostic(&self) -> Result<ProviderDiagnostic, AgentError> {
        let available = self.is_available().await?;
        let script = self.profile.command.get(1).cloned().unwrap_or_default();
        Ok(ProviderDiagnostic {
            available,
            authenticated: available,
            version: None,
            message: Some(if available {
                format!("sidecar: {script}")
            } else {
                format!("sidecar not found: {script} (run `cd src-tauri/scripts && npm install`)")
            }),
        })
    }

    async fn fetch_catalog(&self, cwd: &Path) -> Result<ProviderCatalog, AgentError> {
        self.inner.fetch_catalog(cwd).await
    }

    async fn create_session(
        &self,
        config: AgentSessionConfig,
        sink: Arc<dyn AgentEventSink>,
    ) -> Result<Arc<dyn AgentSession>, AgentError> {
        self.inner.create_session(config, sink).await
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
        self.inner.resume_session(handle, overrides, sink).await
    }
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
