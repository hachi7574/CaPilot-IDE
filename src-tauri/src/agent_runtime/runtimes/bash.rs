use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, ModelInfo, PermissionMode, Speed,
};
use std::process::Command;

/// Minimal shell runtime — useful for testing the PTY loop and for plain
/// terminal tabs. Also acts as a fallback when no agent CLI is installed.
pub struct BashAdapter;

impl BashAdapter {
    pub fn new() -> Self {
        Self
    }

    fn check_available() -> bool {
        Command::new("bash")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

impl AgentRuntimeAdapter for BashAdapter {
    fn id(&self) -> &str {
        "bash"
    }

    fn name(&self) -> &str {
        "Bash"
    }

    fn is_available(&self) -> bool {
        Self::check_available()
    }

    fn is_authenticated(&self) -> bool {
        true
    }

    fn list_models(&self) -> Vec<ModelInfo> {
        vec![]
    }

    fn spawn_interactive(&self, _session: &AgentSession) -> Result<(String, Vec<String>), String> {
        Ok(("bash".to_string(), vec!["--norc".to_string()]))
    }

    fn spawn_headless(&self, _session: &AgentSession, prompt: &str) -> Result<(String, Vec<String>), String> {
        Ok((
            "bash".to_string(),
            vec!["-lc".to_string(), prompt.to_string()],
        ))
    }

    fn resume_args(&self, _session: &AgentSession) -> Vec<String> {
        vec![]
    }

    fn speed_args(&self, _speed: Speed) -> Vec<String> {
        vec![]
    }

    fn mode_args(&self, _mode: PermissionMode) -> Vec<String> {
        vec![]
    }
}
