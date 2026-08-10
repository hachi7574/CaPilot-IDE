use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, ModelInfo, PermissionModeInfo, ThinkingOptionInfo,
};
use serde_json::Value;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// Oh My Pi (`omp`) runtime.
///
/// OMP is a multi-provider harness. Its model catalog is therefore queried
/// from the installed CLI instead of being duplicated in CaPilot.
pub struct OmpAdapter;

static MODEL_CACHE: OnceLock<Mutex<Option<(Instant, Vec<ModelInfo>)>>> = OnceLock::new();

impl OmpAdapter {
    pub fn new() -> Self {
        Self
    }

    fn check_available() -> bool {
        Command::new("omp")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    /// `omp models --json` reports the effective catalog for the current OMP
    /// profile, including all configured providers. Keep the query bounded so
    /// opening the terminal picker cannot hang on a provider/network problem.
    fn query_models() -> Vec<ModelInfo> {
        let Ok(mut child) = Command::new("omp")
            .args(["models", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return vec![];
        };
        let Some(mut stdout) = child.stdout.take() else {
            let _ = child.kill();
            return vec![];
        };
        // Drain concurrently: a multi-provider catalog can exceed the OS pipe
        // buffer, in which case waiting for process exit before reading would
        // deadlock until our timeout.
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut output = String::new();
            let result = stdout.read_to_string(&mut output).map(|_| output);
            let _ = sender.send(result);
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        return vec![];
                    }
                    let remaining = deadline
                        .checked_duration_since(Instant::now())
                        .unwrap_or(Duration::ZERO);
                    return receiver
                        .recv_timeout(remaining)
                        .ok()
                        .and_then(Result::ok)
                        .map(|output| Self::parse_models(&output))
                        .unwrap_or_default();
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return vec![];
                }
            }
        }
    }

    fn discover_models() -> Vec<ModelInfo> {
        let cache = MODEL_CACHE.get_or_init(|| Mutex::new(None));
        if let Ok(guard) = cache.lock() {
            if let Some((created, models)) = guard.as_ref() {
                if created.elapsed() < Duration::from_secs(10) {
                    return models.clone();
                }
            }
        }
        let models = Self::query_models();
        if let Ok(mut guard) = cache.lock() {
            *guard = Some((Instant::now(), models.clone()));
        }
        models
    }

    fn parse_models(raw: &str) -> Vec<ModelInfo> {
        let Ok(value) = serde_json::from_str::<Value>(raw) else {
            return vec![];
        };
        let Some(rows) = value
            .get("models")
            .and_then(Value::as_array)
            .or_else(|| value.as_array())
        else {
            return vec![];
        };

        let mut models: Vec<ModelInfo> = rows
            .iter()
            .filter_map(|row| {
                let id = row
                    .get("id")
                    .or_else(|| row.get("model"))
                    .and_then(Value::as_str)?
                    .to_owned();
                let provider = row
                    .get("provider")
                    .or_else(|| row.get("providerId"))
                    .and_then(Value::as_str)
                    .unwrap_or("omp")
                    .to_owned();
                // OMP accepts the unambiguous provider/model form. Some
                // versions already return it as `id`; avoid double-prefixing.
                let qualified_id = if id.contains('/') || provider == "omp" {
                    id.clone()
                } else {
                    format!("{provider}/{id}")
                };
                let name = row
                    .get("name")
                    .or_else(|| row.get("displayName"))
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_owned();
                let is_default = row
                    .get("isDefault")
                    .or_else(|| row.get("is_default"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Some(ModelInfo {
                    id: qualified_id,
                    name,
                    provider,
                    is_default,
                })
            })
            .collect();
        models.sort_by(|a, b| a.provider.cmp(&b.provider).then(a.name.cmp(&b.name)));
        models.dedup_by(|a, b| a.id == b.id);
        models
    }

    fn sessions_dir() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("OMP_SESSION_DIR") {
            return Some(PathBuf::from(dir));
        }
        if let Some(dir) = std::env::var_os("PI_CODING_AGENT_DIR") {
            return Some(PathBuf::from(dir).join("sessions"));
        }
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        let config = std::env::var_os("PI_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".omp"));
        Some(config.join("agent/sessions"))
    }

    fn visit_sessions(dir: &Path, sessions: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::visit_sessions(&path, sessions);
            } else if path.file_name().and_then(|name| name.to_str()) == Some("session.jsonl") {
                sessions.push(path);
            }
        }
    }

    fn json_contains_cwd(value: &Value, cwd: &str) -> bool {
        match value {
            Value::Object(map) => {
                map.get("cwd").and_then(Value::as_str) == Some(cwd)
                    || map
                        .values()
                        .any(|child| Self::json_contains_cwd(child, cwd))
            }
            Value::Array(values) => values
                .iter()
                .any(|child| Self::json_contains_cwd(child, cwd)),
            _ => false,
        }
    }

    fn session_matches_cwd(path: &Path, cwd: &Path) -> bool {
        let Ok(file) = std::fs::File::open(path) else {
            return false;
        };
        let cwd = cwd.to_string_lossy();
        std::io::BufReader::new(file)
            .lines()
            .take(32)
            .filter_map(Result::ok)
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
            .any(|value| Self::json_contains_cwd(&value, &cwd))
    }

    fn detect_recent_session(cwd: &Path) -> Option<String> {
        let mut sessions = vec![];
        Self::visit_sessions(&Self::sessions_dir()?, &mut sessions);
        let now = SystemTime::now();
        sessions
            .into_iter()
            .filter_map(|path| {
                let modified = path.metadata().ok()?.modified().ok()?;
                let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
                (age <= Duration::from_secs(10) && Self::session_matches_cwd(&path, cwd))
                    .then_some((modified, path))
            })
            .max_by_key(|(modified, _)| *modified)
            .map(|(_, path)| path.to_string_lossy().into_owned())
    }
}

