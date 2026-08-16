use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, AgentUsage, ModelInfo, PermissionModeInfo,
    ThinkingOptionInfo,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

/// Adapter for the Pi coding agent CLI (`pi`, npm `@earendil-works/pi-coding-agent`).
///
/// Pi is an interactive TUI coding agent with read / bash / edit / write tools.
/// Its session store lives under `~/.pi/agent/sessions/<--cwd-key-->/` as JSONL
/// files named `<timestamp>_<uuidv7>.jsonl`; the file stem's uuidv7 is the
/// provider session id used to resume (`pi --session <id>`).
///
/// Unlike claude/codex/opencode, pi has no per-invocation status-hook seam, so
/// the tab strip degrades to PTY-activity heuristics (the same fallback a plain
/// bash terminal uses). Model/thinking/mode are argv-injected; there is no
/// per-session config file to clean up on delete.
pub struct PiAdapter;

impl PiAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Pi config root: `$PI_CODING_AGENT_DIR` when set, else `~/.pi/agent`.
    fn config_dir() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("PI_CODING_AGENT_DIR") {
            return Some(PathBuf::from(dir));
        }
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".pi").join("agent"))
    }

    /// Session storage root: `$PI_CODING_AGENT_SESSION_DIR` when set (or
    /// `--session-dir`), else `<config>/sessions`.
    fn sessions_dir() -> Option<PathBuf> {
        std::env::var_os("PI_CODING_AGENT_SESSION_DIR")
            .map(PathBuf::from)
            .or_else(|| Self::config_dir().map(|dir| dir.join("sessions")))
    }

    /// Session directory for a cwd, mirroring pi's own `getDefaultSessionDir`
    /// encoding: leading `/` stripped, `/ \ :` collapse to `-`, wrapped in
    /// `--…--` (`/home/x/my.proj` → `--home-x-my-proj--`).
    fn project_session_dir(cwd: &Path) -> Option<PathBuf> {
        let readable: String = cwd
            .to_string_lossy()
            .trim_start_matches(['/', '\\'])
            .chars()
            .map(|c| if c == '/' || c == '\\' || c == ':' { '-' } else { c })
            .collect();
        Some(Self::sessions_dir()?.join(format!("--{readable}--")))
    }

    fn check_available() -> bool {
        Command::new("pi")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    /// Pi stores one credential per provider in `~/.pi/agent/auth.json`
    /// (OAuth tokens / API keys), but a provider can also be configured purely
    /// via an env API key. Any usable credential counts as authenticated.
    fn check_authenticated() -> bool {
        if let Some(dir) = Self::config_dir() {
            if let Ok(content) = std::fs::read_to_string(dir.join("auth.json")) {
                if let Ok(value) = serde_json::from_str::<Value>(&content) {
                    if value
                        .as_object()
                        .is_some_and(|providers| {
                            providers.values().any(|cred| {
                                cred.get("type").and_then(Value::as_str).is_some()
                            })
                        })
                    {
                        return true;
                    }
                }
            }
        }
        // Env API keys pi reads for its built-in providers (providers.md).
        [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "GEMINI_API_KEY",
            "DEEPSEEK_API_KEY",
            "OPENROUTER_API_KEY",
            "XAI_API_KEY",
            "GROQ_API_KEY",
            "MISTRAL_API_KEY",
            "OPENCODE_API_KEY",
        ]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|v| !v.is_empty()))
    }

    /// Default provider/model pair from `~/.pi/agent/settings.json` — the same
    /// config pi's own TUI reads for its default selection.
    fn default_model() -> Option<(String, String)> {
        let content = std::fs::read_to_string(Self::config_dir()?.join("settings.json")).ok()?;
        let value = serde_json::from_str::<Value>(&content).ok()?;
        let provider = value.get("defaultProvider")?.as_str()?.to_string();
        let model = value.get("defaultModel")?.as_str()?.to_string();
        Some((provider, model))
    }

    /// Current thinking level as last persisted by pi. `setThinkingLevel` writes
    /// `defaultThinkingLevel` to settings.json on the next tick after every live
    /// Shift+Tab cycle (and on spawn when the level actually changes), so this
    /// is the source of truth the Composer uses to compute the cycle distance
    /// — re-read after each `shift+tab` press, clamping can't overshoot.
    pub fn current_thinking_level() -> Option<String> {
        let content = std::fs::read_to_string(Self::config_dir()?.join("settings.json")).ok()?;
        let value = serde_json::from_str::<Value>(&content).ok()?;
        value
            .get("defaultThinkingLevel")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// Model catalog from `pi --list-models`. The CLI prints a fixed-width
    /// table; provider and model are the first two whitespace-separated fields
    /// of each row (provider/model ids never contain spaces). Runs offline so
    /// the bundled/cached catalog is used instead of a startup network fetch.
    fn list_models() -> Vec<ModelInfo> {
        let Ok(output) = Command::new("pi")
            .env("PI_OFFLINE", "1")
            .arg("--list-models")
            .output()
        else {
            return vec![];
        };
        if !output.status.success() {
            return vec![];
        }
        let Ok(text) = String::from_utf8(output.stdout) else {
            return vec![];
        };
        Self::parse_model_table(&text)
    }

    /// Parse the `pi --list-models` table. Provider-qualified ids avoid
    /// collisions when the same model id exists under several routes (e.g.
    /// anthropic vs openai-codex); the settings-default model is marked
    /// `is_default` so a fresh spawn pins it.
    fn parse_model_table(table: &str) -> Vec<ModelInfo> {
        let default = Self::default_model();
        let mut models = Vec::new();
        for line in table.lines() {
            let mut fields = line.split_whitespace();
            let (Some(provider), Some(model)) = (fields.next(), fields.next()) else {
                continue;
            };
            // Header row.
            if provider == "provider" && model == "model" {
                continue;
            }
            let is_default = default
                .as_ref()
                .is_some_and(|(dp, dm)| dp == provider && dm == model);
            models.push(ModelInfo {
                id: format!("{provider}/{model}"),
                name: model.to_string(),
                provider: provider.to_string(),
                is_default,
                efforts: None,
            });
        }
        models
    }

    /// Split a provider-qualified CaPilot model id (`openai-codex/gpt-5.4-mini`)
    /// into the `pi` `--provider` / `--model` pair. A bare id (no `/`) is passed
    /// as `--model` only, letting pi resolve it with its default provider.
    fn split_model_id(model: &str) -> (Option<&str>, &str) {
        match model.rsplit_once('/') {
            Some((provider, bare)) if !provider.is_empty() && !bare.is_empty() => {
                (Some(provider), bare)
            }
            _ => (None, model),
        }
    }

    /// Pi thinking level for one CaPilot speed tier. `auto` (and unknown tiers)
    /// omit the flag so `~/.pi/agent/settings.json` `defaultThinkingLevel`
    /// applies.
    fn thinking_for_speed(speed: &str) -> Option<&'static str> {
        match speed {
            "off" => Some("off"),
            "fast" => Some("low"),
            "mid" => Some("medium"),
            "high" => Some("high"),
            "xhigh" => Some("xhigh"),
            _ => None,
        }
    }

    /// Session id from a pi session file name (`<timestamp>_<uuidv7>.jsonl`).
    /// The uuidv7 contains no `_`, so the id is everything after the first `_`.
    fn session_id_from_file_name(path: &Path) -> Option<String> {
        let stem = path.file_stem()?.to_string_lossy().into_owned();
        stem.split_once('_').map(|(_, id)| id.to_string())
    }

    /// Resolve the session file a `resume_key` names. The key is the uuidv7
    /// stored in the file name; when it no longer matches any file the session
    /// is gone, so return `None` rather than substituting another conversation
    /// that merely shares the cwd.
    fn session_file_for_key(session_dir: &Path, key: &str) -> Option<PathBuf> {
        std::fs::read_dir(session_dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .find(|path| Self::session_id_from_file_name(path).as_deref() == Some(key))
    }

    /// Newest session file in a project dir (the one `pi -c` would continue).
    fn newest_session_file(session_dir: &Path) -> Option<PathBuf> {
        std::fs::read_dir(session_dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .filter_map(|path| {
                std::fs::metadata(&path)
                    .ok()
                    .and_then(|meta| meta.modified().ok())
                    .map(|modified| (path, modified))
            })
            .max_by_key(|(_, modified)| *modified)
            .map(|(path, _)| path)
    }

    /// Session-cumulative context + cache accounting from a pi session JSONL.
    ///
    /// Each `type: "message"` line carries the AgentMessage under `message`;
    /// assistant messages expose a camelCase `usage` (`input`, `output`,
    /// `cacheRead`, `cacheWrite`). Following pi's own accounting (input excludes
    /// cache reads), the active-context estimate for one turn is
    /// `input + cacheRead + cacheWrite` and the cache-hit denominator is the
    /// same sum across all assistant messages.
    fn usage_from_content(content: &str) -> Option<AgentUsage> {
        let mut cache_hit: u64 = 0;
        let mut cache_total: u64 = 0;
        let mut used: Option<u64> = None;
        let mut actual_model: Option<String> = None;
        for line in content.lines() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if value.get("type").and_then(Value::as_str) != Some("message") {
                continue;
            }
            let Some(message) = value.get("message") else { continue };
            if message.get("role").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let Some(usage) = message.get("usage") else { continue };
            if usage.is_null() {
                continue;
            }
            let input = usage.get("input").and_then(Value::as_u64).unwrap_or(0);
            let cache_read = usage.get("cacheRead").and_then(Value::as_u64).unwrap_or(0);
            let cache_write = usage.get("cacheWrite").and_then(Value::as_u64).unwrap_or(0);
            // Skip trailing zero-usage bookkeeping rows; the last non-empty
            // assistant turn is the live active-context reading.
            if input + cache_read + cache_write > 0 {
                used = Some(input + cache_read + cache_write);
            }
            cache_hit += cache_read;
            cache_total += input + cache_read + cache_write;
            if let Some(model) = message.get("model").and_then(Value::as_str) {
                actual_model = Some(model.to_string());
            }
        }
        used.map(|used| AgentUsage {
            context_window_used_tokens: Some(used),
            // The model's context capacity is not recorded in the session file
            // and would require a catalog probe per poll — leave it unset so
            // the meter renders without a ratio.
            context_window_max_tokens: None,
            cache_hit_tokens: Some(cache_hit),
            cache_total_input_tokens: Some(cache_total),
            actual_model,
        })
    }
}

