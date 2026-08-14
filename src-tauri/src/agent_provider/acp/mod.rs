//! Generic ACP v1 stdio adapter (architecture §8.1).
//!
//! Any ACP stdio agent can plug in as a provider without Rust changes: a
//! profile is just a command + env. [`AcpClient`] implements the provider-neutral
//! [`AgentClient`] contract as a lazy factory — it spawns the agent process only
//! when a session is created or a catalog is fetched. [`AcpSession`] owns one
//! spawned agent and maps its ACP events to the domain model.
//!
//! ```text
//! agent_provider
//!   acp/
//!     mod.rs      — AcpProfile, AcpClient (factory), capability/catalog mapping
//!     protocol.rs — ACP v1 wire types (NDJSON JSON-RPC)
//!     client.rs   — stdio child process + message demux
//!     session.rs  — AcpSession (ACP updates → AgentEvents, permission, turns)
//! ```

pub mod client;
pub mod protocol;
pub mod session;

use crate::agent_provider::acp::client::AcpConnection;
use crate::agent_provider::acp::protocol::{
    AgentCapabilities, ClientCapabilities, ClientInfo, InitializeParams, NewSessionParams,
    NewSessionResult, PROTOCOL_VERSION,
};
use crate::agent_provider::acp::session::{AcpSession, AcpSessionInit};
use crate::agent_provider::traits::{AgentClient, AgentEventSink, AgentSession};
use crate::agent_provider::types::*;
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Static configuration for one ACP provider (architecture §7.2).
#[derive(Debug, Clone)]
pub struct AcpProfile {
    pub provider_id: String,
    /// argv array (never a whitespace-split string).
    pub command: Vec<String>,
    /// Extra environment injected only into this provider's process.
    pub env: Vec<(String, String)>,
}

/// The default OpenCode profile (`opencode acp`).
pub fn opencode_profile() -> AcpProfile {
    AcpProfile {
        provider_id: "opencode".into(),
        command: vec!["opencode".into(), "acp".into()],
        env: vec![],
    }
}

/// A generic ACP v1 stdio provider client.
pub struct AcpClient {
    profile: AcpProfile,
}

impl AcpClient {
    pub fn new(profile: AcpProfile) -> Self {
        Self { profile }
    }

    pub fn profile(&self) -> &AcpProfile {
        &self.profile
    }

