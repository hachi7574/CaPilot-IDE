use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, EffortInfo, ModelInfo, PermissionModeInfo,
    ThinkingOptionInfo,
};
use serde_json::Value;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

pub struct CodexAdapter;

impl CodexAdapter {
    pub fn new() -> Self {
        Self
    }

    fn check_available() -> bool {
        Command::new("codex")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn check_authenticated() -> bool {
        Command::new("codex")
            .args(["login", "status"])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    /// Query the installed Codex app-server catalog. This is the same catalog
    /// the native Codex model picker uses, so it reflects the installed CLI,
    /// current authentication, account availability, and hidden-model policy.
    fn discover_models() -> Vec<ModelInfo> {
        let Ok(mut child) = Command::new("codex")
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        else {
            return vec![];
        };

        let Some(mut stdin) = child.stdin.take() else {
            let _ = child.kill();
            return vec![];
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            return vec![];
        };
        let initialize = serde_json::json!({
            "id": 1,
            "method": "initialize",
            "params": { "clientInfo": { "name": "capilot-ide", "version": env!("CARGO_PKG_VERSION") } }
        });
        let list = serde_json::json!({
            "id": 2,
            "method": "model/list",
            "params": { "limit": 100, "includeHidden": false }
        });
        if writeln!(stdin, "{initialize}").is_err()
            || writeln!(stdin, "{list}").is_err()
            || stdin.flush().is_err()
        {
            let _ = child.kill();
            return vec![];
        }

        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut models = vec![];
        while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
            let Ok(line) = receiver.recv_timeout(remaining) else {
                break;
            };
            if let Some(discovered) = Self::parse_model_list_response(&line) {
                models = discovered;
                break;
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        models
    }

    fn parse_model_list_response(line: &str) -> Option<Vec<ModelInfo>> {
        let value: Value = serde_json::from_str(line).ok()?;
        if value.get("id").and_then(Value::as_i64) != Some(2) {
            return None;
        }
        let rows = value.get("result")?.get("data")?.as_array()?;
        Some(
            rows.iter()
                .filter_map(|row| {
                    let id = row
                        .get("model")
                        .or_else(|| row.get("id"))?
                        .as_str()?
                        .to_string();
                    let name = row
                        .get("displayName")
                        .and_then(Value::as_str)
                        .unwrap_or(&id)
                        .to_string();
                    let is_default = row
                        .get("isDefault")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    // The native reasoning popup lists these efforts in catalog
                    // order with the model's default highlighted. CaPilot drives
                    // that popup from the GUI, so the order + default must match.
                    let efforts = row
                        .get("supportedReasoningEfforts")
                        .and_then(Value::as_array)
                        .map(|options| {
                            let default = row
                                .get("defaultReasoningEffort")
                                .and_then(Value::as_str);
                            options
                                .iter()
                                .filter_map(|option| {
                                    let id = option
                                        .get("reasoningEffort")?
                                        .as_str()?
                                        .to_string();
                                    let description = option
                                        .get("description")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string();
                                    Some(EffortInfo {
                                        is_default: default == Some(id.as_str()),
                                        label: Self::effort_label(&id),
                                        description,
                                        id,
                                    })
                                })
                                .collect()
                        });
                    Some(ModelInfo {
                        id,
                        name,
                        provider: "openai".into(),
                        is_default,
                        efforts,
                    })
                })
                .collect(),
        )
    }

    fn effort_label(id: &str) -> String {
        match id {
            "low" => "Low",
            "medium" => "Medium",
            "high" => "High",
            "xhigh" => "Extra high",
            "max" => "Max",
            "ultra" => "Ultra",
            other => other,
        }
        .to_string()
    }

    fn sessions_dir() -> Option<PathBuf> {
        if let Some(home) = std::env::var_os("CODEX_HOME") {
            return Some(PathBuf::from(home).join("sessions"));
        }
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex/sessions"))
    }

    fn visit_jsonl(dir: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                Self::visit_jsonl(&path, files);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                files.push(path);
            }
        }
    }

    fn resume_key_from_file(path: &Path, cwd: &Path) -> Option<String> {
        let file = std::fs::File::open(path).ok()?;
        let mut first_line = String::new();
        std::io::BufReader::new(file)
            .read_line(&mut first_line)
            .ok()?;
        let value: Value = serde_json::from_str(&first_line).ok()?;
        let payload = value.get("payload")?;
        (payload.get("cwd")?.as_str()? == cwd.to_string_lossy())
            .then(|| payload.get("id").and_then(Value::as_str).map(str::to_owned))?
    }

    /// Fresh Codex TUIs write a session_meta record below $CODEX_HOME/sessions.
    /// Limit candidates to the spawn window so another old terminal sharing the
    /// cwd can never be captured as this terminal's resume key.
    fn detect_recent_resume_key(cwd: &Path) -> Option<String> {
        let mut files = Vec::new();
        Self::visit_jsonl(&Self::sessions_dir()?, &mut files);
        let now = SystemTime::now();
        files
            .into_iter()
            .filter_map(|path| {
                let modified = path.metadata().ok()?.modified().ok()?;
                let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
                if age > Duration::from_secs(10) {
                    return None;
                }
                let key = Self::resume_key_from_file(&path, cwd)?;
                Some((modified, key))
            })
            .max_by_key(|(modified, _)| *modified)
            .map(|(_, key)| key)
    }
}

impl AgentRuntimeAdapter for CodexAdapter {
    fn id(&self) -> &str {
        "codex"
    }
    fn name(&self) -> &str {
        "Codex"
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
                label: "read only".into(),
                description: "Codex untrusted 审批策略；工作区写入需要确认".into(),
                requires_confirmation: false,
            },
            PermissionModeInfo {
                id: "auto".into(),
                label: "workspace".into(),
                description: "允许工作区写入，禁止工作区外操作".into(),
                requires_confirmation: false,
            },
            PermissionModeInfo {
                id: "yolo".into(),
                label: "full access".into(),
                description: "Codex --yolo：关闭审批和沙箱".into(),
                requires_confirmation: true,
            },
        ]
    }

    fn list_thinking_options(&self) -> Vec<ThinkingOptionInfo> {
        vec![
            ThinkingOptionInfo {
                id: "auto".into(),
                label: "Auto".into(),
                description: "使用 Codex 模型默认推理强度".into(),
            },
            ThinkingOptionInfo {
                id: "fast".into(),
                label: "Low".into(),
                description: "低推理强度".into(),
            },
            ThinkingOptionInfo {
                id: "mid".into(),
                label: "Medium".into(),
                description: "中等推理强度".into(),
            },
            ThinkingOptionInfo {
                id: "high".into(),
                label: "High".into(),
                description: "高推理强度".into(),
            },
            ThinkingOptionInfo {
                id: "xhigh".into(),
                label: "Extra high".into(),
                description: "最高推理强度，耗时和 token 更多".into(),
            },
        ]
    }

    fn spawn_interactive(&self, session: &AgentSession) -> Result<(String, Vec<String>), String> {
        let mut args = Vec::new();
        if let Some(model) = &session.model {
            args.extend(["--model".to_string(), model.clone()]);
        }
        args.extend(self.mode_args(&session.mode));
        args.extend(self.speed_args(&session.speed));
        // Inline mode makes the PTY's scrollback behave like the other runtimes.
        args.push("--no-alt-screen".to_string());
        Ok(("codex".to_string(), args))
    }

    fn resume_args(&self, session: &AgentSession) -> Vec<String> {
        session
            .resume_key
            .as_ref()
            .map(|key| vec!["resume".to_string(), key.clone()])
            .unwrap_or_default()
    }

    fn supports_resume(&self) -> bool {
        true
    }
    fn capture_resume_key(&self, cwd: &Path) -> Option<String> {
        Self::detect_recent_resume_key(cwd)
    }

    fn speed_args(&self, speed: &str) -> Vec<String> {
        let effort = match speed {
            "high" => "high",
            "mid" => "medium",
            "xhigh" => "xhigh",
            "fast" => "low",
            _ => return vec![],
        };
        vec!["-c".into(), format!("model_reasoning_effort=\"{effort}\"")]
    }

    fn mode_args(&self, mode: &str) -> Vec<String> {
        match mode {
            "ask" => vec![
                "--ask-for-approval".into(),
                "untrusted".into(),
                "--sandbox".into(),
                "read-only".into(),
            ],
            "auto" => vec![
                "--ask-for-approval".into(),
                "never".into(),
                "--sandbox".into(),
                "workspace-write".into(),
            ],
            "yolo" => vec!["--yolo".into()],
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(resume_key: Option<&str>) -> AgentSession {
        AgentSession {
            id: "test".into(),
            runtime: "codex".into(),
            mode: "ask".into(),
            speed: "high".into(),
            model: Some("gpt-5.4".into()),
            cwd: "/tmp/project".into(),
            context_dir: "/tmp/project".into(),
            rows: 24,
            cols: 80,
            resume_key: resume_key.map(str::to_owned),
        }
    }

    #[test]
    fn builds_codex_flags_and_stable_resume_syntax() {
        let adapter = CodexAdapter::new();
        let (_, args) = adapter
            .spawn_interactive(&session(Some("session-id")))
            .unwrap();
        assert!(args.windows(2).any(|v| v == ["--model", "gpt-5.4"]));
        assert!(args
            .windows(2)
            .any(|v| v == ["--ask-for-approval", "untrusted"]));
        assert!(args.windows(2).any(|v| v == ["--sandbox", "read-only"]));
        assert_eq!(
            adapter.resume_args(&session(Some("session-id"))),
            ["resume", "session-id"]
        );
    }

    #[test]
    fn parses_only_matching_cwd_session_metadata() {
        let dir = std::env::temp_dir().join(format!("capilot-codex-test-{}", std::process::id()));
        let file = dir.join("session.jsonl");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &file,
            r#"{"type":"session_meta","payload":{"id":"abc","cwd":"/tmp/project"}}
"#,
        )
        .unwrap();
        assert_eq!(
            CodexAdapter::resume_key_from_file(&file, Path::new("/tmp/project")),
            Some("abc".into())
        );
        assert_eq!(
            CodexAdapter::resume_key_from_file(&file, Path::new("/tmp/other")),
            None
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parses_app_server_model_catalog() {
        let line = r#"{"id":2,"result":{"data":[{"id":"gpt-x","model":"gpt-x","displayName":"GPT X","hidden":false}]}}"#;
        let models = CodexAdapter::parse_model_list_response(line).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-x");
        assert_eq!(models[0].name, "GPT X");
    }

    #[test]
    fn parses_reasoning_efforts_in_catalog_order_with_default() {
        let line = concat!(
            r#"{"id":2,"result":{"data":[{"id":"gpt-5.6-sol","model":"gpt-5.6-sol","#,
            r#""displayName":"GPT-5.6-Sol","supportedReasoningEfforts":["#,
            r#"{"reasoningEffort":"low","description":"Fast responses"},"#,
            r#"{"reasoningEffort":"medium","description":"Balanced"},"#,
            r#"{"reasoningEffort":"high","description":"Deep"}],"#,
            r#""defaultReasoningEffort":"low","isDefault":true}]}}"#
        );
        let models = CodexAdapter::parse_model_list_response(line).unwrap();
        let efforts = models[0].efforts.as_ref().expect("efforts parsed");
        assert_eq!(efforts.len(), 3);
        assert_eq!(efforts[0].id, "low");
        assert!(efforts[0].is_default);
        assert_eq!(efforts[1].id, "medium");
        assert!(!efforts[1].is_default);
        assert_eq!(efforts[2].label, "High");
    }
}
