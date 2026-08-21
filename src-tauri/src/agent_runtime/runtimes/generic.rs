//! v1 catch-all adapter: spawn `<id>` (or an explicit binary) in a PTY.
//!
//! No resume, hooks, models, or permission flags. Used for new CLIs and for
//! unknown runtime ids so they never silently become Claude.

use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, ModelInfo, PermissionModeInfo, ThinkingOptionInfo,
};

/// One v1 CLI. First-class adapters (claude / codex / dsh / pi / opencode / shells)
/// are **not** in this table.
pub struct V1Runtime {
    pub id: &'static str,
    pub name: &'static str,
    pub binary: &'static str,
    pub extra_args: &'static [&'static str],
}

/// Detectable v1 agent CLIs. Binary names follow Orca `TUI_AGENT_CONFIG` plus
/// CodeBuddy / Qoder. Extra argv is only the flags required to open an
/// interactive TUI (never permission/model/hook injection).
pub const V1_RUNTIMES: &[V1Runtime] = &[
    V1Runtime { id: "codebuddy", name: "CodeBuddy", binary: "codebuddy", extra_args: &[] },
    V1Runtime { id: "gemini", name: "Gemini", binary: "gemini", extra_args: &[] },
    V1Runtime { id: "grok", name: "Grok", binary: "grok", extra_args: &[] },
    V1Runtime { id: "kimi", name: "Kimi", binary: "kimi", extra_args: &[] },
    V1Runtime { id: "hermes", name: "Hermes", binary: "hermes", extra_args: &["--tui"] },
    V1Runtime { id: "trae", name: "Trae", binary: "traecli", extra_args: &[] },
    V1Runtime { id: "qoder", name: "Qoder", binary: "qoderclicn", extra_args: &[] },
    V1Runtime { id: "cursor", name: "Cursor", binary: "cursor-agent", extra_args: &[] },
    V1Runtime { id: "copilot", name: "Copilot", binary: "copilot", extra_args: &[] },
    V1Runtime { id: "cline", name: "Cline", binary: "cline", extra_args: &[] },
    V1Runtime { id: "openclaude", name: "OpenClaude", binary: "openclaude", extra_args: &[] },
    V1Runtime { id: "autohand", name: "Autohand", binary: "autohand", extra_args: &[] },
    V1Runtime { id: "mimo-code", name: "MiMo Code", binary: "mimo", extra_args: &[] },
    V1Runtime { id: "aider", name: "Aider", binary: "aider", extra_args: &[] },
    V1Runtime { id: "goose", name: "Goose", binary: "goose", extra_args: &[] },
    V1Runtime { id: "amp", name: "Amp", binary: "amp", extra_args: &[] },
    V1Runtime { id: "kilo", name: "Kilo", binary: "kilo", extra_args: &[] },
    V1Runtime {
        id: "kiro",
        name: "Kiro",
        binary: "kiro-cli",
        extra_args: &["chat", "--tui"],
    },
    V1Runtime { id: "crush", name: "Crush", binary: "crush", extra_args: &[] },
    V1Runtime { id: "aug", name: "Auggie", binary: "auggie", extra_args: &[] },
    V1Runtime { id: "codebuff", name: "Codebuff", binary: "codebuff", extra_args: &[] },
    V1Runtime {
        id: "command-code",
        name: "Command Code",
        binary: "command-code",
        extra_args: &["--trust"],
    },
    V1Runtime { id: "continue", name: "Continue", binary: "cn", extra_args: &[] },
    V1Runtime { id: "droid", name: "Droid", binary: "droid", extra_args: &[] },
    V1Runtime { id: "qwen-code", name: "Qwen Code", binary: "qwen", extra_args: &[] },
    V1Runtime { id: "rovo", name: "Rovo", binary: "rovo", extra_args: &[] },
    V1Runtime { id: "openclaw", name: "OpenClaw", binary: "openclaw", extra_args: &[] },
    V1Runtime { id: "devin", name: "Devin", binary: "devin", extra_args: &[] },
    V1Runtime { id: "ante", name: "Ante", binary: "ante", extra_args: &[] },
    V1Runtime { id: "prime-agent", name: "Prime Agent", binary: "prime-agent", extra_args: &[] },
    V1Runtime { id: "omp", name: "OMP", binary: "omp", extra_args: &[] },
    V1Runtime { id: "antigravity", name: "Antigravity", binary: "agy", extra_args: &[] },
];

