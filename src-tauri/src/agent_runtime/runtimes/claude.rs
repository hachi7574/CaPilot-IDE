use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, ModelInfo, PermissionModeInfo, ThinkingOptionInfo,
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
        let dir = PathBuf::from(&home)
            .join(".claude")
            .join("projects")
            .join(Self::claude_project_key(cwd));
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
                is_default: true,
            },
            ModelInfo {
                id: "claude-opus-5".into(),
                name: "Claude Opus 5".into(),
                provider: "anthropic".into(),
                is_default: false,
            },
            ModelInfo {
                id: "claude-haiku-4-5".into(),
                name: "Claude Haiku 4.5".into(),
                provider: "anthropic".into(),
                is_default: false,
            },
        ]
    }

    fn list_permission_modes(&self) -> Vec<PermissionModeInfo> {
        vec![
            PermissionModeInfo {
                id: "ask".into(),
                label: "manual".into(),
                description: "Claude 在执行需要授权的工具前询问".into(),
                requires_confirmation: false,
            },
            PermissionModeInfo {
                id: "accept_edits".into(),
                label: "accept edits".into(),
                description: "自动接受文件编辑，其他敏感工具仍询问".into(),
                requires_confirmation: false,
            },
            PermissionModeInfo {
                id: "plan".into(),
                label: "plan".into(),
                description: "只进行分析和规划，不执行修改".into(),
                requires_confirmation: false,
            },
            PermissionModeInfo {
                id: "auto".into(),
                label: "auto".into(),
                description: "使用 Claude Code 原生自动权限模式".into(),
                requires_confirmation: false,
            },
            PermissionModeInfo {
                id: "yolo".into(),
                label: "bypass".into(),
                description: "绕过 Claude Code 权限检查".into(),
                requires_confirmation: true,
            },
        ]
    }

    fn list_thinking_options(&self) -> Vec<ThinkingOptionInfo> {
        vec![
            ThinkingOptionInfo {
                id: "auto".into(),
                label: "Auto".into(),
                description: "由 Claude 自动选择思考强度".into(),
            },
            ThinkingOptionInfo {
                id: "fast".into(),
                label: "Low".into(),
                description: "低思考强度，响应更快".into(),
            },
            ThinkingOptionInfo {
                id: "mid".into(),
                label: "Medium".into(),
                description: "中等思考强度".into(),
            },
            ThinkingOptionInfo {
                id: "high".into(),
                label: "High".into(),
                description: "高思考强度".into(),
            },
        ]
    }

    fn spawn_interactive(&self, session: &AgentSession) -> Result<(String, Vec<String>), String> {
        // Composer `[模型↑]` selection wins; fall back to sonnet for interactive.
        let model = session
            .model
            .clone()
            .unwrap_or_else(|| "claude-sonnet-5".to_string());
        let mut args = vec!["--model".to_string(), model];

        // Add permission mode args
        args.extend(self.mode_args(&session.mode));

        // Add speed args
        args.extend(self.speed_args(&session.speed));

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

    fn speed_args(&self, speed: &str) -> Vec<String> {
        match speed {
            "high" => vec!["--thinking-effort".to_string(), "high".to_string()],
            "mid" => vec!["--thinking-effort".to_string(), "medium".to_string()],
            "fast" => vec!["--thinking-effort".to_string(), "low".to_string()],
            _ => vec![],
        }
    }

    fn mode_args(&self, mode: &str) -> Vec<String> {
        let native_mode = match mode {
            "accept_edits" => "acceptEdits",
            "plan" => "plan",
            "auto" => "auto",
            "yolo" => "bypassPermissions",
            _ => "manual",
        };
        vec![
            "--permission-mode".to_string(),
            native_mode.to_string(),
            // Makes bypassPermissions part of Claude Code's live Shift+Tab
            // cycle. It does not enable bypass by itself; Ask and Auto still
            // start in their explicitly selected modes above.
            "--allow-dangerously-skip-permissions".to_string(),
        ]
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
            (
                "/home/hachi/CaPilot/workspaces/demo",
                "-home-hachi-CaPilot-workspaces-demo",
            ),
            (
                "/home/hachi/Project/CaPilot-Ide",
                "-home-hachi-Project-CaPilot-Ide",
            ),
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

    #[test]
    fn composer_permission_modes_map_to_claude_native_modes() {
        let adapter = ClaudeAdapter::new();
        for (mode, expected) in [
            ("ask", "manual"),
            ("accept_edits", "acceptEdits"),
            ("plan", "plan"),
            ("auto", "auto"),
            ("yolo", "bypassPermissions"),
        ] {
            assert_eq!(
                adapter.mode_args(mode),
                vec![
                    "--permission-mode".to_string(),
                    expected.to_string(),
                    "--allow-dangerously-skip-permissions".to_string(),
                ]
            );
        }
    }
}
