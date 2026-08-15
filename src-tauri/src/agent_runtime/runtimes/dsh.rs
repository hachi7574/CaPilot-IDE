use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, AgentUsage, ModelInfo, PermissionModeInfo,
    ThinkingOptionInfo,
};
use crate::agent_runtime::status_hooks::{HOOK_ENV_AGENT, HOOK_ENV_DIR};
use crate::persistence::status_dir;
use ruzstd::decoding::StreamingDecoder;
use serde::Deserialize;
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

/// Adapter for the dsh-TUI (`@deepseek-harness-tui/dsh-tui`) interactive
/// terminal on top of the DeepSeek Harness CLI (`dsh`). dsh is a Commander
/// launcher that parses only its own flags (`--profile <name>`, repeatable
/// `--patch <path>`) and passes everything after them verbatim to the booted
/// Cordis app; the TUI is a plugin mounted by the `dsh-tui` profile (the
/// successor to the old `cc-tui` profile / `dsh-tui` package).
///
/// The per-session injection seam is a `--patch` overlay that REPLACES the
/// `dsh-tui` config row (cordis patch semantics), so it must restate the whole
/// route: `provider` + `model` (a complete pair — an incomplete pair loses to
/// the persisted `/model` choice), the reasoning `effort`, and the
/// resume binding `sessionId`. dsh-tui has NO `--resume` argv parsing
/// (verified — resume attaches via the config row's `sessionId`), so resume is
/// driven purely by `DSH_CC_RESUME_SESSION` env (the new package keeps the
/// legacy `DSH_CC_*` variable names), which the patch's `!!js` expression
/// reads. Permission mode likewise has no argv seam — the bundle patch
/// (`cordis.patch.yml`) reads `DSH_PERMISSION_MODE` env — so the adapter maps
/// CaPilot modes to that env instead of args.
pub struct DshAdapter;

/// Result of one pass over a dsh session log: the last request's context
/// occupancy (input + cache-read, the live active-context estimate) plus the
/// session-cumulative cache-read and total-prompt token counts (the cache hit
/// rate numerator and denominator), and the observed request route's model.
struct DshUsage {
    last_used: Option<u64>,
    observed_model: Option<String>,
    cache_hit: u64,
    cache_total: u64,
}

/// One model entry parsed from `~/.dsh/settings.yaml`.
#[derive(Debug, Clone, Deserialize)]
struct SettingsModel {
    id: String,
    #[serde(default)]
    name: String,
}

/// One `llm-pi-ai.providers` entry: a provider id + its model table.
#[derive(Debug, Clone, Deserialize)]
struct SettingsProvider {
    provider: String,
    models: Vec<SettingsModel>,
}

/// The `agent-default-model` route preference from settings.yaml.
#[derive(Debug, Clone, Deserialize)]
struct SettingsDefaultModel {
    provider: String,
    model: String,
}

/// The settings.yaml model catalog as surfaced by `model_catalog_probe`.
/// All fields default so a probe that only carries part of the file still
/// merges cleanly with the built-in deepseek-official list.
#[derive(Debug, Clone, Default, Deserialize)]
struct ModelCatalogProbe {
    #[serde(default)]
    pi: Vec<SettingsProvider>,
    #[serde(default)]
    deepseek: Vec<SettingsModel>,
    #[serde(default)]
    default: Option<SettingsDefaultModel>,
}

impl DshAdapter {
    pub fn new() -> Self {
        Self
    }

