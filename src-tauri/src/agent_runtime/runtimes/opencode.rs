use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, AgentUsage, ModelInfo, PermissionModeInfo,
    ThinkingOptionInfo,
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Adapter for the OpenCode interactive TUI. The installed CLI is the source
/// of truth for models and resumable sessions.
pub struct OpenCodeAdapter;

impl OpenCodeAdapter {
    pub fn new() -> Self {
        Self
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
                    efforts: None,
                }
            })
            .collect()
    }

    /// Push one parsed `provider/model` entry, using the catalog `name` from
    /// the JSON block that `--verbose` appends (falling back to the id-derived
    /// name when the block is absent or unparsable).
    fn push_model(
        models: &mut Vec<ModelInfo>,
        header: Option<&str>,
        block: &[&str],
        default_model: Option<&str>,
    ) {
        let Some(id_line) = header else {
            return;
        };
        let id = id_line.to_string();
        let provider = id_line
            .split_once('/')
            .map(|(provider, _)| provider)
            .unwrap_or("opencode")
            .to_string();
        let name = if block.is_empty() {
            Self::display_name(&id)
        } else {
            serde_json::from_str::<Value>(&block.join("\n"))
                .ok()
                .and_then(|value| value.get("name").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_else(|| Self::display_name(&id))
        };
        models.push(ModelInfo {
            id,
            name,
            provider,
            is_default: default_model == Some(id_line),
            efforts: None,
        });
    }

    /// Parse `opencode models --verbose`: a column-0 `provider/model` header
    /// line followed by that model's catalog JSON (indented fields).
    fn parse_verbose_models(output: &str, default_model: Option<&str>) -> Vec<ModelInfo> {
        let mut models = Vec::new();
        let mut header: Option<&str> = None;
        let mut block: Vec<&str> = Vec::new();
        for line in output.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // JSON braces sit at column 0 too; only treat a line as a new
            // model header when it starts at column 0 and is `provider/model`.
            let is_header = !trimmed.starts_with('{')
                && !trimmed.starts_with('}')
                && trimmed.contains('/')
                && line.chars().next().is_some_and(|ch| !ch.is_whitespace());
            if is_header {
                Self::push_model(&mut models, header, &block, default_model);
                header = Some(trimmed);
                block.clear();
            } else {
                block.push(line);
            }
        }
        Self::push_model(&mut models, header, &block, default_model);
        models
    }

    fn discover_models() -> Vec<ModelInfo> {
        let default_model = Self::native_default_model();
        // --verbose emits the same provider/model lines as the plain listing
        // but also appends each model's catalog entry, which carries the display
        // name OpenCode's TUI shows in its model dialog. CaPilot selects a model
        // by typing that exact name into the dialog, so the catalog name (not
        // the id-derived one) must be used.
        if let Ok(output) = Command::new("opencode")
            .args(["models", "--verbose"])
            .output()
        {
            if output.status.success() {
                let models = Self::parse_verbose_models(
                    &String::from_utf8_lossy(&output.stdout),
                    default_model.as_deref(),
                );
                if !models.is_empty() {
                    return models;
                }
            }
        }
        // Fall back to the plain listing (older OpenCode without --verbose).
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

    // ── Context-window usage (docs/context-window-usage.md) ────────────────
    //
    // OpenCode keeps sessions in a local SQLite store (`opencode.db`, WAL) and
    // reports per-step token accounting in `step-finish` parts. The current
    // active-context reading is the latest `step-finish`'s `tokens.total` (a
    // single snapshot — NOT the cumulative `session.tokens_*` columns). The
    // capacity comes from the observed assistant model's catalog `limit.context`
    // (`opencode models --verbose`), cached process-wide on a TTL.

    /// OpenCode data dir (SQLite store + session files).
    fn data_dir() -> Option<PathBuf> {
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
            return Some(PathBuf::from(data_home).join("opencode"));
        }
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share/opencode"))
    }

    fn db_path() -> Option<PathBuf> {
        Self::data_dir().map(|dir| dir.join("opencode.db"))
    }

    /// Read-only handle to the opencode SQLite store (WAL — safe to read while
    /// the TUI is running). `None` when the DB is absent or cannot be opened.
    fn open_db() -> Option<Connection> {
        let path = Self::db_path()?;
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
        let _ = conn.busy_timeout(Duration::from_secs(2));
        Some(conn)
    }

    /// Newest session id whose `directory` matches `cwd`.
    fn newest_session_id(conn: &Connection, cwd: &Path) -> Option<String> {
        conn.query_row(
            "SELECT id FROM session WHERE directory = ?1 ORDER BY time_updated DESC LIMIT 1",
            params![cwd.to_string_lossy().to_string()],
            |row| row.get(0),
        )
        .ok()
    }

    /// Current active-context estimate: the latest `step-finish` part's
    /// `tokens.total` (input + output + reasoning + cache read/write — a single
    /// snapshot, never accumulated across parts).
    fn latest_step_finish_tokens(conn: &Connection, cwd: &Path) -> Option<u64> {
        let session_id = Self::newest_session_id(conn, cwd)?;
        let data: Option<String> = conn
            .query_row(
                "SELECT data FROM part WHERE session_id = ?1 AND data LIKE '%step-finish%' \
                 ORDER BY time_created DESC LIMIT 1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()
            .ok()?;
        let value: Value = serde_json::from_str(&data?).ok()?;
        value.pointer("/tokens/total").and_then(Value::as_u64)
    }

    /// Observed assistant model as `providerID/modelID` from the newest
    /// assistant message in the cwd's session. Authoritative over the draft
    /// selection (the doc's "observed assistant-model metadata updates the
    /// maximum").
    fn observed_model_id(conn: &Connection, cwd: &Path) -> Option<String> {
        let session_id = Self::newest_session_id(conn, cwd)?;
        let data: Option<String> = conn
            .query_row(
                "SELECT data FROM message WHERE session_id = ?1 AND data LIKE '%\"role\":\"assistant\"%' \
                 ORDER BY time_created DESC LIMIT 1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()
            .ok()?;
        let value: Value = serde_json::from_str(&data?).ok()?;
        let provider = value.get("providerID").and_then(Value::as_str)?;
        let model = value.get("modelID").and_then(Value::as_str)?;
        Some(format!("{provider}/{model}"))
    }

    /// Process-wide cache of `opencode models --verbose` `limit.context` per
    /// `provider/model`, refreshed on a TTL. Fetching the catalog on every poll
    /// would spawn a subprocess ~3s per running agent.
    fn catalog() -> &'static Mutex<Option<(Instant, HashMap<String, u64>)>> {
        static CATALOG: OnceLock<Mutex<Option<(Instant, HashMap<String, u64>)>>> = OnceLock::new();
        CATALOG.get_or_init(|| Mutex::new(None))
    }

    const CATALOG_TTL: Duration = Duration::from_secs(300);

    /// On-disk copy of the model-limit catalog, so a daemon restart (cold
    /// in-memory cache) never pays the ~0.7s `opencode models --verbose`
    /// subprocess on the meter's first render. Lives under the same cache root
    /// as the session TUI configs.
    fn catalog_cache_path() -> Option<PathBuf> {
        let cache_root = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
        Some(cache_root.join("capilot-ide/opencode-model-limits.json"))
    }

    fn save_catalog_to_disk(map: &HashMap<String, u64>) {
        let Some(path) = Self::catalog_cache_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(bytes) = serde_json::to_vec(map) else {
            return;
        };
        let _ = std::fs::write(path, bytes);
    }

    fn load_catalog_from_disk() -> HashMap<String, u64> {
        let Some(path) = Self::catalog_cache_path() else {
            return HashMap::new();
        };
        std::fs::read_to_string(path)
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    fn catalog_limit_context(model_id: &str) -> Option<u64> {
        // Whether the in-memory cache existed but was stale. This distinguishes
        // a process restart (memory empty → reuse the persisted catalog, no
        // subprocess) from a long-running process whose TTL expired (must
        // refresh against the live CLI to stay fresh). Without the split, a
        // persisted catalog would satisfy every cold miss and the CLI would
        // never be re-queried — staleness would never self-heal.
        let mut had_cached = false;
        {
            if let Ok(guard) = Self::catalog().lock() {
                if let Some((fetched_at, map)) = guard.as_ref() {
                    if fetched_at.elapsed() < Self::CATALOG_TTL {
                        return map.get(model_id).copied();
                    }
                    had_cached = true;
                }
            }
        }
        let map = if had_cached {
            Self::fetch_model_limits()
        } else {
            let mut from_disk = Self::load_catalog_from_disk();
            if from_disk.is_empty() {
                from_disk = Self::fetch_model_limits();
            }
            from_disk
        };
        // Rewrite the persisted copy whenever we fetched fresh, so the file
        // stays current while the app runs. A failed fetch (empty map) never
        // clobbers a good on-disk copy.
        if !map.is_empty() {
            Self::save_catalog_to_disk(&map);
        }
        if let Ok(mut guard) = Self::catalog().lock() {
            *guard = Some((Instant::now(), map.clone()));
        }
        map.get(model_id).copied()
    }

    fn fetch_model_limits() -> HashMap<String, u64> {
        let mut map = HashMap::new();
        let Ok(output) = Command::new("opencode")
            .args(["models", "--verbose"])
            .output()
        else {
            return map;
        };
        if !output.status.success() {
            return map;
        }
        Self::parse_model_limits(&String::from_utf8_lossy(&output.stdout), &mut map);
        map
    }

    /// Parse `opencode models --verbose` into `provider/model → limit.context`:
    /// a column-0 `provider/model` header line followed by that model's catalog
    /// JSON block (mirrors `parse_verbose_models`' line classification).
    fn parse_model_limits(output: &str, map: &mut HashMap<String, u64>) {
        let mut header: Option<String> = None;
        let mut block: Vec<&str> = Vec::new();
        let flush =
            |header: &mut Option<String>, block: &mut Vec<&str>, map: &mut HashMap<String, u64>| {
                if let (Some(id), Ok(value)) = (
                    header.take(),
                    serde_json::from_str::<Value>(&block.join("\n")),
                ) {
                    if let Some(context) = value.pointer("/limit/context").and_then(Value::as_u64) {
                        map.insert(id, context);
                    }
                }
                block.clear();
            };
        for line in output.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let is_header = !trimmed.starts_with('{')
                && !trimmed.starts_with('}')
                && trimmed.contains('/')
                && line.chars().next().is_some_and(|ch| !ch.is_whitespace());
            if is_header {
                flush(&mut header, &mut block, map);
                header = Some(trimmed.to_string());
            } else {
                block.push(line);
            }
        }
        flush(&mut header, &mut block, map);
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

    fn launch_env(&self, _session: &AgentSession) -> Result<Vec<(String, String)>, String> {
        // No per-session env: the status plugin (`OPENCODE_CONFIG_DIR` +
        // `CAPILOT_AGENT_ID`/`CAPILOT_STATUS_DIR`) and the TUI keybind override
        // (`OPENCODE_TUI_CONFIG` F12 command-palette rebind) were retired in
        // Phase 5 — structured agents report lifecycle through the daemon's
        // AgentManager, and the Agent main path no longer injects keys into
        // provider TUIs.
        Ok(vec![])
    }

    fn resume_args(&self, session: &AgentSession) -> Vec<String> {
        // Legacy PTY sessions resume only with the persisted key (stored in the
        // DB row); the dynamic `session list` scan was retired in Phase 5.
        session
            .resume_key
            .as_ref()
            .map(|key| vec!["--session".into(), key.clone()])
            .unwrap_or_default()
    }

    fn context_usage(&self, cwd: &Path, model: Option<&str>) -> Option<AgentUsage> {
        let conn = Self::open_db()?;
        let used = Self::latest_step_finish_tokens(&conn, cwd);
        // Observed assistant model wins over the draft selection (doc: use the
        // model attached to the assistant message when available).
        let model_id = Self::observed_model_id(&conn, cwd).or_else(|| model.map(str::to_owned));
        let max = model_id.as_deref().and_then(Self::catalog_limit_context);
        if used.is_none() && max.is_none() {
            return None;
        }
        Some(AgentUsage {
            context_window_used_tokens: used,
            context_window_max_tokens: max,
        })
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

    /// Serializes tests that repoint `XDG_CACHE_HOME`/`HOME` so parallel runs
    /// don't observe each other's env (shared with lib.rs / claude / codex via
    /// agent_runtime).
    use crate::agent_runtime::ENV_LOCK;

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

    /// Point XDG_CACHE_HOME + HOME at fresh temp dirs for the duration of `f`,
    /// so status-plugin / TUI-config side effects never touch the developer's
    /// real cache dirs or `~/CaPilot`.
    fn with_isolated_cache(f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev_home = std::env::var_os("HOME");
        let prev_xdg_cache = std::env::var_os("XDG_CACHE_HOME");
        let base = std::env::temp_dir().join(format!(
            "capilot_opencode_env_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(base.join("cache")).unwrap();
        std::env::set_var("HOME", &base.join("home"));
        std::env::set_var("XDG_CACHE_HOME", &base.join("cache"));
        f();
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_xdg_cache {
            Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn launch_env_is_empty_after_hook_and_tui_retirement() {
        with_isolated_cache(|| {
            let adapter = OpenCodeAdapter::new();
            // Phase 5: no per-session env — the status plugin, the hook env and
            // the OPENCODE_TUI_CONFIG keybind override are all retired.
            let env = adapter.launch_env(&session("ask", None)).unwrap();
            assert!(env.is_empty(), "launch env must be empty: {env:?}");
        });
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
    fn parses_verbose_catalog_names_used_for_tui_selection() {
        let output = concat!(
            "opencode/deepseek-v4-flash-free\n",
            "{\n",
            "  \"id\": \"deepseek-v4-flash-free\",\n",
            "  \"providerID\": \"opencode\",\n",
            "  \"name\": \"DeepSeek V4 Flash Free\"\n",
            "}\n",
            "opencode-go/deepseek-v4-flash\n",
            "{\n",
            "  \"id\": \"deepseek-v4-flash\",\n",
            "  \"providerID\": \"opencode-go\",\n",
            "  \"name\": \"DeepSeek V4 Flash (2x usage)\"\n",
            "}\n",
            "opencode/big-pickle\n",
            "{\n",
            "  \"id\": \"big-pickle\",\n",
            "  \"providerID\": \"opencode\",\n",
            "  \"name\": \"Big Pickle\"\n",
            "}\n",
        );
        let models =
            OpenCodeAdapter::parse_verbose_models(output, Some("opencode/deepseek-v4-flash-free"));
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "opencode/deepseek-v4-flash-free");
        assert_eq!(models[0].name, "DeepSeek V4 Flash Free");
        assert!(models[0].is_default);
        assert_eq!(models[1].id, "opencode-go/deepseek-v4-flash");
        assert_eq!(models[1].name, "DeepSeek V4 Flash (2x usage)");
        assert!(!models[1].is_default);
        assert_eq!(models[2].name, "Big Pickle");
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
    fn reads_step_finish_tokens_and_observed_model_from_db() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, time_updated INTEGER);
             CREATE TABLE part (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, directory, time_updated) VALUES ('ses_1', '/tmp/project', 100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part (id, session_id, time_created, data) VALUES ('prt_1', 'ses_1', 200, ?1)",
            params![r#"{"type":"step-finish","reason":"stop","tokens":{"total":161848,"input":252,"output":60,"reasoning":0,"cache":{"write":0,"read":161536}}}"#],
        )
        .unwrap();
        // A second step-finish part exercises the newest-wins rule: the
        // active-context estimate comes from the newest part.
        conn.execute(
            "INSERT INTO part (id, session_id, time_created, data) VALUES ('prt_2', 'ses_1', 400, ?1)",
            params![r#"{"type":"step-finish","reason":"stop","tokens":{"total":560,"input":100,"output":40,"reasoning":0,"cache":{"write":20,"read":300}}}"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message (id, session_id, time_created, data) VALUES ('msg_1', 'ses_1', 300, ?1)",
            params![r#"{"role":"assistant","providerID":"opencode","modelID":"deepseek-v4-flash-free","tokens":{"total":161848}}"#],
        )
        .unwrap();

        // Current active context is the newest step-finish total.
        assert_eq!(
            OpenCodeAdapter::latest_step_finish_tokens(&conn, Path::new("/tmp/project")),
            Some(560)
        );
        assert_eq!(
            OpenCodeAdapter::observed_model_id(&conn, Path::new("/tmp/project")).as_deref(),
            Some("opencode/deepseek-v4-flash-free")
        );
        // A cwd with no session yields nothing.
        assert_eq!(
            OpenCodeAdapter::latest_step_finish_tokens(&conn, Path::new("/tmp/other")),
            None
        );
    }

    #[test]
    fn parses_catalog_limit_context() {
        let output = concat!(
            "opencode/big-pickle\n",
            "{\n  \"id\": \"big-pickle\",\n  \"limit\": {\"context\": 200000, \"input\": 160000}\n}\n",
            "opencode-go/deepseek-v4-flash\n",
            "{\n  \"id\": \"deepseek-v4-flash\",\n  \"limit\": {\"context\": 131072}\n}\n",
        );
        let mut map = HashMap::new();
        OpenCodeAdapter::parse_model_limits(output, &mut map);
        assert_eq!(map.get("opencode/big-pickle"), Some(&200000));
        assert_eq!(map.get("opencode-go/deepseek-v4-flash"), Some(&131072));

        // The disk cache serializes `provider/model → context` as a plain JSON
        // object; the `/`-separated keys must round-trip unchanged.
        let bytes = serde_json::to_vec(&map).unwrap();
        let back: HashMap<String, u64> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back, map);
    }
}