pub fn spec_for(id: &str) -> Option<&'static V1Runtime> {
    V1_RUNTIMES.iter().find(|s| s.id == id)
}

pub struct GenericCliAdapter {
    id: String,
    name: String,
    binary: String,
    extra_args: Vec<String>,
}

impl GenericCliAdapter {
    pub fn from_id(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            binary: id.clone(),
            extra_args: Vec::new(),
            id,
        }
    }

    pub fn with_binary(id: impl Into<String>, binary: impl Into<String>, extra_args: Vec<String>) -> Self {
        let id = id.into();
        Self {
            name: id.clone(),
            binary: binary.into(),
            extra_args,
            id,
        }
    }

    pub fn from_spec(spec: &V1Runtime) -> Self {
        Self {
            id: spec.id.to_string(),
            name: spec.name.to_string(),
            binary: spec.binary.to_string(),
            extra_args: spec.extra_args.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    /// Known v1 id → table row; anything else → binary = id (never Claude).
    pub fn for_id(id: &str) -> Self {
        match spec_for(id) {
            Some(spec) => Self::from_spec(spec),
            None => Self::from_id(id),
        }
    }
}

impl AgentRuntimeAdapter for GenericCliAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn is_available(&self) -> bool {
        crate::agent_runtime::adapter::cli_available(&self.binary)
    }

    fn is_authenticated(&self) -> bool {
        self.is_available()
    }

    fn version(&self) -> Option<String> {
        crate::agent_runtime::adapter::cli_version(&self.binary)
    }

    fn list_models(&self) -> Vec<ModelInfo> {
        vec![]
    }

    fn list_permission_modes(&self) -> Vec<PermissionModeInfo> {
        vec![]
    }

    fn list_thinking_options(&self) -> Vec<ThinkingOptionInfo> {
        vec![]
    }

    fn spawn_interactive(&self, _session: &AgentSession) -> Result<(String, Vec<String>), String> {
        if !self.is_available() {
            return Err(format!(
                "运行时「{}」不可用：未在 PATH 中找到命令 `{}`。",
                self.id, self.binary
            ));
        }
        Ok((self.binary.clone(), self.extra_args.clone()))
    }

    fn resume_args(&self, _session: &AgentSession) -> Vec<String> {
        vec![]
    }

    fn speed_args(&self, _speed: &str) -> Vec<String> {
        vec![]
    }

    fn mode_args(&self, _mode: &str) -> Vec<String> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_id_is_not_claude() {
        let adapter = GenericCliAdapter::for_id("not-a-real-cli");
        assert_eq!(adapter.id(), "not-a-real-cli");
        assert_eq!(adapter.name(), "not-a-real-cli");
        assert_eq!(adapter.binary, "not-a-real-cli");
        assert!(adapter.list_models().is_empty());
        assert!(!adapter.supports_resume());
    }

    #[test]
    fn codebuddy_is_bare_binary() {
        let adapter = GenericCliAdapter::for_id("codebuddy");
        assert_eq!(adapter.id(), "codebuddy");
        assert_eq!(adapter.name(), "CodeBuddy");
        assert_eq!(adapter.binary, "codebuddy");
        assert!(adapter.extra_args.is_empty());
        assert!(!adapter.supports_resume());
    }

    #[test]
    fn kiro_uses_chat_tui_argv() {
        let adapter = GenericCliAdapter::for_id("kiro");
        assert_eq!(adapter.binary, "kiro-cli");
        assert_eq!(adapter.extra_args, ["chat", "--tui"]);
    }

    #[test]
    fn hermes_uses_tui_flag() {
        let adapter = GenericCliAdapter::for_id("hermes");
        assert_eq!(adapter.binary, "hermes");
        assert_eq!(adapter.extra_args, ["--tui"]);
    }

    #[test]
    fn continue_binary_is_cn() {
        let adapter = GenericCliAdapter::for_id("continue");
        assert_eq!(adapter.binary, "cn");
        assert_eq!(adapter.name(), "Continue");
    }

    #[test]
    fn trae_binary_is_traecli() {
        let adapter = GenericCliAdapter::for_id("trae");
        assert_eq!(adapter.binary, "traecli");
    }
}
