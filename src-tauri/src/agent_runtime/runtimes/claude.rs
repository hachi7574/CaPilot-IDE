use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, ModelInfo, PermissionMode, Speed,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

pub struct ClaudeAdapter;

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Claude Code's per-cwd project dir name: every non-`[a-zA-Z0-9]` character
    /// becomes `-` (the leading `/` included). Mirrored exactly so the scan finds
    /// the same dir Claude writes to.
    fn claude_project_key(cwd: &Path) -> String {
        cwd.to_string_lossy()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect()
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

    /// Detect the most recent Claude Code session id for a cwd.
    ///
    /// Claude Code stores sessions under `~/.claude/projects/<project-key>/`
    /// where `<project-key>` is the cwd with **every** non-`[a-zA-Z0-9]`
    /// character replaced by `-` (including the leading `/` and any dots/spaces,
    /// e.g. `/home/x/my.proj` → `-home-x-my-proj`). Return the newest `*.jsonl`
    /// stem, or None if the cwd has no session yet (fresh agent).
    fn detect_resume_key(cwd: &Path) -> Option<String> {
        let home = std::env::var("HOME").ok()?;
        let dir = PathBuf::from(&home).join(".claude").join("projects").join(Self::claude_project_key(cwd));
        let mut sessions: Vec<(SystemTime, String)> = Vec::new();
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let mtime = path.metadata().ok()?.modified().ok()?;
            let stem = path.file_stem()?.to_string_lossy().to_string();
            sessions.push((mtime, stem));
        }
        sessions.sort_by(|a, b| b.0.cmp(&a.0));
        sessions.first().map(|(_, s)| s.clone())
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
        // Composer `[模型↑]` selection wins; fall back to sonnet for interactive.
        let model = session
            .model
            .clone()
            .unwrap_or_else(|| "claude-sonnet-5".to_string());
        let mut args = vec![
            "--model".to_string(),
            model,
        ];

        // Add permission mode args
        args.extend(self.mode_args(session.mode));

        // Add speed args
        args.extend(self.speed_args(session.speed));

        Ok(("claude".to_string(), args))
    }

    fn resume_args(&self, session: &AgentSession) -> Vec<String> {
        // An explicit stored key wins; otherwise fall back to detecting the most
        // recent session in this context dir.
        if let Some(key) = &session.resume_key {
            return vec!["--resume".to_string(), key.clone()];
        }
        match Self::detect_resume_key(&session.cwd) {
            Some(key) => vec!["--resume".to_string(), key],
            None => vec![], // no previous session — start fresh
        }
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn capture_resume_key(&self, cwd: &Path) -> Option<String> {
        Self::detect_resume_key(cwd)
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
                "acceptEdits".to_string(),
            ],
            PermissionMode::Auto => vec![], // Claude's default behavior
            PermissionMode::Yolo => vec![
                "--permission-mode".to_string(),
                "bypassPermissions".to_string(),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_project_key_matches_real_dir_encoding() {
        // Claude Code encodes the cwd by replacing every non-[a-zA-Z0-9]
        // character with '-', including the leading slash. These exact strings
        // were verified against real ~/.claude/projects/ entries.
        let cases = [
            ("/home/hachi/CaPilot/workspaces/master", "-home-hachi-CaPilot-workspaces-master"),
            ("/home/hachi/Project/CaPilot-Ide", "-home-hachi-Project-CaPilot-Ide"),
            // Dots and spaces also collapse to '-'.
            ("/home/x/my.proj", "-home-x-my-proj"),
            ("/home/x/my dir", "-home-x-my-dir"),
        ];
        for (cwd, expected) in cases {
            assert_eq!(
                ClaudeAdapter::claude_project_key(Path::new(cwd)),
                expected,
                "cwd {cwd}"
            );
        }
    }
}
