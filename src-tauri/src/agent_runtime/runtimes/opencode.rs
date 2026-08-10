use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, ModelInfo, PermissionModeInfo, ThinkingOptionInfo,
};
use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Adapter for the OpenCode interactive TUI. The installed CLI is the source
/// of truth for models and resumable sessions.
pub struct OpenCodeAdapter;

impl OpenCodeAdapter {
    pub fn new() -> Self {
        Self
    }

    const CAPILOT_COMMAND_PALETTE_KEY: &'static str = "f12";

    fn tui_config_path(session: &AgentSession) -> Result<PathBuf, String> {
        let cache_root = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .ok_or_else(|| {
                "Cannot resolve a cache directory for OpenCode TUI config".to_string()
            })?;
        let safe_id: String = session
            .id
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
            .collect();
        if safe_id.is_empty() {
            return Err("Invalid OpenCode session id".to_string());
        }
        Ok(cache_root
            .join("capilot-ide/opencode-tui")
            .join(format!("{safe_id}.json")))
    }

    fn write_tui_config(session: &AgentSession) -> Result<PathBuf, String> {
        let path = Self::tui_config_path(session)?;
        let parent = path
            .parent()
            .ok_or_else(|| "Invalid OpenCode TUI config path".to_string())?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create OpenCode TUI config directory: {error}"))?;
        let config = serde_json::json!({
            "$schema": "https://opencode.ai/tui.json",
            "keybinds": {
                "command_list": Self::CAPILOT_COMMAND_PALETTE_KEY,
            }
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&config).unwrap())
            .map_err(|error| format!("Failed to write OpenCode TUI config: {error}"))?;
        Ok(path)
    }

