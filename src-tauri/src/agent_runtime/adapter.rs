use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Agent runtime identifier
pub type RuntimeId = String;

/// Agent session identifier
pub type AgentId = String;

/// Speed tier for thinking effort
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speed {
    High,
    Mid,
    Fast,
    Auto,
}

/// Permission mode for agent operations
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Ask,
    Auto,
    Yolo,
}

/// Agent role in the orchestration system
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Master,
    Worker,
    Standalone,
}

/// Agent lifecycle status
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Running,
    WaitingInput,
    Busy,
    Done,
    Failed,
}

/// Metadata about a runtime model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub provider: String,
}

/// Session configuration for spawning an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: AgentId,
    pub runtime: RuntimeId,
    pub mode: PermissionMode,
    pub speed: Speed,
    /// Selected model id (composer `[模型↑]`). `None` = runtime default.
    pub model: Option<String>,
    pub cwd: PathBuf,
    pub context_dir: PathBuf,
    pub role: AgentRole,
    pub rows: u16,
    pub cols: u16,
    /// Provider session id to resume (`None` = start fresh). Each adapter builds
    /// its own resume argv from this, so `claude` uses `--resume <id>` while
    /// other runtimes use their own flag.
    pub resume_key: Option<String>,
}

/// Handle to a running PTY process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: AgentId,
    pub workspace_id: Option<String>,
    pub project: Option<String>,
    pub runtime: RuntimeId,
    pub role: AgentRole,
    pub status: AgentStatus,
    pub title: String,
    pub cwd: PathBuf,
    pub pid: Option<u32>,
    /// Permission mode this session runs under ("ask" | "auto" | "yolo").
    pub mode: String,
    /// Speed tier this session runs under ("high" | "mid" | "fast" | "auto").
    pub speed: String,
    /// Selected model id, or None for the runtime default.
    pub model: Option<String>,
}

/// Summary of an available runtime detected on the system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub id: RuntimeId,
    pub name: String,
    pub available: bool,
    pub authenticated: bool,
    pub models: Vec<ModelInfo>,
}

/// The core trait that every agent CLI must implement.
/// Each runtime (claude, codex, opencode, etc.) gets one file in `runtimes/`.
pub trait AgentRuntimeAdapter: Send + Sync {
    /// Unique identifier (e.g. "claude", "codex")
    fn id(&self) -> &str;

    /// Human-readable display name
    fn name(&self) -> &str;

    /// Is the CLI binary installed and accessible on PATH?
    fn is_available(&self) -> bool;

    /// Is the user authenticated with this runtime?
    fn is_authenticated(&self) -> bool;

    /// List available models for this runtime
    fn list_models(&self) -> Vec<ModelInfo>;

    /// Spawn an interactive TUI session (PTY).
    /// Returns (command, args) to execute.
    fn spawn_interactive(&self, session: &AgentSession) -> Result<(String, Vec<String>), String>;

    /// Build args to resume an existing session
    fn resume_args(&self, session: &AgentSession) -> Vec<String>;

    /// Does this runtime have a resumable session concept? `false` runtimes
    /// (e.g. bash) skip resume-key capture after a fresh spawn.
    fn supports_resume(&self) -> bool {
        false
    }

    /// Best-effort: after a fresh interactive spawn, discover the provider
    /// session the just-started process created, so a later `agent_resume` can
    /// continue it. `None` when nothing is detectable yet / not applicable.
    fn capture_resume_key(&self, _cwd: &std::path::Path) -> Option<String> {
        None
    }

    /// Map speed tier to CLI arguments
    fn speed_args(&self, speed: Speed) -> Vec<String>;

    /// Map permission mode to CLI arguments
    fn mode_args(&self, mode: PermissionMode) -> Vec<String>;
}

/// Error type for agent operations
#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum AgentError {
    #[error("runtime not found: {0}")]
    RuntimeNotFound(String),
    #[error("runtime not available: {0}")]
    RuntimeNotAvailable(String),
    #[error("agent not found: {0}")]
    AgentNotFound(AgentId),
    #[error("PTY error: {0}")]
    PtyError(String),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

// Implement Serialize for AgentError so it can be returned from Tauri commands
impl Serialize for AgentError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}
