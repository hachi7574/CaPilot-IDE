use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, AgentUsage, EffortInfo, ModelInfo, PermissionModeInfo,
    ThinkingOptionInfo,
};
use crate::agent_runtime::status_hooks::{self, ensure_status_hooks, HOOK_ENV_AGENT, HOOK_ENV_DIR};
use crate::persistence::status_dir;
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

    /// Codex config root: `$CODEX_HOME` when set, else `~/.codex`.
    fn codex_home() -> Option<PathBuf> {
        if let Some(home) = std::env::var_os("CODEX_HOME") {
            return Some(PathBuf::from(home));
        }
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex"))
    }

    /// Per-session codex config profile (`$CODEX_HOME/capilot-<agent_id>.config.toml`).
    /// A `-p <name>` launch layers this file ON TOP of the user's real
    /// `config.toml`, so CaPilot's status hooks are injected per-invocation
    /// without touching the user's global config or `hooks.json`.
    fn status_profile(agent_id: &str) -> Option<PathBuf> {
        Self::codex_home().map(|home| home.join(format!("capilot-{agent_id}.config.toml")))
    }

    fn profile_name(agent_id: &str) -> String {
        format!("capilot-{agent_id}")
    }

    /// Write the status-hook profile for one agent. The profile defines inline
    /// TOML hooks (codex's `HookEventsToml` in a `[[hooks.<Event>]]` layer) that
    /// call the shared `~/CaPilot/status/hook.sh` for every lifecycle event
    /// codex supports. The hook script itself is env-gated (no-op when
    /// `CAPILOT_AGENT_ID` is absent), so the same command is safe to run under
    /// any codex invocation. Best-effort: a failed write degrades to no hooks.
    fn write_status_profile(agent_id: &str) -> std::io::Result<()> {
        let Some(profile) = Self::status_profile(agent_id) else {
            return Ok(());
        };
        let hook_sh = status_dir().join("hook.sh");
        let hook_sh = hook_sh.to_string_lossy();
        // TOML basic-string escape for the script path (home dirs are plain, but
        // escape backslash and quote so an odd HOME can never break the file).
        let escaped = hook_sh.replace('\\', "\\\\").replace('"', "\\\"");
        let mut toml = String::new();
        for event in status_hooks::CODEX_HOOK_EVENTS {
            // Codex clamps SessionEnd hook timeouts to 3s and warns on startup
            // when a larger timeout is declared. Declare 3s for SessionEnd
            // (still generous — hook.sh is a sub-millisecond sh script) so the
            // session opens without a clamp warning.
            let timeout = if event == "SessionEnd" { 3 } else { 5 };
            toml.push_str(&format!(
                "[[hooks.{event}]]\n\
                 [[hooks.{event}.hooks]]\n\
                 type = \"command\"\n\
                 command = \"/bin/sh {escaped}\"\n\
                 timeout = {timeout}\n\n"
            ));
        }
        std::fs::write(profile, toml)
    }

    /// Remove a session's codex config profile (session delete / close). No-op
    /// when the file is already gone or `CODEX_HOME` is unresolvable.
    pub fn remove_status_profile(agent_id: &str) {
        if let Some(profile) = Self::status_profile(agent_id) {
            let _ = std::fs::remove_file(profile);
        }
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

    /// Exact session JSONL under `$CODEX_HOME/sessions` whose `session_meta`
    /// id matches the persisted provider `resume_key` and whose cwd still
    /// matches the agent record. The id is the authority: choosing the newest
    /// file by cwd makes two agents in one project silently share usage data.
    fn session_for_resume_key(cwd: &Path, resume_key: &str) -> Option<PathBuf> {
        let mut files = Vec::new();
        Self::visit_jsonl(&Self::sessions_dir()?, &mut files);
        files
            .into_iter()
            .find(|path| Self::resume_key_from_file(path, cwd).as_deref() == Some(resume_key))
    }

    /// Codex session ids are UUIDv7; their first 48 bits are the Unix epoch in
    /// milliseconds. This gives a stable spawn-time signal even when the JSONL
    /// file itself is created later or its mtime keeps changing during a turn.
    fn session_started_at_ms(resume_key: &str) -> Option<u64> {
        let id = uuid::Uuid::parse_str(resume_key).ok()?;
        (id.get_version_num() == 7).then_some((id.as_u128() >> 80) as u64)
    }

    fn sidecar_resume_key(agent_id: &str) -> Option<String> {
        let raw = std::fs::read_to_string(crate::persistence::status_file(agent_id)).ok()?;
        serde_json::from_str::<Value>(&raw)
            .ok()?
            .get("session_id")?
            .as_str()
            .map(str::to_owned)
    }

    /// Recover the exact Codex session for agents whose older metadata has no
    /// resume_key. Prefer the per-agent hook sidecar. As a legacy fallback,
    /// match the UUIDv7 session timestamp to the persisted Agent creation time
    /// within a narrow window, while still requiring the cwd to match.
    fn recover_session_key(agent_id: &str, cwd: &Path, created_at_ms: i64) -> Option<String> {
        if let Some(key) = Self::sidecar_resume_key(agent_id) {
            if Self::session_for_resume_key(cwd, &key).is_some() {
                return Some(key);
            }
        }

        let created_at_ms = u64::try_from(created_at_ms).ok()?;
        let mut files = Vec::new();
        Self::visit_jsonl(&Self::sessions_dir()?, &mut files);
        files
            .into_iter()
            .filter_map(|path| {
                let key = Self::resume_key_from_file(&path, cwd)?;
                let delta = Self::session_started_at_ms(&key)?.abs_diff(created_at_ms);
                (delta <= 5_000).then_some((delta, key))
            })
            .min_by_key(|(delta, _)| *delta)
            .map(|(_, key)| key)
    }

    /// Context-window reading from one session transcript: the LAST
    /// `token_count` event's `info.last_token_usage.total_tokens` as used
    /// (docs/context-window-usage.md — the `last` object, not the session
    /// total) and the last seen `model_context_window` as max (emitted on both
    /// `token_count` and `task_started`). Both fields stay optional: a session
    /// still on its first turn may have no `token_count` yet.
    ///
    /// Also accumulates session-cumulative cache stats across ALL `token_count`
    /// events. Codex accounting (OpenAI style): `input_tokens` ALREADY includes
    /// the cached portion (verified: `total_tokens == input_tokens +
    /// output_tokens`), so the total prompt is `input_tokens` and the hit
    /// portion is `cached_input_tokens`. Older transcripts may name them
    /// `cache_read_input_tokens` / `cache_creation_input_tokens`; both are
    /// accepted.
    fn latest_usage_from_content(content: &str) -> AgentUsage {
        let mut used = None;
        let mut max = None;
        let mut cache_hit = 0u64;
        let mut cache_total = 0u64;
        let mut cache_seen = false;
        let mut actual_model = None;
        for line in content.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(model) = v
                .pointer("/payload/model")
                .or_else(|| v.pointer("/payload/thread_settings/model"))
                .and_then(Value::as_str)
                .filter(|model| !model.is_empty())
            {
                actual_model = Some(model.to_owned());
            }
            if let Some(n) = v
                .pointer("/payload/info/model_context_window")
                .or_else(|| v.pointer("/payload/model_context_window"))
                .and_then(Value::as_u64)
            {
                max = Some(n);
            }
            if v.pointer("/payload/type").and_then(Value::as_str) != Some("token_count") {
                continue;
            }
            if let Some(n) = v
                .pointer("/payload/info/last_token_usage/total_tokens")
                .and_then(Value::as_u64)
            {
                used = Some(n);
            }
            let lu = v
                .pointer("/payload/info/last_token_usage")
                .and_then(Value::as_object);
            let input = lu
                .and_then(|o| o.get("input_tokens"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let hit = lu
                .and_then(|o| o.get("cached_input_tokens"))
                .and_then(Value::as_u64)
                .or_else(|| {
                    lu.and_then(|o| o.get("cache_read_input_tokens"))
                        .and_then(Value::as_u64)
                })
                .unwrap_or(0);
            if input > 0 {
                cache_hit += hit;
                cache_total += input;
                cache_seen = true;
            }
        }
        AgentUsage {
            context_window_used_tokens: used,
            context_window_max_tokens: max,
            cache_hit_tokens: cache_seen.then_some(cache_hit),
            cache_total_input_tokens: cache_seen.then_some(cache_total),
            actual_model,
        }
    }

    fn latest_usage(cwd: &Path, resume_key: Option<&str>) -> Option<AgentUsage> {
        // A fresh process needs a brief moment before background capture stores
        // its provider session id. Showing no sample during that window is safer
        // than borrowing another Codex conversation from the same directory.
        let path = Self::session_for_resume_key(cwd, resume_key?)?;
        let content = std::fs::read_to_string(path).ok()?;
        let usage = Self::latest_usage_from_content(&content);
        (usage.context_window_used_tokens.is_some() || usage.context_window_max_tokens.is_some())
            .then_some(usage)
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

        // Status-reporting hooks. Codex has no per-invocation hook flag like
        // claude's `--settings`, so CaPilot layers a per-session config profile
        // (`$CODEX_HOME/capilot-<id>.config.toml`) that defines inline TOML
        // hooks calling the shared `~/CaPilot/status/hook.sh`. The profile is
        // loaded on top of the user's real config — it never modifies it — and
        // removed on session delete. `--dangerously-bypass-hook-trust` is
        // required because the profile hooks are new to codex (not in the
        // user's persisted trust state); CaPilot writes the script itself, so
        // the bypass is safe and scoped to this invocation. A failed profile
        // write degrades to no hooks — it must never abort a spawn.
        args.extend(self.status_hook_args(session));

        Ok(("codex".to_string(), args))
    }

    fn status_hook_args(&self, session: &AgentSession) -> Vec<String> {
        let _ = ensure_status_hooks();
        if Self::write_status_profile(&session.id).is_ok() {
            vec![
                "-p".to_string(),
                Self::profile_name(&session.id),
                "--dangerously-bypass-hook-trust".to_string(),
            ]
        } else {
            vec![]
        }
    }

    fn launch_env(&self, session: &AgentSession) -> Result<Vec<(String, String)>, String> {
        // Session-scoped env for the status hook script: it must know which
        // agent this codex process belongs to and where to write the sidecar.
        // Injected into THIS PTY only — the user's own codex runs stay clean.
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
            .map(|key| vec!["resume".to_string(), key.clone()])
            .unwrap_or_default()
    }

    fn supports_resume(&self) -> bool {
        true
    }
    fn capture_resume_key(&self, cwd: &Path) -> Option<String> {
        Self::detect_recent_resume_key(cwd)
    }

    fn recover_resume_key(
        &self,
        agent_id: &str,
        cwd: &Path,
        created_at_ms: i64,
    ) -> Option<String> {
        Self::recover_session_key(agent_id, cwd, created_at_ms)
    }

    fn context_usage(
        &self,
        cwd: &Path,
        _model: Option<&str>,
        resume_key: Option<&str>,
    ) -> Option<AgentUsage> {
        // Session snapshot: the last token_count's `last_token_usage` is the
        // current active-context reading, and `model_context_window` supplies
        // the capacity from the session itself — no model manifest needed.
        Self::latest_usage(cwd, resume_key)
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

    /// Serializes tests that repoint `HOME`/`CODEX_HOME` so parallel runs don't
    /// observe each other's env (shared with lib.rs / claude / opencode via
    /// agent_runtime).
    use crate::agent_runtime::ENV_LOCK;

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

    /// Point HOME + CODEX_HOME at fresh temp dirs for the duration of `f`, so
    /// status-hook side effects (sidecar dir, codex config profile) never touch
    /// the developer's real `~/CaPilot` / `~/.codex`.
    fn with_isolated_homes(f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev_home = std::env::var_os("HOME");
        let prev_codex_home = std::env::var_os("CODEX_HOME");
        let base = std::env::temp_dir().join(format!(
            "capilot_codex_env_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(base.join("codex-home")).unwrap();
        std::env::set_var("HOME", &base.join("home"));
        std::env::set_var("CODEX_HOME", &base.join("codex-home"));
        f();
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_codex_home {
            Some(v) => std::env::set_var("CODEX_HOME", v),
            None => std::env::remove_var("CODEX_HOME"),
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn builds_codex_flags_and_stable_resume_syntax() {
        with_isolated_homes(|| {
            let adapter = CodexAdapter::new();
            let (_, args) = adapter
                .spawn_interactive(&session(Some("session-id")))
                .unwrap();
            assert!(args.windows(2).any(|v| v == ["--model", "gpt-5.4"]));
            assert!(args
                .windows(2)
                .any(|v| v == ["--ask-for-approval", "untrusted"]));
            assert!(args.windows(2).any(|v| v == ["--sandbox", "read-only"]));
            // Status hooks: the per-session profile is written into $CODEX_HOME
            // and wired via `-p` + the trust bypass flag.
            assert!(args.windows(2).any(|v| v == ["-p", "capilot-test"]));
            assert!(args
                .iter()
                .any(|a| a == "--dangerously-bypass-hook-trust"));
            let profile = CodexAdapter::status_profile("test").unwrap();
            let toml = std::fs::read_to_string(&profile).unwrap();
            assert!(toml.contains("[[hooks.UserPromptSubmit]]"));
            assert!(toml.contains("[[hooks.PermissionRequest]]"));
            // Codex clamps SessionEnd hook timeouts to 3s and warns on startup
            // when a larger value is declared. Trim each hook block (the format
            // string carries source indentation) and assert the per-event
            // timeout: SessionEnd declares 3s, every other event 5s.
            let blocks: Vec<&str> = toml
                .split("\n\n")
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .collect();
            let block_for = |event: &str| {
                blocks
                    .iter()
                    .find(|b| b.starts_with(&format!("[[hooks.{event}]]")))
                    .unwrap_or_else(|| panic!("missing {event} hook block"))
            };
            assert!(block_for("SessionEnd").contains("timeout = 3"));
            assert!(block_for("UserPromptSubmit").contains("timeout = 5"));
            assert_eq!(
                adapter.resume_args(&session(Some("session-id"))),
                ["resume", "session-id"]
            );
        });
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

    #[test]
    fn parses_codex_token_count_for_context_usage() {
        // Two token_count events: the `last` reading comes from the LAST event,
        // and the session-cumulative cache stats sum across BOTH. Codex
        // accounting (OpenAI style): `input_tokens` already includes the cached
        // portion, so the prompt total is `input_tokens` and the hit portion is
        // `cached_input_tokens`.
        let content = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/tmp/project\"}}\n",
            "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.6-sol\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"model_context_window\":258400}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"total_tokens\":31114},\"last_token_usage\":{\"input_tokens\":15674,\"cached_input_tokens\":11008,\"cache_write_input_tokens\":0,\"output_tokens\":715,\"reasoning_output_tokens\":373,\"total_tokens\":16389},\"model_context_window\":258400}}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"last_token_usage\":{\"input_tokens\":8000,\"cached_input_tokens\":3000,\"cache_write_input_tokens\":0,\"output_tokens\":200,\"total_tokens\":8200},\"model_context_window\":258400}}}\n",
        );
        let usage = CodexAdapter::latest_usage_from_content(content);
        // `last` usage object, not the session total.
        assert_eq!(usage.context_window_used_tokens, Some(8200));
        assert_eq!(usage.context_window_max_tokens, Some(258400));
        // Session cumulative: 15674 + 8000 prompt, 11008 + 3000 hit.
        assert_eq!(usage.cache_hit_tokens, Some(11008 + 3000));
        assert_eq!(usage.cache_total_input_tokens, Some(15674 + 8000));
        assert_eq!(usage.actual_model.as_deref(), Some("gpt-5.6-sol"));
    }

    #[test]
    fn codex_usage_has_max_before_first_token_count() {
        // A session still on its first turn emits task_started (with the model
        // window) before any token_count — used stays absent, max is usable.
        let content = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"cwd\":\"/tmp/project\"}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"model_context_window\":258400}}\n",
        );
        let usage = CodexAdapter::latest_usage_from_content(content);
        assert_eq!(usage.context_window_used_tokens, None);
        assert_eq!(usage.context_window_max_tokens, Some(258400));
    }

    #[test]
    fn codex_usage_uses_exact_resume_key_when_sessions_share_cwd() {
        with_isolated_homes(|| {
            let dir = CodexAdapter::sessions_dir().unwrap().join("2026/08/14");
            std::fs::create_dir_all(&dir).unwrap();
            let write_session = |name: &str, id: &str, input: u64, cached: u64| {
                let content = [
                    serde_json::json!({
                        "type": "session_meta",
                        "payload": { "cwd": "/tmp/project", "id": id }
                    })
                    .to_string(),
                    serde_json::json!({
                        "type": "event_msg",
                        "payload": {
                            "type": "token_count",
                            "info": {
                                "last_token_usage": {
                                    "input_tokens": input,
                                    "cached_input_tokens": cached,
                                    "output_tokens": 10,
                                    "total_tokens": input + 10
                                },
                                "model_context_window": 258400
                            }
                        }
                    })
                    .to_string(),
                ]
                .join("\n");
                std::fs::write(dir.join(name), format!("{content}\n")).unwrap();
            };

            // The second transcript is written last, reproducing the old bug:
            // a cwd-only lookup returned it for both agents.
            write_session("first.jsonl", "session-a", 10_000, 2_000);
            write_session("second.jsonl", "session-b", 20_000, 18_000);

            let usage_a = CodexAdapter::latest_usage(
                Path::new("/tmp/project"),
                Some("session-a"),
            )
            .unwrap();
            let usage_b = CodexAdapter::latest_usage(
                Path::new("/tmp/project"),
                Some("session-b"),
            )
            .unwrap();
            assert_eq!(usage_a.cache_hit_tokens, Some(2_000));
            assert_eq!(usage_a.cache_total_input_tokens, Some(10_000));
            assert_eq!(usage_b.cache_hit_tokens, Some(18_000));
            assert_eq!(usage_b.cache_total_input_tokens, Some(20_000));

            // Missing identity or a mismatched cwd must never borrow usage from
            // another conversation merely because its transcript is newest.
            assert!(CodexAdapter::latest_usage(Path::new("/tmp/project"), None).is_none());
            assert!(CodexAdapter::latest_usage(
                Path::new("/tmp/other"),
                Some("session-a")
            )
            .is_none());
        });
    }

    #[test]
    fn recovers_missing_resume_key_from_uuid_v7_spawn_time() {
        with_isolated_homes(|| {
            let dir = CodexAdapter::sessions_dir().unwrap().join("2026/08/14");
            std::fs::create_dir_all(&dir).unwrap();
            let write_meta = |name: &str, id: &str| {
                let line = serde_json::json!({
                    "type": "session_meta",
                    "payload": { "cwd": "/tmp/project", "id": id }
                });
                std::fs::write(dir.join(name), format!("{line}\n")).unwrap();
            };
            // UUIDv7 timestamps: session-a is 213 ms after the Agent row was
            // created; session-b is more than two minutes away in the same cwd.
            write_meta(
                "a.jsonl",
                "01a000d2-c39c-7510-86a0-193c9651b2aa",
            );
            write_meta(
                "b.jsonl",
                "01a000de-1cd7-7d13-a9b7-16b93b7376cf",
            );
            assert_eq!(
                CodexAdapter::recover_session_key(
                    "agent-without-sidecar",
                    Path::new("/tmp/project"),
                    1_786_720_207_559,
                )
                .as_deref(),
                Some("01a000d2-c39c-7510-86a0-193c9651b2aa")
            );

            // A hook-bound session id is authoritative even when another
            // transcript has a timestamp closer to the Agent creation time.
            let sidecar = crate::persistence::status_file("bound-agent");
            std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
            std::fs::write(
                sidecar,
                r#"{"status":"idle","ts":1,"session_id":"01a000de-1cd7-7d13-a9b7-16b93b7376cf"}"#,
            )
            .unwrap();
            assert_eq!(
                CodexAdapter::recover_session_key(
                    "bound-agent",
                    Path::new("/tmp/project"),
                    1_786_720_207_559,
                )
                .as_deref(),
                Some("01a000de-1cd7-7d13-a9b7-16b93b7376cf")
            );
            assert!(CodexAdapter::recover_session_key(
                "agent-without-sidecar",
                Path::new("/tmp/other"),
                1_786_720_207_559,
            )
            .is_none());
        });
    }
}