    /// dsh config root: `$DSH_HOME` when set, else `~/.dsh`.
    fn dsh_home() -> Option<PathBuf> {
        if let Some(home) = std::env::var_os("DSH_HOME") {
            return Some(PathBuf::from(home));
        }
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".dsh"))
    }

    /// Session logs root: `$DSH_HOME/sessions/`. dsh writes its JSONL session
    /// store here (shared with the dsh web UI).
    fn sessions_dir() -> Option<PathBuf> {
        Self::dsh_home().map(|home| home.join("sessions"))
    }

    /// The dsh-tui profile dir (`~/.dsh/profiles/dsh-tui`), which must exist
    /// before `dsh --profile dsh-tui` can boot. Created by
    /// `dsh plugin --profile dsh-tui add @deepseek-harness-tui/dsh-tui`.
    fn dsh_tui_profile_dir() -> Option<PathBuf> {
        Self::dsh_home().map(|home| home.join("profiles").join("dsh-tui"))
    }

    /// DeepSeek's project-directory key for a cwd (mirrors
    /// `projectKey` in `@deepseek-ai/dsh-session-persistence-jsonl`):
    /// separators collapse to `-`, unsafe code units become `~XXXX` hex
    /// escapes, leading dashes are stripped, the slug truncates to 251 chars
    /// and is wrapped in `--…--`. The cwd's sessions live under
    /// `<sessions>/<key>/`.
    fn project_key(cwd: &Path) -> String {
        let mut readable = String::new();
        let mut separator_run = false;
        for c in cwd.to_string_lossy().chars() {
            if c == '/' || c == '\\' || c == ':' {
                if !separator_run {
                    readable.push('-');
                }
                separator_run = true;
            } else if c != '~' && (c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-') {
                readable.push(c);
                separator_run = false;
            } else {
                readable.push('~');
                readable.push_str(&format!("{:04X}", c as u32));
                separator_run = false;
            }
        }
        let slug = readable.trim_start_matches('-');
        let slug = if slug.is_empty() {
            "root"
        } else {
            &slug[..slug.len().min(251)]
        };
        format!("--{slug}--")
    }

    /// The per-cwd session dir: `<sessions>/--<project key>--/`.
    fn project_sessions_dir(cwd: &Path) -> Option<PathBuf> {
        Some(Self::sessions_dir()?.join(Self::project_key(cwd)))
    }

    /// Every `<session-dir>/session.jsonl[.zstd]` log under a project dir, with
    /// its mtime. dsh names the session dir by the encoded session id (the
    /// `session-<uuid>` strings observed under `--home-hachi-Project-CaPilot--`
    /// are literal session ids), so the subdir name is the resume key.
    fn visit_session_logs(project_dir: &Path) -> Vec<(SystemTime, PathBuf)> {
        let Ok(entries) = std::fs::read_dir(project_dir) else {
            return vec![];
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let log = if dir.join("session.jsonl.zstd").exists() {
                dir.join("session.jsonl.zstd")
            } else if dir.join("session.jsonl").exists() {
                dir.join("session.jsonl")
            } else {
                continue;
            };
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    out.push((modified, log));
                }
            }
        }
        out
    }

    /// The `session` header line's `cwd` (first JSONL record of a dsh log).
    /// Reads only a prefix — decoding the whole multi-MB log just for a resume
    /// scan is wasteful.
    fn session_header_cwd(log: &Path) -> Option<String> {
        let bytes = Self::read_zstd_prefix(log, 8192).unwrap_or_default();
        let content = String::from_utf8_lossy(&bytes);
        let line = content.lines().next()?.trim();
        let value: Value = serde_json::from_str(line).ok()?;
        value.get("cwd").and_then(Value::as_str).map(str::to_owned)
    }

    /// Newest session log under the cwd's project dir whose header `cwd`
    /// matches (guards a shared project dir where a session from a slightly
    /// different path landed in the same slug). `None` when the cwd has no
    /// session yet.
    fn newest_session_log(cwd: &Path) -> Option<PathBuf> {
        Self::newest_session_log_meta(cwd).map(|(log, _, _)| log)
    }

    /// Like `newest_session_log`, but also returns the log's mtime and size —
    /// the change fingerprint the status poll uses to decide whether an already
    /// decoded inference is still fresh (mtime + length catch an appended
    /// `turn/end` even where the filesystem rounds mtime to whole seconds).
    /// Exposed to lib.rs so the poll can check the fingerprint WITHOUT decoding
    /// the log, then only decode when the log actually changed.
    pub(crate) fn newest_session_log_meta(cwd: &Path) -> Option<(PathBuf, SystemTime, u64)> {
        let dir = Self::project_sessions_dir(cwd)?;
        let cwd = cwd.to_string_lossy();
        Self::visit_session_logs(&dir)
            .into_iter()
            .filter(|(_, log)| Self::session_header_cwd(log).is_none_or(|header| header == cwd))
            .max_by_key(|(modified, _)| *modified)
            .and_then(|(modified, log)| {
                let len = log.metadata().ok().map(|m| m.len())?;
                Some((log, modified, len))
            })
    }

    /// Decode the first `max` uncompressed bytes of a zstd file. Used to peek
    /// a session header without pulling the whole log.
    fn read_zstd_prefix(path: &Path, max: usize) -> Option<Vec<u8>> {
        let file = std::fs::File::open(path).ok()?;
        let decoder = StreamingDecoder::new(file).ok()?;
        let mut out = Vec::new();
        decoder.take(max as u64).read_to_end(&mut out).ok()?;
        Some(out)
    }

    /// Decode a whole zstd session log (dsh's default `session.jsonl.zstd`;
    /// the `session.jsonl` plaintext variant is read directly).
    fn read_session_log(path: &Path) -> Option<String> {
        if path.extension().and_then(|e| e.to_str()) == Some("zstd") {
            let file = std::fs::File::open(path).ok()?;
            let mut decoder = StreamingDecoder::new(file).ok()?;
            let mut content = String::new();
            decoder.read_to_string(&mut content).ok()?;
            Some(content)
        } else {
            std::fs::read_to_string(path).ok()
        }
    }

    /// One pass over a dsh session log's JSONL computing:
    ///  - `last_used`: the LAST meaningful `assistant/chunk` usage event's
    ///    `inputTokens + cacheReadTokens`. DeepSeek accounting splits input
    ///    into fresh (`inputTokens`) and cache-served (`cacheReadTokens`)
    ///    portions (verified against a real log: the two are separate, and
    ///    `cacheReadTokens` grows monotonically within a turn as context
    ///    accumulates), so their sum is the request's total context occupancy.
    ///    A trailing `{inputTokens:0, outputTokens:0}` reset chunk is skipped.
    ///  - session-cumulative cache stats across ALL usage events (the cache hit
    ///    rate numerator/denominator, matching the codex/claude adapters).
    ///  - `observed_model`: the route model from the last `request/header`.
    fn parse_usage_from_content(content: &str) -> DshUsage {
        let mut last_used = None;
        let mut observed_model = None;
        let mut cache_hit = 0u64;
        let mut cache_total = 0u64;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            match v.get("type").and_then(Value::as_str) {
                Some("request/header") => {
                    if let Some(model) = v
                        .pointer("/data/header/config/model")
                        .and_then(Value::as_str)
                    {
                        observed_model = Some(model.to_string());
                    }
                }
                Some("assistant/chunk")
                    if v.pointer("/data/chunk/type").and_then(Value::as_str) == Some("usage") =>
                {
                    let Some(usage) = v.pointer("/data/chunk/usage") else {
                        continue;
                    };
                    let input = usage
                        .get("inputTokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let read = usage
                        .get("cacheReadTokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if input + read > 0 {
                        last_used = Some(input + read);
                        cache_hit += read;
                        cache_total += input + read;
                    }
                }
                _ => {}
            }
        }
        DshUsage {
            last_used,
            observed_model,
            cache_hit,
            cache_total,
        }
    }

    /// Infer the dsh session's live status from one pass over its session JSONL
    /// (integration doc §4.8 方案 B — dsh has no shell hook system, so the
    /// adapter tails the session store instead). `turn/start` and any
    /// `assistant/chunk` mean the harness is mid-turn (`working`); `turn/end`
    /// means it went back `idle`. Last relevant event wins, so a log that ends
    /// on `turn/end` reads idle while a mid-turn read (the log is appended to
    /// as chunks stream) still reads working. Records that carry no turn signal
    /// (session/header, request/header, trailing usage resets) are skipped.
    /// Returns `"idle"` for a log with no turn events yet.
    fn infer_status_from_content(content: &str) -> &'static str {
        let mut status = "idle";
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            match v.get("type").and_then(Value::as_str) {
                Some("turn/start") => status = "working",
                Some("turn/end") => status = "idle",
                Some("assistant/chunk") => status = "working",
                _ => {}
            }
        }
        status
    }

    /// Confirmed dsh model context capacities (DeepSeek V4 catalog). The log's
    /// `request/header` records the bare model id, while the caller may pass a
    /// provider-qualified CaPilot id (`opencode-go/deepseek-v4-flash`), so any
    /// `provider/` prefix is stripped before matching. Unknown models → `None`;
    /// the max is never guessed from visible text.
    fn context_window_max(model: Option<&str>) -> Option<u64> {
        let bare = model.map(|m| m.rsplit_once('/').map(|(_, b)| b).unwrap_or(m));
        match bare {
            Some("deepseek-v4-flash") | Some("deepseek-v4-pro") => Some(1_000_000),
            _ => None,
        }
    }

    /// Effort for one CaPilot speed tier (dsh adapter levels: `off`/`high`/
    /// `max`). `auto` and unknown tiers omit the key so the profile default
    /// (`max`) applies.
    fn effort_for_speed(speed: &str) -> Option<&'static str> {
        match speed {
            "fast" => Some("off"),
            "mid" => Some("high"),
            "high" => Some("max"),
            _ => None,
        }
    }

    /// `DSH_PERMISSION_MODE` for one CaPilot mode (the value the bundle patch's
    /// `sandbox-policy` / `user-approval` rows read). Unknown modes stay on the
    /// safest option.
    fn dsh_permission_mode(mode: &str) -> &'static str {
        match mode {
            "auto" => "workspace-write",
            "yolo" => "danger-full-access",
            _ => "read-only",
        }
    }

    /// The TUI's default reasoning effort for an un-pinned (`auto`) session.
    /// dsh boots with `state.reasoningEffort = options.effort ?? readEffortPref()`,
    /// where `readEffortPref` reads `~/.dsh-cc/effort.json`; a missing/invalid
    /// file falls back to the connection default (`deepseek-official` → `high`).
    /// The composer's Shift+Tab cycle math assumes this position, so it is
    /// surfaced here both for the `auto` option's description and so the two
    /// stay in sync.
    fn dsh_default_effort() -> &'static str {
        let Some(home) = std::env::var_os("HOME") else {
            return "high";
        };
        let path = PathBuf::from(home).join(".dsh-cc").join("effort.json");
        let Ok(content) = std::fs::read_to_string(path) else {
            return "high";
        };
        let parsed = serde_json::from_str::<Value>(&content).ok();
        let effort = parsed
            .as_ref()
            .and_then(|v| v.get("effort").and_then(Value::as_str));
        match effort {
            Some("off") => "off",
            Some("max") => "max",
            _ => "high",
        }
    }

    /// Split a CaPilot model id into `(provider, model)` for the patch overlay.
    /// Provider-qualified ids (`opencode-go/deepseek-v4-flash`) pass through;
    /// a bare legacy id (stored before the multi-provider catalog, or the
    /// `None` → flash fallback) routes via `deepseek-official`, preserving the
    /// pre-catalog behavior for existing sessions.
    fn split_model_id(model: &str) -> (String, String) {
        match model.rsplit_once('/') {
            Some((provider, bare)) if !provider.is_empty() && !bare.is_empty() => {
                (provider.to_string(), bare.to_string())
            }
            _ => ("deepseek-official".to_string(), model.to_string()),
        }
    }

    /// Per-session `--patch` overlay path (`$XDG_CACHE_HOME/capilot-ide/dsh/
    /// <safe-id>.patch.yml`, `~/.cache` fallback). The overlay replaces the
    /// `dsh-tui` config row for THIS spawn only — the user's global
    /// `~/.dsh/profiles/dsh-tui/cordis.yml` and `~/.dsh-cc/model.json` are never
    /// touched.
    fn patch_path(agent_id: &str) -> Option<PathBuf> {
        let cache_root = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
        let safe_id: String = agent_id
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
            .collect();
        if safe_id.is_empty() {
            return None;
        }
        Some(
            cache_root
                .join("capilot-ide/dsh")
                .join(format!("{safe_id}.patch.yml")),
        )
    }

    /// Write the per-session patch overlay. The `sessionId` binding is restated
    /// unconditionally: replacing the whole `dsh-tui` config row would otherwise
    /// DROP the resume seam (dsh-tui reads resume only from this config key,
    /// which is fed by `DSH_CC_RESUME_SESSION` env). Best-effort — a failed
    /// write degrades to no model/effort pin, never an abort.
    fn write_patch(session: &AgentSession) -> Result<PathBuf, String> {
        let path = Self::patch_path(&session.id)
            .ok_or_else(|| "Cannot resolve a cache directory for dsh patch".to_string())?;
        let parent = path
            .parent()
            .ok_or_else(|| "Invalid dsh patch path".to_string())?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Failed to create dsh patch directory: {error}"))?;
        let model = session
            .model
            .clone()
            .unwrap_or_else(|| "deepseek-v4-flash".to_string());
        let (provider, model_name) = Self::split_model_id(&model);
        let mut config = format!(
            "- id: dsh-tui\n  config:\n    provider: {provider}\n    model: {model_name}\n"
        );
        // Reasoning effort. The deepseek-official route guarantees off/high/max
        // (the `dsh-tui` config `effort` also wins over the persisted
        // Shift+Tab choice in ~/.dsh-cc/effort.json), so pin the CaPilot speed.
        // pi-ai providers (e.g. opencode-go) may declare no `reasoning`
        // metadata — dsh then only offers "off" and resolveReasoningLevel would
        // throw UNSUPPORTED_REASONING_EFFORT for anything else. Pin `effort: off`
        // there so the machine's persisted effort.json (often high) can't leak
        // in and the status line shows the level actually applied.
        if provider == "deepseek-official" {
            if let Some(effort) = Self::effort_for_speed(&session.speed) {
                config.push_str(&format!("    effort: {effort}\n"));
            }
        } else {
            config.push_str("    effort: off\n");
        }
        config.push_str("    sessionId: !!js process.env.DSH_CC_RESUME_SESSION ?? undefined\n");
        std::fs::write(&path, config)
            .map_err(|error| format!("Failed to write dsh patch: {error}"))?;
        Ok(path)
    }

    /// Remove a session's patch overlay (session delete / close). No-op when
    /// already gone or the cache root is unresolvable.
    pub fn remove_session_patch(agent_id: &str) {
        if let Some(path) = Self::patch_path(agent_id) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Newest session created in the cwd's project dir within the spawn
    /// window. dsh writes its session log as soon as the agent resolves, so a
    /// freshly spawned TUI's session is the newest young file — and the subdir
    /// name is its session id (the `DSH_CC_RESUME_SESSION` value).
    fn detect_recent_resume_key(cwd: &Path) -> Option<String> {
        let dir = Self::project_sessions_dir(cwd)?;
        let now = SystemTime::now();
        let cwd = cwd.to_string_lossy();
        Self::visit_session_logs(&dir)
            .into_iter()
            .filter(|(modified, _)| {
                let age = now.duration_since(*modified).unwrap_or(Duration::ZERO);
                age <= Duration::from_secs(10)
            })
            .filter(|(_, log)| Self::session_header_cwd(log).is_none_or(|header| header == cwd))
            .max_by_key(|(modified, _)| *modified)
            .and_then(|(_, log)| {
                log.parent()
                    .and_then(|dir| dir.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
            })
    }

    /// The launcher resume marker (`~/.dsh-cc/resume.txt`). The TUI writes it
    /// when the USER exits via `/exit` — a convenience for manual `dsh-cc
    /// --resume` runs, NOT the session a fresh IDE spawn creates, so it is only
    /// a fallback.
    fn read_resume_txt() -> Option<String> {
        let home = std::env::var_os("HOME")?;
        let path = PathBuf::from(home).join(".dsh-cc").join("resume.txt");
        let content = std::fs::read_to_string(path).ok()?;
        let trimmed = content.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn check_available() -> bool {
        Command::new("dsh")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    /// Pre-flight probe: run `dsh --dump-config --profile dsh-tui` and resolve
    /// every plugin package the composed profile will load. Returns a Chinese
    /// diagnostic when a package is unresolvable — the "spawn → immediately
    /// clean-exit" failure, which dsh only surfaces at TUI boot as a fiber
    /// unload + `process.exit(0)` with no stderr. Returns `None` when the
    /// profile is healthy OR the probe itself is unreliable (dump/node
    /// unavailable): a flaky probe must never block a spawn — the fast-exit
    /// net in `build_on_exit` is the fallback.
    fn preflight_diagnostic() -> Option<String> {
        let profile = Self::dsh_tui_profile_dir()?;
        let names = Self::parse_dump_names(&Self::profile_dump()?);
        if names.is_empty() {
            return None;
        }
        let missing = Self::unresolvable(&profile, &names)?;
        if missing.is_empty() {
            return None;
        }
        Some(Self::format_missing(&missing))
    }

    /// Format a missing-package list into a user-facing diagnostic (≤3 shown,
    /// count suffix when truncated).
    fn format_missing(missing: &[String]) -> String {
        let shown = missing.iter().take(3).cloned().collect::<Vec<_>>().join("、");
        let extra = missing.len().saturating_sub(3);
        let tail = if extra > 0 {
            format!(" 等 {extra} 个")
        } else {
            String::new()
        };
        format!(
            "dsh 无法启动：dsh-tui 配置中有插件包无法加载（{shown}{tail}）。\n\
             请检查 ~/.dsh/cordis.patch.yml 等 profile 补丁，禁用未安装的插件，\n\
             或运行 `dsh --dump-config --profile dsh-tui` 核对插件列表。"
        )
    }

    /// Effective plugin list of the dsh-tui profile, rendered by dsh itself
    /// (~0.1s, no plugin mounting). Disabled entries are omitted by the
    /// composer, so this is exactly what the TUI boot will try to load.
    fn profile_dump() -> Option<String> {
        let output = Command::new("dsh")
            .args(["--dump-config", "--profile", "dsh-tui"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()
    }

    /// Extract the package specifier from each `name:` row in a dump. Handles
    /// both quoted (`name: '@scope/pkg'`) and bare (`name: dsh-tui`) values.
    fn parse_dump_names(dump: &str) -> Vec<String> {
        dump.lines()
            .filter_map(|line| {
                let value = line.trim_start().strip_prefix("name:")?.trim();
                let value = value.trim_matches(|c| c == '\'' || c == '"');
                if value.is_empty() {
                    None
                } else {
                    Some(value.to_string())
                }
            })
            .collect()
    }

    /// Which of `names` cannot be resolved from the profile directory. Delegates
    /// to Node's own `require.resolve` (the same chain the cordis loader walks,
    /// including the profile → dsh-install pnpm fallback) in a single subprocess
    /// instead of re-implementing module resolution in Rust. Two fallbacks cover
    /// the packages CJS resolution can't see: an `exports` map that is
    /// `import`-only (e.g. `dsh-tui`'s root, resolved via its `package.json`)
    /// and an ESM-only subpath export (`@…/dsh-tui/working-activity`, resolved
    /// by reading the root package.json's exports map and checking the target
    /// file exists). `None` when node is unavailable or the subprocess fails.
    fn unresolvable(profile: &Path, names: &[String]) -> Option<Vec<String>> {
        let script = r#"
const names = JSON.parse(process.argv[1]);
const base = process.argv[2];
const fs = require("fs");
const path = require("path");
const missing = [];
const esmSubpath = (n, pkgRoot) => {
  // ESM-only exports entry (import condition, no require/default): CJS
  // require.resolve can't see it, but the cordis loader's `import` can. Resolve
  // the package root's package.json, then confirm the subpath's import target
  // actually exists on disk.
  let pj;
  try { pj = require.resolve(pkgRoot + "/package.json", { paths: [base] }); } catch (_) { return false; }
  let exp;
  try { exp = JSON.parse(fs.readFileSync(pj, "utf8")).exports; } catch (_) { return false; }
  if (!exp || typeof exp !== "object") return false;
  const sub = "./" + n.slice(pkgRoot.length + 1);
  const entry = exp[sub];
  const target = entry && (typeof entry === "string" ? entry : (entry.import || entry.default));
  if (typeof target !== "string") return false;
  return fs.existsSync(path.resolve(path.dirname(pj), target));
};
for (const n of names) {
  let ok = false;
  try { require.resolve(n, { paths: [base] }); ok = true; } catch (_) {}
  if (!ok) {
    try { require.resolve(n + "/package.json", { paths: [base] }); ok = true; } catch (_) {}
  }
  if (!ok) {
    const pkgRoot = n.startsWith("@") ? n.split("/").slice(0, 2).join("/") : n.split("/")[0];
    if (pkgRoot !== n && esmSubpath(n, pkgRoot)) ok = true;
  }
  if (!ok) missing.push(n);
}
console.log(JSON.stringify(missing));
"#;
        let names_json = serde_json::to_string(names).ok()?;
        let output = Command::new("node")
            .arg("-e")
            .arg(script)
            .arg(&names_json)
            .arg(profile)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        serde_json::from_slice(&output.stdout).ok()
    }

    /// The `~/.dsh/settings.yaml` model catalog (llm-pi-ai providers + optional
    /// llm-deepseek overrides + the agent-default-model), read via a single
    /// `node -e` subprocess that reuses dsh's own YAML parser. `None` when the
    /// settings file is absent/unparseable or node/yaml is unavailable — the
    /// catalog then falls back to the built-in deepseek-official list.
    fn model_catalog_probe() -> Option<ModelCatalogProbe> {
        let profile = Self::dsh_tui_profile_dir()?;
        let settings_path = Self::dsh_home()?.join("settings.yaml");
        if !settings_path.exists() {
            return None;
        }
        let script = r#"
const base = process.argv[1];
const settingsPath = process.argv[2];
let YAML = null;
try { YAML = require(require.resolve("js-yaml", { paths: [base] })); } catch (_) {}
if (!YAML) { try { YAML = require(require.resolve("yaml", { paths: [base] })); } catch (_) {} }
const parse = YAML && (YAML.load || YAML.parse);
if (!parse) { console.log("null"); process.exit(0); }
const fs = require("fs");
let settings;
try { settings = parse(fs.readFileSync(settingsPath, "utf8")); }
catch (_) { console.log("null"); process.exit(0); }
const out = { pi: [], deepseek: [], default: null };
const pi = settings && settings["llm-pi-ai"] && settings["llm-pi-ai"].providers;
if (pi && typeof pi === "object") {
  for (const [provider, cfg] of Object.entries(pi)) {
    const list = [];
    for (const m of (cfg && cfg.models) || []) {
      if (!m || !m.id) continue;
      list.push({ id: String(m.id), name: m.name ? String(m.name) : String(m.id) });
    }
    if (list.length) out.pi.push({ provider, models: list });
  }
}
const ds = settings && settings["llm-deepseek"] && settings["llm-deepseek"].models;
if (Array.isArray(ds)) {
  out.deepseek = ds.filter((m) => m && m.id)
    .map((m) => ({ id: String(m.id), name: m.name ? String(m.name) : String(m.id) }));
}
const adm = settings && settings["agent-default-model"];
if (adm && adm.provider && adm.model) {
  out.default = { provider: String(adm.provider), model: String(adm.model) };
}
console.log(JSON.stringify(out));
"#;
        let output = Command::new("node")
            .arg("-e")
            .arg(script)
            .arg(&profile)
            .arg(&settings_path)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        serde_json::from_slice(&output.stdout).ok()
    }

    /// Assemble the provider-qualified ModelInfo list from the deepseek-official
    /// entries plus any llm-pi-ai providers, marking the user's
    /// `agent-default-model` (when present in the catalog) as the default.
    fn build_model_list(
        ds_models: &[(&str, &str)],
        probe: Option<&ModelCatalogProbe>,
    ) -> Vec<ModelInfo> {
        let mut models: Vec<ModelInfo> = ds_models
            .iter()
            .map(|(id, name)| ModelInfo {
                id: format!("deepseek-official/{id}"),
                name: name.to_string(),
                provider: "deepseek-official".into(),
                is_default: false,
                efforts: None,
            })
            .collect();
        if let Some(probe) = probe {
            for provider in &probe.pi {
                for m in &provider.models {
                    models.push(ModelInfo {
                        id: format!("{}/{}", provider.provider, m.id),
                        name: m.name.clone(),
                        provider: provider.provider.clone(),
                        is_default: false,
                        efforts: None,
                    });
                }
            }
        }
        // Default = the model the user's settings.yaml points at (what a bare
        // `dsh` TUI boots), else the first deepseek-official flash entry.
        let preferred = probe
            .and_then(|p| p.default.as_ref())
            .map(|d| format!("{}/{}", d.provider, d.model));
        let default_idx = preferred
            .as_ref()
            .and_then(|id| models.iter().position(|m| &m.id == id))
            .unwrap_or_else(|| {
                models
                    .iter()
                    .position(|m| m.id == "deepseek-official/deepseek-v4-flash")
                    .unwrap_or(0)
            });
        if let Some(m) = models.get_mut(default_idx) {
            m.is_default = true;
        }
        models
    }

    /// Status-sidecar fallback for the frontend's hook-status poll (dsh has no
    /// hook — integration doc §4.8 方案 B). Returns `(status, ts)` where status
    /// is `idle`/`working` inferred from the newest session log under `cwd` and
    /// `ts` is the log's last-modified epoch seconds (the turn boundary the
    /// inference reflects, used by the tab bar for change detection). `None`
    /// when the cwd has no session log yet (freshly spawned session).
    pub fn infer_status(&self, cwd: &Path) -> Option<(String, i64)> {
        let log = Self::newest_session_log(cwd)?;
        Self::infer_status_from_log(&log)
    }

    /// Infer + timestamp from an already-located log file. Split out of
    /// `infer_status` so lib.rs's status poll can reuse the log it already
    /// fingerprinted (cache check) instead of rescanning the session dir.
    pub(crate) fn infer_status_from_log(log: &Path) -> Option<(String, i64)> {
        let content = Self::read_session_log(log)?;
        let status = Self::infer_status_from_content(&content).to_string();
        let ts = log
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Some((status, ts))
    }
}

impl AgentRuntimeAdapter for DshAdapter {
    fn id(&self) -> &str {
        "dsh"
    }
    fn name(&self) -> &str {
        "DeepSeek dsh"
    }
    fn is_available(&self) -> bool {
        // `dsh --version` alone is not enough: the TUI boots from the dsh-tui
        // profile, which must have been created once
        // (`dsh plugin --profile dsh-tui add @deepseek-harness-tui/dsh-tui`).
        Self::check_available() && Self::dsh_tui_profile_dir().is_some_and(|dir| dir.exists())
    }
    fn is_authenticated(&self) -> bool {
        std::env::var_os("DEEPSEEK_API_KEY").is_some()
            || Self::dsh_home().is_some_and(|home| home.join(".credentials.yaml").exists())
    }
    fn preflight(&self) -> Option<String> {
        Self::preflight_diagnostic()
    }

    fn list_models(&self) -> Vec<ModelInfo> {
        // dsh has no model-list CLI subcommand. The deepseek-official catalog
        // entries are hardcoded (mirrors the claude adapter's hardcoded list),
        // optionally replaced by an `llm-deepseek.models` override from
        // settings.yaml; the user's `llm-pi-ai.providers` (settings.yaml) are
        // merged in so the composer shows the same union the dsh TUI's
        // ModelPicker lists. Ids are provider-qualified (`provider/model`) —
        // `deepseek-v4-flash` exists in both deepseek-official and the pi-ai
        // provider, so a bare id would make the composer's menu rows collide.
        const DEEPSEEK_FLASH: (&str, &str) = ("deepseek-v4-flash", "DeepSeek-V4-Flash");
        const DEEPSEEK_PRO: (&str, &str) = ("deepseek-v4-pro", "DeepSeek-V4-Pro");
        let probe = Self::model_catalog_probe();
        let ds_models: Vec<(&str, &str)> = match probe.as_ref() {
            Some(p) if !p.deepseek.is_empty() => p
                .deepseek
                .iter()
                .map(|m| (m.id.as_str(), m.name.as_str()))
                .collect(),
            _ => vec![DEEPSEEK_FLASH, DEEPSEEK_PRO],
        };
        Self::build_model_list(&ds_models, probe.as_ref())
    }

    fn list_permission_modes(&self) -> Vec<PermissionModeInfo> {
        // The three dsh `sandbox-policy` presets map 1:1 to CaPilot modes. A
        // running TUI switches via `/permission <preset>` (durable session-log
        // events); a fresh spawn defaults from DSH_PERMISSION_MODE.
        vec![
            PermissionModeInfo {
                id: "ask".into(),
                label: "read only".into(),
                description: "只读沙箱 + 工作区写入需确认（dsh 预设 read-only）".into(),
                requires_confirmation: false,
            },
            PermissionModeInfo {
                id: "auto".into(),
                label: "workspace".into(),
                description: "工作区写 + 需确认（dsh 预设 workspace-write）".into(),
                requires_confirmation: false,
            },
            PermissionModeInfo {
                id: "yolo".into(),
                label: "full access".into(),
                description: "全权限 + 免确认（dsh 预设 danger-full-access）".into(),
                requires_confirmation: true,
            },
        ]
    }

    fn list_thinking_options(&self) -> Vec<ThinkingOptionInfo> {
        // Labels mirror the dsh TUI's own effort vocabulary (off/high/max), so
        // the composer's ⚡ menu reads the same as the Shift+Tab cycle it
        // drives. `auto` (no effort pinned) sits at the dsh default.
        let default = Self::dsh_default_effort();
        let default_label = match default {
            "off" => "Off",
            "max" => "Max",
            _ => "High",
        };
        vec![
            ThinkingOptionInfo {
                id: "auto".into(),
                label: "Auto".into(),
                description: format!(
                    "使用 dsh 当前默认思考强度（{default_label}，读 ~/.dsh-cc/effort.json）"
                ),
            },
            ThinkingOptionInfo {
                id: "fast".into(),
                label: "Off".into(),
                description: "effort=off：不思考，响应最快".into(),
            },
            ThinkingOptionInfo {
                id: "mid".into(),
                label: "High".into(),
                description: "effort=high".into(),
            },
            ThinkingOptionInfo {
                id: "high".into(),
                label: "Max".into(),
                description: "effort=max：最强推理".into(),
            },
        ]
    }

    fn spawn_interactive(&self, session: &AgentSession) -> Result<(String, Vec<String>), String> {
        // The per-session `--patch` overlay pins the model/effort route and the
        // resume binding without touching the user's global profile. The patch
        // file is written under the app cache dir and removed on session delete.
        let patch = Self::write_patch(session)?;
        Ok((
            "dsh".to_string(),
            vec![
                "--profile".to_string(),
                "dsh-tui".to_string(),
                "--patch".to_string(),
                patch.to_string_lossy().into_owned(),
            ],
        ))
    }

    fn status_hook_args(&self, session: &AgentSession) -> Vec<String> {
        // The profile + patch overlay is CaPilot infrastructure that must
        // survive a user launch override (Settings → 已安装 → ⚙), which replaces
        // the adapter's arg list wholesale. Rewriting the patch here is
        // idempotent, so a cold call (e.g. after an override that never ran
        // spawn_interactive) still produces the file. Mirrors codex's `-p`
        // status profile re-append.
        match Self::write_patch(session) {
            Ok(patch) => vec![
                "--profile".to_string(),
                "dsh-tui".to_string(),
                "--patch".to_string(),
                patch.to_string_lossy().into_owned(),
            ],
            Err(_) => vec![],
        }
    }

    fn launch_env(&self, session: &AgentSession) -> Result<Vec<(String, String)>, String> {
        // NODE_ENV=production is mandatory: dsh-cc's React dev renderer
        // accumulates unbounded performance.measure() records and OOMs on long
        // sessions (dsh-cc.cmd sets it). Permission mode and the resume key are
        // injected as env (dsh-tui has no argv seams for them). The status
        // hook env is env-gated and inert without a hook consumer.
        let mut env = vec![
            ("NODE_ENV".to_string(), "production".to_string()),
            (
                "DSH_PERMISSION_MODE".to_string(),
                Self::dsh_permission_mode(&session.mode).to_string(),
            ),
            (HOOK_ENV_AGENT.to_string(), session.id.clone()),
            (
                HOOK_ENV_DIR.to_string(),
                status_dir().to_string_lossy().into_owned(),
            ),
        ];
        if let Some(key) = &session.resume_key {
            env.push(("DSH_CC_RESUME_SESSION".to_string(), key.clone()));
        }
        Ok(env)
    }

    fn resume_args(&self, _session: &AgentSession) -> Vec<String> {
        // dsh-tui resume is driven solely by `DSH_CC_RESUME_SESSION` env
        // (read by the dsh-tui config row's `sessionId` binding); there is no
        // resume argv to build.
        vec![]
    }

    fn supports_resume(&self) -> bool {
        true
    }

    fn capture_resume_key(&self, cwd: &Path) -> Option<String> {
        // Prefer the session this spawn actually created (newest young log in
        // the cwd's project dir); fall back to the launcher marker only when
        // nothing recent is found yet.
        Self::detect_recent_resume_key(cwd).or_else(Self::read_resume_txt)
    }

    fn context_usage(&self, cwd: &Path, model: Option<&str>) -> Option<AgentUsage> {
        let log = Self::newest_session_log(cwd)?;
        let content = Self::read_session_log(&log)?;
        let parsed = Self::parse_usage_from_content(&content);
        // The route the log actually records wins over the caller's model.
        let model_id = parsed.observed_model.as_deref().or(model);
        let max = Self::context_window_max(model_id);
        if parsed.last_used.is_none() && max.is_none() && parsed.cache_total == 0 {
            return None;
        }
        Some(AgentUsage {
            context_window_used_tokens: parsed.last_used,
            context_window_max_tokens: max,
            cache_hit_tokens: (parsed.cache_hit > 0).then_some(parsed.cache_hit),
            cache_total_input_tokens: (parsed.cache_total > 0).then_some(parsed.cache_total),
        })
    }

    fn speed_args(&self, _speed: &str) -> Vec<String> {
        // Effort is pinned via the `--patch` overlay, not argv.
        vec![]
    }

    fn mode_args(&self, _mode: &str) -> Vec<String> {
        // Permission mode is injected via `DSH_PERMISSION_MODE` env, not argv.
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that repoint `HOME`/`XDG_CACHE_HOME` so parallel runs
    /// don't observe each other's env (shared with lib.rs / claude / codex via
    /// agent_runtime).
    use crate::agent_runtime::ENV_LOCK;

    fn session(mode: &str, resume_key: Option<&str>) -> AgentSession {
        AgentSession {
            id: "test".into(),
            runtime: "dsh".into(),
            mode: mode.into(),
            speed: "high".into(),
            model: Some("deepseek-v4-flash".into()),
            cwd: "/tmp/project".into(),
            context_dir: "/tmp/project".into(),
            rows: 24,
            cols: 80,
            resume_key: resume_key.map(str::to_owned),
        }
    }

    /// Point HOME + XDG_CACHE_HOME at fresh temp dirs for the duration of `f`,
    /// so patch / status side effects never touch the developer's real cache
    /// dirs or `~/CaPilot`.
    fn with_isolated_env(f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev_home = std::env::var_os("HOME");
        let prev_xdg_cache = std::env::var_os("XDG_CACHE_HOME");
        let prev_dsh_home = std::env::var_os("DSH_HOME");
        let base = std::env::temp_dir().join(format!(
            "capilot_dsh_env_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(base.join("cache")).unwrap();
        std::fs::create_dir_all(base.join("home")).unwrap();
        std::env::set_var("HOME", base.join("home"));
        std::env::set_var("XDG_CACHE_HOME", base.join("cache"));
        std::env::remove_var("DSH_HOME");
        f();
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prev_xdg_cache {
            Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
        match prev_dsh_home {
            Some(v) => std::env::set_var("DSH_HOME", v),
            None => std::env::remove_var("DSH_HOME"),
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn project_key_matches_real_dsh_dir_encoding() {
        // The `--…--` project dir names are the cwd with separators collapsed
        // to `-`, unsafe units escaped, truncated to 251 chars, and wrapped.
        // Verified against real `~/.dsh/sessions/` entries and the
        // session-persistence-jsonl `projectKey` source.
        let cases = [
            ("/home/hachi", "--home-hachi--"),
            (
                "/home/hachi/Project/CaPilot",
                "--home-hachi-Project-CaPilot--",
            ),
            ("/home/x/my.proj", "--home-x-my.proj--"),
            // Space escapes to ~0020 (the `my dir` case).
            ("/home/x/my dir", "--home-x-my~0020dir--"),
        ];
        for (cwd, expected) in cases {
            assert_eq!(
                DshAdapter::project_key(Path::new(cwd)),
                expected,
                "cwd {cwd}"
            );
        }
    }

    #[test]
    fn builds_dsh_launch_args_and_resume_env() {
        with_isolated_env(|| {
            let adapter = DshAdapter::new();
            let (cmd, args) = adapter
                .spawn_interactive(&session("ask", Some("session-abc")))
                .unwrap();
            assert_eq!(cmd, "dsh");
            assert!(args.windows(2).any(|v| v == ["--profile", "dsh-tui"]));
            let patch_idx = args
                .windows(2)
                .position(|v| v == ["--patch", args.last().unwrap().as_str()]);
            assert!(patch_idx.is_some());
            let patch = PathBuf::from(&args[args.len() - 1]);
            assert!(patch.exists());
            let content = std::fs::read_to_string(&patch).unwrap();
            assert!(content.contains("model: deepseek-v4-flash"));
            assert!(content.contains("effort: max"));
            // The resume seam must survive the whole-row replacement.
            assert!(
                content.contains("sessionId: !!js process.env.DSH_CC_RESUME_SESSION ?? undefined")
            );
            // Launch env: mandatory production node + permission mapping +
            // resume key + status hook env.
            let env = adapter
                .launch_env(&session("ask", Some("session-abc")))
                .unwrap();
            let get = |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
            assert_eq!(get("NODE_ENV").as_deref(), Some("production"));
            assert_eq!(get("DSH_PERMISSION_MODE").as_deref(), Some("read-only"));
            assert_eq!(get("DSH_CC_RESUME_SESSION").as_deref(), Some("session-abc"));
            assert_eq!(get("CAPILOT_AGENT_ID").as_deref(), Some("test"));
            assert!(get("CAPILOT_STATUS_DIR").is_some());
            // A fresh spawn (no resume key) omits the resume env.
            let fresh_env = adapter.launch_env(&session("auto", None)).unwrap();
            assert!(!fresh_env.iter().any(|(k, _)| k == "DSH_CC_RESUME_SESSION"));
            // Cleanup removes the patch.
            DshAdapter::remove_session_patch("test");
            assert!(!patch.exists());
        });
    }

    #[test]
    fn write_patch_routes_provider_qualified_model() {
        with_isolated_env(|| {
            // A provider-qualified model (opencode-go) must route the patch via
            // that provider instead of the hardcoded deepseek-official.
            let mut s = session("auto", None);
            s.model = Some("opencode-go/deepseek-v4-flash".into());
            let patch = DshAdapter::write_patch(&s).unwrap();
            let content = std::fs::read_to_string(&patch).unwrap();
            assert!(content.contains("provider: opencode-go"));
            assert!(content.contains("model: deepseek-v4-flash"));
            assert!(!content.contains("provider: deepseek-official"));
            DshAdapter::remove_session_patch("test");
        });
    }

    #[test]
    fn write_patch_pins_effort_only_for_deepseek_official_route() {
        with_isolated_env(|| {
            // speed "high" maps to effort "max" on the deepseek-official route,
            // which guarantees off/high/max support.
            let mut ds = session("auto", None);
            ds.speed = "high".into();
            ds.model = Some("deepseek-official/deepseek-v4-pro".into());
            let content = std::fs::read_to_string(DshAdapter::write_patch(&ds).unwrap()).unwrap();
            assert!(content.contains("effort: max"));

            // pi-ai providers may declare no `reasoning` metadata for a model —
            // dsh then offers only "off" and resolveReasoningLevel throws
            // UNSUPPORTED_REASONING_EFFORT for anything else. Pin `effort: off`
            // (valid on every route) so the machine's ~/.dsh-cc/effort.json
            // (often high) can't leak in and mislabel the status line.
            let mut pi = session("auto", None);
            pi.speed = "high".into();
            pi.model = Some("opencode-go/deepseek-v4-flash".into());
            let content = std::fs::read_to_string(DshAdapter::write_patch(&pi).unwrap()).unwrap();
            assert!(content.contains("effort: off"));
            assert!(!content.contains("effort: max"));
            assert!(content.contains("provider: opencode-go"));
            DshAdapter::remove_session_patch("test");
        });
    }

    #[test]
    fn status_hook_args_reappends_profile_and_patch() {
        with_isolated_env(|| {
            // A user launch override replaces the adapter's arg list wholesale;
            // status_hook_args must re-append the profile + patch scaffolding so
            // the TUI still boots the dsh-tui profile with the model/effort/resume
            // overlay (mirror of codex's `-p` re-append).
            let args = DshAdapter::new().status_hook_args(&session("yolo", None));
            assert!(args.windows(2).any(|v| v == ["--profile", "dsh-tui"]));
            assert!(args.windows(2).any(|v| v == ["--patch", args[3].as_str()]));
            let patch = PathBuf::from(&args[3]);
            assert!(patch.exists());
        });
    }

    #[test]
    fn mode_and_speed_maps() {
        let adapter = DshAdapter::new();
        assert_eq!(adapter.mode_args("ask"), Vec::<String>::new());
        // Permission mode travels via env, not argv.
        assert_eq!(
            adapter.launch_env(&session("ask", None)).unwrap()[1],
            ("DSH_PERMISSION_MODE".to_string(), "read-only".to_string())
        );
        assert_eq!(DshAdapter::dsh_permission_mode("auto"), "workspace-write");
        assert_eq!(
            DshAdapter::dsh_permission_mode("yolo"),
            "danger-full-access"
        );
        // Effort mapping per the doc (§4.5): fast→off, mid→high, high→max,
        // auto→omitted.
        assert_eq!(DshAdapter::effort_for_speed("fast"), Some("off"));
        assert_eq!(DshAdapter::effort_for_speed("mid"), Some("high"));
        assert_eq!(DshAdapter::effort_for_speed("high"), Some("max"));
        assert_eq!(DshAdapter::effort_for_speed("auto"), None);
    }

    #[test]
    fn parses_dsh_session_log_for_context_usage() {
        // Synthetic log mirroring the real event shapes:
        //  - request/header carries the route model;
        //  - assistant/chunk usage events split fresh input and cache read
        //    (DeepSeek reports cacheReadTokens separately, and it grows within a
        //    turn as context accumulates);
        //  - a trailing `{inputTokens:0, outputTokens:0}` reset chunk is skipped.
        let content = concat!(
            "{\"type\":\"session\",\"id\":\"ses_1\",\"cwd\":\"/tmp/project\"}\n",
            "{\"type\":\"request/header\",\"data\":{\"header\":{\"config\":{\"provider\":\"deepseek-official\",\"model\":\"deepseek-v4-flash\"}}}}\n",
            "{\"type\":\"assistant/chunk\",\"data\":{\"chunk\":{\"type\":\"usage\",\"usage\":{\"inputTokens\":100,\"outputTokens\":20,\"cacheReadTokens\":1000}}}}\n",
            "{\"type\":\"assistant/chunk\",\"data\":{\"chunk\":{\"type\":\"usage\",\"usage\":{\"inputTokens\":50,\"outputTokens\":30,\"cacheReadTokens\":1500}}}}\n",
            "{\"type\":\"assistant/chunk\",\"data\":{\"chunk\":{\"type\":\"usage\",\"usage\":{\"inputTokens\":0,\"outputTokens\":0}}}}\n",
        );
        let usage = DshAdapter::parse_usage_from_content(content);
        // Last meaningful request: 50 fresh + 1500 cache-read = 1550.
        assert_eq!(usage.last_used, Some(1550));
        // Session-cumulative: 100+1000 + 50+1500 = 2650 prompt, 2500 hit.
        assert_eq!(usage.cache_hit, 1000 + 1500);
        assert_eq!(usage.cache_total, 100 + 1000 + 50 + 1500);
        assert_eq!(usage.observed_model.as_deref(), Some("deepseek-v4-flash"));
    }

    #[test]
    fn infers_idle_from_log_with_no_turn_events() {
        // Empty log and header-only logs carry no turn signal → idle.
        assert_eq!(DshAdapter::infer_status_from_content(""), "idle");
        let header_only = concat!(
            "{\"type\":\"session\",\"id\":\"ses_1\",\"cwd\":\"/tmp/project\"}\n",
            "{\"type\":\"request/header\",\"data\":{\"header\":{\"config\":{\"model\":\"deepseek-v4-flash\"}}}}\n",
        );
        assert_eq!(DshAdapter::infer_status_from_content(header_only), "idle");
    }

    #[test]
    fn infers_working_from_turn_start_and_chunks() {
        // turn/start alone marks the session working; streaming chunks keep it.
        let start = "{\"type\":\"turn/start\",\"data\":{}}\n";
        assert_eq!(DshAdapter::infer_status_from_content(start), "working");
        let streaming = concat!(
            "{\"type\":\"turn/start\",\"data\":{}}\n",
            "{\"type\":\"assistant/chunk\",\"data\":{\"chunk\":{\"type\":\"text\",\"text\":\"hi\"}}}\n",
            "{\"type\":\"assistant/chunk\",\"data\":{\"chunk\":{\"type\":\"usage\",\"usage\":{\"inputTokens\":7,\"cacheReadTokens\":90}}}}\n",
        );
        assert_eq!(DshAdapter::infer_status_from_content(streaming), "working");
    }

    #[test]
    fn infers_idle_after_turn_end_and_working_on_reopen() {
        // A completed turn reads idle; the next turn/start flips it back.
        let completed = concat!(
            "{\"type\":\"turn/start\",\"data\":{}}\n",
            "{\"type\":\"assistant/chunk\",\"data\":{\"chunk\":{\"type\":\"text\",\"text\":\"done\"}}}\n",
            "{\"type\":\"turn/end\",\"data\":{}}\n",
        );
        assert_eq!(DshAdapter::infer_status_from_content(completed), "idle");
        // Multi-turn log: the LAST turn's signal wins.
        let reopened = concat!(
            "{\"type\":\"turn/start\",\"data\":{}}\n",
            "{\"type\":\"turn/end\",\"data\":{}}\n",
            "{\"type\":\"turn/start\",\"data\":{}}\n",
        );
        assert_eq!(DshAdapter::infer_status_from_content(reopened), "working");
    }

    #[test]
    fn infer_status_reads_newest_log_and_reports_mtime() {
        with_isolated_env(|| {
            // A log written under the cwd's project dir must be found and its
            // status inferred; ts is non-zero (the file has an mtime).
            let dsh_home = PathBuf::from(std::env::var_os("HOME").unwrap()).join(".dsh");
            let project_dir = dsh_home
                .join("sessions")
                .join(DshAdapter::project_key(Path::new("/tmp/project")));
            let session_dir = project_dir.join("ses_status");
            std::fs::create_dir_all(&session_dir).unwrap();
            std::fs::write(
                session_dir.join("session.jsonl"),
                concat!(
                    "{\"type\":\"session\",\"id\":\"ses_status\",\"cwd\":\"/tmp/project\"}\n",
                    "{\"type\":\"turn/start\",\"data\":{}}\n",
                ),
            )
            .unwrap();
            let (status, ts) = DshAdapter::new()
                .infer_status(Path::new("/tmp/project"))
                .expect("log exists");
            assert_eq!(status, "working");
            assert!(ts > 0);
            // No log under an unrelated cwd → None.
            assert_eq!(
                DshAdapter::new().infer_status(Path::new("/tmp/elsewhere")),
                None
            );
        });
    }

    #[test]
    fn context_window_max_maps_known_models_only() {
        assert_eq!(
            DshAdapter::context_window_max(Some("deepseek-v4-flash")),
            Some(1_000_000)
        );
        assert_eq!(
            DshAdapter::context_window_max(Some("deepseek-v4-pro")),
            Some(1_000_000)
        );
        // Provider-qualified ids (the composer's catalog form) strip to the bare
        // model before matching.
        assert_eq!(
            DshAdapter::context_window_max(Some("opencode-go/deepseek-v4-flash")),
            Some(1_000_000)
        );
        assert_eq!(
            DshAdapter::context_window_max(Some("deepseek-official/deepseek-v4-pro")),
            Some(1_000_000)
        );
        assert_eq!(DshAdapter::context_window_max(Some("unknown-model")), None);
        assert_eq!(DshAdapter::context_window_max(None), None);
    }

    #[test]
    fn split_model_id_handles_qualified_and_legacy_bare_ids() {
        // Provider-qualified (the catalog form) passes through.
        assert_eq!(
            DshAdapter::split_model_id("opencode-go/deepseek-v4-flash"),
            ("opencode-go".to_string(), "deepseek-v4-flash".to_string())
        );
        assert_eq!(
            DshAdapter::split_model_id("deepseek-official/deepseek-v4-pro"),
            ("deepseek-official".to_string(), "deepseek-v4-pro".to_string())
        );
        // Bare legacy ids (stored before the multi-provider catalog) route via
        // deepseek-official, preserving existing sessions.
        assert_eq!(
            DshAdapter::split_model_id("deepseek-v4-flash"),
            ("deepseek-official".to_string(), "deepseek-v4-flash".to_string())
        );
        // A stray trailing slash must not produce an empty provider/model.
        assert_eq!(
            DshAdapter::split_model_id("opencode-go/"),
            ("deepseek-official".to_string(), "opencode-go/".to_string())
        );
    }

    #[test]
    fn dsh_default_effort_reads_effort_json_and_falls_back_to_high() {
        with_isolated_env(|| {
            // No effort.json → the deepseek connection default (high).
            assert_eq!(DshAdapter::dsh_default_effort(), "high");
            let home = PathBuf::from(std::env::var_os("HOME").unwrap());
            let cc_dir = home.join(".dsh-cc");
            std::fs::create_dir_all(&cc_dir).unwrap();
            std::fs::write(cc_dir.join("effort.json"), "{\"effort\":\"max\"}").unwrap();
            assert_eq!(DshAdapter::dsh_default_effort(), "max");
            std::fs::write(cc_dir.join("effort.json"), "{\"effort\":\"off\"}").unwrap();
            assert_eq!(DshAdapter::dsh_default_effort(), "off");
            // Invalid JSON → fall back.
            std::fs::write(cc_dir.join("effort.json"), "not json").unwrap();
            assert_eq!(DshAdapter::dsh_default_effort(), "high");
        });
    }

    #[test]
    fn list_models_falls_back_to_builtins_without_settings_and_qualifies_ids() {
        with_isolated_env(|| {
            // No settings.yaml / js-yaml in the isolated profile → the probe
            // returns None and the catalog is just the two deepseek-official
            // entries, with provider-qualified ids so the composer's menu keys
            // stay unique.
            let models = DshAdapter::new().list_models();
            assert_eq!(models.len(), 2);
            assert_eq!(models[0].id, "deepseek-official/deepseek-v4-flash");
            assert!(models[0].is_default);
            assert_eq!(models[1].id, "deepseek-official/deepseek-v4-pro");
            assert!(!models[1].is_default);
        });
    }

    #[test]
    fn build_model_list_merges_pi_ai_providers_and_marks_user_default() {
        // Pure assembly test (no node needed): a settings.yaml carrying an
        // opencode-go provider + an agent-default-model must produce the union
        // with the opencode-go model as default.
        let probe = ModelCatalogProbe {
            pi: vec![SettingsProvider {
                provider: "opencode-go".into(),
                models: vec![SettingsModel {
                    id: "deepseek-v4-flash".into(),
                    name: "DeepSeek V4 Flash".into(),
                }],
            }],
            deepseek: vec![],
            default: Some(SettingsDefaultModel {
                provider: "opencode-go".into(),
                model: "deepseek-v4-flash".into(),
            }),
        };
        let models = DshAdapter::build_model_list(
            &[("deepseek-v4-flash", "DeepSeek-V4-Flash"), ("deepseek-v4-pro", "DeepSeek-V4-Pro")],
            Some(&probe),
        );
        assert_eq!(models.len(), 3);
        // The deepseek-official flash keeps its id but loses the default to the
        // user's agent-default-model.
        assert_eq!(models[0].id, "deepseek-official/deepseek-v4-flash");
        assert!(!models[0].is_default);
        assert_eq!(models[1].id, "deepseek-official/deepseek-v4-pro");
        assert!(!models[1].is_default);
        assert_eq!(models[2].id, "opencode-go/deepseek-v4-flash");
        assert_eq!(models[2].provider, "opencode-go");
        assert!(models[2].is_default);
    }

    #[test]
    fn build_model_list_default_falls_back_to_deepseek_flash() {
        // No agent-default-model in settings.yaml → deepseek-official flash
        // stays the default.
        let probe = ModelCatalogProbe {
            pi: vec![SettingsProvider {
                provider: "opencode-go".into(),
                models: vec![SettingsModel {
                    id: "deepseek-v4-flash".into(),
                    name: "DeepSeek V4 Flash".into(),
                }],
            }],
            deepseek: vec![],
            default: None,
        };
        let models = DshAdapter::build_model_list(
            &[("deepseek-v4-flash", "DeepSeek-V4-Flash"), ("deepseek-v4-pro", "DeepSeek-V4-Pro")],
            Some(&probe),
        );
        assert!(models.iter().any(|m| m.id == "deepseek-official/deepseek-v4-flash" && m.is_default));
        assert!(models.iter().any(|m| m.id == "opencode-go/deepseek-v4-flash" && !m.is_default));
    }

    #[test]
    fn reads_real_zstd_session_log() {
        // A session log compressed with the real libzstd (produced by dsh —
        // ruzstd's own encoder could mask a symmetric decode bug). The bytes
        // are a two-line log: header + one usage chunk.
        let content = concat!(
            "{\"type\":\"session\",\"id\":\"ses_z\",\"cwd\":\"/tmp/project\"}\n",
            "{\"type\":\"assistant/chunk\",\"data\":{\"chunk\":{\"type\":\"usage\",\"usage\":{\"inputTokens\":7,\"cacheReadTokens\":90}}}}\n",
        );
        let dir = std::env::temp_dir().join(format!("capilot-dsh-zstd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("session.jsonl.zstd");
        let status = std::process::Command::new("zstd")
            .arg("-q")
            .arg("-f")
            .arg("-")
            .arg("-o")
            .arg(&log)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                child
                    .stdin
                    .as_mut()
                    .unwrap()
                    .write_all(content.as_bytes())?;
                drop(child.stdin.take());
                child.wait()
            })
            .map(|status| status.success())
            .unwrap_or(false);
        if !status {
            // No `zstd` binary in the test environment — skip rather than fail
            // (the ruzstd encoder roundtrip below still covers the decode path).
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let decoded = DshAdapter::read_session_log(&log).expect("zstd log decodes");
        assert!(decoded.contains("\"id\":\"ses_z\""));
        let parsed = DshAdapter::parse_usage_from_content(&decoded);
        assert_eq!(parsed.last_used, Some(97));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn zstd_roundtrip_via_ruzstd_encoder() {
        // Compress with ruzstd's own encoder and decode through the adapter —
        // exercises the StreamingDecoder path even where the `zstd` CLI is
        // absent.
        let content = concat!(
            "{\"type\":\"session\",\"id\":\"ses_r\",\"cwd\":\"/tmp/project\"}\n",
            "{\"type\":\"request/header\",\"data\":{\"header\":{\"config\":{\"model\":\"deepseek-v4-pro\"}}}}\n",
            "{\"type\":\"assistant/chunk\",\"data\":{\"chunk\":{\"type\":\"usage\",\"usage\":{\"inputTokens\":10,\"cacheReadTokens\":20}}}}\n",
        );
        let bytes = ruzstd::encoding::compress_to_vec(
            std::io::Cursor::new(content.as_bytes()),
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let dir = std::env::temp_dir().join(format!("capilot-dsh-ruz-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("session.jsonl.zstd");
        std::fs::write(&log, &bytes).unwrap();
        let decoded = DshAdapter::read_session_log(&log).unwrap();
        let parsed = DshAdapter::parse_usage_from_content(&decoded);
        assert_eq!(parsed.last_used, Some(30));
        assert_eq!(parsed.observed_model.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(parsed.cache_hit, 20);
        assert_eq!(parsed.cache_total, 30);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn capture_resume_key_reads_launcher_marker_and_recent_session() {
        with_isolated_env(|| {
            // Missing marker → None.
            assert_eq!(DshAdapter::read_resume_txt(), None);
            let home = PathBuf::from(std::env::var_os("HOME").unwrap());
            let cc_dir = home.join(".dsh-cc");
            std::fs::create_dir_all(&cc_dir).unwrap();
            std::fs::write(cc_dir.join("resume.txt"), "  ses_marker  \n").unwrap();
            assert_eq!(DshAdapter::read_resume_txt().as_deref(), Some("ses_marker"));

            // A freshly written session log under the cwd's project dir wins
            // over the marker (capture must find THIS spawn's session).
            let dsh_home = home.join(".dsh");
            let project_dir = dsh_home
                .join("sessions")
                .join(DshAdapter::project_key(Path::new("/tmp/project")));
            let session_dir = project_dir.join("ses_recent");
            std::fs::create_dir_all(&session_dir).unwrap();
            std::fs::write(
                session_dir.join("session.jsonl"),
                "{\"type\":\"session\",\"id\":\"ses_recent\",\"cwd\":\"/tmp/project\"}\n",
            )
            .unwrap();
            assert_eq!(
                DshAdapter::new()
                    .capture_resume_key(Path::new("/tmp/project"))
                    .as_deref(),
                Some("ses_recent")
            );
        });
    }

    #[test]
    fn parse_dump_names_handles_quoted_and_bare_specifiers() {
        let dump = "# == @deepseek-ai/dsh-base\n\
- id: timer\n\
  name: '@deepseek-ai/cordis-plugin-timer'\n\
- id: dsh-tui\n\
  name: dsh-tui\n\
  config:\n\
    root:\n      - .\n\
- id: hmr\n\
  name: '@deepseek-ai/cordis-plugin-hmr'\n";
        assert_eq!(
            DshAdapter::parse_dump_names(dump),
            vec![
                "@deepseek-ai/cordis-plugin-timer".to_string(),
                "dsh-tui".to_string(),
                "@deepseek-ai/cordis-plugin-hmr".to_string(),
            ]
        );
    }

    #[test]
    fn parse_dump_names_skips_non_name_lines_and_empty_values() {
        let dump = "- id: a\n\
  name: ''\n\
# == bundle\n\
  name: \"@scope/pkg\"\n\
  extra: value\n\
  name:   'plain'\n";
        assert_eq!(
            DshAdapter::parse_dump_names(dump),
            vec!["@scope/pkg".to_string(), "plain".to_string()]
        );
    }

    #[test]
    fn format_missing_lists_up_to_three_and_counts_rest() {
        let one = vec!["@a/pkg".to_string()];
        let formatted = DshAdapter::format_missing(&one);
        assert!(formatted.contains("（@a/pkg）。"));
        assert!(!formatted.contains("等 1 个"));
        assert!(formatted.contains("dsh 无法启动"));

        let many = vec![
            "@a/one".to_string(),
            "@b/two".to_string(),
            "@c/three".to_string(),
            "@d/four".to_string(),
            "@e/five".to_string(),
        ];
        let formatted_many = DshAdapter::format_missing(&many);
        assert!(formatted_many.contains("@a/one、@b/two、@c/three 等 2 个"));
        assert!(!formatted_many.contains("@d/four"));
    }
}
