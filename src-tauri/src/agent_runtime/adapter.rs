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
    pub cwd: PathBuf,
    pub context_dir: PathBuf,
    pub role: AgentRole,
    pub rows: u16,
    pub cols: u16,
}

/// Handle to a running PTY process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub id: AgentId,
    pub runtime: RuntimeId,
    pub role: AgentRole,
    pub status: AgentStatus,
    pub title: String,
    pub cwd: PathBuf,
    pub pid: Option<u32>,
}

/// Result of a headless (non-interactive) agent run
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct HeadlessRun {
    pub agent_id: AgentId,
    pub output: String,
    pub exit_code: i32,
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
#[allow(dead_code)]
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

    /// Spawn a headless one-shot task.
    /// Returns (command, args) to execute.
    fn spawn_headless(&self, session: &AgentSession, prompt: &str) -> Result<(String, Vec<String>), String>;

    /// Build args to resume an existing session
    fn resume_args(&self, session: &AgentSession) -> Vec<String>;

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