impl AgentRuntimeAdapter for PiAdapter {
    fn id(&self) -> &str {
        "pi"
    }

    fn name(&self) -> &str {
        "Pi"
    }

    fn is_available(&self) -> bool {
        Self::check_available()
    }

    fn is_authenticated(&self) -> bool {
        Self::check_authenticated()
    }

    fn version(&self) -> Option<String> {
        crate::agent_runtime::adapter::cli_version("pi")
    }

    fn list_models(&self) -> Vec<ModelInfo> {
        Self::list_models()
    }

    fn list_permission_modes(&self) -> Vec<PermissionModeInfo> {
        // Pi has no built-in sandbox and no per-tool approval gating; its only
        // launch knob is project trust (whether `.pi` extensions/settings load).
        // The three CaPilot modes therefore map onto that single dimension —
        // "ask" keeps pi's default ask-on-project-trust, the others auto-trust.
        vec![
            PermissionModeInfo {
                id: "ask".into(),
                label: "ask".into(),
                description: "项目资源（.pi 扩展/设置）在 TUI 内询问（pi 默认）；无沙箱".into(),
                requires_confirmation: false,
            },
            PermissionModeInfo {
                id: "auto".into(),
                label: "workspace".into(),
                description: "信任项目资源（--approve）；工具可读写工作区（无沙箱）".into(),
                requires_confirmation: false,
            },
            PermissionModeInfo {
                id: "yolo".into(),
                label: "full access".into(),
                description: "信任项目资源并全权限运行（同 --approve，pi 无沙箱）".into(),
                requires_confirmation: true,
            },
        ]
    }