    fn check_available() -> bool {
        Command::new("opencode")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn model_state_path() -> Option<std::path::PathBuf> {
        if let Some(state_home) = std::env::var_os("XDG_STATE_HOME") {
            return Some(std::path::PathBuf::from(state_home).join("opencode/model.json"));
        }
        std::env::var_os("HOME")
            .map(|home| std::path::PathBuf::from(home).join(".local/state/opencode/model.json"))
    }

    /// OpenCode records the model selected in its native picker. This makes
    /// CaPilot's initial label agree with a newly opened native TUI.
    fn native_default_model() -> Option<String> {
        let value: Value =
            serde_json::from_slice(&std::fs::read(Self::model_state_path()?).ok()?).ok()?;
        let recent = value.get("recent")?.as_array()?.first()?;
        Some(format!(
            "{}/{}",
            recent.get("providerID")?.as_str()?,
            recent.get("modelID")?.as_str()?
        ))
    }

    fn display_name(id: &str) -> String {
        id.split_once('/')
            .map(|(_, model)| model)
            .unwrap_or(id)
            .split('-')
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut chars = part.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn parse_models(output: &str, default_model: Option<&str>) -> Vec<ModelInfo> {
        output
            .lines()
            .map(str::trim)
            .filter(|line| {
                !line.is_empty() && line.contains('/') && !line.contains(char::is_whitespace)
            })
            .map(|id| {
                let provider = id
                    .split_once('/')
                    .map(|(provider, _)| provider)
                    .unwrap_or("opencode");
                ModelInfo {
                    id: id.to_string(),
                    name: Self::display_name(id),
                    provider: provider.to_string(),
                    is_default: default_model == Some(id),
                }
            })
            .collect()
    }

    fn discover_models() -> Vec<ModelInfo> {
        let default_model = Self::native_default_model();
        let Ok(output) = Command::new("opencode").arg("models").output() else {
            return vec![];
        };
        if !output.status.success() {
            return vec![];
        }
        Self::parse_models(
            &String::from_utf8_lossy(&output.stdout),
            default_model.as_deref(),
        )
    }

    fn check_authenticated() -> bool {
        // The catalog contains providers usable by this installation, including
        // OpenCode's credential-free provider, so it is a stronger readiness
        // test than merely checking that auth.json exists.
        !Self::discover_models().is_empty()
    }

    fn parse_session_list(output: &str, cwd: &Path) -> Option<String> {
        let sessions: Vec<Value> = serde_json::from_str(output).ok()?;
        let cwd = cwd.to_string_lossy();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis() as u64;
        sessions
            .into_iter()
            .filter_map(|session| {
                if session.get("directory")?.as_str()? != cwd {
                    return None;
                }
                let created = session.get("created")?.as_u64()?;
                if now.saturating_sub(created) > 10_000 {
                    return None;
                }
                Some((created, session.get("id")?.as_str()?.to_string()))
            })
            .max_by_key(|(created, _)| *created)
            .map(|(_, id)| id)
    }

    fn detect_recent_resume_key(cwd: &Path) -> Option<String> {
        let output = Command::new("opencode")
            .args(["session", "list", "--format", "json", "--max-count", "20"])
            .current_dir(cwd)
            .output()
            .ok()?;
        output.status.success().then_some(())?;
        Self::parse_session_list(&String::from_utf8_lossy(&output.stdout), cwd)
    }
}

impl AgentRuntimeAdapter for OpenCodeAdapter {
    fn id(&self) -> &str {
        "opencode"
    }
    fn name(&self) -> &str {
        "OpenCode"
    }
    fn is_available(&self) -> bool {
        Self::check_available()
    }
    fn is_authenticated(&self) -> bool {
        Self::check_authenticated()
    }
    fn list_models(&self) -> Vec<ModelInfo> {
        Self::discover_models()
    }

    fn list_permission_modes(&self) -> Vec<PermissionModeInfo> {
        vec![
            PermissionModeInfo {
                id: "ask".into(),
                label: "Normal".into(),
                description: "遵循 OpenCode 配置中的 allow、ask 和 deny 权限规则".into(),
                requires_confirmation: false,
            },
            PermissionModeInfo {
                id: "auto".into(),
                label: "Auto approve".into(),
                description: "OpenCode --auto：自动批准未被配置明确拒绝的权限请求".into(),
                requires_confirmation: true,
            },
        ]
    }

    fn list_thinking_options(&self) -> Vec<ThinkingOptionInfo> {
        // Variants belong to individual models and differ by model. OpenCode's
        // full TUI has no --variant launch flag, so a global list would be false.
        vec![]
    }

    fn spawn_interactive(&self, session: &AgentSession) -> Result<(String, Vec<String>), String> {
        let mut args = Vec::new();
        if let Some(model) = &session.model {
            args.extend(["--model".to_string(), model.clone()]);
        }
        args.extend(self.mode_args(&session.mode));
        Ok(("opencode".into(), args))
    }

    fn launch_env(&self, session: &AgentSession) -> Result<Vec<(String, String)>, String> {
        let config = Self::write_tui_config(session)?;
        Ok(vec![(
            "OPENCODE_TUI_CONFIG".into(),
            config.to_string_lossy().into_owned(),
        )])
    }

    fn resume_args(&self, session: &AgentSession) -> Vec<String> {
        session
            .resume_key
            .as_ref()
            .map(|key| vec!["--session".into(), key.clone()])
            .unwrap_or_default()
    }
    fn supports_resume(&self) -> bool {
        true
    }
    fn capture_resume_key(&self, cwd: &Path) -> Option<String> {
        Self::detect_recent_resume_key(cwd)
    }
    fn speed_args(&self, _speed: &str) -> Vec<String> {
        vec![]
    }
    fn mode_args(&self, mode: &str) -> Vec<String> {
        if mode == "auto" {
            vec!["--auto".into()]
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(mode: &str, resume_key: Option<&str>) -> AgentSession {
        AgentSession {
            id: "test".into(),
            runtime: "opencode".into(),
            mode: mode.into(),
            speed: "auto".into(),
            model: Some("openai/gpt-test".into()),
            cwd: "/tmp/project".into(),
            context_dir: "/tmp/project".into(),
            rows: 24,
            cols: 80,
            resume_key: resume_key.map(str::to_owned),
        }
    }

    #[test]
    fn parses_native_model_catalog_and_default() {
        let models = OpenCodeAdapter::parse_models(
            "opencode/big-pickle\nopenai/gpt-5.4\n",
            Some("openai/gpt-5.4"),
        );
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].provider, "opencode");
        assert_eq!(models[0].name, "Big Pickle");
        assert!(models[1].is_default);
    }

    #[test]
    fn builds_model_permission_and_resume_flags() {
        let adapter = OpenCodeAdapter::new();
        let (_, args) = adapter
            .spawn_interactive(&session("auto", Some("ses_123")))
            .unwrap();
        assert_eq!(args, ["--model", "openai/gpt-test", "--auto"]);
        assert_eq!(
            adapter.resume_args(&session("ask", Some("ses_123"))),
            ["--session", "ses_123"]
        );
    }

    #[test]
    fn parses_only_recent_session_for_matching_directory() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let json = format!(
            r#"[
            {{"id":"other","created":{now},"directory":"/tmp/other"}},
            {{"id":"mine","created":{now},"directory":"/tmp/project"}}
        ]"#
        );
        assert_eq!(
            OpenCodeAdapter::parse_session_list(&json, Path::new("/tmp/project")),
            Some("mine".into())
        );
    }
}