    async fn handshake_session(
        &self,
        config: &AgentSessionConfig,
        sink: Arc<dyn AgentEventSink>,
    ) -> Result<Arc<AcpSession>, AgentError> {
        let (conn, rx) =
            AcpConnection::spawn(&self.profile.command, &self.profile.env, &config.cwd)?;
        let init = initialize(&conn)?;
        let capabilities = map_capabilities(&init.agent_capabilities);
        let params = NewSessionParams {
            cwd: config.cwd.clone(),
            mcp_servers: vec![],
        };
        let raw = conn.send_request(
            "session/new",
            serde_json::to_value(&params).map_err(|e| AgentError::Protocol(e.to_string()))?,
        )?;
        let result: NewSessionResult =
            serde_json::from_value(raw).map_err(|e| AgentError::Protocol(e.to_string()))?;
        let session_id = result.session_id;
        let handle = PersistenceHandle {
            provider_id: self.profile.provider_id.clone(),
            runtime_session_id: session_id.clone(),
            native_handle: Some(serde_json::json!({ "protocolVersion": init.protocol_version })),
            metadata: None,
        };
        let session = Arc::new(AcpSession::new(AcpSessionInit {
            provider_id: self.profile.provider_id.clone(),
            agent_id: config.agent_id.clone(),
            runtime_session_id: session_id.clone(),
            capabilities,
            persistence: handle.clone(),
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

    async fn handshake_resume(
        &self,
        handle: PersistenceHandle,
        overrides: &AgentSessionConfig,
        sink: Arc<dyn AgentEventSink>,
    ) -> Result<Arc<AcpSession>, AgentError> {
        let (conn, rx) =
            AcpConnection::spawn(&self.profile.command, &self.profile.env, &overrides.cwd)?;
        let init = initialize(&conn)?;
        let capabilities = map_capabilities(&init.agent_capabilities);
        let session_id = handle.runtime_session_id.clone();
        // ACP `session/resume` reopens an existing native session in this (new)
        // process; the agent restores its own history server-side.
        let params = serde_json::json!({
            "sessionId": session_id,
            "cwd": overrides.cwd,
        });
        conn.send_request("session/resume", params)?;
        let session = Arc::new(AcpSession::new(AcpSessionInit {
            provider_id: self.profile.provider_id.clone(),
            agent_id: overrides.agent_id.clone(),
            runtime_session_id: session_id.clone(),
            capabilities,
            persistence: handle.clone(),
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
impl AgentClient for AcpClient {
    fn provider_id(&self) -> &str {
        &self.profile.provider_id
    }

    fn backend_kind(&self) -> &str {
        "acp"
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
        let (conn, rx) = AcpConnection::spawn(&self.profile.command, &self.profile.env, cwd)?;
        let init = initialize(&conn)?;
        let params = NewSessionParams {
            cwd: cwd.to_path_buf(),
            mcp_servers: vec![],
        };
        let raw = conn.send_request(
            "session/new",
            serde_json::to_value(&params).map_err(|e| AgentError::Protocol(e.to_string()))?,
        )?;
        let result: NewSessionResult =
            serde_json::from_value(raw).map_err(|e| AgentError::Protocol(e.to_string()))?;
        // Close the probe session and reap the child (best effort).
        let _ = conn.send_request(
            "session/close",
            serde_json::json!({ "sessionId": result.session_id }),
        );
        conn.shutdown();
        drop(rx);
        Ok(extract_catalog(
            &result.config_options,
            &init.agent_capabilities,
        ))
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

// ── Mapping helpers ─────────────────────────────────────────────

/// ACP `initialize` handshake + protocol-version negotiation.
fn initialize(
    conn: &AcpConnection,
) -> Result<crate::agent_provider::acp::protocol::InitializeResult, AgentError> {
    let params = InitializeParams {
        protocol_version: PROTOCOL_VERSION,
        client_capabilities: ClientCapabilities::default(),
        client_info: ClientInfo {
            name: "capilot-ide".into(),
            title: Some("CaPilot IDE".into()),
            version: Some(env!("CARGO_PKG_VERSION").into()),
        },
    };
    let raw = conn.send_request(
        "initialize",
        serde_json::to_value(&params).map_err(|e| AgentError::Protocol(e.to_string()))?,
    )?;
    let result: crate::agent_provider::acp::protocol::InitializeResult =
        serde_json::from_value(raw).map_err(|e| AgentError::Protocol(e.to_string()))?;
    if result.protocol_version != PROTOCOL_VERSION {
        return Err(AgentError::Protocol(format!(
            "unsupported ACP protocol version {} (need {PROTOCOL_VERSION})",
            result.protocol_version
        )));
    }
    Ok(result)
}

/// Map ACP negotiated capabilities onto the provider-neutral flag set. Phase 2
/// deliberately under-reports what the adapter cannot yet deliver: image prompts
/// (no base64 encoder), MCP server configuration, slash commands, and dynamic
/// config options are all surfaced as `false` so the UI never offers a path the
/// adapter would reject.
fn map_capabilities(ac: &AgentCapabilities) -> ProviderCapabilities {
    ProviderCapabilities {
        session_resume: ac.session_capabilities.resume.is_some() || ac.load_session,
        session_list: ac.session_capabilities.list.is_some(),
        structured_tools: true,
        reasoning_stream: true,
        permissions: true,
        config_options: false,
        slash_commands: false,
        mcp_servers: false,
        images: false,
        context_usage: true,
    }
}

fn extract_catalog(
    options: &[crate::agent_provider::acp::protocol::AcpConfigOption],
    capabilities: &AgentCapabilities,
) -> ProviderCatalog {
    let mut models = Vec::new();
    let mut config_options = Vec::new();
    for opt in options {
        match opt.r#type.as_deref() {
            Some("select") => {
                let current = opt
                    .current_value
                    .as_ref()
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let select_options: Vec<SelectOption> = opt
                    .options
                    .iter()
                    .map(|o| SelectOption {
                        id: o.value.clone(),
                        label: o.name.clone(),
                    })
                    .collect();
                if opt.id == "model" {
                    for o in &opt.options {
                        models.push(ModelDefinition {
                            id: o.value.clone(),
                            label: o.name.clone(),
                            context_window: None,
                            reasoning_efforts: vec![],
                            is_default: o.value == current,
                        });
                    }
                }
                config_options.push(ConfigOption::Select {
                    id: opt.id.clone(),
                    label: opt.name.clone(),
                    category: opt.category.clone(),
                    current,
                    options: select_options,
                });
            }
            Some("boolean") => {
                config_options.push(ConfigOption::Boolean {
                    id: opt.id.clone(),
                    label: opt.name.clone(),
                    category: opt.category.clone(),
                    current: opt
                        .current_value
                        .as_ref()
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                });
            }
            _ => {}
        }
    }
    if models.is_empty() {
        models.push(ModelDefinition {
            id: "default".into(),
            label: "Default".into(),
            context_window: None,
            reasoning_efforts: vec![],
            is_default: true,
        });
    }
    ProviderCatalog {
        models,
        config_options,
        capabilities: map_capabilities(capabilities),
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