impl AgentRuntimeAdapter for OmpAdapter {
    fn id(&self) -> &str {
        "omp"
    }

    fn name(&self) -> &str {
        "Oh My Pi"
    }

    fn is_available(&self) -> bool {
        Self::check_available()
    }

    fn is_authenticated(&self) -> bool {
        // OMP supports many independent providers. A non-empty effective model
        // catalog is the only provider-neutral authentication signal.
        !Self::discover_models().is_empty()
    }

    fn list_models(&self) -> Vec<ModelInfo> {
        Self::discover_models()
    }

    fn list_permission_modes(&self) -> Vec<PermissionModeInfo> {
        vec![
            PermissionModeInfo {
                id: "ask".into(),
                label: "Always ask".into(),
                description: "OMP always-ask：写入和执行工具前都请求授权".into(),
                requires_confirmation: false,
            },
            PermissionModeInfo {
                id: "write".into(),
                label: "Write approval".into(),
                description: "OMP write：读取自动允许，写入或执行前请求授权".into(),
                requires_confirmation: false,
            },
            PermissionModeInfo {
                id: "full".into(),
                label: "Full access".into(),
                description: "OMP yolo：自动批准所有工具调用".into(),
                requires_confirmation: true,
            },
        ]
    }

    fn list_thinking_options(&self) -> Vec<ThinkingOptionInfo> {
        [
            ("auto", "Auto", "由 OMP 和当前模型选择思考强度"),
            ("off", "Off", "关闭显式思考"),
            ("minimal", "Minimal", "最少思考"),
            ("low", "Low", "低思考强度"),
            ("medium", "Medium", "中等思考强度"),
            ("high", "High", "高思考强度"),
            ("xhigh", "Extra high", "超高思考强度"),
            ("max", "Max", "最大思考强度"),
        ]
        .into_iter()
        .map(|(id, label, description)| ThinkingOptionInfo {
            id: id.into(),
            label: label.into(),
            description: description.into(),
        })
        .collect()
    }

    fn spawn_interactive(&self, session: &AgentSession) -> Result<(String, Vec<String>), String> {
        let mut args = vec![];
        if let Some(model) = &session.model {
            args.extend(["--model".into(), model.clone()]);
        }
        args.extend(self.mode_args(&session.mode));
        args.extend(self.speed_args(&session.speed));
        Ok(("omp".into(), args))
    }

    fn resume_args(&self, session: &AgentSession) -> Vec<String> {
        session
            .resume_key
            .as_ref()
            .map(|key| vec!["--resume".into(), key.clone()])
            .unwrap_or_default()
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn capture_resume_key(&self, cwd: &Path) -> Option<String> {
        Self::detect_recent_session(cwd)
    }

    fn speed_args(&self, speed: &str) -> Vec<String> {
        let thinking = match speed {
            "off" => "off",
            "minimal" => "minimal",
            "low" => "low",
            "medium" => "medium",
            "high" => "high",
            "xhigh" => "xhigh",
            "max" => "max",
            _ => return vec![],
        };
        vec!["--thinking".into(), thinking.into()]
    }

    fn mode_args(&self, mode: &str) -> Vec<String> {
        let approval = match mode {
            "ask" => "always-ask",
            "write" => "write",
            "full" => "yolo",
            _ => return vec![],
        };
        vec!["--approval-mode".into(), approval.into()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::adapter::AgentRole;

    fn session() -> AgentSession {
        AgentSession {
            id: "test".into(),
            runtime: "omp".into(),
            mode: "write".into(),
            speed: "xhigh".into(),
            model: Some("openai/gpt-test".into()),
            cwd: "/tmp/project".into(),
            context_dir: "/tmp/project".into(),
            role: AgentRole::Standalone,
            rows: 24,
            cols: 80,
            resume_key: Some("/tmp/session.jsonl".into()),
        }
    }

    #[test]
    fn parses_qualified_multi_provider_models() {
        let models = OmpAdapter::parse_models(
            r#"{"models":[{"provider":"openai","id":"gpt-x","name":"GPT X"},{"providerId":"anthropic","model":"anthropic/claude-x","displayName":"Claude X","isDefault":true}]}"#,
        );
        assert_eq!(models.len(), 2);
        assert!(models.iter().any(|model| model.id == "openai/gpt-x"));
        assert!(models
            .iter()
            .any(|model| model.id == "anthropic/claude-x" && model.is_default));
    }

    #[test]
    fn maps_omp_native_launch_flags() {
        let adapter = OmpAdapter::new();
        let (command, args) = adapter.spawn_interactive(&session()).unwrap();
        assert_eq!(command, "omp");
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--model", "openai/gpt-test"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--approval-mode", "write"]));
        assert!(args.windows(2).any(|pair| pair == ["--thinking", "xhigh"]));
        assert_eq!(
            adapter.resume_args(&session()),
            ["--resume", "/tmp/session.jsonl"]
        );
    }

    #[test]
    fn exposes_all_native_thinking_levels() {
        let options = OmpAdapter::new().list_thinking_options();
        assert_eq!(options.len(), 8);
        assert!(options.iter().any(|option| option.id == "max"));
        assert!(options.iter().any(|option| option.id == "off"));
    }
}
