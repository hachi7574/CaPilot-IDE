use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, AgentUsage, ModelInfo, PermissionModeInfo,
    ThinkingOptionInfo,
};
use crate::agent_runtime::status_hooks::{ensure_status_hooks, HOOK_ENV_AGENT, HOOK_ENV_DIR};
use crate::persistence::status_dir;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct ClaudeAdapter;

/// Result of one pass over a Claude transcript: the LAST usable assistant
/// record's summed usage (the live active-context estimate) plus the
/// session-cumulative cache-read and total-prompt token counts (the cache hit
/// rate numerator and denominator).
struct TranscriptUsage {
    last_used: Option<u64>,
    cache_hit: u64,
    cache_total: u64,
    actual_model: Option<String>,
}

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

    fn project_dir(cwd: &Path) -> Option<PathBuf> {
        let home = std::env::var("HOME").ok()?;
        Some(
            PathBuf::from(&home)
                .join(".claude")
                .join("projects")
                .join(Self::claude_project_key(cwd)),
        )
    }

    /// Resolve exactly one provider session. Never substitute the newest file:
    /// IDE and standalone Claude processes commonly share a working directory.
    fn transcript_for_resume_key(cwd: &Path, resume_key: &str) -> Option<PathBuf> {
        uuid::Uuid::parse_str(resume_key).ok()?;
        let path = Self::project_dir(cwd)?.join(format!("{resume_key}.jsonl"));
        path.is_file().then_some(path)
    }

    fn sidecar_resume_key(agent_id: &str) -> Option<String> {
        let raw = std::fs::read_to_string(crate::persistence::status_file(agent_id)).ok()?;
        serde_json::from_str::<Value>(&raw)
            .ok()?
            .get("session_id")?
            .as_str()
            .map(str::to_owned)
    }

    fn recover_session_key(agent_id: &str, cwd: &Path) -> Option<String> {
        let key = Self::sidecar_resume_key(agent_id)?;
        Self::transcript_for_resume_key(cwd, &key).map(|_| key)
    }

    /// Missing binding means missing usage, not permission to borrow another
    /// Claude process's transcript from the same cwd.
    fn read_transcript(cwd: &Path, resume_key: &str) -> Option<TranscriptUsage> {
        let path = Self::transcript_for_resume_key(cwd, resume_key)?;
        let content = std::fs::read_to_string(path).ok()?;
        Some(Self::parse_transcript_usage(&content))
    }

    /// One pass over JSONL `lines` computing:
    ///  - `last_used`: the summed `message.usage` of the LAST assistant record
    ///    (`input_tokens` + `cache_creation_input_tokens` +
    ///    `cache_read_input_tokens` + `output_tokens`);
    ///  - session-cumulative cache stats across unique assistant messages;
    ///  - the last provider-observed assistant model.
    ///
    /// Records carrying `isSidechain: true` are skipped so main-thread turns
    /// win (a sidechain record that appears last must not shadow the main
    /// conversation). Lines that fail to parse or carry no `message.usage` are
    /// ignored.
    ///
    /// Anthropic accounting: `input_tokens` EXCLUDES cache reads, so each
    /// turn's total prompt is `input + cache_creation + cache_read` and the
    /// cached portion is `cache_read`.
    fn parse_transcript_usage(content: &str) -> TranscriptUsage {
        let mut last_used = None;
        let mut actual_model = None;
        let mut by_message: HashMap<String, (u64, u64, u64)> = HashMap::new();
        let mut anonymous_hit = 0u64;
        let mut anonymous_total = 0u64;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            // Prefer main-thread records when the sidechain marker is present.
            if v.get("isSidechain")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                continue;
            }
            let Some(message) = v.get("message") else {
                continue;
            };
            let Some(usage) = message.get("usage") else {
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
            if any && sum > 0 {
                last_used = Some(sum);
                if let Some(observed) = message
                    .get("model")
                    .and_then(Value::as_str)
                    .filter(|model| !model.is_empty())
                {
                    actual_model = Some(observed.to_owned());
                }
            }
            let input = usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let created = usage
                .get("cache_creation_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let read = usage
                .get("cache_read_input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            if input + created + read > 0 {
                if let Some(message_id) = message
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                {
                    by_message.insert(message_id.to_owned(), (input, created, read));
                } else {
                    anonymous_hit += read;
                    anonymous_total += input + created + read;
                }
            }
        }
        let (cache_hit, cache_total) = by_message.values().fold(
            (anonymous_hit, anonymous_total),
            |(hit, total), (input, created, read)| (hit + read, total + input + created + read),
        );
        TranscriptUsage {
            last_used,
            cache_hit,
            cache_total,
            actual_model,
        }
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

    fn version(&self) -> Option<String> {
        // `claude --version` prints "2.1.233 (Claude Code)" — keep the bare
        // semver for the settings chip.
        crate::agent_runtime::adapter::cli_version("claude")
            .map(|v| v.split('(').next().unwrap_or(&v).trim().to_string())
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
        session
            .resume_key
            .as_ref()
            .map(|key| vec!["--resume".to_string(), key.clone()])
            .unwrap_or_default()
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn capture_resume_key(&self, _cwd: &Path) -> Option<String> {
        // A cwd alone cannot identify one Claude process when sessions overlap.
        // The per-agent SessionStart hook is the authoritative capture source.
        None
    }

    fn recover_resume_key(
        &self,
        agent_id: &str,
        cwd: &Path,
        _created_at_ms: i64,
    ) -> Option<String> {
        Self::recover_session_key(agent_id, cwd)
    }

    fn context_usage(
        &self,
        cwd: &Path,
        model: Option<&str>,
        resume_key: Option<&str>,
    ) -> Option<AgentUsage> {
        // Single-snapshot read: the LAST assistant record's summed usage is the
        // provider's current active-context estimate (compaction can lower it).
        let parsed = Self::read_transcript(cwd, resume_key?)?;
        let used = parsed.last_used?;
        Some(AgentUsage {
            context_window_used_tokens: Some(used),
            context_window_max_tokens: Self::context_window_max(model),
            // Zero is a valid measured hit count. Null is reserved for no
            // accounting data, otherwise the UI hides a real 0%.
            cache_hit_tokens: (parsed.cache_total > 0).then_some(parsed.cache_hit),
            cache_total_input_tokens: (parsed.cache_total > 0).then_some(parsed.cache_total),
            actual_model: parsed.actual_model,
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
        // fields) on the last — the last record wins for the live estimate and
        // the session-cumulative cache stats sum across BOTH records.
        let content = "\
{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n\
{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":100,\"output_tokens\":50}}}\n\
{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":200,\"cache_creation_input_tokens\":30,\"cache_read_input_tokens\":40,\"output_tokens\":60}}}\n\
";
        let parsed = ClaudeAdapter::parse_transcript_usage(content);
        assert_eq!(
            parsed.last_used,
            Some(200 + 30 + 40 + 60),
            "last assistant record's present fields are summed"
        );
        // Anthropic accounting: prompt = input + cache_creation + cache_read,
        // hit = cache_read. Record 1: 100 prompt / 0 hit; record 2: 270 / 40.
        assert_eq!(parsed.cache_hit, 40);
        assert_eq!(parsed.cache_total, 100 + 270);
    }

    #[test]
    fn deduplicates_streamed_message_ids_and_reports_observed_model() {
        let content = "\
{\"type\":\"assistant\",\"message\":{\"id\":\"msg-1\",\"model\":\"deepseek-v4-flash\",\"usage\":{\"input_tokens\":100,\"cache_read_input_tokens\":900,\"output_tokens\":10}}}\n\
{\"type\":\"assistant\",\"message\":{\"id\":\"msg-1\",\"model\":\"deepseek-v4-flash\",\"usage\":{\"input_tokens\":100,\"cache_read_input_tokens\":900,\"output_tokens\":10}}}\n\
{\"type\":\"assistant\",\"message\":{\"id\":\"partial\",\"model\":\"must-not-win\",\"usage\":{\"input_tokens\":0,\"cache_read_input_tokens\":0,\"output_tokens\":0}}}\n\
";
        let parsed = ClaudeAdapter::parse_transcript_usage(content);
        assert_eq!(parsed.last_used, Some(1_010));
        assert_eq!(parsed.cache_hit, 900);
        assert_eq!(parsed.cache_total, 1_000);
        assert_eq!(parsed.actual_model.as_deref(), Some("deepseek-v4-flash"));
    }

    #[test]
    fn skips_sidechain_records_for_main_thread_usage() {
        // Sidechain records that appear after the main-thread turn must not
        // shadow it — the LAST main-thread record wins, and sidechain usage is
        // excluded from the session-cumulative cache stats too.
        let content = "\
{\"type\":\"assistant\",\"isSidechain\":true,\"message\":{\"usage\":{\"input_tokens\":999,\"output_tokens\":999}}}\n\
{\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":3}}}\n\
{\"type\":\"assistant\",\"isSidechain\":true,\"message\":{\"usage\":{\"input_tokens\":888,\"output_tokens\":888}}}\n\
";
        let parsed = ClaudeAdapter::parse_transcript_usage(content);
        assert_eq!(parsed.last_used, Some(10));
        assert_eq!(parsed.cache_hit, 0);
        assert_eq!(parsed.cache_total, 7);
    }

    #[test]
    fn usage_none_when_no_usable_record() {
        // Unparseable lines, records without `message.usage`, and empty content
        // all degrade to a no-usage result instead of failing.
        for content in [
            "not json\n",
            "{\"type\":\"user\"}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":\"no usage\"}}\n",
            "",
        ] {
            let parsed = ClaudeAdapter::parse_transcript_usage(content);
            assert_eq!(parsed.last_used, None);
            assert_eq!(parsed.cache_total, 0);
        }
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
        assert_eq!(
            ClaudeAdapter::context_window_max(Some("deepseek-v4-flash")),
            None
        );
        assert_eq!(ClaudeAdapter::context_window_max(None), None);
    }

    #[test]
    fn agent_usage_serializes_to_camel_case_wire_shape() {
        // Frontend contract: `{ contextWindowUsedTokens, contextWindowMaxTokens }`.
        let usage = AgentUsage {
            context_window_used_tokens: Some(123_456),
            context_window_max_tokens: Some(1_000_000),
            cache_hit_tokens: Some(88_000),
            cache_total_input_tokens: Some(110_000),
            actual_model: Some("deepseek-v4-flash".into()),
        };
        assert_eq!(
            serde_json::to_string(&usage).unwrap(),
            r#"{"contextWindowUsedTokens":123456,"contextWindowMaxTokens":1000000,"cacheHitTokens":88000,"cacheTotalInputTokens":110000,"actualModel":"deepseek-v4-flash"}"#
        );
        // Optional fields serialize as null (still present on the wire).
        let empty = AgentUsage {
            context_window_used_tokens: None,
            context_window_max_tokens: None,
            cache_hit_tokens: None,
            cache_total_input_tokens: None,
            actual_model: None,
        };
        assert_eq!(
            serde_json::to_string(&empty).unwrap(),
            r#"{"contextWindowUsedTokens":null,"contextWindowMaxTokens":null,"cacheHitTokens":null,"cacheTotalInputTokens":null,"actualModel":null}"#
        );
    }

    #[test]
    fn context_usage_reads_only_the_requested_claude_session() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev_home = std::env::var_os("HOME");
        let base = std::env::temp_dir().join(format!(
            "capilot-claude-usage-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::env::set_var("HOME", &base);
        let cwd = Path::new("/tmp/shared-project");
        let project_dir = ClaudeAdapter::project_dir(cwd).unwrap();
        std::fs::create_dir_all(&project_dir).unwrap();
        let wanted = uuid::Uuid::new_v4().to_string();
        let other = uuid::Uuid::new_v4().to_string();
        std::fs::write(
            project_dir.join(format!("{wanted}.jsonl")),
            "{\"type\":\"assistant\",\"message\":{\"id\":\"wanted\",\"model\":\"actual-model\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":90,\"output_tokens\":5}}}\n",
        )
        .unwrap();
        std::fs::write(
            project_dir.join(format!("{other}.jsonl")),
            "{\"type\":\"assistant\",\"message\":{\"id\":\"other\",\"model\":\"wrong-model\",\"usage\":{\"input_tokens\":999,\"output_tokens\":1}}}\n",
        )
        .unwrap();

        let usage = ClaudeAdapter::new()
            .context_usage(cwd, Some("claude-sonnet-5"), Some(&wanted))
            .unwrap();
        assert_eq!(usage.context_window_used_tokens, Some(105));
        assert_eq!(usage.cache_hit_tokens, Some(90));
        assert_eq!(usage.cache_total_input_tokens, Some(100));
        assert_eq!(usage.actual_model.as_deref(), Some("actual-model"));
        let sidecar = crate::persistence::status_file("claude-agent");
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        std::fs::write(
            sidecar,
            format!(r#"{{"status":"idle","ts":1,"session_id":"{wanted}"}}"#),
        )
        .unwrap();
        assert_eq!(
            ClaudeAdapter::recover_session_key("claude-agent", cwd).as_deref(),
            Some(wanted.as_str())
        );
        assert!(ClaudeAdapter::new()
            .context_usage(cwd, None, Some("00000000-0000-4000-8000-000000000000"))
            .is_none());

        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(base);
    }

    // Serializes tests that repoint `HOME` so parallel runs don't observe each
    // other's env (shared with lib.rs / codex / opencode via agent_runtime).
    use crate::agent_runtime::ENV_LOCK;

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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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
