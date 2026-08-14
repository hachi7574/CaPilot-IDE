use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Agent runtime identifier
pub type RuntimeId = String;

/// Agent session identifier
pub type AgentId = String;

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

/// One provider-native reasoning effort a model supports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffortInfo {
    /// Provider-native effort id (e.g. `low`, `xhigh`).
    pub id: String,
    pub label: String,
    pub description: String,
    /// True when this is the model's native default reasoning effort.
    pub is_default: bool,
}

/// Metadata about a runtime model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub is_default: bool,
    /// Per-model reasoning efforts (currently only codex). `None` = the runtime
    /// exposes a single global list (`thinking_options`) or none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub efforts: Option<Vec<EffortInfo>>,
}

/// One permission preset exposed by a runtime. `id` is CaPilot's persisted
/// policy key; label/description describe the provider-native behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionModeInfo {
    pub id: String,
    pub label: String,
    pub description: String,
    pub requires_confirmation: bool,
}

/// One provider-native thinking/effort choice exposed by a runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingOptionInfo {
    pub id: String,
    pub label: String,
    pub description: String,
}

/// Session configuration for spawning an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: AgentId,
    pub runtime: RuntimeId,
    pub mode: String,
    pub speed: String,
    /// Selected model id (composer `[模型↑]`). `None` = runtime default.
    pub model: Option<String>,
    pub cwd: PathBuf,
    pub context_dir: PathBuf,
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
    pub status: AgentStatus,
    pub title: String,
    pub cwd: PathBuf,
    pub pid: Option<u32>,
    /// Provider-specific permission mode id.
    pub mode: String,
    /// Provider-specific thinking/effort option id.
    pub speed: String,
    /// Selected model id, or None for the runtime default.
    pub model: Option<String>,
    /// Provider's estimate of the CURRENT active-context occupancy, when the
    /// runtime can supply a trustworthy value. Daemon-memory only (not
    /// persisted); a restart clears it, a model switch / reconnect preserves it.
    pub last_usage: Option<AgentUsage>,
}

/// Provider's estimate of the current active-context occupancy for an agent.
///
/// One normalized context-usage sample for an agent.
///
/// The two `context_window_*` fields are the CURRENT active-context occupancy
/// and capacity — they are NOT cumulative token-spend counters: compaction can
/// reduce `context_window_used_tokens`, and `context_window_max_tokens` is the
/// selected model's capacity (never guessed from visible text). Both stay
/// optional — a provider with no trustworthy value omits the field instead of
/// estimating.
///
/// The two `cache_*` fields are SESSION-CUMULATIVE prompt-token counts feeding
/// the cache hit rate (`cache_hit_tokens / cache_total_input_tokens`). Each
/// adapter normalizes its runtime's accounting into this pair — the definition
/// of "hit" and "total prompt" differs per provider (Claude's `input_tokens`
/// excludes cache reads; codex's `input_tokens` already includes them;
/// opencode's `tokens.input` excludes cache reads and reports
/// `cache.read/write` separately), so the ratio is only computed at the display
/// layer.
///
/// A measured zero hit MUST be `Some(0)` when the denominator is present;
/// `None` means unavailable, not zero. Adapters must scope cumulative values to
/// the exact provider session identified by `resume_key`.
///
/// `actual_model` is provider-observed telemetry (for example Claude's
/// `message.model`). It is display-only: the persisted Agent model remains the
/// configured model used for spawning, switching, and catalog matching.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUsage {
    pub context_window_used_tokens: Option<u64>,
    pub context_window_max_tokens: Option<u64>,
    pub cache_hit_tokens: Option<u64>,
    pub cache_total_input_tokens: Option<u64>,
    pub actual_model: Option<String>,
}

/// Summary of an available runtime detected on the system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeInfo {
    pub id: RuntimeId,
    pub name: String,
    pub available: bool,
    pub authenticated: bool,
    pub models: Vec<ModelInfo>,
    pub permission_modes: Vec<PermissionModeInfo>,
    pub thinking_options: Vec<ThinkingOptionInfo>,
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

    /// Permission presets supported by this runtime. An empty list means the
    /// runtime has no agent permission control (for example a plain shell).
    fn list_permission_modes(&self) -> Vec<PermissionModeInfo>;

    /// Thinking/effort choices supported by this runtime.
    fn list_thinking_options(&self) -> Vec<ThinkingOptionInfo>;

    /// Spawn an interactive TUI session (PTY).
    /// Returns (command, args) to execute.
    fn spawn_interactive(&self, session: &AgentSession) -> Result<(String, Vec<String>), String>;

    /// Args that are CaPilot infrastructure and must survive a user launch
    /// override (Settings → 已安装 → ⚙), which replaces the adapter's arg list
    /// wholesale. The status-hook injection (claude `--settings`, codex `-p`
    /// profile) is what makes lifecycle status reporting work; without it the
    /// tab strip silently falls back to PTY-activity heuristics. `lib.rs`
    /// re-appends this set after applying an override. Default: no args.
    fn status_hook_args(&self, _session: &AgentSession) -> Vec<String> {
        vec![]
    }

    /// Provider-specific environment injected into this one PTY process.
    /// Keeping this session-scoped avoids modifying the user's global CLI
    /// configuration just to make an IDE integration reliable.
    fn launch_env(&self, _session: &AgentSession) -> Result<Vec<(String, String)>, String> {
        Ok(vec![])
    }

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

    /// Recover a missing provider session id for an already-persisted agent.
    /// Runtimes with a reliable per-agent signal can override this; the default
    /// leaves legacy capture behavior unchanged.
    fn recover_resume_key(
        &self,
        _agent_id: &str,
        _cwd: &Path,
        _created_at_ms: i64,
    ) -> Option<String> {
        None
    }

    /// Provider's estimate of the current active-context occupancy for the agent
    /// running in `cwd` with `model`. `resume_key` identifies the exact provider
    /// conversation when the runtime exposes one; adapters must not substitute
    /// another conversation merely because it shares the same cwd. Default: the
    /// runtime reports no context usage (so the UI renders no meter).
    /// Implementations should return `None` when no trustworthy value exists
    /// rather than estimating.
    fn context_usage(
        &self,
        _cwd: &Path,
        _model: Option<&str>,
        _resume_key: Option<&str>,
    ) -> Option<AgentUsage> {
        None
    }

    /// Map speed tier to CLI arguments
    fn speed_args(&self, speed: &str) -> Vec<String>;

    /// Map permission mode to CLI arguments
    fn mode_args(&self, mode: &str) -> Vec<String>;
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
    #[error("session limit reached ({limit})")]
    CapacityReached { limit: usize },
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
