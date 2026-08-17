use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, AgentUsage, ModelInfo, PermissionModeInfo,
    ThinkingOptionInfo,
};
use crate::agent_runtime::status_hooks::{HOOK_ENV_AGENT, HOOK_ENV_DIR};
use crate::persistence::status_dir;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Adapter for the OpenCode interactive TUI. The installed CLI is the source
/// of truth for models and resumable sessions.
pub struct OpenCodeAdapter;

/// Status-reporting plugin (`plugin/capilot-status.js`) written into a
/// per-session config dir passed via `OPENCODE_CONFIG_DIR`. OpenCode has no
/// shell-hook surface like claude's `--settings` or codex's config-profile
/// hooks; the only hook point is its in-process plugin event bus. A plugin is
/// a JS module exporting a default async factory that returns a `Hooks` object
/// — this one listens on the `event` hook for session/permission lifecycle
/// events and writes the SAME sidecar format as `~/CaPilot/status/hook.sh`
/// (`{"status","ts","session_id"}`), driven by
/// `CAPILOT_AGENT_ID`/`CAPILOT_STATUS_DIR`.
/// Env-gated: a standalone `opencode` run (no env) is a no-op.
///
/// `OPENCODE_CONFIG_DIR` APPENDS a config dir to opencode's search path — the
/// user's global config still loads (verified on 1.18.16, see the run log
/// ordering in docs/ai-runtime-references.md §2.3), so injection is per-session
/// and leaves the user's global opencode config untouched.
const STATUS_PLUGIN: &str = r#"// CaPilot status plugin — reports opencode session lifecycle to the IDE sidecar.
// Loaded per-session via OPENCODE_CONFIG_DIR (see opencode.rs). Env-gated: when
// CAPILOT_AGENT_ID / CAPILOT_STATUS_DIR are absent this is a no-op, so a
// standalone `opencode` run is never touched. All writes are best-effort and
// must never break the opencode host.
export default async () => {
  const id = process.env.CAPILOT_AGENT_ID;
  const dir = process.env.CAPILOT_STATUS_DIR;
  if (!id || !dir) return {};
  const fs = await import("node:fs");
  const sidecar = `${dir}/${id}.json`;
  let currentStatus = "idle";
  let sessionID;
  const write = () => {
    try {
      fs.mkdirSync(dir, { recursive: true });
      const tmp = `${sidecar}.tmp`;
      const state = { status: currentStatus, ts: Math.floor(Date.now() / 1000) };
      if (sessionID) state.session_id = sessionID;
      fs.writeFileSync(tmp, JSON.stringify(state) + "\n");
      fs.renameSync(tmp, sidecar);
    } catch {}
  };
  // SessionStart has no opencode event; writing idle at load gives the same
  // baseline the other runtimes get from their SessionStart hook.
  write();
  const statusFor = (event) => {
    const payload = event?.properties ?? event?.data;
    if (event?.type === "session.status") {
      const kind = payload?.status?.type;
      if (kind === "busy" || kind === "retry") return "working";
      if (kind === "idle") return "idle";
      return null;
    }
    if (event?.type === "session.idle") return "idle";
    if (event?.type === "permission.asked") return "waiting_input";
    if (event?.type === "permission.replied") return "working";
    // The `question` tool blocks on the user picking an option (claude's
    // AskUserQuestion equivalent) — `awaiting_choice`, distinct from a
    // permission prompt. Answering (replied) or dismissing (rejected) resumes
    // work.
    if (event?.type === "question.asked" || event?.type === "question.v2.asked")
      return "awaiting_choice";
    if (
      event?.type === "question.replied" ||
      event?.type === "question.rejected" ||
      event?.type === "question.v2.replied"
    )
      return "working";
    return null;
  };
  return {
    event: async ({ event }) => {
      // v1 plugin events expose `properties`; v2 uses `data`. Bind once to the
      // first root session observed by this per-process plugin, so child-agent
      // sessions cannot replace the IDE Agent's provider identity later.
      const payload = event?.properties ?? event?.data;
      const candidate = payload?.sessionID ?? payload?.info?.id;
      const info = payload?.info;
      const isRootInCwd =
        info && !info.parentID && (!info.directory || info.directory === process.cwd());
      let learnedSession = false;
      if (!sessionID && typeof candidate === "string" && candidate.startsWith("ses_") && isRootInCwd) {
        sessionID = candidate;
        learnedSession = true;
      }
      const status = statusFor(event);
      if (status) currentStatus = status;
      if (status || learnedSession) write();
    },
  };
};
"#;

