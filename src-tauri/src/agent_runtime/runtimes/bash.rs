use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, ModelInfo, PermissionMode, Speed,
};
use std::process::Command;

/// Shell runtime in two flavours:
/// - `"bash"` (norc: true) — minimal shell, skips `~/.bashrc` (clean, fast).
/// - `"bash-rc"` (norc: false) — full interactive bash that sources the user's
///   `~/.bashrc`, so the prompt / aliases / PATH match the system terminal.
pub struct BashAdapter {
    id: &'static str,
    norc: bool,
}

impl BashAdapter {
    pub fn new(id: &'static str, norc: bool) -> Self {
        Self { id, norc }
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
        self.id
    }

    fn name(&self) -> &str {
        if self.norc {
            "Bash"
        } else {
            "Bash (rc)"
        }
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
        // `--norc` for the minimal shell; the full variant just runs `bash`
        // (interactive), which sources /etc/bash.bashrc + ~/.bashrc.
        let args = if self.norc {
            vec!["--norc".to_string()]
        } else {
            vec![]
        };
        Ok(("bash".to_string(), args))
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
