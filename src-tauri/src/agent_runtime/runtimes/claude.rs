use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, ModelInfo, PermissionMode, Speed,
};
use std::process::Command;

pub struct ClaudeAdapter;

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Run `claude --version` and check if it succeeds
    fn check_available() -> bool {
        Command::new("claude")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Check if the user has a Claude session/credentials file
    fn check_authenticated() -> bool {
        // Check common credential locations
        let home = std::env::var("HOME").unwrap_or_default();
        let cred_paths = [
            format!("{}/.claude/credentials", home),
            format!("{}/.claude.json", home),
        ];
        cred_paths.iter().any(|p| std::path::Path::new(p).exists())
    }
}

impl AgentRuntimeAdapter for ClaudeAdapter {
    fn id(&self) -> &str {
        "claude"
    }

    fn name(&self) -> &str {
        "Claude Code"
    }

    fn is_available(&self) -> bool {
        Self::check_available()
    }

    fn is_authenticated(&self) -> bool {
        Self::check_authenticated()
    }

    fn list_models(&self) -> Vec<ModelInfo> {
        // Claude Code uses the API models; try to query from `claude --help` or hardcode
        vec![
            ModelInfo {
                id: "claude-sonnet-5".into(),
                name: "Claude Sonnet 5".into(),
                provider: "anthropic".into(),
            },
            ModelInfo {
                id: "claude-opus-5".into(),
                name: "Claude Opus 5".into(),
                provider: "anthropic".into(),
            },
            ModelInfo {
                id: "claude-haiku-4-5".into(),
                name: "Claude Haiku 4.5".into(),
                provider: "anthropic".into(),
            },
        ]
    }

    fn spawn_interactive(&self, session: &AgentSession) -> Result<(String, Vec<String>), String> {
        let mut args = vec![
            "--model".to_string(),
            // Default to sonnet for interactive; user can change via composer
            "claude-sonnet-5".to_string(),
        ];

        // Add permission mode args
        args.extend(self.mode_args(session.mode));

        // Add speed args
        args.extend(self.speed_args(session.speed));

        Ok(("claude".to_string(), args))
    }

    fn spawn_headless(&self, session: &AgentSession, prompt: &str) -> Result<(String, Vec<String>), String> {
        let mut args = vec![
            "-p".to_string(),
            prompt.to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--model".to_string(),
            "claude-sonnet-5".to_string(),
        ];

        args.extend(self.speed_args(session.speed));
        args.extend(self.mode_args(session.mode));

        Ok(("claude".to_string(), args))
    }

    fn resume_args(&self, _session: &AgentSession) -> Vec<String> {
        // Claude Code uses --resume to continue the last session
        // For now, always resume the last session in the CWD
        vec!["--resume".to_string()]
    }

    fn speed_args(&self, speed: Speed) -> Vec<String> {
        match speed {
            Speed::High => vec!["--thinking-effort".to_string(), "high".to_string()],
            Speed::Mid => vec!["--thinking-effort".to_string(), "medium".to_string()],
            Speed::Fast => vec!["--thinking-effort".to_string(), "low".to_string()],
            Speed::Auto => vec![], // Let Claude decide based on the task
        }
    }

    fn mode_args(&self, mode: PermissionMode) -> Vec<String> {
        match mode {
            PermissionMode::Ask => vec![
                "--permission-mode".to_string(),
                "accept-edits".to_string(),
            ],
            PermissionMode::Auto => vec![], // Claude's default behavior
            PermissionMode::Yolo => vec![
                "--permission-mode".to_string(),
                "bypassPermissions".to_string(),
            ],
        }
    }
}