impl OpenCodeAdapter {
    pub fn new() -> Self {
        Self
    }

    const CAPILOT_COMMAND_PALETTE_KEY: &'static str = "f12";

    fn tui_config_path(session: &AgentSession) -> Result<PathBuf, String> {
        let cache_root = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| crate::persistence::user_home().ok().map(|home| home.join(".cache")))
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

    /// Per-session config dir for the status plugin
    /// (`$XDG_CACHE_HOME/capilot-ide/opencode-status/<agent_id>/`), passed to
    /// the spawned opencode via `OPENCODE_CONFIG_DIR`. `None` when the cache
    /// root is unresolvable or the id sanitizes to empty.
    fn status_config_dir(agent_id: &str) -> Option<PathBuf> {
        let cache_root = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| crate::persistence::user_home().ok().map(|home| home.join(".cache")))?;
        let safe_id: String = agent_id
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
            .collect();
        if safe_id.is_empty() {
            return None;
        }
        Some(cache_root.join("capilot-ide/opencode-status").join(safe_id))
    }

    /// Write the status plugin (`plugin/capilot-status.js`) into a per-session
    /// config dir. Best-effort: `None` (no plugin injected, status falls back
    /// to the activity heuristic) on any failure — never aborts a spawn.
    fn write_status_plugin(agent_id: &str) -> Option<PathBuf> {
        let dir = Self::status_config_dir(agent_id)?;
        let plugin_dir = dir.join("plugin");
        std::fs::create_dir_all(&plugin_dir).ok()?;
        std::fs::write(plugin_dir.join("capilot-status.js"), STATUS_PLUGIN).ok()?;
        Some(dir)
    }

    /// Remove a session's status-plugin config dir (session delete / close).
    /// No-op when already gone or the cache root is unresolvable.
    pub fn remove_status_plugin(agent_id: &str) {
        if let Some(dir) = Self::status_config_dir(agent_id) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    fn check_available() -> bool {
        crate::agent_runtime::adapter::cli_available("opencode")
    }

    fn model_state_path() -> Option<std::path::PathBuf> {
        if let Some(state_home) = std::env::var_os("XDG_STATE_HOME") {
            return Some(std::path::PathBuf::from(state_home).join("opencode/model.json"));
        }
        crate::persistence::user_home()
            .ok()
            .map(|home| home.join(".local/state/opencode/model.json"))
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

    /// Current variant for a model (`provider/model`) from OpenCode's own
    /// `model.json` (`variant[model]`), which the TUI rewrites on every
    /// `variant_cycle` (Ctrl+T) / `variant_list` selection. `"default"` or an
    /// absent entry means the model's native default reasoning — return `None`
    /// so the Composer renders "Default" rather than a stale variant name.
    pub fn current_variant(model: &str) -> Option<String> {
        let value: Value =
            serde_json::from_slice(&std::fs::read(Self::model_state_path()?).ok()?).ok()?;
        let variant = value.get("variant")?.get(model)?.as_str()?;
        let trimmed = variant.trim();
        (!trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("default"))
            .then(|| trimmed.to_string())
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
        if let Some(output) = crate::agent_runtime::executable::run_cli(
            "opencode",
            &["models", "--verbose"],
            std::time::Duration::from_secs(8),
        ) {
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
        let Some(output) = crate::agent_runtime::executable::run_cli(
            "opencode",
            &["models"],
            std::time::Duration::from_secs(8),
        ) else {
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

    // ── Context-window usage ─────────────────────────────────────────────
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
        crate::persistence::user_home().ok().map(|home| home.join(".local/share/opencode"))
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

    /// Verify that a persisted provider session belongs to this cwd. The
    /// session id is authoritative; cwd is a defense against stale/corrupt
    /// metadata. Never substitute the newest session in the same directory.
    fn session_matches(conn: &Connection, cwd: &Path, session_id: &str) -> bool {
        conn.query_row(
            "SELECT 1 FROM session WHERE id = ?1 AND directory = ?2 LIMIT 1",
            params![session_id, cwd.to_string_lossy().to_string()],
            |_| Ok(()),
        )
        .is_ok()
    }

    fn sidecar_resume_key(agent_id: &str) -> Option<String> {
        let raw = std::fs::read_to_string(crate::persistence::status_file(agent_id)).ok()?;
        serde_json::from_str::<Value>(&raw)
            .ok()?
            .get("session_id")?
            .as_str()
            .filter(|id| id.starts_with("ses_"))
            .map(str::to_owned)
    }

    /// Legacy recovery for sessions created before the status plugin recorded
    /// `session_id`. OpenCode may defer session creation until the first prompt
    /// (observed 12.7s after the Agent row), so the old 2s/10s newest-by-cwd
    /// capture missed it. Recover only when exactly one cwd-matching session was
    /// created from 2s before through 30s after the Agent; ambiguity is safer as
    /// no data than cross-session data.
    fn legacy_session_key(
        conn: &Connection,
        cwd: &Path,
        created_at_ms: i64,
    ) -> Option<String> {
        let from = created_at_ms.saturating_sub(2_000);
        let to = created_at_ms.saturating_add(30_000);
        let mut stmt = conn
            .prepare(
                "SELECT id FROM session \
                 WHERE directory = ?1 AND time_created BETWEEN ?2 AND ?3 \
                 ORDER BY ABS(time_created - ?4) LIMIT 2",
            )
            .ok()?;
        let candidates: Vec<String> = stmt
            .query_map(
                params![cwd.to_string_lossy().to_string(), from, to, created_at_ms],
                |row| row.get(0),
            )
            .ok()?
            .flatten()
            .collect();
        (candidates.len() == 1).then(|| candidates[0].clone())
    }

    fn recover_session_key(agent_id: &str, cwd: &Path, created_at_ms: i64) -> Option<String> {
        let conn = Self::open_db()?;
        if let Some(key) = Self::sidecar_resume_key(agent_id) {
            if Self::session_matches(&conn, cwd, &key) {
                return Some(key);
            }
        }
        Self::legacy_session_key(&conn, cwd, created_at_ms)
    }

    /// Current active-context estimate: the latest `step-finish` part's
    /// `tokens.total` (input + output + reasoning + cache read/write — a single
    /// snapshot, never accumulated across parts).
    fn latest_step_finish_tokens(conn: &Connection, session_id: &str) -> Option<u64> {
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

    /// Session-cumulative cache stats from OpenCode's own aggregate columns.
    /// These are the authoritative lifetime counters and avoid reparsing or
    /// accidentally double-counting repeated `step-finish` parts. They are NOT
    /// used for current context occupancy, which remains the latest step.
    fn session_cache_stats(conn: &Connection, cwd: &Path, session_id: &str) -> Option<(u64, u64)> {
        let (input, read, write): (i64, i64, i64) = conn
            .query_row(
                "SELECT tokens_input, tokens_cache_read, tokens_cache_write \
                 FROM session WHERE id = ?1 AND directory = ?2",
                params![session_id, cwd.to_string_lossy().to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok()?;
        let input = u64::try_from(input).ok()?;
        let read = u64::try_from(read).ok()?;
        let write = u64::try_from(write).ok()?;
        let total = input.saturating_add(read).saturating_add(write);
        (total > 0).then_some((read, total))
    }

    /// Observed assistant model as `providerID/modelID` from the newest
    /// assistant message in the exact session. Authoritative over the draft
    /// selection (the doc's "observed assistant-model metadata updates the
    /// maximum").
    fn observed_model_id(conn: &Connection, session_id: &str) -> Option<String> {
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
            .or_else(|| crate::persistence::user_home().ok().map(|home| home.join(".cache")))?;
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
        let Some(output) = crate::agent_runtime::executable::run_cli(
            "opencode",
            &["models", "--verbose"],
            std::time::Duration::from_secs(8),
        ) else {
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
        let flush = |header: &mut Option<String>, block: &mut Vec<&str>, map: &mut HashMap<String, u64>| {
            if let (Some(id), Ok(value)) =
                (header.take(), serde_json::from_str::<Value>(&block.join("\n")))
            {
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
        // Live adjustment is the TUI's `variant_cycle` keybind (Ctrl+T), which
        // the Composer drives directly (see `Composer.tsx` `cycleOpenCodeVariant`).
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
        let mut env = vec![(
            "OPENCODE_TUI_CONFIG".into(),
            config.to_string_lossy().into_owned(),
        )];
        // Status-reporting plugin. OpenCode's only hook surface is the JS
        // plugin event bus; the plugin is loaded from a per-session config dir
        // via OPENCODE_CONFIG_DIR, which APPENDS to opencode's search path (the
        // user's global config keeps loading — verified on 1.18.16). The hook
        // env is injected into THIS PTY only; a failed plugin write degrades to
        // no hooks (the env vars alone are inert without the plugin).
        if let Some(status_config) = Self::write_status_plugin(&session.id) {
            env.push((
                "OPENCODE_CONFIG_DIR".into(),
                status_config.to_string_lossy().into_owned(),
            ));
        }
        env.push((HOOK_ENV_AGENT.to_string(), session.id.clone()));
        env.push((
            HOOK_ENV_DIR.to_string(),
            status_dir().to_string_lossy().into_owned(),
        ));
        Ok(env)
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
    fn capture_resume_key(&self, _cwd: &Path) -> Option<String> {
        // OpenCode can defer root-session creation until the first prompt, well
        // beyond the generic startup polling window. The per-process plugin's
        // observed session id is the only collision-safe capture source.
        None
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
        model: Option<&str>,
        resume_key: Option<&str>,
    ) -> Option<AgentUsage> {
        let conn = Self::open_db()?;
        let session_id = resume_key?;
        if !Self::session_matches(&conn, cwd, session_id) {
            return None;
        }
        let used = Self::latest_step_finish_tokens(&conn, session_id);
        // Observed assistant model wins over the draft selection (doc: use the
        // model attached to the assistant message when available).
        let model_id =
            Self::observed_model_id(&conn, session_id).or_else(|| model.map(str::to_owned));
        let max = model_id.as_deref().and_then(Self::catalog_limit_context);
        let (cache_hit, cache_total) =
            Self::session_cache_stats(&conn, cwd, session_id).unwrap_or((0, 0));
        if used.is_none() && max.is_none() && cache_total == 0 {
            return None;
        }
        Some(AgentUsage {
            context_window_used_tokens: used,
            context_window_max_tokens: max,
            cache_hit_tokens: (cache_total > 0).then_some(cache_hit),
            cache_total_input_tokens: (cache_total > 0).then_some(cache_total),
            actual_model: model_id,
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
    fn launch_env_injects_status_plugin_config_dir() {
        with_isolated_cache(|| {
            let adapter = OpenCodeAdapter::new();
            let env = adapter.launch_env(&session("ask", None)).unwrap();
            let get = |name: &str| {
                env.iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.clone())
            };
            // TUI keybinding config still wired (existing behavior preserved).
            assert!(get("OPENCODE_TUI_CONFIG").unwrap().ends_with(".json"));
            // Status plugin: the per-session config dir is injected and the
            // hook env points at a fresh temp sidecar dir.
            let config_dir = PathBuf::from(get("OPENCODE_CONFIG_DIR").unwrap());
            let plugin = config_dir.join("plugin/capilot-status.js");
            let source = std::fs::read_to_string(&plugin).expect("plugin written");
            assert!(source.contains("session.status"));
            assert!(source.contains("permission.asked"));
            assert!(source.contains("question.asked"));
            assert!(source.contains("awaiting_choice"));
            assert!(source.contains("payload?.sessionID"));
            assert!(source.contains("session_id"));
            assert!(source.contains("learnedSession"));
            assert!(source.contains("CAPILOT_AGENT_ID"));
            assert_eq!(get("CAPILOT_AGENT_ID").as_deref(), Some("test"));
            assert!(get("CAPILOT_STATUS_DIR").is_some());
            // Cleanup removes the whole per-session config dir.
            OpenCodeAdapter::remove_status_plugin("test");
            assert!(!config_dir.exists());
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
    fn reads_current_variant_from_model_state() {
        with_isolated_cache(|| {
            let model = "anthropic/claude-sonnet-5";
            // No model.json yet → no variant (default reasoning).
            assert_eq!(OpenCodeAdapter::current_variant(model), None);
            let state = std::path::PathBuf::from(std::env::var("HOME").unwrap())
                .join(".local/state/opencode");
            std::fs::create_dir_all(&state).unwrap();
            std::fs::write(
                state.join("model.json"),
                r#"{"recent":[{"providerID":"anthropic","modelID":"claude-sonnet-5"}],"variant":{"anthropic/claude-sonnet-5":"high","openai/gpt-5.4":"default"}}"#,
            )
            .unwrap();
            // A selected variant reads back; "default" / absent models read None.
            assert_eq!(
                OpenCodeAdapter::current_variant(model).as_deref(),
                Some("high")
            );
            assert_eq!(OpenCodeAdapter::current_variant("openai/gpt-5.4"), None);
            assert_eq!(OpenCodeAdapter::current_variant("openai/unknown"), None);
        });
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
        let models = OpenCodeAdapter::parse_verbose_models(
            output,
            Some("opencode/deepseek-v4-flash-free"),
        );
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
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                directory TEXT,
                time_created INTEGER,
                time_updated INTEGER,
                tokens_input INTEGER,
                tokens_cache_read INTEGER,
                tokens_cache_write INTEGER
             );
             CREATE TABLE part (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session
             (id, directory, time_created, time_updated, tokens_input, tokens_cache_read, tokens_cache_write)
             VALUES ('ses_1', '/tmp/project', 100, 500, 352, 161836, 20)",
            [],
        )
        .unwrap();
        // A newer session in the same cwd reproduces the old cross-session
        // bug. Exact helpers must keep reading ses_1.
        conn.execute(
            "INSERT INTO session
             (id, directory, time_created, time_updated, tokens_input, tokens_cache_read, tokens_cache_write)
             VALUES ('ses_other', '/tmp/project', 40000, 999, 999, 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part (id, session_id, time_created, data) VALUES ('prt_1', 'ses_1', 200, ?1)",
            params![r#"{"type":"step-finish","reason":"stop","tokens":{"total":161848,"input":252,"output":60,"reasoning":0,"cache":{"write":0,"read":161536}}}"#],
        )
        .unwrap();
        // A second step-finish part confirms active context still comes from
        // the newest exact-session snapshot. Cache lifetime totals come from
        // the provider-maintained session columns above.
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

        // Current active context is the step-finish total, NOT the cumulative
        // session columns.
        assert_eq!(
            OpenCodeAdapter::latest_step_finish_tokens(&conn, "ses_1"),
            Some(560)
        );
        // Session cumulative cache stats represented by the aggregate columns.
        // OpenCode accounting:
        // total prompt = input + cache.read + cache.write, hit = cache.read.
        // prt_1: 252 + 161536 = 161788 prompt / 161536 hit;
        // prt_2: 100 + 20 + 300 = 420 / 300.
        assert_eq!(
            OpenCodeAdapter::session_cache_stats(&conn, Path::new("/tmp/project"), "ses_1"),
            Some((161536 + 300, 161788 + 420))
        );
        assert_eq!(
            OpenCodeAdapter::observed_model_id(&conn, "ses_1").as_deref(),
            Some("opencode/deepseek-v4-flash-free")
        );
        assert!(OpenCodeAdapter::session_matches(
            &conn,
            Path::new("/tmp/project"),
            "ses_1"
        ));
        assert!(!OpenCodeAdapter::session_matches(
            &conn,
            Path::new("/tmp/other"),
            "ses_1"
        ));
        assert_eq!(
            OpenCodeAdapter::session_cache_stats(&conn, Path::new("/tmp/other"), "ses_1"),
            None
        );
        assert_eq!(
            OpenCodeAdapter::session_cache_stats(
                &conn,
                Path::new("/tmp/project"),
                "ses_other"
            ),
            Some((0, 999)),
            "a measured zero hit remains displayable data"
        );

        // Legacy recovery accepts one delayed root session but refuses an
        // ambiguous same-cwd launch window.
        assert_eq!(
            OpenCodeAdapter::legacy_session_key(&conn, Path::new("/tmp/project"), 80)
                .as_deref(),
            Some("ses_1")
        );
        conn.execute(
            "UPDATE session SET time_created = 105 WHERE id = 'ses_other'",
            [],
        )
        .unwrap();
        assert_eq!(
            OpenCodeAdapter::legacy_session_key(&conn, Path::new("/tmp/project"), 80),
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
