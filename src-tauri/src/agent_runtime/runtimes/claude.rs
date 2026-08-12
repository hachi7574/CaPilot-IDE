use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, AgentUsage, ModelInfo, PermissionModeInfo,
    ThinkingOptionInfo,
};
use crate::agent_runtime::status_hooks::{ensure_status_hooks, HOOK_ENV_AGENT, HOOK_ENV_DIR};
use crate::persistence::status_dir;
use serde_json::Value;
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

    /// Newest `*.jsonl` transcript under `~/.claude/projects/<project-key>/`, or
    /// `None` when the cwd has no session yet. Shares the project-key encoding
    /// and directory with `detect_resume_key`, but returns the full path (the
    /// context-usage read needs the file, not the resume stem).
    fn newest_transcript(cwd: &Path) -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        let dir = PathBuf::from(&home)
            .join(".claude")
            .join("projects")
            .join(Self::claude_project_key(cwd));
        let mut sessions: Vec<(SystemTime, PathBuf)> = Vec::new();
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                if let Ok(mtime) = meta.modified() {
                    sessions.push((mtime, path));
                }
            }
        }
        sessions.sort_by(|a, b| b.0.cmp(&a.0));
        sessions.first().map(|(_, p)| p.clone())
    }

    /// Current active-context estimate for a cwd: the summed `message.usage` of
    /// the LAST assistant record in the newest transcript. `None` when there is
    /// no transcript, no readable file, or no usable usage record. This is a
    /// single-snapshot read — it never accumulates usage across messages.
    fn latest_used_tokens(cwd: &Path) -> Option<u64> {
        let path = Self::newest_transcript(cwd)?;
        let content = std::fs::read_to_string(path).ok()?;
        Self::sum_usage_from_lines(&content)
    }

    /// Sum the LAST assistant record's `message.usage` across JSONL `lines`.
    ///
    /// Fields present are summed: `input_tokens` + `cache_creation_input_tokens`
    /// + `cache_read_input_tokens` + `output_tokens`. Records carrying
    /// `isSidechain: true` are skipped so main-thread turns win (a sidechain
    /// record that appears last must not shadow the main conversation). Lines
    /// that fail to parse or carry no `message.usage` are ignored; `None` when
    /// no usable record exists.
    fn sum_usage_from_lines(content: &str) -> Option<u64> {
        let mut last: Option<u64> = None;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            // Prefer main-thread records when the sidechain marker is present.
            if v.get("isSidechain").and_then(Value::as_bool).unwrap_or(false) {
                continue;
            }
            let Some(usage) = v.get("message").and_then(|m| m.get("usage")) else {
                continue;
            };
            if !usage.is_object() {
                continue;
            }
            let mut sum: u64 = 0;
            let mut any = false;
            for field in [
                "input_tokens",
                "cache_creation_input_tokens",
                "cache_read_input_tokens",
                "output_tokens",
            ] {
                if let Some(n) = usage.get(field).and_then(Value::as_u64) {
                    sum += n;
                    any = true;
                }
            }
            if any {
                last = Some(sum);
            }
        }
        last
    }

    /// Confirmed model context capacities (Anthropic model catalog). Unknown or
    /// gateway models (e.g. a proxied `deepseek-v4-flash`) → `None`; the max is
    /// never guessed from visible text.
    fn context_window_max(model: Option<&str>) -> Option<u64> {
        match model {
            Some("claude-sonnet-5") | Some("claude-opus-5") => Some(1_000_000),
            Some("claude-haiku-4-5") => Some(200_000),
            _ => None,
        }
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
        // Intentionally false: Claude Code login state is not probed. The
        // credentials-file check reported "logged in" even for expired
        // sessions, and availability is already gated by check_available().
        // Only installation is surfaced to the settings/onboarding UI.
        false
    }

    fn list_models(&self) -> Vec<ModelInfo> {
        // Claude Code uses the API models; try to query from `claude --help` or hardcode
        vec![
            ModelInfo {
                id: "claude-sonnet-5".into(),
                name: "Claude Sonnet 5".into(),
                provider: "anthropic".into(),
                is_default: true,
                efforts: None,
            },
            ModelInfo {
                id: "claude-opus-5".into(),
                name: "Claude Opus 5".into(),
                provider: "anthropic".into(),
                is_default: false,
                efforts: None,
            },
            ModelInfo {
                id: "claude-haiku-4-5".into(),
                name: "Claude Haiku 4.5".into(),
                provider: "anthropic".into(),
                is_default: false,
                efforts: None,
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

        // Status-reporting hooks. The hook files are written idempotently before
        // the arg list is built so the FIRST spawn already carries `--settings`;
        // claude loads it as an ADDITIONAL settings source, keeping the user's
        // global config untouched. A failed sidecar write degrades to no hooks —
        // it must never abort a spawn.
        args.extend(self.status_hook_args(session));

        Ok(("claude".to_string(), args))
    }

    fn status_hook_args(&self, _session: &AgentSession) -> Vec<String> {
        let _ = ensure_status_hooks();
        let hooks_settings = status_dir().join("hooks.json");
        if hooks_settings.exists() {
            vec![
                "--settings".to_string(),
                hooks_settings.to_string_lossy().into_owned(),
            ]
        } else {
            vec![]
        }
    }

    fn launch_env(&self, session: &AgentSession) -> Result<Vec<(String, String)>, String> {
        // Session-scoped env for the status hook script: it must know which
        // agent this claude process belongs to and where to write the sidecar.
        // Injected into THIS PTY only — the user's own claude runs stay clean.
        Ok(vec![
            (HOOK_ENV_AGENT.to_string(), session.id.clone()),
            (
                HOOK_ENV_DIR.to_string(),
                status_dir().to_string_lossy().into_owned(),
            ),
        ])
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

    fn context_usage(&self, cwd: &Path, model: Option<&str>) -> Option<AgentUsage> {
        // Single-snapshot read: the LAST assistant record's summed usage is the
        // provider's current active-context estimate (compaction can lower it).
        let used = Self::latest_used_tokens(cwd)?;
        Some(AgentUsage {
            context_window_used_tokens: Some(used),
            context_window_max_tokens: Self::context_window_max(model),
        })
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

    #[test]
    fn sums_last_assistant_usage_across_jsonl_lines() {
        // Simple shape on the first assistant record, rich shape (with cache
        // fields) on the last — the last record wins and sums only fields that
        // are present.
        let content = "\
{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n\
{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":100,\"output_tokens\":50}}}\n\
{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":200,\"cache_creation_input_tokens\":30,\"cache_read_input_tokens\":40,\"output_tokens\":60}}}\n\
";
        assert_eq!(
            ClaudeAdapter::sum_usage_from_lines(content),
            Some(200 + 30 + 40 + 60),
            "last assistant record's present fields are summed"
        );
    }

    #[test]
    fn skips_sidechain_records_for_main_thread_usage() {
        // Sidechain records that appear after the main-thread turn must not
        // shadow it — the LAST main-thread record wins.
        let content = "\
{\"type\":\"assistant\",\"isSidechain\":true,\"message\":{\"usage\":{\"input_tokens\":999,\"output_tokens\":999}}}\n\
{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":3}}}\n\
{\"type\":\"assistant\",\"isSidechain\":true,\"message\":{\"usage\":{\"input_tokens\":888,\"output_tokens\":888}}}\n\
";
        assert_eq!(ClaudeAdapter::sum_usage_from_lines(content), Some(10));
    }

    #[test]
    fn usage_none_when_no_usable_record() {
        // Unparseable lines, records without `message.usage`, and empty content
        // all degrade to None instead of failing.
        assert_eq!(ClaudeAdapter::sum_usage_from_lines("not json\n"), None);
        assert_eq!(ClaudeAdapter::sum_usage_from_lines("{\"type\":\"user\"}\n"), None);
        assert_eq!(
            ClaudeAdapter::sum_usage_from_lines(
                "{\"type\":\"assistant\",\"message\":{\"content\":\"no usage\"}}\n"
            ),
            None
        );
        assert_eq!(ClaudeAdapter::sum_usage_from_lines(""), None);
    }

    #[test]
    fn model_manifest_maps_known_models_only() {
        assert_eq!(
            ClaudeAdapter::context_window_max(Some("claude-sonnet-5")),
            Some(1_000_000)
        );
        assert_eq!(
            ClaudeAdapter::context_window_max(Some("claude-opus-5")),
            Some(1_000_000)
        );
        assert_eq!(
            ClaudeAdapter::context_window_max(Some("claude-haiku-4-5")),
            Some(200_000)
        );
        // Unknown / gateway models never get a guessed max.
        assert_eq!(ClaudeAdapter::context_window_max(Some("deepseek-v4-flash")), None);
        assert_eq!(ClaudeAdapter::context_window_max(None), None);
    }

    #[test]
    fn agent_usage_serializes_to_camel_case_wire_shape() {
        // Frontend contract: `{ contextWindowUsedTokens, contextWindowMaxTokens }`.
        let usage = AgentUsage {
            context_window_used_tokens: Some(123_456),
            context_window_max_tokens: Some(1_000_000),
        };
        assert_eq!(
            serde_json::to_string(&usage).unwrap(),
            r#"{"contextWindowUsedTokens":123456,"contextWindowMaxTokens":1000000}"#
        );
        // Optional fields serialize as null (still present on the wire).
        let empty = AgentUsage {
            context_window_used_tokens: None,
            context_window_max_tokens: None,
        };
        assert_eq!(
            serde_json::to_string(&empty).unwrap(),
            r#"{"contextWindowUsedTokens":null,"contextWindowMaxTokens":null}"#
        );
    }

    // Serializes tests that repoint `HOME` so parallel runs don't observe each
    // other's env (mirrors the codex/lib.rs ENV_LOCK pattern).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn session() -> AgentSession {
        AgentSession {
            id: "test".into(),
            runtime: "claude".into(),
            mode: "ask".into(),
            speed: "mid".into(),
            model: Some("claude-sonnet-5".into()),
            cwd: "/tmp/project".into(),
            context_dir: "/tmp/project".into(),
            rows: 24,
            cols: 80,
            resume_key: None,
        }
    }

    /// `status_hook_args` is the hook-injection set that survives a user launch
    /// override (Settings → 已安装 → ⚙), which replaces the adapter's arg list
    /// wholesale. Guarding the extraction: it must return the `--settings` path
    /// under the (isolated) status dir, exactly as `spawn_interactive` relied on.
    #[test]
    fn status_hook_args_injects_settings_flag() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev_home = std::env::var_os("HOME");
        let base = std::env::temp_dir().join(format!("capilot-claude-test-{}", std::process::id()));
        std::env::set_var("HOME", &base);
        // Re-run ensure_status_hooks so hooks.json exists under the temp HOME.
        let _ = ensure_status_hooks();
        let args = ClaudeAdapter::new().status_hook_args(&session());
        let expected = status_dir().join("hooks.json");
        assert_eq!(
            args,
            vec!["--settings".to_string(), expected.to_string_lossy().into_owned()]
        );
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(base);
    }
}