    fn list_thinking_options(&self) -> Vec<ThinkingOptionInfo> {
        vec![
            ThinkingOptionInfo {
                id: "auto".into(),
                label: "Auto".into(),
                description: "使用 pi 默认思考强度（settings.json defaultThinkingLevel）".into(),
            },
            ThinkingOptionInfo {
                id: "fast".into(),
                label: "Low".into(),
                description: "--thinking low".into(),
            },
            ThinkingOptionInfo {
                id: "mid".into(),
                label: "Medium".into(),
                description: "--thinking medium".into(),
            },
            ThinkingOptionInfo {
                id: "high".into(),
                label: "High".into(),
                description: "--thinking high".into(),
            },
            ThinkingOptionInfo {
                id: "xhigh".into(),
                label: "Extra high".into(),
                description: "--thinking xhigh".into(),
            },
        ]
    }

    fn spawn_interactive(&self, session: &AgentSession) -> Result<(String, Vec<String>), String> {
        let mut args = Vec::new();
        if let Some(model) = &session.model {
            match Self::split_model_id(model) {
                (Some(provider), bare) => {
                    args.extend(["--provider".to_string(), provider.to_string()]);
                    args.extend(["--model".to_string(), bare.to_string()]);
                }
                (None, bare) => args.extend(["--model".to_string(), bare.to_string()]),
            }
        }
        args.extend(self.speed_args(&session.speed));
        args.extend(self.mode_args(&session.mode));
        // pi's regular TUI mode (the default) renders inline in the PTY without
        // an alternate screen, so scrollback behaves like the other runtimes.
        Ok(("pi".to_string(), args))
    }

    fn resume_args(&self, session: &AgentSession) -> Vec<String> {
        session
            .resume_key
            .as_ref()
            .map(|key| vec!["--session".to_string(), key.clone()])
            .unwrap_or_default()
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn capture_resume_key(&self, cwd: &Path) -> Option<String> {
        // A freshly started pi writes its session header immediately, so the
        // newest JSONL under the project dir within the last seconds is the
        // process this spawn just created.
        let session_dir = Self::project_session_dir(cwd)?;
        let now = SystemTime::now();
        let modified_in = |path: &Path| {
            std::fs::metadata(path)
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|modified| now.duration_since(modified).ok())
        };
        std::fs::read_dir(session_dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .filter(|path| {
                modified_in(path)
                    .is_some_and(|age| age <= Duration::from_secs(10))
            })
            .max_by_key(|path| modified_in(path))
            .and_then(|path| Self::session_id_from_file_name(&path))
    }

    fn context_usage(
        &self,
        cwd: &Path,
        _model: Option<&str>,
        resume_key: Option<&str>,
    ) -> Option<AgentUsage> {
        let session_dir = Self::project_session_dir(cwd)?;
        let file = match resume_key {
            Some(key) => Self::session_file_for_key(&session_dir, key)?,
            None => Self::newest_session_file(&session_dir)?,
        };
        let content = std::fs::read_to_string(file).ok()?;
        Self::usage_from_content(&content)
    }

    fn speed_args(&self, speed: &str) -> Vec<String> {
        match Self::thinking_for_speed(speed) {
            Some(level) => vec!["--thinking".to_string(), level.to_string()],
            None => vec![],
        }
    }

    fn mode_args(&self, mode: &str) -> Vec<String> {
        match mode {
            "auto" | "yolo" => vec!["--approve".to_string()],
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::ENV_LOCK;

    fn session(model: Option<&str>, speed: &str, mode: &str, resume_key: Option<&str>) -> AgentSession {
        AgentSession {
            id: "test".into(),
            runtime: "pi".into(),
            mode: mode.into(),
            speed: speed.into(),
            model: model.map(str::to_owned),
            cwd: "/home/hachi/Project/my.proj".into(),
            context_dir: "/home/hachi/Project/my.proj".into(),
            rows: 24,
            cols: 80,
            resume_key: resume_key.map(str::to_owned),
        }
    }

    /// Point PI_CODING_AGENT_DIR (+ env keys) at a fresh temp dir for the
    /// duration of `f`, so auth/session side effects never touch the developer's
    /// real `~/.pi/agent`.
    fn with_isolated_env(f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev_pi_dir = std::env::var_os("PI_CODING_AGENT_DIR");
        let prev_pi_session_dir = std::env::var_os("PI_CODING_AGENT_SESSION_DIR");
        let prev_home = std::env::var_os("HOME");
        let base = std::env::temp_dir().join(format!(
            "capilot_pi_env_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(base.join("agent")).unwrap();
        std::env::set_var("PI_CODING_AGENT_DIR", &base.join("agent"));
        std::env::remove_var("PI_CODING_AGENT_SESSION_DIR");
        std::env::set_var("HOME", &base.join("home"));
        f();
        match prev_pi_dir {
            Some(v) => std::env::set_var("PI_CODING_AGENT_DIR", v),
            None => std::env::remove_var("PI_CODING_AGENT_DIR"),
        }
        match prev_pi_session_dir {
            Some(v) => std::env::set_var("PI_CODING_AGENT_SESSION_DIR", v),
            None => std::env::remove_var("PI_CODING_AGENT_SESSION_DIR"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn project_session_dir_matches_pi_encoding() {
        let dir = PiAdapter::project_session_dir(Path::new("/home/hachi/Project/my.proj"));
        assert_eq!(
            dir.unwrap().file_name().unwrap().to_str().unwrap(),
            "--home-hachi-Project-my.proj--"
        );
    }

    #[test]
    fn builds_pi_flags() {
        with_isolated_env(|| {
            let adapter = PiAdapter::new();
            let (cmd, args) = adapter
                .spawn_interactive(&session(Some("openai-codex/gpt-5.4-mini"), "mid", "auto", None))
                .unwrap();
            assert_eq!(cmd, "pi");
            assert!(args.windows(2).any(|v| v == ["--provider", "openai-codex"]));
            assert!(args.windows(2).any(|v| v == ["--model", "gpt-5.4-mini"]));
            assert!(args.windows(2).any(|v| v == ["--thinking", "medium"]));
            assert!(args.iter().any(|a| a == "--approve"));
            // spawn_interactive does NOT append resume args; lib.rs adds them.
            assert!(!args.windows(2).any(|v| v == ["--session", "abcd1234"]));
        });
    }

    #[test]
    fn bare_model_id_passes_through_without_provider() {
        let (cmd, args) = PiAdapter::new()
            .spawn_interactive(&session(Some("gpt-5.4-mini"), "auto", "ask", None))
            .unwrap();
        assert_eq!(cmd, "pi");
        assert!(args.windows(2).any(|v| v == ["--model", "gpt-5.4-mini"]));
        assert!(!args.windows(2).any(|v| v == ["--provider", "gpt-5.4-mini"]));
        assert!(!args.iter().any(|a| a == "--approve"));
        // auto speed → no thinking flag.
        assert!(!args.iter().any(|a| a == "--thinking"));
    }

    #[test]
    fn resume_args_and_supports_resume() {
        let adapter = PiAdapter::new();
        assert_eq!(
            adapter.resume_args(&session(None, "auto", "ask", Some("0f1a2b3c-0000-4000-8000-000000000000"))),
            ["--session", "0f1a2b3c-0000-4000-8000-000000000000"]
        );
        assert!(adapter.supports_resume());
    }

    #[test]
    fn parses_model_catalog_table() {
        let table = "\
provider      model                       context  max-out  thinking  images
anthropic     claude-sonnet-5             1M       128K     yes       yes
openai-codex  gpt-5.4-mini                272K     128K     yes       yes
";
        with_isolated_env(|| {
            let base = std::env::var("PI_CODING_AGENT_DIR").unwrap();
            std::fs::write(
                PathBuf::from(&base).join("settings.json"),
                r#"{"defaultProvider":"openai-codex","defaultModel":"gpt-5.4-mini"}"#,
            )
            .unwrap();
            let models = PiAdapter::parse_model_table(table);
            assert_eq!(models.len(), 2);
            assert_eq!(models[0].id, "anthropic/claude-sonnet-5");
            assert_eq!(models[0].provider, "anthropic");
            assert!(!models[0].is_default);
            assert_eq!(models[1].id, "openai-codex/gpt-5.4-mini");
            assert!(models[1].is_default);
        });
    }

    #[test]
    fn current_thinking_level_reads_persisted_default() {
        with_isolated_env(|| {
            let base = std::env::var("PI_CODING_AGENT_DIR").unwrap();
            std::fs::write(
                PathBuf::from(&base).join("settings.json"),
                r#"{"defaultProvider":"openai-codex","defaultModel":"gpt-5.4-mini","defaultThinkingLevel":"high"}"#,
            )
            .unwrap();
            assert_eq!(
                PiAdapter::current_thinking_level().as_deref(),
                Some("high")
            );
        });
    }

    #[test]
    fn current_thinking_level_missing_or_unset_returns_none() {
        with_isolated_env(|| {
            // No settings.json at all → None.
            assert_eq!(PiAdapter::current_thinking_level(), None);
            // Present but no defaultThinkingLevel key → None.
            let base = std::env::var("PI_CODING_AGENT_DIR").unwrap();
            std::fs::write(
                PathBuf::from(&base).join("settings.json"),
                r#"{"defaultProvider":"openai-codex","defaultModel":"gpt-5.4-mini"}"#,
            )
            .unwrap();
            assert_eq!(PiAdapter::current_thinking_level(), None);
        });
    }

    #[test]
    fn context_usage_parses_assistant_usage_from_session_jsonl() {
        let jsonl = r#"{"type":"session","version":3,"id":"sess1"}
{"type":"message","id":"m1","parentId":"sess1","timestamp":"2026-08-16T00:00:00.000Z","message":{"role":"user","content":"hi"}}
{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-08-16T00:00:01.000Z","message":{"role":"assistant","api":"openai","provider":"openai-codex","model":"gpt-5.4-mini","usage":{"input":100,"output":20,"cacheRead":50,"cacheWrite":10,"totalTokens":180}}}
{"type":"message","id":"m3","parentId":"m2","timestamp":"2026-08-16T00:00:02.000Z","message":{"role":"assistant","api":"openai","provider":"openai-codex","model":"gpt-5.4-mini","usage":{"input":120,"output":5,"cacheRead":60,"cacheWrite":0,"totalTokens":185}}}
"#;
        let usage = PiAdapter::usage_from_content(jsonl).unwrap();
        // Last non-empty assistant turn: 120 + 60 + 0.
        assert_eq!(usage.context_window_used_tokens, Some(180));
        // Cumulative: cacheRead 50+60; total 100+50+10 + 120+60+0.
        assert_eq!(usage.cache_hit_tokens, Some(110));
        assert_eq!(usage.cache_total_input_tokens, Some(340));
        assert_eq!(usage.actual_model.as_deref(), Some("gpt-5.4-mini"));
        // No max reported (capacity not derivable from the session file).
        assert_eq!(usage.context_window_max_tokens, None);
    }

    #[test]
    fn context_usage_skips_non_assistant_and_zero_usage_rows() {
        let jsonl = r#"{"type":"message","id":"m1","parentId":"s","timestamp":"t","message":{"role":"toolResult","toolName":"bash","content":[{"type":"text","text":"ok"}],"isError":false}}
{"type":"message","id":"m2","parentId":"m1","timestamp":"t","message":{"role":"assistant","provider":"p","model":"m","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0}}}
{"type":"thinking_level_change","id":"c1","parentId":"m2","timestamp":"t","thinkingLevel":"high"}
"#;
        assert!(PiAdapter::usage_from_content(jsonl).is_none());
    }

    #[test]
    fn session_file_for_key_matches_uuid_from_file_name() {
        with_isolated_env(|| {
            let dir = PiAdapter::project_session_dir(Path::new("/home/hachi/Project/my.proj"))
                .unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            let id = "0f1a2b3c-0000-4000-8000-000000000000";
            std::fs::write(
                dir.join(format!("2026-08-16T00-00-00-000Z_{id}.jsonl")),
                "",
            )
            .unwrap();
            let found = PiAdapter::session_file_for_key(&dir, id).unwrap();
            assert_eq!(
                found.file_name().unwrap().to_string_lossy().as_ref(),
                format!("2026-08-16T00-00-00-000Z_{id}.jsonl")
            );
            // Unknown key → None, never a cwd-substitute.
            assert_eq!(
                PiAdapter::session_file_for_key(&dir, "00000000-0000-4000-8000-000000000000"),
                None
            );
            // Newest-file fallback picks the same file.
            let newest = PiAdapter::newest_session_file(&dir).unwrap();
            assert_eq!(newest, found);
        });
    }
}
