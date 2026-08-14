pub mod agent_provider;
pub mod agent_runtime;
pub mod bridge;
pub mod daemon;
mod git_gate;
pub mod lifecycle_journal;
pub mod output_hub;
pub mod persistence;
mod resource;
pub mod session_store;
mod slash;
pub mod terminal;
mod usage;

use agent_provider::manager::{NewAgentRequest, ResumeAgentRequest};
use agent_provider::types::{AgentPrompt, ConfigValue, TurnId};
use agent_runtime::adapter::{
    AgentError, AgentInfo, AgentRuntimeAdapter, AgentSession, AgentUsage,
};
use agent_runtime::runtimes::{get_adapter, known_runtimes};
use persistence::{
    agent_dir, ensure_project, read_agent_meta, write_agent_meta, AgentMeta, AgentSessionRecord,
    Persistence, DEFAULT_PROJECT,
};
use serde::{Deserialize, Serialize};
use session_store::SESSION_END_MODE_KEY;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::ipc::Channel;
use tauri::{Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use terminal::pty_core::OnExit;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Validate a project name: reject absolute paths and `..`/`.` traversal so a
/// project can't escape the workspace root (persistence::project_dir joins it).
fn sanitize_project(project: &str) -> Result<(), String> {
    if project.is_empty() {
        return Err("Project name cannot be empty".to_string());
    }
    let p = std::path::Path::new(project);
    use std::path::Component;
    if p.is_absolute()
        || p.components().any(|c| {
            matches!(
                c,
                Component::ParentDir
                    | Component::CurDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err("Invalid project name".to_string());
    }
    Ok(())
}

// ── Agent commands ──────────────────────────────────────────────

/// Settings KV key: per-runtime launch overrides persisted from
/// Settings → 已安装 → ⚙. Value is a JSON map of `runtime_id` → RuntimeOverride;
/// empty `command`/`args` leave the adapter's default launch untouched.
const RUNTIME_OVERRIDES_KEY: &str = "runtime_overrides";

/// Settings KV keys for the rate-limit usage readout (Settings → 已安装 → ⚙ →
/// 用量统计). `usage_enabled` is `{"<runtime>": bool}`; `usage_config` is
/// `{"<runtime>": usage::UsageConfig}`. Both are allow-listed in `setting_set`.
const USAGE_ENABLED_KEY: &str = usage::USAGE_ENABLED_KEY;
const USAGE_CONFIG_KEY: &str = usage::USAGE_CONFIG_KEY;

/// Settings KV key for the right sidebar's todo-tag list (概览 panel). Value is
/// a JSON array of `TodoTag` objects (see `ui/state/store.ts`); written by the
/// frontend on every tag mutation.
const TODOS_KEY: &str = "todos";

/// User-configured launch override for one runtime (the adapter's defaults win
/// unless a field is set to a non-empty string).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RuntimeOverride {
    /// Replaces the adapter's executable (binary path or name).
    #[serde(default)]
    command: Option<String>,
    /// Replaces the adapter's argument list (whitespace-separated).
    #[serde(default)]
    args: Option<String>,
}

/// Daemon-side `lastUsage` cache: agent id → (last computed context-window
/// usage, computed-at). Lives ONLY in daemon memory (never persisted) — a
/// daemon restart clears it, while a model switch / reconnect preserves it.
/// Mirrors the context-window contract: the value is the provider's current
/// active-context estimate, not cumulative token spend.
pub struct ContextUsageCache {
    inner: Mutex<HashMap<String, (AgentUsage, Instant)>>,
}

/// Reuse a computed sample for this long before the next `agent_context_usage`
/// recomputes it. Short on purpose: it only dedups the burst when the frontend's
/// immediate on-open poll and the 3s scheduled tick land in the same window —
/// it does not cap how often the meter refreshes (the poll cadence already
/// bounds freshness).
const CONTEXT_USAGE_TTL: Duration = Duration::from_millis(800);

impl ContextUsageCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

/// Payload emitted on `agent://exited` — a session's process ended naturally
/// and the record was kept (marked done).
#[derive(Clone, Serialize)]
struct AgentExited {
    id: String,
    exit_code: i32,
}

/// Payload emitted on `agent://removed` — the record was deleted by the
/// "session ended → delete" setting.
#[derive(Clone, Serialize)]
struct AgentRemoved {
    id: String,
}

/// Wrap the natural-exit bookkeeping into an `OnExit` for `PtyBridge.spawn`.
/// Fired only on natural exit (EOF / read error); intentional kills never reach
/// it.
///
/// The persistence half lives in `SessionStore` / `Persistence::apply_natural_exit`
/// (shared with the daemon, §6.1); this closure only layers the Tauri `emit` on
/// top, so "persist the event" and "tell the WebView" stay separate.
fn build_on_exit(persistence: Arc<Persistence>, app: tauri::AppHandle) -> OnExit {
    Arc::new(move |agent_id, exit_code| {
        // Poisoned lock / read error → defaults to "keep" (see SessionStore), so
        // a session is never silently dropped because of a transient DB failure.
        let outcome = persistence.apply_natural_exit(&agent_id, exit_code);
        if outcome.deleted {
            let _ = app.emit("agent://removed", AgentRemoved { id: agent_id });
        } else {
            let _ = app.emit(
                "agent://exited",
                AgentExited {
                    id: agent_id,
                    exit_code,
                },
            );
        }
    })
}

/// Shared spawn path used by `agent_spawn` (new) and `agent_resume` (restored).
#[allow(clippy::too_many_arguments)]
fn build_and_spawn(
    bridge: &Arc<bridge::PtyBridge>,
    persistence: &Arc<Persistence>,
    app: &tauri::AppHandle,
    id: &str,
    project: &str,
    workspace_id: Option<String>,
    runtime: &str,
    resume: bool,
    resume_key: Option<String>,
    preserved_title: Option<String>,
    model: Option<String>,
    speed: &str,
    mode: &str,
    cwd: PathBuf,
    on_data: Channel<Vec<u8>>,
) -> Result<AgentInfo, String> {
    let workspace_id =
        workspace_id.unwrap_or_else(|| format!("wks_{}", uuid::Uuid::new_v4().simple()));
    let adapter = get_adapter(runtime);
    if !adapter.is_available() {
        return Err(format!("Runtime '{}' is not available", runtime));
    }

    // Provider adapters own the valid choices. This also normalizes a legacy
    // value when a session switches to a harness with a different capability
    // set. Shell runtimes expose no choices and keep the stored value unused.
    let permission_modes = adapter.list_permission_modes();
    let normalized_mode =
        if permission_modes.is_empty() || permission_modes.iter().any(|choice| choice.id == mode) {
            mode.to_string()
        } else {
            permission_modes[0].id.clone()
        };
    let thinking_options = adapter.list_thinking_options();
    let normalized_speed = if thinking_options.is_empty()
        || thinking_options.iter().any(|choice| choice.id == speed)
    {
        speed.to_string()
    } else {
        thinking_options
            .iter()
            .find(|choice| choice.id == "auto")
            .unwrap_or(&thinking_options[0])
            .id
            .clone()
    };
    let models = adapter.list_models();
    let normalized_model = model
        .filter(|selected| models.iter().any(|item| item.id == *selected))
        .or_else(|| {
            models
                .iter()
                .find(|item| item.is_default)
                .map(|item| item.id.clone())
        });

    let session = AgentSession {
        id: id.to_string(),
        runtime: runtime.to_string(),
        mode: normalized_mode,
        speed: normalized_speed,
        model: normalized_model,
        cwd: cwd.clone(),
        context_dir: cwd.clone(),
        rows: 24,
        cols: 80,
        resume_key: resume_key.clone(),
    };

    let (cmd, mut args) = apply_launch_overrides(
        adapter.as_ref(),
        &session,
        &load_runtime_overrides(persistence),
        adapter
            .spawn_interactive(&session)
            .map_err(|e| format!("Failed to build command: {}", e))?,
    );
    // Resume an existing conversation in the same context dir — only when the
    // caller asked for a resume (restored session / runtime switch). A brand-new
    // spawn stays fresh so it can never hijack the newest session in a shared
    // cwd (e.g. two claude terminals in one custom-rooted project).
    let resume_args = if resume {
        adapter.resume_args(&session)
    } else {
        vec![]
    };
    let detected_key = (!resume_args.is_empty())
        .then(|| resume_args.last().cloned().filter(|s| s != "--resume"))
        .flatten();
    if !resume_args.is_empty() {
        args.extend(resume_args);
    }

    let launch_env = adapter.launch_env(&session)?;
    let mut info = bridge
        .spawn(
            id,
            &cmd,
            &args,
            &cwd,
            24,
            80,
            &launch_env,
            on_data,
            Some(build_on_exit(persistence.clone(), app.clone())),
        )
        .map_err(|e| match e {
            AgentError::CapacityReached { limit } => {
                format!("会话数已达上限 ({limit})，请先关闭部分终端")
            }
            other => other.to_string(),
        })?;
    info.runtime = runtime.to_string();
    info.workspace_id = Some(workspace_id.clone());
    info.project = Some(project.to_string());
    info.mode = session.mode.clone();
    info.speed = session.speed.clone();
    info.model = session.model.clone();
    // Resuming an existing session must keep its persisted title. Previously
    // this shared spawn path generated a fresh cat-breed title on every resume,
    // so the first click after reopening the IDE appeared to rename the session.
    info.title = preserved_title.unwrap_or_else(|| {
        // Every new terminal receives a unique cat-breed title. Persisted
        // titles are excluded across IDE restarts.
        let existing_titles = persistence
            .db()
            .lock()
            .ok()
            .and_then(|db| db.list_all().ok())
            .unwrap_or_default()
            .into_iter()
            .map(|record| record.title)
            .collect();
        agent_runtime::cat_breeds::next_breed_excluding(&existing_titles).to_string()
    });

    // Persist metadata + session (best-effort; PTY already running).
    let now = now_ms();
    // The stored key is the provider session to continue on the next launch.
    // Fresh spawns have no session yet — agent_spawn's background capture fills
    // it in shortly after; resume carries the explicit key or the detected one.
    let persisted_key = session.resume_key.clone().or_else(|| detected_key.clone());
    let meta = AgentMeta {
        id: id.to_string(),
        workspace_id: Some(workspace_id.clone()),
        runtime: runtime.to_string(),
        resume_key: persisted_key.clone(),
        backend_kind: persistence::BACKEND_KIND_LEGACY_PTY.to_string(),
        status: "running".to_string(),
        cwd: cwd.clone(),
        title: info.title.clone(),
        mode: session.mode.clone(),
        speed: session.speed.clone(),
        model: session.model.clone(),
        updated_at: now,
    };
    // Metadata always lives under the workspace layout
    // (`~/CaPilot/workspaces/<project>/agents/<id>`) — even for custom-rooted
    // projects — so the tree / session restore can find it without touching the
    // project root.
    if let Err(e) = write_agent_meta(project, &meta) {
        log::warn!("failed to write .agent-meta.json for {id}: {e}");
    }
    let record = AgentSessionRecord {
        id: id.to_string(),
        workspace_id: Some(workspace_id),
        project: project.to_string(),
        runtime: runtime.to_string(),
        resume_key: persisted_key,
        backend_kind: persistence::BACKEND_KIND_LEGACY_PTY.to_string(),
        cwd: cwd.clone(),
        title: info.title.clone(),
        status: "running".to_string(),
        mode: session.mode.clone(),
        speed: session.speed.clone(),
        model: session.model.clone(),
        created_at: now,
        updated_at: now,
    };
    if let Ok(db) = persistence.db().lock() {
        if let Err(e) = db.insert(&record) {
            log::warn!("failed to persist session {id}: {e}");
        }
    }

    Ok(info)
}

/// Apply user-configured launch overrides (Settings → 已安装 → ⚙) to the
/// adapter's spawn argv. A non-empty command replaces the adapter's executable;
/// a non-empty args string replaces the adapter's argument list wholesale.
/// Permission/speed flags are re-appended on top of the user's args so the
/// legacy spawn keeps its configured policy (the status-hook injection was
/// retired in Phase 5).
fn apply_launch_overrides(
    adapter: &dyn AgentRuntimeAdapter,
    session: &AgentSession,
    overrides: &HashMap<String, RuntimeOverride>,
    (mut cmd, mut args): (String, Vec<String>),
) -> (String, Vec<String>) {
    if let Some(ov) = overrides.get(session.runtime.as_str()) {
        if let Some(c) = ov
            .command
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            cmd = c.to_string();
        }
        if let Some(a) = ov.args.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            args = a.split_whitespace().map(|t| t.to_string()).collect();
            args.extend(adapter.mode_args(&session.mode));
            args.extend(adapter.speed_args(&session.speed));
        }
    }
    (cmd, args)
}

#[tauri::command]
async fn agent_spawn(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    persistence: tauri::State<'_, Arc<Persistence>>,
    app: tauri::AppHandle,
    runtime: String,
    project: String,
    resume_key: Option<String>,
    model: Option<String>,
    speed: Option<String>,
    mode: Option<String>,
    // Custom project root (git-cloned / local-folder project). When provided,
    // the agent's cwd lives under this root instead of `workspace_root()/name`.
    project_root: Option<String>,
    on_data: Channel<Vec<u8>>,
) -> Result<AgentInfo, String> {
    // Session cap is enforced atomically inside `pty_core` (a live-slot
    // reservation covering in-flight spawns + live children), fixing the
    // check-then-act TOCTOU of the old `live_count() >= MAX` pre-check (§3).
    let agent_id = uuid::Uuid::new_v4().to_string();
    let project = if project.is_empty() {
        DEFAULT_PROJECT.to_string()
    } else {
        project
    };
    sanitize_project(&project)?;

    // Every project hosts its per-agent session metadata under the workspace
    // layout (`~/CaPilot/workspaces/<project>/agents/<id>`), so the tree and
    // session restore can always find it. Custom-rooted projects (git-cloned /
    // picked folder) get this layout too — it never touches the project root.
    ensure_project(&project).map_err(|e| format!("Failed to init workspace: {}", e))?;

    // PTY working directory: custom-rooted agents open a terminal directly in
    // the project root (cloned repo / picked folder); workspace projects keep
    // the per-agent dir so each session's context stays isolated.
    let cwd = match &project_root {
        Some(pr) => {
            // A caller-supplied project root feeds both `create_dir_all` and the
            // spawned shell's cwd — constrain it to $HOME so an arbitrary path
            // can't be created / used as a shell working dir.
            let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
            let home_path = std::path::Path::new(&home);
            let p = std::path::PathBuf::from(pr);
            if !p.starts_with(home_path) {
                return Err("project root escapes allowed directories".to_string());
            }
            std::fs::create_dir_all(&p)
                .map_err(|e| format!("Failed to create project root: {}", e))?;
            p.canonicalize()
                .map_err(|e| format!("Invalid project root: {}", e))?
        }
        None => {
            let dir = agent_dir(&project, &agent_id);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("Failed to create agent dir: {}", e))?;
            dir
        }
    };

    // A fresh spawn only resumes when the caller passed an explicit key;
    // otherwise it stays brand-new (no auto-detect) so it can't hijack the
    // newest session in a shared cwd.
    let resume = resume_key.is_some();
    let info = build_and_spawn(
        bridge.inner(),
        persistence.inner(),
        &app,
        &agent_id,
        &project,
        None,
        &runtime,
        resume,
        resume_key,
        None, // genuinely new IDE session: assign a new display title
        model,
        &speed.unwrap_or_else(|| "auto".to_string()),
        &mode.unwrap_or_else(|| "ask".to_string()),
        cwd,
        on_data,
    )?;

    // Phase 5: no background resume-key capture. The dynamic "newest provider
    // session in cwd" scan was retired — legacy PTY sessions resume only with
    // the key persisted in the DB row (written when the session was created or
    // resumed with an explicit key).
    Ok(info)
}

#[tauri::command]
async fn agent_resume(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    persistence: tauri::State<'_, Arc<Persistence>>,
    app: tauri::AppHandle,
    id: String,
    on_data: Channel<Vec<u8>>,
    rows: Option<u16>,
    cols: Option<u16>,
) -> Result<AgentInfo, String> {
    let record = {
        let db = persistence.db().lock().unwrap();
        db.get(&id).map_err(|e| e.to_string())?
    };
    let Some(rec) = record else {
        return Err(format!("Session not found: {}", id));
    };

    // Attach-first (§6.3): when the daemon still owns the live session, re-attach
    // and stream the checkpoint instead of respawning. Only a session the daemon
    // has actually reaped (AgentNotFound) — or in-process fallback, which has no
    // attach — falls through to respawn.
    let (rows, cols) = (rows.unwrap_or(24), cols.unwrap_or(80));
    match bridge.attach(&id, rows, cols, on_data.clone()) {
        Ok(mut info) => {
            // The attach returns the live incarnation; carry the persisted session
            // metadata so the tab looks identical to a respawn.
            info.runtime = rec.runtime.clone();
            info.workspace_id = rec.workspace_id.clone();
            info.project = Some(rec.project.clone());
            info.mode = rec.mode.clone();
            info.speed = rec.speed.clone();
            info.model = rec.model.clone();
            info.title = rec.title.clone();
            info.cwd = rec.cwd.clone();
            Ok(info)
        }
        Err(AgentError::AgentNotFound(_)) => {
            // Not live — kill any leftover PTY and respawn, continuing the
            // stored/detected conversation.
            bridge.kill(&id).map_err(|e| e.to_string())?;
            build_and_spawn(
                bridge.inner(),
                persistence.inner(),
                &app,
                &id,
                &rec.project,
                rec.workspace_id.clone(),
                &rec.runtime,
                true, // resume — continue the stored/detected conversation
                rec.resume_key.clone(),
                Some(rec.title.clone()),
                rec.model.clone(),
                &rec.speed,
                &rec.mode,
                rec.cwd.clone(),
                on_data,
            )
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Payload emitted on `agent://usage` — an agent's context-window usage was
/// (re)computed. `usage` is null when the runtime has no trustworthy value.
#[derive(Clone, Serialize)]
struct AgentUsageUpdate {
    id: String,
    usage: Option<AgentUsage>,
}

/// Compute + cache an agent's current context-window usage and emit it as
/// `agent://usage`. Best-effort by design: an unknown agent or a runtime with
/// no usage value returns `Ok(None)` — the command never fails on a missing
/// session. The result is also cached daemon-side (agent id → usage) so the
/// `lastUsage` contract holds in memory without persisting to the DB.
#[tauri::command]
async fn agent_context_usage(
    persistence: tauri::State<'_, Arc<Persistence>>,
    cache: tauri::State<'_, ContextUsageCache>,
    app: tauri::AppHandle,
    id: String,
) -> Result<Option<AgentUsage>, String> {
    let record = {
        let db = persistence.db().lock().unwrap();
        db.get(&id).map_err(|e| e.to_string())?
    };
    let Some(rec) = record else {
        return Ok(None);
    };

    // Reuse a recent sample. The frontend's immediate on-open poll and its 3s
    // scheduled tick can both fire inside the same window; recomputing (file
    // I/O, possibly an opencode catalog subprocess) would be wasted work.
    // The TTL is short, so this only dedups bursts — it does not throttle the
    // meter's refresh rate.
    {
        let cache = cache.inner.lock().unwrap();
        if let Some((usage, at)) = cache.get(&id) {
            if at.elapsed() < CONTEXT_USAGE_TTL {
                return Ok(Some(usage.clone()));
            }
        }
    }

    // The read is blocking file I/O and can shell out to a subprocess (opencode
    // catalog refresh) — run it on the blocking pool so it never stalls the
    // async runtime's other commands. Build the adapter inside the closure so
    // the boxed trait object never crosses threads.
    let cwd = rec.cwd.clone();
    let model = rec.model.clone();
    let runtime = rec.runtime.clone();
    let usage = tauri::async_runtime::spawn_blocking(move || {
        get_adapter(&runtime).context_usage(&cwd, model.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?;

    {
        let mut cache = cache.inner.lock().unwrap();
        match &usage {
            Some(u) => {
                cache.insert(id.clone(), (u.clone(), Instant::now()));
            }
            // A runtime that now reports nothing clears the stale cached value
            // (e.g. after switching to a non-tracking runtime).
            None => {
                cache.remove(&id);
            }
        }
    }
    let _ = app.emit(
        "agent://usage",
        AgentUsageUpdate {
            id: id.clone(),
            usage: usage.clone(),
        },
    );
    Ok(usage)
}

/// Pull journaled lifecycle events that happened while the GUI was offline
/// (Phase 4b, §6.2). The frontend passes its high-water mark — the highest
/// `event_seq` it has already applied via live `agent://*` events — and gets
/// every natural exit / delete-mode removal recorded past that point, in journal
/// order, plus the journal's new watermark. Call this AFTER registering the live
/// listeners so replay and live delivery never race.
#[tauri::command]
fn agent_sync_events(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    last_seq: u64,
) -> bridge::SyncEventsResult {
    bridge.sync_events(last_seq)
}

#[tauri::command]
async fn agent_write(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    persistence: tauri::State<'_, Arc<Persistence>>,
    id: String,
    data: String,
    raw: Option<bool>,
) -> Result<(), String> {
    // Phase 5: legacy PTY Agent sessions (claude/codex/opencode) are read-only
    // EOL compat entries — no keystroke / prompt injection into their TUIs.
    // bash terminals (the TerminalService path) stay fully writable.
    {
        let db = persistence.db().lock().unwrap();
        if let Ok(Some(rec)) = db.get(&id) {
            let runtime = rec.runtime.as_str();
            if runtime != "bash" && runtime != "bash-rc" {
                return Err(
                    "该会话为旧版 PTY Agent（EOL），已进入只读兼容模式；请新建结构化 Agent 会话"
                        .to_string(),
                );
            }
        }
    }
    // DevPlan §4.2: composer send = pty_write(文本 + \r) — Enter submits the TUI
    // input line. `raw: true` is used by the xterm panel for keystroke passthrough.
    let payload = if raw.unwrap_or(false) {
        data
    } else {
        format!("{}\r", data)
    };
    bridge
        .write(&id, payload.as_bytes())
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn agent_kill(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    persistence: tauri::State<'_, Arc<Persistence>>,
    id: String,
) -> Result<(), String> {
    bridge.kill(&id).map_err(|e| e.to_string())?;
    // Only flip an active session to `idle` (reopenable). A session that already
    // ended naturally is `done` — flipping it back to `idle` would make a
    // finished conversation resurrect as an active tab after a restart (sleep on
    // a project with ended agents would revive them).
    if let Ok(db) = persistence.db().lock() {
        let is_done = db
            .get(&id)
            .ok()
            .flatten()
            .map(|rec| rec.status == "done")
            .unwrap_or(false);
        if !is_done {
            let _ = db.update_status(&id, "idle", now_ms());
        }
    }
    Ok(())
}

#[tauri::command]
async fn agent_resize(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    id: String,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    bridge.resize(&id, rows, cols).map_err(|e| e.to_string())
}

/// Update a session's composer config (permission mode / speed / model).
/// Persists to the DB row + `.agent-meta.json` so the next `agent_resume` uses
/// the new values; the running PTY is intentionally NOT touched (no restart, no
/// interruption). Omitted fields keep their current value. Legacy-compat
/// surface: structured agents configure through `agent_set_config` instead.
#[tauri::command]
async fn agent_set_session_config(
    persistence: tauri::State<'_, Arc<Persistence>>,
    id: String,
    mode: Option<String>,
    speed: Option<String>,
    model: Option<String>,
) -> Result<(), String> {
    // Read the record, validate/normalize the new values (unknown strings keep
    // the stored value rather than clobbering it), and update the DB row.
    let (project, mode, speed, model) = {
        let db = persistence.db().lock().unwrap();
        let rec = db
            .get(&id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Session not found: {id}"))?;
        let adapter = get_adapter(&rec.runtime);
        let permission_modes = adapter.list_permission_modes();
        let mode = match mode {
            Some(m) if permission_modes.iter().any(|choice| choice.id == m) => m,
            _ => rec.mode.clone(),
        };
        let thinking_options = adapter.list_thinking_options();
        let speed = match speed {
            Some(s) if thinking_options.iter().any(|choice| choice.id == s) => s,
            _ => rec.speed.clone(),
        };
        let model = model.or_else(|| rec.model.clone());
        let now = now_ms();
        db.update_config(&id, &mode, &speed, model.as_deref(), now)
            .map_err(|e| e.to_string())?;
        (rec.project.clone(), mode, speed, model)
    };

    // Keep the per-agent meta file in sync (used by custom_project_root recovery).
    let now = now_ms();
    if let Ok(mut meta) = read_agent_meta(&project, &id) {
        meta.mode = mode;
        meta.speed = speed;
        meta.model = model;
        meta.updated_at = now;
        let _ = write_agent_meta(&project, &meta);
    }
    Ok(())
}

/// Rename a terminal session (tab-bar / sidebar right-click → 重命名).
/// Persists to the DB row + `.agent-meta.json` so the new title survives a
/// restart, sleepProject and `agent_resume` (which keeps the stored title).
/// Returns the updated record so the frontend can sync its store atomically.
#[tauri::command]
async fn agent_rename(
    persistence: tauri::State<'_, Arc<Persistence>>,
    id: String,
    title: String,
) -> Result<AgentSessionRecord, String> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("终端名称不能为空".to_string());
    }
    if title.chars().count() > 80 {
        return Err("终端名称不能超过 80 个字符".to_string());
    }
    let rec = {
        let db = persistence.db().lock().unwrap();
        db.get(&id).map_err(|e| e.to_string())?
    };
    let Some(mut rec) = rec else {
        return Err(format!("Session not found: {id}"));
    };
    {
        let db = persistence.db().lock().unwrap();
        db.update_title(&id, &title, now_ms())
            .map_err(|e| e.to_string())?;
    }
    // Keep `.agent-meta.json` in sync (mirrors agent_set_session_config).
    if let Ok(mut meta) = read_agent_meta(&rec.project, &id) {
        meta.title = title.clone();
        meta.updated_at = now_ms();
        let _ = write_agent_meta(&rec.project, &meta);
    }
    rec.title = title;
    Ok(rec)
}

// ── Structured agent API (architecture §13) ────────────────────────
//
// Thin Tauri proxies to the daemon's `AgentManager` via the bridge. The daemon
// owns the manager and pushes sequenced `AgentEvent`s (forwarded to the frontend
// as `agent://agent-event` by the bridge); these commands are the pull surface:
// create/resume/list/snapshot, drive a turn, resolve permissions, and fetch
// provider catalogs.

#[tauri::command]
async fn agent_provider_list(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
) -> Result<Vec<agent_provider::types::ProviderInfo>, String> {
    bridge.provider_list()
}

#[tauri::command]
async fn agent_provider_diagnostic(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    provider_id: String,
) -> Result<agent_provider::types::ProviderDiagnostic, String> {
    bridge.provider_diagnostic(&provider_id)
}

#[tauri::command]
async fn agent_provider_catalog(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    provider_id: String,
    cwd: String,
) -> Result<agent_provider::types::ProviderCatalog, String> {
    bridge.provider_refresh_catalog(&provider_id, &cwd)
}

#[tauri::command]
async fn agent_create(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    agent_id: String,
    provider_id: String,
    backend_kind: String,
    cwd: String,
    model: Option<String>,
    config: Vec<(String, ConfigValue)>,
) -> Result<agent_provider::manager::AgentSnapshot, String> {
    bridge.agent_create(NewAgentRequest {
        agent_id,
        provider_id,
        backend_kind,
        workspace_id: None,
        cwd: cwd.into(),
        model,
        config,
    })
}

#[tauri::command]
async fn agent_resume_structured(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    agent_id: String,
    provider_id: String,
    runtime_session_id: String,
    cwd: String,
    model: Option<String>,
    config: Vec<(String, ConfigValue)>,
) -> Result<agent_provider::manager::AgentSnapshot, String> {
    bridge.agent_resume(ResumeAgentRequest {
        agent_id,
        handle: agent_provider::types::PersistenceHandle {
            provider_id,
            runtime_session_id,
            native_handle: None,
            metadata: None,
        },
        cwd: cwd.into(),
        model,
        config,
    })
}

#[tauri::command]
async fn agent_snapshot(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    agent_id: String,
) -> Result<agent_provider::manager::AgentSnapshot, String> {
    bridge.agent_get_snapshot(&agent_id)
}

#[tauri::command]
async fn agent_start_turn(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    agent_id: String,
    client_message_id: String,
    text: String,
) -> Result<TurnId, String> {
    bridge.agent_start_turn(
        &agent_id,
        AgentPrompt {
            client_message_id,
            content: vec![agent_provider::types::PromptContent::Text { text }],
        },
    )
}

#[tauri::command]
async fn agent_interrupt_turn(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    agent_id: String,
) -> Result<(), String> {
    bridge.agent_interrupt(&agent_id)
}

#[tauri::command]
async fn agent_set_config(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    agent_id: String,
    config_id: String,
    value: ConfigValue,
) -> Result<(), String> {
    bridge.agent_set_config_option(&agent_id, &config_id, value)
}

#[tauri::command]
async fn agent_respond_permission(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    agent_id: String,
    request_id: String,
    action_id: String,
) -> Result<(), String> {
    bridge.agent_respond_permission(&agent_id, &request_id, &action_id)
}

#[tauri::command]
async fn agent_close_structured(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    agent_id: String,
) -> Result<(), String> {
    bridge.agent_close(&agent_id)
}

#[tauri::command]
async fn agent_list_structured(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
) -> Result<Vec<agent_provider::manager::AgentRecord>, String> {
    bridge.agent_list()
}

#[tauri::command]
async fn sessions_list(
    persistence: tauri::State<'_, Arc<Persistence>>,
) -> Result<Vec<AgentSessionRecord>, String> {
    let db = persistence.db().lock().unwrap();
    db.list_all().map_err(|e| e.to_string())
}

#[tauri::command]
async fn workspace_root() -> Result<String, String> {
    Ok(persistence::workspace_root().to_string_lossy().to_string())
}

/// Create a new workspace project: validates the name, then initialises
/// `~/CaPilot/workspaces/<name>/{context, agents}` (+ git init). Returns the
/// project name on success.
#[tauri::command]
fn create_project(name: String, path: Option<String>) -> Result<String, String> {
    sanitize_project(&name)?;
    if let Some(path) = path {
        // Project rooted at an EXISTING local folder the user picked.
        let dir = std::path::Path::new(&path);
        if !dir.is_dir() {
            return Err("所选文件夹不存在或不是目录".to_string());
        }
        let canonical = dir.canonicalize().map_err(|e| format!("无效路径: {}", e))?;
        // Per-agent metadata lives under the workspace layout (created by
        // agent_spawn), never inside the picked folder. git init is best-effort
        // (the Git panel depends on a repo).
        let _ = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&canonical)
            .status();
        // Persist the root so terminals keep opening there even before any agent
        // exists (agent-meta recovery needs one).
        let _ = persistence::write_project_root(&name, &canonical);
        Ok(canonical.to_string_lossy().to_string())
    } else {
        ensure_project(&name).map_err(|e| format!("Failed to init workspace: {}", e))?;
        Ok(name)
    }
}

/// One workspace project from `list_projects`: its display name and on-disk
/// root. `root` is `workspace_root().join(name)` for the default flow; the
/// frontend also keeps folder/clone-rooted projects here (their root differs).
#[derive(Debug, Clone, Serialize)]
pub struct ProjectInfo {
    pub name: String,
    pub root: String,
}

/// List all workspace project names under `~/CaPilot/workspaces/` (directories
/// only, hidden entries excluded). Powers the sidebar's project tree so empty
/// projects show up too.
#[tauri::command]
fn list_projects() -> Result<Vec<ProjectInfo>, String> {
    let root = persistence::workspace_root();
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut projects = Vec::new();
    for entry in
        std::fs::read_dir(&root).map_err(|e| format!("Failed to read workspaces dir: {}", e))?
    {
        let entry = entry.map_err(|e| format!("Failed to read workspace entry: {}", e))?;
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Failed to read entry type: {}", e))?;
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        // A custom-rooted project (cloned / picked folder) keeps its real root
        // in the agents' metadata — surface it instead of the workspace dir so
        // the sidebar restores the right cwd after a restart.
        let project_root =
            persistence::custom_project_root(&name).unwrap_or_else(|| root.join(&name));
        projects.push(ProjectInfo {
            root: project_root.to_string_lossy().to_string(),
            name,
        });
    }
    projects.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(projects)
}

fn delete_project_dir(name: &str) -> Result<(), String> {
    sanitize_project(name)?;
    let dir = persistence::project_dir(name);
    // Belt-and-braces: the resolved path must stay under the workspace root.
    if !dir.starts_with(persistence::workspace_root()) {
        return Err("非法路径".to_string());
    }
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("删除项目目录失败: {}", e))?;
    }
    Ok(())
}

/// Delete a project and all persisted sessions that belong to it. Session
/// ownership comes from the database's `project` field, never from cwd parsing;
/// custom-rooted and legacy sessions may not share the project's casing/path.
/// Only CaPilot's workspace metadata directory is removed—picked/cloned source
/// folders remain untouched.
#[tauri::command]
fn delete_project(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    persistence: tauri::State<'_, Arc<Persistence>>,
    name: String,
) -> Result<(), String> {
    sanitize_project(&name)?;
    let session_ids = {
        let db = persistence
            .db_tolerant()
            .ok_or_else(|| "persistence unavailable".to_string())?;
        db.list_all()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|record| record.project == name)
            .map(|record| record.id)
            .collect::<Vec<_>>()
    };
    for id in session_ids {
        let _ = bridge.kill(&id);
    }
    {
        let db = persistence
            .db_tolerant()
            .ok_or_else(|| "persistence unavailable".to_string())?;
        db.delete_project(&name).map_err(|e| e.to_string())?;
    }
    delete_project_dir(&name)
}

/// Rename a workspace project: renames `~/CaPilot/workspaces/<old>` →
/// `~/CaPilot/workspaces/<new>` and rewrites the project name / cwd prefix in
/// `sessions.db` and per-agent `.agent-meta.json`. Custom-rooted projects keep
/// their picked/cloned root folder (only the workspace metadata dir + DB row
/// change). Returns the project's (possibly unchanged) root path.
#[tauri::command]
fn rename_project(
    persistence: tauri::State<'_, Arc<Persistence>>,
    old: String,
    new: String,
) -> Result<String, String> {
    rename_project_inner(&persistence, &old, &new)
}

/// Pure rename logic (separable for tests, which can't build a Tauri State).
fn rename_project_inner(persistence: &Persistence, old: &str, new: &str) -> Result<String, String> {
    let root = persistence::workspace_root();
    let old_dir = root.join(old);
    let new_dir = root.join(new);
    if old == new {
        return Ok(persistence::custom_project_root(old)
            .unwrap_or(old_dir)
            .to_string_lossy()
            .to_string());
    }
    // `old`/`new` are joined into paths below — reject traversal so neither can
    // escape the workspace root (e.g. `../../.ssh` would rename ~/.ssh).
    sanitize_project(old)?;
    sanitize_project(new)?;
    if !old_dir.exists() {
        return Err(format!("项目不存在: {old}"));
    }
    if new_dir.exists() {
        return Err(format!("已存在同名项目: {new}"));
    }
    // The real root for custom-rooted projects is the picked/cloned folder —
    // capture it before the workspace metadata dir moves.
    let custom = persistence::custom_project_root(old);
    std::fs::rename(&old_dir, &new_dir).map_err(|e| format!("重命名项目目录失败: {e}"))?;
    // Rewrite sessions + agent metadata whose cwd points into the old dir.
    let old_prefix = old_dir.to_string_lossy().into_owned();
    let new_prefix = new_dir.to_string_lossy().into_owned();
    // Sessions live in the SINGLE top-level `~/CaPilot/sessions.db` — rewrite
    // the project column + cwd prefix there (not a per-project db, which does
    // not exist). Otherwise renamed projects' sessions point at the old path
    // and fail to resume after a restart.
    if let Some(db) = persistence.db_tolerant() {
        let _ = db.rename_project(old, new, &old_prefix, &new_prefix);
    }
    if let Ok(agents_dir) = std::fs::read_dir(new_dir.join("agents")) {
        for entry in agents_dir.flatten() {
            let meta_path = entry.path().join(".agent-meta.json");
            if let Ok(data) = std::fs::read(&meta_path) {
                if let Ok(mut meta) = serde_json::from_slice::<persistence::AgentMeta>(&data) {
                    let cwd_str = meta.cwd.to_string_lossy();
                    if cwd_str.starts_with(old_prefix.as_str()) {
                        meta.cwd = std::path::PathBuf::from(format!(
                            "{}{}",
                            new_prefix,
                            &cwd_str[old_prefix.len()..]
                        ));
                        let _ = persistence::write_agent_meta_to_dir(&entry.path(), &meta);
                    }
                }
            }
        }
    }
    Ok(custom.unwrap_or(new_dir).to_string_lossy().to_string())
}

#[tauri::command]
async fn sessions_delete(
    bridge: tauri::State<'_, Arc<bridge::PtyBridge>>,
    persistence: tauri::State<'_, Arc<Persistence>>,
    id: String,
) -> Result<(), String> {
    // Best-effort end to end: a failed kill (e.g. the PTY was already reaped by
    // the reader task) must not skip session cleanup, or the DB row survives and
    // the terminal resurrects on the next restart.
    let _ = bridge.kill(&id);
    // The agent's own project (from its DB row) — its metadata dir lives under
    // `workspaces/<project>/agents/<id>`, so remove exactly that. The session
    // MUST exist: `id` is caller-supplied, and `agent_dir` joins it into a path
    // (an absolute/`..` id would escape the workspace — a bare delete primitive).
    let record = {
        let db = persistence
            .db_tolerant()
            .ok_or_else(|| "persistence unavailable".to_string())?;
        db.get(&id).map_err(|e| e.to_string())?
    };
    let Some(rec) = record else {
        return Err(format!("Session not found: {id}"));
    };
    let dir = agent_dir(&rec.project, &id);
    // Belt-and-braces: the resolved dir must stay under the workspace root.
    if dir.starts_with(persistence::workspace_root()) && dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    // Phase 5: no status-hook sidecar / codex profile / opencode plugin to
    // clean up — the hook machinery was retired with the legacy Agent path.
    if let Some(db) = persistence.db_tolerant() {
        let _ = db.delete(&id);
    }
    Ok(())
}

// ── Settings KV commands ─────────────────────────────────────────

/// Read the persisted per-runtime launch overrides map (empty when unset or
/// malformed). Callers fall back to each adapter's default command/args.
fn load_runtime_overrides(persistence: &Arc<Persistence>) -> HashMap<String, RuntimeOverride> {
    let db = persistence.db().lock().unwrap();
    match db.get_setting(RUNTIME_OVERRIDES_KEY) {
        Ok(Some(raw)) => serde_json::from_str(&raw).unwrap_or_default(),
        _ => HashMap::new(),
    }
}

/// Read a persisted app setting (`settings` KV table), or null when unset.
#[tauri::command]
fn setting_get(
    persistence: tauri::State<'_, Arc<Persistence>>,
    key: String,
) -> Result<Option<String>, String> {
    let db = persistence.db().lock().unwrap();
    db.get_setting(&key).map_err(|e| e.to_string())
}

/// Upsert a persisted app setting (`settings` KV table). Keys are allow-listed
/// so a compromised frontend can't mint arbitrary settings (e.g. a key some
/// future feature reads as a path).
#[tauri::command]
fn setting_set(
    persistence: tauri::State<'_, Arc<Persistence>>,
    key: String,
    value: String,
) -> Result<(), String> {
    // Allow-listed setting keys. Add future settings here.
    const ALLOWED: &[&str] = &[
        SESSION_END_MODE_KEY,
        RUNTIME_OVERRIDES_KEY,
        USAGE_ENABLED_KEY,
        USAGE_CONFIG_KEY,
        TODOS_KEY,
    ];
    if !ALLOWED.contains(&key.as_str()) {
        return Err(format!("unknown setting key: {}", key));
    }
    let db = persistence.db().lock().unwrap();
    db.set_setting(&key, &value).map_err(|e| e.to_string())
}

// ── Runtime commands ────────────────────────────────────────────

#[tauri::command]
async fn runtime_list_available() -> Vec<agent_runtime::adapter::RuntimeInfo> {
    let mut out = Vec::new();
    for id in known_runtimes() {
        let adapter = get_adapter(id);
        out.push(agent_runtime::adapter::RuntimeInfo {
            id: adapter.id().to_string(),
            name: adapter.name().to_string(),
            available: adapter.is_available(),
            authenticated: adapter.is_authenticated(),
            models: adapter.list_models(),
            permission_modes: adapter.list_permission_modes(),
            thinking_options: adapter.list_thinking_options(),
        });
    }
    out
}

// ── Rate-limit usage commands ───────────────────────────────────

/// Fetch the current remaining usage for a runtime. The status bar polls this
/// for every runtime enabled under Settings → 已安装 → ⚙ → 用量统计.
#[tauri::command]
async fn usage_fetch(
    persistence: tauri::State<'_, Arc<Persistence>>,
    runtime: String,
) -> Result<usage::RuntimeUsage, String> {
    usage::fetch(&runtime, &persistence).await
}

/// Settings availability check: fetch usage regardless of the enable toggle and
/// report a short human verdict (可用/不可用 + reason).
#[tauri::command]
async fn usage_check(
    persistence: tauri::State<'_, Arc<Persistence>>,
    runtime: String,
) -> Result<usage::UsageCheck, String> {
    usage::check(&runtime, &persistence).await
}

// ── Filesystem commands ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct FsEntryBrief {
    pub name: String,
    pub is_dir: bool,
    /// Unix executable bit set (for non-directories) — runnable scripts.
    pub executable: bool,
}

#[tauri::command]
async fn fs_read(path: String) -> Result<String, String> {
    let resolved = std::path::Path::new(&path)
        .canonicalize()
        .map_err(|e| format!("Invalid path: {}", e))?;
    let home = std::env::var("HOME").map_err(|e| format!("HOME not set: {}", e))?;
    if !resolved.starts_with(&home) {
        return Err("Path escapes allowed directories".to_string());
    }
    std::fs::read_to_string(&resolved).map_err(|e| format!("Failed to read file: {}", e))
}

#[tauri::command]
async fn fs_write(path: String, content: String) -> Result<(), String> {
    let home = std::env::var("HOME").map_err(|e| format!("HOME not set: {}", e))?;
    let home_path = std::path::Path::new(&home);

    let raw = std::path::Path::new(&path);
    // Canonicalize the PARENT (which must exist) to resolve any symlinks in the
    // path, then re-join the file name. This prevents symlink traversal when the
    // target file doesn't exist yet (canonicalize() of the full path would fail
    // and a raw fallback could write through a $HOME/... -> /etc symlink).
    let parent = raw
        .parent()
        .ok_or_else(|| "Invalid path: no parent directory".to_string())?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|e| format!("Invalid path: {}", e))?;
    if !canonical_parent.starts_with(home_path) {
        return Err("Path escapes allowed directories".to_string());
    }
    let file_name = raw
        .file_name()
        .ok_or_else(|| "Invalid path: no file name".to_string())?;
    let resolved = canonical_parent.join(file_name);

    // Reject symlink final components (including DANGLING ones — a dangling
    // symlink outside HOME would otherwise be followed by fs::write after the
    // canonicalize() checks pass). Resolve the link target and verify it stays
    // in HOME; if the target is itself a symlink or escapes, refuse.
    if let Ok(meta) = std::fs::symlink_metadata(&resolved) {
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&resolved)
                .map_err(|e| format!("Failed to read symlink: {}", e))?;
            let real = if target.is_absolute() {
                target
            } else {
                resolved
                    .parent()
                    .unwrap_or(std::path::Path::new("/"))
                    .join(&target)
            };
            let canonical_target = std::fs::canonicalize(&real)
                .map_err(|_| "Symlink target could not be resolved".to_string())?;
            if !canonical_target.starts_with(home_path) {
                return Err("Path escapes allowed directories".to_string());
            }
            return std::fs::write(&canonical_target, &content)
                .map_err(|e| format!("Failed to write file: {}", e));
        }
    }

    // If the target already exists and is a regular file, double-check the
    // canonical path stays in HOME.
    if let Ok(canon) = resolved.canonicalize() {
        if !canon.starts_with(home_path) {
            return Err("Path escapes allowed directories".to_string());
        }
    }

    std::fs::write(&resolved, &content).map_err(|e| format!("Failed to write file: {}", e))
}

#[tauri::command]
async fn fs_list(dir: String) -> Result<Vec<FsEntryBrief>, String> {
    let resolved = std::path::Path::new(&dir)
        .canonicalize()
        .map_err(|e| format!("Invalid path: {}", e))?;
    let home = std::env::var("HOME").map_err(|e| format!("HOME not set: {}", e))?;
    if !resolved.starts_with(&home) {
        return Err("Path escapes allowed directories".to_string());
    }
    let mut entries = Vec::new();
    let read_dir =
        std::fs::read_dir(&resolved).map_err(|e| format!("Failed to read directory: {}", e))?;
    for entry in read_dir {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Failed to read file type: {}", e))?;
        let executable = if file_type.is_dir() {
            false
        } else {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                entry
                    .metadata()
                    .map(|m| m.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false)
            }
            #[cfg(not(unix))]
            {
                false
            }
        };
        entries.push(FsEntryBrief {
            name: entry.file_name().to_string_lossy().to_string(),
            is_dir: file_type.is_dir(),
            executable,
        });
    }
    Ok(entries)
}

/// Resolve a to-be-created path against the allowed root: canonicalize the parent
/// (must exist, must stay under $HOME), re-join the file name, and refuse to
/// create through a symlink final component. Mirrors `fs_write`'s pre-write
/// resolution so new paths get the same traversal defense.
fn resolve_in_home(raw: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let home = std::env::var("HOME").map_err(|e| format!("HOME not set: {}", e))?;
    let home_path = std::path::Path::new(&home);

    let parent = raw
        .parent()
        .ok_or_else(|| "Invalid path: no parent directory".to_string())?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|e| format!("Invalid path: {}", e))?;
    if !canonical_parent.starts_with(home_path) {
        return Err("Path escapes allowed directories".to_string());
    }
    let file_name = raw
        .file_name()
        .ok_or_else(|| "Invalid path: no file name".to_string())?;
    let resolved = canonical_parent.join(file_name);

    // A dangling symlink final component would otherwise be followed on create
    // after the checks pass — refuse outright.
    if let Ok(meta) = std::fs::symlink_metadata(&resolved) {
        if meta.file_type().is_symlink() {
            return Err("Refusing to create through symlink".to_string());
        }
    }
    Ok(resolved)
}

#[tauri::command]
async fn fs_create_file(path: String) -> Result<(), String> {
    let resolved = resolve_in_home(std::path::Path::new(&path))?;
    if resolved.exists() {
        return Err("文件已存在".to_string());
    }
    std::fs::write(&resolved, "").map_err(|e| format!("Failed to create file: {}", e))
}

#[tauri::command]
async fn fs_create_dir(path: String) -> Result<(), String> {
    let resolved = resolve_in_home(std::path::Path::new(&path))?;
    if resolved.exists() {
        return Err("目录已存在".to_string());
    }
    std::fs::create_dir(&resolved).map_err(|e| format!("Failed to create directory: {}", e))
}

/// Canonicalize an existing path and require it stays under $HOME. Used for the
/// source (and the paste destination, which must exist) of `fs_paste`, where
/// following a symlink final component to a HOME-internal target is legitimate.
fn resolve_existing_in_home(raw: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let home = std::env::var("HOME").map_err(|e| format!("HOME not set: {}", e))?;
    let resolved = raw
        .canonicalize()
        .map_err(|e| format!("Invalid path: {}", e))?;
    if !resolved.starts_with(&home) {
        return Err("Path escapes allowed directories".to_string());
    }
    Ok(resolved)
}

/// Recursively copy a directory into `dest` (created if missing). Symlinks are
/// re-created as symlinks and never followed — following them could escape
/// $HOME or loop forever through a cycle.
fn copy_dir_recursive(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let s = entry.path();
        let d = dest.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            let target = std::fs::read_link(&s)?;
            std::os::unix::fs::symlink(target, d)?;
        } else if ft.is_dir() {
            copy_dir_recursive(&s, &d)?;
        } else {
            std::fs::copy(&s, &d)?;
        }
    }
    Ok(())
}

/// VS Code-style conflict resolution: if `p` exists, pick the next free
/// "stem copy.ext" / "stem copy N.ext" sibling.
fn dedupe_path(p: &std::path::Path) -> std::path::PathBuf {
    if !p.exists() {
        return p.to_path_buf();
    }
    let parent = p.parent().unwrap_or_else(|| std::path::Path::new(""));
    let file_name = p
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let (stem, ext) = match file_name.rfind('.') {
        Some(i) if i > 0 => (file_name[..i].to_string(), file_name[i..].to_string()),
        _ => (file_name, String::new()),
    };
    let mut candidate = parent.join(format!("{} copy{}", stem, ext));
    let mut n = 2;
    while candidate.exists() {
        candidate = parent.join(format!("{} copy {}{}", stem, n, ext));
        n += 1;
    }
    candidate
}

/// Paste (copy or move) `src` into `dest_dir`, auto-renaming on name conflicts.
/// `is_move` true = cut → move (rename, with EXDEV cross-device copy+delete
/// fallback). Both paths are canonicalized and must stay under $HOME.
#[tauri::command]
async fn fs_paste(src: String, dest_dir: String, is_move: bool) -> Result<String, String> {
    let src = resolve_existing_in_home(std::path::Path::new(&src))?;
    let dest_dir = resolve_existing_in_home(std::path::Path::new(&dest_dir))?;
    if !dest_dir.is_dir() {
        return Err("目标不是目录".to_string());
    }
    // Pasting a folder into itself (or its own subtree) would recurse forever.
    if src.is_dir() && dest_dir.starts_with(&src) {
        return Err("不能把文件夹移动到它自身内部".to_string());
    }
    let file_name = src
        .file_name()
        .ok_or_else(|| "Invalid path: no file name".to_string())?
        .to_string_lossy()
        .into_owned();
    let dest = dedupe_path(&dest_dir.join(&file_name));

    if is_move {
        match std::fs::rename(&src, &dest) {
            Ok(_) => {}
            Err(e) if e.raw_os_error() == Some(18) => {
                // EXDEV: rename across devices — copy then remove the source.
                if src.is_dir() {
                    copy_dir_recursive(&src, &dest).map_err(|e| format!("移动失败: {}", e))?;
                } else {
                    std::fs::copy(&src, &dest).map_err(|e| format!("移动失败: {}", e))?;
                }
                let clean = if src.is_dir() {
                    std::fs::remove_dir_all(&src)
                } else {
                    std::fs::remove_file(&src)
                };
                clean.map_err(|e| format!("清理源文件失败: {}", e))?;
            }
            Err(e) => return Err(format!("移动失败: {}", e)),
        }
    } else if src.is_dir() {
        copy_dir_recursive(&src, &dest).map_err(|e| format!("复制失败: {}", e))?;
    } else {
        std::fs::copy(&src, &dest).map_err(|e| format!("复制失败: {}", e))?;
    }
    Ok(dest.to_string_lossy().into_owned())
}

/// Delete a file or a directory recursively. The path is canonicalized and must
/// stay under $HOME; deleting $HOME itself is refused (a path equal to it would
/// otherwise wipe the whole user directory).
#[tauri::command]
async fn fs_delete(path: String) -> Result<(), String> {
    let home = std::env::var("HOME").map_err(|e| format!("HOME not set: {}", e))?;
    let home_canon = std::path::Path::new(&home)
        .canonicalize()
        .unwrap_or_else(|_| std::path::Path::new(&home).to_path_buf());
    let resolved = resolve_existing_in_home(std::path::Path::new(&path))?;
    if resolved == home_canon {
        return Err("不能删除主目录".to_string());
    }
    if resolved.is_dir() {
        std::fs::remove_dir_all(&resolved).map_err(|e| format!("删除目录失败: {}", e))
    } else {
        std::fs::remove_file(&resolved).map_err(|e| format!("删除文件失败: {}", e))
    }
}

/// Rename a file or directory within its parent. The source is resolved by
/// canonicalizing the parent (must exist, stay under $HOME) and re-joining the
/// original name — this preserves a symlink final component, so renaming a
/// symlink renames the link rather than its target. The new name must not
/// contain `/`, and a same-name sibling is refused (renames are explicit, so no
/// auto-suffix like `fs_paste`).
#[tauri::command]
async fn fs_rename(src: String, new_name: String) -> Result<String, String> {
    let name = new_name.trim();
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err("名称不能为空、包含 / 或为 . / ..".to_string());
    }
    let home = std::env::var("HOME").map_err(|e| format!("HOME not set: {}", e))?;
    let home_path = std::path::Path::new(&home);
    let raw = std::path::Path::new(&src);
    let parent = raw
        .parent()
        .ok_or_else(|| "无效路径：无父目录".to_string())?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|e| format!("无效路径: {}", e))?;
    if !canonical_parent.starts_with(home_path) {
        return Err("路径越界".to_string());
    }
    let file_name = raw
        .file_name()
        .ok_or_else(|| "无效路径：无文件名".to_string())?;
    let resolved = canonical_parent.join(file_name);
    // Renaming $HOME itself would otherwise surface as a confusing escape error.
    if resolved == home_path {
        return Err("不能重命名主目录".to_string());
    }
    // symlink_metadata so a dangling symlink can still be renamed.
    if std::fs::symlink_metadata(&resolved).is_err() {
        return Err("路径不存在".to_string());
    }
    let dest = canonical_parent.join(name);
    if dest.exists() {
        return Err("已存在同名文件或文件夹".to_string());
    }
    std::fs::rename(&resolved, &dest).map_err(|e| format!("重命名失败: {}", e))?;
    Ok(dest.to_string_lossy().into_owned())
}

// ── Git commands ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct GitEntry {
    pub index: String,
    pub worktree: String,
    pub path: String,
    pub add: i32,
    pub del: i32,
}

/// Reverse git's C-style path quoting (core.quotePath, on by default). Porcelain
/// and numstat output wrap paths with special characters in double quotes and
/// escape `\t \n \\ \"` plus non-ASCII bytes as `\NNN` octals — e.g. a file
/// named `readme copy.md` comes back as `"readme copy.md"`. Without unquoting,
/// the literal quotes leak into every downstream `git add/diff/show` path spec
/// and fail with "pathspec did not match".
fn unquote_git_path(s: &str) -> String {
    if s.len() < 2 || !s.starts_with('"') || !s.ends_with('"') {
        return s.to_string();
    }
    let inner = &s[1..s.len() - 1];
    let bytes = inner.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b't' => {
                    out.push(b'\t');
                    i += 2;
                }
                b'b' => {
                    out.push(0x08);
                    i += 2;
                }
                b'n' => {
                    out.push(b'\n');
                    i += 2;
                }
                b'f' => {
                    out.push(0x0C);
                    i += 2;
                }
                b'r' => {
                    out.push(b'\r');
                    i += 2;
                }
                b'"' => {
                    out.push(b'"');
                    i += 2;
                }
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                }
                b'0'..=b'7' => {
                    let mut code = (bytes[i + 1] - b'0') as u32;
                    let mut len = 2;
                    while len <= 3
                        && i + len < bytes.len()
                        && bytes[i + len].is_ascii_digit()
                        && bytes[i + len] < b'8'
                    {
                        code = code * 8 + (bytes[i + len] - b'0') as u32;
                        len += 1;
                    }
                    // `\NNN` is a raw byte (non-ASCII paths are multi-byte UTF-8
                    // escape sequences, e.g. `\346\226\207` = 文); the whole
                    // output is decoded as UTF-8 at the end.
                    out.push(code as u8);
                    i += len;
                }
                _ => {
                    out.push(b'\\');
                    i += 2;
                }
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse `git status --porcelain` output into structured entries.
fn parse_porcelain(text: &str) -> Vec<GitEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        if line.len() < 3 {
            continue;
        }
        let index = &line[..1];
        let worktree = &line[1..2];
        let raw_path = line.get(3..).unwrap_or("").trim();
        // porcelain rename form: "old -> new"
        let path = if let Some(arrow) = raw_path.find(" -> ") {
            unquote_git_path(&raw_path[arrow + 4..])
        } else {
            unquote_git_path(raw_path)
        };
        entries.push(GitEntry {
            index: index.to_string(),
            worktree: worktree.to_string(),
            path,
            add: 0,
            del: 0,
        });
    }
    entries
}

/// Run `git` in `repo`, returning trimmed stdout. Errors surface stderr.
fn git_run(repo: &str, args: &[&str]) -> Result<String, String> {
    let out = git_gate::run(repo, args)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git error: {}", err.trim()));
    }
    // Only trim trailing whitespace: a leading `.trim()` would eat the first
    // status column of `git status --porcelain` (and leading blank lines of a
    // file via `git show`), misparsing the first entry as staged.
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Resolve a caller-supplied `repo` path and verify it is a real directory
/// inside `$HOME`. `git_*` commands run arbitrary git in `repo`, so it must be
/// pinned to the user's tree rather than accepting any path.
fn validate_repo(repo: &str) -> Result<std::path::PathBuf, String> {
    git_gate::validate_repo(repo)
}

/// Stream-count lines in a file without loading it into memory (a huge untracked
/// file would otherwise be read whole just to report `+N`). Capped at 1M lines so
/// a pathological file can't stall `git_status`.
fn count_lines(path: &std::path::Path) -> i32 {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return 0;
    };
    std::io::BufReader::new(file)
        .lines()
        .take(1_000_000)
        .count() as i32
}

/// Parse `git diff --numstat` lines ("adds\tdeletes\tpath") into a path→(add,del) map.
fn parse_numstat(text: &str) -> std::collections::HashMap<String, (i32, i32)> {
    let mut map = std::collections::HashMap::new();
    for line in text.lines() {
        let mut parts = line.split('\t');
        let add = parts.next().unwrap_or("0").trim();
        let del = parts.next().unwrap_or("0").trim();
        let path = parts.next().unwrap_or("").trim();
        // Binary files report "-" instead of a number.
        if path.is_empty() || add == "-" || del == "-" {
            continue;
        }
        if let (Ok(a), Ok(d)) = (add.parse::<i32>(), del.parse::<i32>()) {
            map.insert(unquote_git_path(path), (a, d));
        }
    }
    map
}

/// Parse `git diff-tree --name-status` output ("XY\tpath", renames "XY\told\tnew")
/// into entries; the change char (A/M/D/R) goes in `index`. Powers the Git
/// panel's "已提交的更改" group.
fn parse_name_status(text: &str) -> Vec<GitEntry> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let status = parts.next()?.trim();
            let path = parts.last()?.trim();
            if path.is_empty() {
                return None;
            }
            Some(GitEntry {
                index: status.to_string(),
                worktree: " ".to_string(),
                path: path.to_string(),
                add: 0,
                del: 0,
            })
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct GitBranch {
    pub name: String,
    pub current: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct GitLogEntry {
    /// Full commit hash (`%H`) — parents reference these exact values.
    pub hash: String,
    /// Full parent hashes (`%P`, space separated in the raw log line).
    pub parents: Vec<String>,
    /// Local refs (branches + `tag:` prefixed tags) pointing at this commit.
    pub refs: Vec<String>,
    pub subject: String,
    pub author: String,
    pub email: String,
    pub ts: i64,
}

/// One file touched by a commit (from `git diff-tree --numstat --name-status`).
#[derive(Debug, Clone, Serialize)]
pub struct GitFileStat {
    pub path: String,
    pub status: String,
    pub add: i32,
    pub del: i32,
}

/// Full commit payload for the "查看提交详情" modal: message + author/date plus
/// per-file stats. `body` is the multi-line message after the subject ("" when
/// the commit has no body).
#[derive(Debug, Clone, Serialize)]
pub struct GitCommitDetail {
    pub hash: String,
    pub subject: String,
    pub body: String,
    pub author: String,
    pub email: String,
    pub ts: i64,
    pub files: Vec<GitFileStat>,
}

/// Parse `git branch` porcelain output into (name, current) pairs. The current
/// branch carries a `* ` prefix; `+ ` marks a branch checked out in another
/// worktree (treated as non-current). Detached-HEAD placeholders like
/// `* (HEAD detached at …)` are skipped.
fn parse_branches(text: &str) -> Vec<GitBranch> {
    let mut branches = Vec::new();
    for line in text.lines() {
        let line = line.trim_end();
        if line.len() < 2 {
            continue;
        }
        let (current, raw) = if line.starts_with("* ") {
            (true, &line[2..])
        } else if line.starts_with('+') {
            (false, line[1..].trim_start())
        } else {
            (false, line.trim())
        };
        let name = raw.trim();
        // `git branch` renders detached HEAD as `* (HEAD detached at …)`.
        if name.is_empty() || name.starts_with('(') {
            continue;
        }
        branches.push(GitBranch {
            name: name.to_string(),
            current,
        });
    }
    branches
}

/// Parse `git log --pretty=format:%H%x1f%P%x1f%s%x1f%an%x1f%ae%x1f%ct` output.
/// Each commit is one line; `%x1f` (unit separator) delimits the six fields
/// hash / parents / subject / author / email / timestamp. `refs` are filled in
/// separately by `git_log` from `for-each-ref` (a hash→name map).
fn parse_log(text: &str) -> Vec<GitLogEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\x1f');
        let hash = parts.next().unwrap_or("").trim().to_string();
        if hash.is_empty() {
            continue;
        }
        let parents = parts
            .next()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let subject = parts.next().unwrap_or("").trim().to_string();
        let author = parts.next().unwrap_or("").trim().to_string();
        let email = parts.next().unwrap_or("").trim().to_string();
        let ts = parts
            .next()
            .unwrap_or("0")
            .trim()
            .parse::<i64>()
            .unwrap_or(0);
        entries.push(GitLogEntry {
            hash,
            parents,
            refs: vec![],
            subject,
            author,
            email,
            ts,
        });
    }
    entries
}

/// Parse `git for-each-ref --format=%(objectname) %(refname)` into a
/// full-hash → ref-names map. Local branches, tags and remote-tracking refs are
/// kept: `git_log` reads `--all`, so a lane that diverges from HEAD (e.g. a
/// branch only reachable via `refs/remotes/…`) must still get a label. Tags are
/// emitted as `tag: <name>` so the frontend can distinguish tags from branches.
///
/// NOTE: the format is space-separated, NOT `%x1f`: `git for-each-ref` does not
/// interpret `%x1f` the way `git log --pretty` does (it prints it literally),
/// so a byte separator would never match. Object names are exactly 40 hex chars,
/// so splitting on the first space is unambiguous.
fn parse_ref_map(text: &str) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, ' ');
        let Some(hash) = parts.next().map(str::trim).filter(|h| !h.is_empty()) else {
            continue;
        };
        let Some(full_name) = parts.next().map(str::trim).filter(|n| !n.is_empty()) else {
            continue;
        };
        let name = if let Some(tag) = full_name.strip_prefix("refs/tags/") {
            format!("tag: {tag}")
        } else if let Some(head) = full_name.strip_prefix("refs/heads/") {
            head.to_string()
        } else if let Some(remote) = full_name.strip_prefix("refs/remotes/") {
            remote.to_string()
        } else {
            continue;
        };
        map.entry(hash.to_string()).or_default().push(name);
    }
    map
}

#[tauri::command]
async fn git_status(dir: String) -> Result<Vec<GitEntry>, String> {
    let text = git_run(&dir, &["status", "--porcelain"])?;
    let mut entries = parse_porcelain(&text);

    // Per-file line counts: staged (--cached) + unstaged diffs.
    let mut counts: std::collections::HashMap<String, (i32, i32)> =
        std::collections::HashMap::new();
    for args in [
        &["diff", "--cached", "--numstat"][..],
        &["diff", "--numstat"][..],
    ] {
        if let Ok(out) = git_run(&dir, args) {
            for (path, (a, d)) in parse_numstat(&out) {
                let c = counts.entry(path).or_insert((0, 0));
                c.0 += a;
                c.1 += d;
            }
        }
    }
    for e in &mut entries {
        if let Some((a, d)) = counts.get(&e.path) {
            e.add = *a;
            e.del = *d;
        } else if e.index == "?" && e.worktree == "?" {
            // Untracked file: every line counts as an addition. Stream-count so
            // a large file isn't read whole into memory.
            let full = std::path::Path::new(&dir).join(&e.path);
            e.add = count_lines(&full);
        }
    }
    Ok(entries)
}

/// Whether a directory is a git repo / has a remote / current branch. Powers the
/// Git panel's "未初始化 git" prompt and "无远程仓库" hint.
#[derive(Debug, Clone, Serialize)]
pub struct RepoInfo {
    pub is_repo: bool,
    pub has_remote: bool,
    pub branch: Option<String>,
    /// Commits ahead of the upstream branch (0 when none / no upstream). Feeds
    /// the panel's "↑ N 未推送" indicator so a local-only commit is visible.
    pub ahead: i32,
    /// Whether the current branch has a configured upstream (else it needs
    /// publishing — VS Code's "Publish Branch").
    pub has_upstream: bool,
}

/// `git init` in `repo` (idempotent). Called from the Git panel when the focused
/// project is not yet a git repository.
#[tauri::command]
async fn git_init(repo: String) -> Result<(), String> {
    git_run(&repo, &["init"]).map(|_| ())
}

/// Probe a directory's git state: is it a repo, does it have a remote, what is
/// the current branch (best-effort). Never fails — a non-git / missing dir just
/// reports `is_repo: false`.
#[tauri::command]
async fn git_repo_info(repo: String) -> Result<RepoInfo, String> {
    // `git rev-parse --is-inside-work-tree` succeeds inside a work tree (or bare
    // repo). A missing dir / not-a-repo simply fails → is_repo=false.
    let is_repo = git_gate::run(&repo, &["rev-parse", "--is-inside-work-tree"])
        .map(|o| o.status.success())
        .unwrap_or(false);
    // Non-empty `git remote` output ⇒ at least one remote is configured.
    let has_remote = git_run(&repo, &["remote"])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    // Current branch (empty on a fresh repo with no commits) — best-effort.
    let branch = git_run(&repo, &["branch", "--show-current"])
        .ok()
        .filter(|s| !s.trim().is_empty());
    // Upstream tracking → ahead count for the "未推送" indicator. No upstream
    // (fresh branch) reports has_upstream=false so the panel can offer publish.
    let upstream = branch
        .as_deref()
        .and_then(|_| git_run(&repo, &["rev-parse", "--abbrev-ref", "@{upstream}"]).ok())
        .filter(|s| !s.trim().is_empty() && s.trim() != "@{upstream}");
    let has_upstream = upstream.is_some();
    let ahead = upstream
        .and_then(|_| git_run(&repo, &["rev-list", "--count", "@{upstream}..HEAD"]).ok())
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(0);
    Ok(RepoInfo {
        is_repo,
        has_remote,
        branch,
        ahead,
        has_upstream,
    })
}

#[tauri::command]
async fn git_stage(repo: String, files: Vec<String>) -> Result<(), String> {
    let mut args: Vec<&str> = vec!["add", "--"];
    args.extend(files.iter().map(String::as_str));
    git_run(&repo, &args).map(|_| ())
}

#[tauri::command]
async fn git_unstage(repo: String, files: Vec<String>) -> Result<(), String> {
    let mut args: Vec<&str> = vec!["reset", "--"];
    args.extend(files.iter().map(String::as_str));
    git_run(&repo, &args).map(|_| ())
}

/// Discard a file's unstaged changes (VS Code "Discard Changes"): tracked files
/// are restored from the index (`git restore`); untracked files can't be
/// restored, so they are deleted. The file list comes from our own `git status`
/// listing and is passed via `Command::arg` (no shell) after `--`.
#[tauri::command]
async fn git_discard(repo: String, files: Vec<String>) -> Result<(), String> {
    if files.is_empty() {
        return Ok(());
    }
    // `repo` is caller-supplied and feeds both `git restore` and raw file
    // deletion — pin it to $HOME so a `../` repo can't delete arbitrary files.
    let repo_path = validate_repo(&repo)?;
    let repo_str = repo_path.to_string_lossy().into_owned();
    // `git ls-files -z -- <paths>` lists only the tracked ones; the rest are
    // untracked and get deleted from disk.
    let mut ls: Vec<&str> = vec!["ls-files", "-z", "--"];
    ls.extend(files.iter().map(String::as_str));
    let tracked_out = git_run(&repo_str, &ls).unwrap_or_default();
    let tracked: std::collections::HashSet<String> = tracked_out
        .split('\0')
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let tracked_files: Vec<&str> = files
        .iter()
        .filter(|f| tracked.contains(*f))
        .map(String::as_str)
        .collect();
    if !tracked_files.is_empty() {
        let mut args: Vec<&str> = vec!["restore", "--"];
        args.extend(tracked_files.iter().copied());
        git_run(&repo_str, &args)?;
    }
    for f in files.iter().filter(|f| !tracked.contains(*f)) {
        // Untracked file deletion: resolve the target and require it stays
        // under the repo (no `..` escaping the validated root).
        let raw = repo_path.join(f);
        let p = raw
            .canonicalize()
            .map_err(|_| format!("无法解析删除路径: {}", f))?;
        if !p.starts_with(&repo_path) {
            return Err(format!("删除路径越界: {}", f));
        }
        if p.is_file() {
            std::fs::remove_file(&p).map_err(|e| format!("删除未跟踪文件失败 {}: {}", f, e))?;
        }
    }
    Ok(())
}

/// Discard all unstaged changes (`git restore .`). Untracked files are left in
/// place — like VS Code's "Discard All Changes", which only restores tracked files.
#[tauri::command]
async fn git_discard_all(repo: String) -> Result<(), String> {
    git_run(&repo, &["restore", "."]).map(|_| ())
}

#[tauri::command]
async fn git_commit(repo: String, message: String) -> Result<(), String> {
    git_run(&repo, &["commit", "-m", message.as_str()]).map(|_| ())
}

#[tauri::command]
async fn git_branch(repo: String) -> Result<String, String> {
    git_run(&repo, &["branch", "--show-current"])
}

/// List all branches in `repo` (name + current flag). Powers the Git panel's
/// branch switcher (DevPlan §7.4A).
#[tauri::command]
async fn git_branches(repo: String) -> Result<Vec<GitBranch>, String> {
    let text = git_run(&repo, &["branch"])?;
    Ok(parse_branches(&text))
}

/// Switch to an existing branch. `--` after the branch name forces branch
/// interpretation so a branch whose name collides with a file still checks out
/// the branch. The branch arg is trusted — it comes from our own `git branch`
/// listing (DevPlan §7.4A).
#[tauri::command]
async fn git_checkout(repo: String, branch: String) -> Result<(), String> {
    git_run(&repo, &["checkout", branch.as_str(), "--"]).map(|_| ())
}

/// Validate a branch name before it reaches `git branch` / `git switch`.
/// Mirrors `git check-ref-format`'s hard rules loosely (enough to stop `-`-prefixed
/// or whitespace args from being misread as flags — git_gate passes every arg via
/// `Command::arg`, so there is no shell to break, but a name that starts with `-`
/// would still be parsed as an option).
fn validate_branch_name(name: &str) -> Result<(), String> {
    if name.trim() != name {
        return Err("分支名不能包含首尾空白".to_string());
    }
    if name.is_empty() {
        return Err("分支名不能为空".to_string());
    }
    if name.starts_with('-') {
        return Err("分支名不能以 - 开头".to_string());
    }
    if name.starts_with('.') || name.ends_with('.') {
        return Err("分支名不能以 . 开头或结尾".to_string());
    }
    if name.ends_with(".lock") {
        return Err("分支名不能以 .lock 结尾".to_string());
    }
    if name.contains(char::is_whitespace) {
        return Err("分支名不能包含空白字符".to_string());
    }
    if name.contains("..") || name.contains("@{") {
        return Err("分支名包含非法序列 .. 或 @{".to_string());
    }
    for bad in ['~', '^', ':', '?', '*', '[', '\\', '\u{7f}'] {
        if name.contains(bad) {
            return Err(format!("分支名包含非法字符: {bad}"));
        }
    }
    Ok(())
}

/// Create a branch pointing at `start_at` (a hash from our own `git_log`, or a
/// branch name) WITHOUT checking it out — the commit context menu's "从此提交新建分支".
#[tauri::command]
async fn git_create_branch(repo: String, name: String, start_at: String) -> Result<(), String> {
    validate_branch_name(&name)?;
    git_run(&repo, &["branch", name.as_str(), start_at.as_str()]).map(|_| ())
}

/// Create a branch AND switch to it — the branch-chip menu's "新建分支".
#[tauri::command]
async fn git_switch_new(repo: String, name: String) -> Result<(), String> {
    validate_branch_name(&name)?;
    git_run(&repo, &["switch", "-c", name.as_str()]).map(|_| ())
}

/// Force-delete a branch. Only ever called for a non-current branch (the menu
/// hides the delete affordance on the current one) and only after a frontend
/// confirm dialog.
#[tauri::command]
async fn git_delete_branch(repo: String, name: String) -> Result<(), String> {
    validate_branch_name(&name)?;
    git_run(&repo, &["branch", "-D", name.as_str()]).map(|_| ())
}

/// Detach HEAD at a historical commit (`git switch --detach`) — the commit
/// context menu's "检出此提交". Uncommitted changes block the switch, which
/// surfaces git's error to the panel.
#[tauri::command]
async fn git_checkout_commit(repo: String, hash: String) -> Result<(), String> {
    git_run(&repo, &["switch", "--detach", hash.as_str()]).map(|_| ())
}

/// Full detail for one commit (message + author/date + file stats) — powers the
/// "查看提交详情" modal. `hash` comes from our own `git_log`, so it is trusted.
#[tauri::command]
async fn git_show_commit(repo: String, hash: String) -> Result<GitCommitDetail, String> {
    let info = git_run(
        &repo,
        &[
            "log",
            "-1",
            "--pretty=format:%H%x1f%s%x1f%b%x1f%an%x1f%ae%x1f%ct",
            hash.as_str(),
        ],
    )?;
    let detail = parse_commit_detail(&info)?;

    let names = parse_name_status(&git_run(
        &repo,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-status",
            "-r",
            hash.as_str(),
        ],
    )?);
    let stats = parse_numstat(&git_run(
        &repo,
        &[
            "diff-tree",
            "--no-commit-id",
            "--numstat",
            "-r",
            hash.as_str(),
        ],
    )?);
    let files = names
        .into_iter()
        .map(|e| {
            let (add, del) = stats.get(&e.path).copied().unwrap_or((0, 0));
            GitFileStat {
                path: e.path,
                status: e.index,
                add,
                del,
            }
        })
        .collect();
    Ok(GitCommitDetail { files, ..detail })
}

/// Parse `git log -1 --pretty=format:%H%x1f%s%x1f%b%x1f%an%x1f%ae%x1f%ct`
/// output into a `GitCommitDetail` (with an empty `files` vec; the caller fills
/// it). `%b` is the multi-line body, so the whole text is split on the FIRST
/// five `\x1f` separators (newlines inside the body never appear as `\x1f`).
fn parse_commit_detail(text: &str) -> Result<GitCommitDetail, String> {
    let mut parts = text.splitn(6, '\x1f');
    let hash = parts.next().unwrap_or("").trim();
    if hash.is_empty() {
        return Err("无法解析提交信息".to_string());
    }
    let subject = parts.next().unwrap_or("").trim().to_string();
    let body = parts.next().unwrap_or("").trim().to_string();
    let author = parts.next().unwrap_or("").trim().to_string();
    let email = parts.next().unwrap_or("").trim().to_string();
    let ts = parts
        .next()
        .unwrap_or("0")
        .trim()
        .parse::<i64>()
        .unwrap_or(0);
    Ok(GitCommitDetail {
        hash: hash.to_string(),
        subject,
        body,
        author,
        email,
        ts,
        files: vec![],
    })
}

/// Recent commit history (short hash, subject, author, timestamp) for the Git
/// panel's "提交历史" section (DevPlan §7.4B). Defaults to the latest 20.
#[tauri::command]
async fn git_log(repo: String, count: Option<i32>) -> Result<Vec<GitLogEntry>, String> {
    let n = count.unwrap_or(20).clamp(1, 200).to_string();
    let text = git_run(
        &repo,
        &[
            "log",
            // `--all` = all local branches + tags + remote-tracking refs. A plain
            // `git log` only walks HEAD's ancestors, so a divergent branch (e.g.
            // one only reachable via `refs/remotes/…`) would never appear and the
            // commit tree would collapse to a single lane.
            "--all",
            // Parents always after their children (a child's commit date can be
            // older than its parent's after `--amend`/rebase/clock skew, and the
            // default date order would then draw the parent ABOVE the child).
            "--date-order",
            "--pretty=format:%H%x1f%P%x1f%s%x1f%an%x1f%ae%x1f%ct",
            "-n",
            n.as_str(),
        ],
    )?;
    let mut entries = parse_log(&text);
    // Annotate each commit with its refs (local + remote-tracking) so the
    // frontend's commit tree can label every lane, including ones only reachable
    // via a remote. Best-effort: a for-each-ref failure leaves
    // refs empty and the panel still renders (single-lane history) rather than
    // failing the whole git panel.
    if let Ok(refs_text) = git_run(
        &repo,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/heads",
            "refs/tags",
            "refs/remotes",
        ],
    ) {
        let ref_map = parse_ref_map(&refs_text);
        for entry in entries.iter_mut() {
            if let Some(names) = ref_map.get(&entry.hash) {
                entry.refs.clone_from(names);
            }
        }
    }
    Ok(entries)
}

/// Read a file's content from a git object (`git show <rev>:<file>`). `rev` is a
/// trusted constant the frontend sends — `"HEAD"` (committed) or `":0:"` (index /
/// staged) — and `file` comes from our own `git status` listing, so both are passed
/// via `Command::arg` with no shell. Unlike `git_run`, the output is NOT trimmed:
/// exact file content (incl. trailing newline) is required for the merge view.
/// Binary blobs (invalid UTF-8) surface a clean error instead of garbage.
#[tauri::command]
async fn git_show(repo: String, file: String, rev: String) -> Result<String, String> {
    let spec = format!("{}:{}", rev, file);
    let out = git_gate::run(&repo, &["show", &spec])?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git error: {}", err.trim()));
    }
    String::from_utf8(out.stdout)
        .map_err(|_| format!("该文件为二进制内容，无法预览 diff: {}", file))
}

#[tauri::command]
async fn git_pull(repo: String) -> Result<(), String> {
    git_run(&repo, &["pull"]).map(|_| ())
}

/// Validate a git clone URL: only remote-ish prefixes are allowed so a URL
/// can't be abused as a local path trick (`file://`, leading `-`, whitespace…).
fn validate_git_url(url: &str) -> Result<(), String> {
    if url.trim() != url || url.contains(char::is_whitespace) {
        return Err("Git 地址不能包含空白字符".to_string());
    }
    if url.starts_with('-') {
        return Err("Git 地址不能以 - 开头".to_string());
    }
    const PREFIXES: [&str; 5] = ["http://", "https://", "ssh://", "git@", "git://"];
    if !PREFIXES.iter().any(|p| url.starts_with(p)) {
        return Err(
            "不支持的 Git 地址（仅支持 http://、https://、ssh://、git@、git://）".to_string(),
        );
    }
    Ok(())
}

/// Payload emitted on `git://cloned` — a background clone finished and the
/// target dir is ready. `id` matches the value `git_clone` returned.
#[derive(Clone, Serialize)]
struct GitCloneDone {
    id: String,
    name: String,
    root: String,
}

/// Payload emitted on `git://clone-error` — a background clone failed.
#[derive(Clone, Serialize)]
struct GitCloneError {
    id: String,
    name: String,
    error: String,
}

/// Start cloning a remote git repository into `<parent_dir>/<name>`, then
/// return immediately so the UI can close the import dialog and show the
/// project as "正在克隆中" in the sidebar. The actual `git clone` runs on the
/// background runtime; completion is reported on `git://cloned` (with the real
/// root) or `git://clone-error` (with the git error). The parent dir must
/// already exist; the URL is validated and passed via `Command::arg` (no shell)
/// after `--`, so a `-`-prefixed URL is never treated as a flag. Returns
/// `(clone_id, target_dir)` — the target dir may not exist yet.
#[tauri::command]
async fn git_clone(
    app: tauri::AppHandle,
    url: String,
    name: String,
    parent_dir: String,
) -> Result<(String, String), String> {
    validate_git_url(&url)?;
    sanitize_project(&name)?;
    let parent = std::path::Path::new(&parent_dir)
        .canonicalize()
        .map_err(|e| format!("无效的父目录: {}", e))?;
    if !parent.is_dir() {
        return Err("父目录不存在或不是目录".to_string());
    }
    let target = parent.join(&name);
    if target.exists() {
        return Err(format!("目标目录已存在: {}", target.display()));
    }
    // Unique id lets the frontend match the completion event back to the exact
    // placeholder project it put in the sidebar.
    let id = uuid::Uuid::new_v4().simple().to_string();
    let id_for_task = id.clone();
    let name_for_task = name.clone();
    let target_for_task = target.clone();
    let app_for_task = app.clone();
    tauri::async_runtime::spawn(async move {
        let url_for_cmd = url.clone();
        let target_for_cmd = target_for_task.clone();
        let out = tauri::async_runtime::spawn_blocking(move || {
            git_gate::clone_into(&url_for_cmd, &target_for_cmd)
        })
        .await;
        let result: Result<(), String> = match out {
            Ok(Ok(o)) if o.status.success() => Ok(()),
            Ok(Ok(o)) => {
                let err = String::from_utf8_lossy(&o.stderr);
                Err(format!("git clone 失败: {}", err.trim()))
            }
            Ok(Err(e)) => Err(format!("git 启动失败: {}", e)),
            Err(e) => Err(format!("git clone 任务失败: {}", e)),
        };
        match result {
            Ok(()) => {
                // Persist the cloned folder as the project root so terminals
                // open there.
                let _ = persistence::write_project_root(&name_for_task, &target_for_task);
                let _ = app_for_task.emit(
                    "git://cloned",
                    GitCloneDone {
                        id: id_for_task,
                        name: name_for_task,
                        root: target_for_task.to_string_lossy().to_string(),
                    },
                );
            }
            Err(error) => {
                let _ = app_for_task.emit(
                    "git://clone-error",
                    GitCloneError {
                        id: id_for_task,
                        name: name_for_task,
                        error,
                    },
                );
            }
        }
    });
    Ok((id, target.to_string_lossy().to_string()))
}

#[tauri::command]
async fn git_push(repo: String) -> Result<(), String> {
    // No upstream yet → publish the current branch to origin and set upstream
    // (`git push -u origin HEAD`), matching VS Code's "Publish Branch". A plain
    // `git push` would fail with "no upstream branch" on a fresh local branch.
    let has_upstream = git_run(&repo, &["rev-parse", "--abbrev-ref", "@{upstream}"])
        .map(|s| !s.trim().is_empty() && s.trim() != "@{upstream}")
        .unwrap_or(false);
    if has_upstream {
        git_run(&repo, &["push"]).map(|_| ())
    } else {
        git_run(&repo, &["push", "-u", "origin", "HEAD"]).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_launch_overrides, create_project, delete_project_dir, git_run, parse_branches,
        parse_commit_detail, parse_log, parse_name_status, parse_porcelain, parse_ref_map,
        persistence, rename_project_inner, validate_branch_name, RuntimeOverride,
    };

    #[test]
    fn parses_porcelain_basic() {
        let text = " M src/lib.rs\nM  ui/App.tsx\n?? new-file.txt\nR  old.rs -> new.rs\n";
        let entries = parse_porcelain(text);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].index, " ");
        assert_eq!(entries[0].worktree, "M");
        assert_eq!(entries[0].path, "src/lib.rs");
        assert_eq!(entries[1].path, "ui/App.tsx");
        assert_eq!(entries[2].path, "new-file.txt");
        assert_eq!(entries[3].path, "new.rs");
    }

    #[test]
    fn parses_branches_porcelain() {
        let text = "* main\n  feature/foo\n+ other-worktree\n* (HEAD detached at abc1234)\n";
        let branches = parse_branches(text);
        assert_eq!(branches.len(), 3);
        assert!(branches[0].current);
        assert_eq!(branches[0].name, "main");
        assert!(!branches[1].current);
        assert_eq!(branches[1].name, "feature/foo");
        assert!(!branches[2].current);
        assert_eq!(branches[2].name, "other-worktree");
    }

    #[test]
    fn parses_log_lines_with_parents_email_and_refs_placeholder() {
        let text = "abc1234\u{1f}deadbeef f00f00\u{1f}Fix crash on startup\u{1f}Alice\u{1f}alice@x.dev\u{1f}1712345678\n\
                     d111111\u{1f}\u{1f}Add docs\u{1f}Bob\u{1f}bob@x.dev\u{1f}1712345600\n";
        let log = parse_log(text);
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].hash, "abc1234");
        assert_eq!(log[0].parents, vec!["deadbeef", "f00f00"]);
        assert_eq!(log[0].subject, "Fix crash on startup");
        assert_eq!(log[0].author, "Alice");
        assert_eq!(log[0].email, "alice@x.dev");
        assert_eq!(log[0].ts, 1712345678);
        // Root commit has an empty `%P` parent field.
        assert_eq!(log[1].parents, Vec::<String>::new());
        assert_eq!(log[1].subject, "Add docs");
        assert_eq!(log[1].author, "Bob");
    }

    #[test]
    fn parses_commit_detail_with_multiline_body() {
        let text = "abc1234\u{1f}Fix crash on startup\u{1f}First line of body.\nSecond line.\n\u{1f}Alice\u{1f}alice@x.dev\u{1f}1712345678\n";
        let d = parse_commit_detail(text).unwrap();
        assert_eq!(d.hash, "abc1234");
        assert_eq!(d.subject, "Fix crash on startup");
        assert_eq!(d.body, "First line of body.\nSecond line.");
        assert_eq!(d.author, "Alice");
        assert_eq!(d.email, "alice@x.dev");
        assert_eq!(d.ts, 1712345678);
        assert!(d.files.is_empty());
    }

    #[test]
    fn parse_commit_detail_rejects_empty_line() {
        assert!(parse_commit_detail("").is_err());
    }

    #[test]
    fn branch_name_validation_accepts_common_names() {
        for ok in [
            "main",
            "feature/foo",
            "fix-issue-42",
            "a_b",
            "v1.2.3",
            "user/very-long-name",
        ] {
            assert!(validate_branch_name(ok).is_ok(), "should accept {ok}");
        }
    }

    #[test]
    fn branch_name_validation_rejects_unsafe_names() {
        for bad in [
            "",
            "  ",
            "-flag",
            " has space",
            "has\ttab",
            "..",
            "foo..bar",
            "@{",
            "foo@{",
            "a~b",
            "a^b",
            "a:b",
            "a?b",
            "a*b",
            "a[b",
            "a\\b",
            "ends.lock",
            ".hidden",
            "trailing.",
        ] {
            assert!(validate_branch_name(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn parses_ref_map_branches_tags_and_remotes() {
        let text = "aaa refs/heads/main\n\
                    aaa refs/remotes/origin/main\n\
                    bbb refs/heads/feature/foo\n\
                    bbb refs/tags/v1.0\n\
                    ccc refs/remotes/origin/HEAD\n";
        let map = parse_ref_map(text);
        assert_eq!(
            map.get("aaa").unwrap(),
            &vec!["main".to_string(), "origin/main".to_string()],
            "local branch sorts first, then its remote-tracking twin"
        );
        assert_eq!(
            map.get("bbb").unwrap(),
            &vec!["feature/foo".to_string(), "tag: v1.0".to_string()]
        );
        assert_eq!(
            map.get("ccc").unwrap(),
            &vec!["origin/HEAD".to_string()],
            "remote-tracking refs are labeled so --all lanes stay readable"
        );
    }

    #[test]
    fn parses_name_status() {
        let text = "M\treadme.md\nA\tapp.js\nD\told.txt\nR100\told.rs\tnew.rs\n";
        let entries = parse_name_status(text);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].index, "M");
        assert_eq!(entries[0].path, "readme.md");
        assert_eq!(entries[1].index, "A");
        assert_eq!(entries[1].path, "app.js");
        assert_eq!(entries[2].index, "D");
        assert_eq!(entries[2].path, "old.txt");
        // Rename form: the new path is kept.
        assert_eq!(entries[3].index, "R100");
        assert_eq!(entries[3].path, "new.rs");
    }

    // Tests that point HOME at a temp dir must serialize — `workspace_root()`
    // reads the process-global HOME, and parallel tests would clobber each
    // other. Shares ONE lock with the runtime test modules (claude/codex/
    // opencode), which also mutate process-global env.
    use crate::agent_runtime::ENV_LOCK;

    #[test]
    fn custom_project_root_survives_without_agents() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Isolate from the real ~/CaPilot by pointing HOME at a temp dir. A
        // custom-rooted project's root must persist (project.json) even when the
        // project has zero agents — agent-meta based recovery needs one.
        let home = std::env::temp_dir().join(format!(
            "capilot_root_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);
        let root_folder = home.join("realproj");
        std::fs::create_dir_all(&root_folder).unwrap();

        let ret = create_project(
            "myproj".into(),
            Some(root_folder.to_string_lossy().to_string()),
        )
        .unwrap();
        assert_eq!(ret, root_folder.to_string_lossy());

        let ws = home.join("CaPilot/workspaces/myproj");
        assert!(ws.join("project.json").exists(), "root not persisted");
        // No agents present — the persisted root must still be recovered.
        assert_eq!(
            persistence::custom_project_root("myproj"),
            Some(root_folder)
        );

        std::env::remove_var("HOME");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn delete_project_removes_workspace_dir_only() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = std::env::temp_dir().join(format!(
            "capilot_delete_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("HOME", &home);
        let root_folder = home.join("realproj");
        std::fs::create_dir_all(&root_folder).unwrap();
        create_project(
            "proj".into(),
            Some(root_folder.to_string_lossy().to_string()),
        )
        .unwrap();
        let ws = home.join("CaPilot/workspaces/proj");
        assert!(ws.exists());

        delete_project_dir("proj").unwrap();
        // Workspace metadata dir is gone, the custom root folder is untouched.
        assert!(!ws.exists());
        assert!(root_folder.exists());

        std::env::remove_var("HOME");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn persistence_open_creates_no_default_project() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = std::env::temp_dir().join(format!(
            "capilot_persist_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("HOME", &home);
        // A leftover empty "default" scaffold dir must be cleaned up on open.
        let legacy = home.join("CaPilot/workspaces/default");
        std::fs::create_dir_all(legacy.join("context")).unwrap();

        let _p = persistence::Persistence::open().unwrap();
        assert!(
            !legacy.exists(),
            "scaffold 'default' project dir should not be re-created"
        );
        assert!(
            home.join("CaPilot/sessions.db").exists(),
            "sessions DB should live at ~/CaPilot/sessions.db"
        );

        std::env::remove_var("HOME");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn git_status_first_entry_not_misparsed_as_staged() {
        // Regression: git_run used to `.trim()` stdout, which eats the leading
        // status column of the FIRST `git status --porcelain` line (" M f" →
        // "M f"), so a worktree-modified file was split into staged. The raw
        // output must keep its leading space and parse as worktree-modified.
        let dir = std::env::temp_dir().join(format!(
            "capilot_git_status_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            out
        };
        run(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "v1").unwrap();
        run(&["add", "a.txt"]);
        run(&[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@test.dev",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
        std::fs::write(dir.join("a.txt"), "v2").unwrap();

        let text = git_run(dir.to_str().unwrap(), &["status", "--porcelain"]).unwrap();
        assert!(
            text.starts_with(" M "),
            "porcelain lost leading status column: {text:?}"
        );
        let entries = parse_porcelain(&text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].index, " ");
        assert_eq!(entries[0].worktree, "M");
        assert_eq!(entries[0].path, "a.txt");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rename_project_moves_dir_and_rewrites_state() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Isolate from the real ~/CaPilot by pointing HOME at a temp dir.
        let home = std::env::temp_dir().join(format!(
            "capilot_rename_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let old_dir = home.join("CaPilot/workspaces/oldproj");
        std::fs::create_dir_all(old_dir.join("agents/a1")).unwrap();
        std::fs::create_dir_all(old_dir.join("context")).unwrap();
        std::env::set_var("HOME", &home);

        let meta = persistence::AgentMeta {
            id: "a1".into(),
            workspace_id: Some("wks_a1".into()),
            runtime: "claude".into(),
            resume_key: None,
            backend_kind: persistence::BACKEND_KIND_LEGACY_PTY.into(),
            status: "idle".into(),
            cwd: old_dir.clone(),
            title: "w".into(),
            mode: "ask".into(),
            speed: "auto".into(),
            model: None,
            updated_at: 0,
        };
        persistence::write_agent_meta_to_dir(&old_dir.join("agents/a1"), &meta).unwrap();
        // Sessions live in the SINGLE top-level `~/CaPilot/sessions.db`.
        let pers = persistence::Persistence::open().unwrap();
        let db = pers.db_tolerant().unwrap();
        db.insert(&persistence::AgentSessionRecord {
            id: "a1".into(),
            workspace_id: Some("wks_a1".into()),
            project: "oldproj".into(),
            runtime: "claude".into(),
            resume_key: None,
            backend_kind: persistence::BACKEND_KIND_LEGACY_PTY.into(),
            cwd: old_dir.clone(),
            title: "w".into(),
            status: "idle".into(),
            mode: "ask".into(),
            speed: "auto".into(),
            model: None,
            created_at: 0,
            updated_at: 0,
        })
        .unwrap();
        drop(db);

        let new_root = rename_project_inner(&pers, "oldproj", "newproj").unwrap();
        let new_dir = home.join("CaPilot/workspaces/newproj");
        assert_eq!(new_root, new_dir.to_string_lossy());
        assert!(!old_dir.exists());
        assert!(new_dir.exists());

        // Agent metadata cwd rewritten to the renamed dir.
        let meta2 = persistence::read_agent_meta("newproj", "a1").unwrap();
        assert_eq!(meta2.cwd, new_dir);
        // Session row (top-level DB): project + cwd rewritten.
        let db2 = pers.db_tolerant().unwrap();
        let s = db2.get("a1").unwrap().unwrap();
        assert_eq!(s.project, "newproj");
        assert_eq!(s.cwd, new_dir);
        drop(db2);

        std::env::remove_var("HOME");
        std::fs::remove_dir_all(&home).ok();
    }

    /// The Settings → 已安装 → ⚙ launch override replaces the adapter's arg list
    /// wholesale. Regression: the re-append must keep the permission/speed flags
    /// and the status-hook injection (claude `--settings`, codex `-p` profile)
    /// on top of the user's args — dropping them silently killed hook reporting
    /// (false 空闲 during runs, false 运行中 on input echo). Mirrors the user's
    /// actual override config.
    #[test]
    fn launch_override_reappends_hooks_and_mode_flags() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        use crate::agent_runtime::adapter::{AgentRuntimeAdapter, AgentSession};
        use crate::agent_runtime::runtimes::claude::ClaudeAdapter;
        use crate::agent_runtime::runtimes::codex::CodexAdapter;
        use std::collections::HashMap;

        let prev_home = std::env::var_os("HOME");
        let prev_codex_home = std::env::var_os("CODEX_HOME");
        let base = std::env::temp_dir().join(format!(
            "capilot_override_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("HOME", &base);
        let codex_home = base.join(".codex");
        std::fs::create_dir_all(&codex_home).unwrap();
        std::env::set_var("CODEX_HOME", &codex_home);

        let overrides = HashMap::from([
            (
                "claude".to_string(),
                RuntimeOverride {
                    command: Some("claude".into()),
                    args: Some("--model claude-sonnet-5".into()),
                },
            ),
            (
                "codex".to_string(),
                RuntimeOverride {
                    command: Some("codex".into()),
                    args: Some("--no-alt-screen".into()),
                },
            ),
        ]);

        let session = AgentSession {
            id: "ovr".into(),
            runtime: "claude".into(),
            mode: "ask".into(),
            speed: "mid".into(),
            model: Some("claude-opus-5".into()),
            cwd: "/tmp/p".into(),
            context_dir: "/tmp/p".into(),
            rows: 24,
            cols: 80,
            resume_key: None,
        };

        // claude: the override's own args win over the adapter's model pick...
        let claude = ClaudeAdapter::new();
        let (cmd, args) = apply_launch_overrides(
            &claude,
            &session,
            &overrides,
            claude.spawn_interactive(&session).unwrap(),
        );
        assert_eq!(cmd, "claude");
        assert!(args.windows(2).any(|v| v == ["--model", "claude-sonnet-5"]));
        assert!(!args.windows(2).any(|v| v == ["--model", "claude-opus-5"]));
        // ...but the permission mode survives the replacement.
        assert!(args
            .windows(2)
            .any(|v| v == ["--permission-mode", "manual"]));
        // No status-hook injection: the `--settings` override (Phase 5
        // retirement) must not come back through the override path.
        assert!(!args.windows(2).any(|v| v[0] == "--settings"));

        // codex: the override drops alt-screen; no hook profile is re-appended.
        let codex_session = AgentSession {
            id: "ovr".into(),
            runtime: "codex".into(),
            ..session
        };
        let codex = CodexAdapter::new();
        let (cmd, args) = apply_launch_overrides(
            &codex,
            &codex_session,
            &overrides,
            codex.spawn_interactive(&codex_session).unwrap(),
        );
        assert_eq!(cmd, "codex");
        assert!(args.iter().any(|a| a == "--no-alt-screen"));
        assert!(!args.windows(2).any(|v| v == ["-p", "capilot-ovr"]));
        assert!(!args.iter().any(|a| a == "--dangerously-bypass-hook-trust"));

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
}

// ── Notification command ────────────────────────────────────────

/// Show a system notification for background IDE events.
/// The frontend `notify()` helper gates this on the 系统通知 toggle.
#[tauri::command]
fn notify(app: tauri::AppHandle, title: String, body: String) -> Result<(), String> {
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|e| e.to_string())
}

// ── Resource monitor commands ──────────────────────────────────

/// System-wide CPU + memory for the Computer Status panel.
#[derive(serde::Serialize)]
struct SystemStats {
    cpu_pct: f32,
    mem_used: u64,
    mem_total: u64,
}

#[tauri::command]
fn system_stats(monitor: tauri::State<'_, Arc<resource::ResourceMonitor>>) -> SystemStats {
    // Served from the sampler's per-tick cache — no re-scan of /proc, and no
    // lock contention with the sampler's own `System`.
    let (cpu_pct, mem_used, mem_total) = monitor.snapshot();
    SystemStats {
        cpu_pct,
        mem_used,
        mem_total,
    }
}

// ── App entry point ─────────────────────────────────────────────

/// Repair names created by older builds by keeping the first occurrence and
/// renaming later duplicates from the shared cat-name pool.
fn repair_session_titles(persistence: &Persistence) {
    let sessions = persistence
        .db()
        .lock()
        .ok()
        .and_then(|db| db.list_all().ok())
        .unwrap_or_default();
    let mut occupied = std::collections::HashSet::new();
    for session in sessions {
        let duplicate = !occupied.insert(session.title.clone());
        if !duplicate {
            continue;
        }
        let title = agent_runtime::cat_breeds::next_breed_excluding(&occupied).to_string();
        occupied.insert(title.clone());
        if let Ok(db) = persistence.db().lock() {
            let _ = db.update_title(&session.id, &title, now_ms());
        }
        if let Ok(mut meta) = read_agent_meta(&session.project, &session.id) {
            meta.title = title;
            meta.updated_at = now_ms();
            let _ = write_agent_meta(&session.project, &meta);
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let persistence = Arc::new(Persistence::open().expect("Failed to init persistence"));
    repair_session_titles(&persistence);
    // Startup repair: recreate any missing/corrupt `.agent-meta.json` from the
    // DB row (source of truth). Best-effort — a failed repair only logs.
    if let Err(e) = persistence.store().repair() {
        log::warn!("agent-meta startup repair failed: {e}");
    }
    let resource = Arc::new(resource::ResourceMonitor::new());

    let _app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(
            // Console/webview logging. The default filter would forward every
            // DEBUG/TRACE record from the HTTP stack — reqwest/rustls/hyper emit
            // "starting new connection" / "tunneling HTTPS over proxy" / CA-load
            // chatter on EVERY network call (usage fetch, updater) and drown the
            // terminal. Keep those crates' logs at/above Info; everything else
            // (including the app's own `log::warn!`) passes through untouched.
            tauri_plugin_log::Builder::new()
                .filter(|metadata| {
                    let target = metadata.target();
                    let noisy_http = target.starts_with("reqwest")
                        || target.starts_with("rustls")
                        || target.starts_with("hyper")
                        || target.starts_with("h2")
                        || target.starts_with("tower")
                        || target.starts_with("tungstenite")
                        || target.starts_with("want")
                        || target.starts_with("mio");
                    !noisy_http || metadata.level() <= log::LevelFilter::Info
                })
                .build(),
        )
        .plugin(tauri_plugin_single_instance::init(|_app, _args, _cwd| {}))
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(persistence)
        .manage(resource)
        .manage(ContextUsageCache::new())
        .invoke_handler(tauri::generate_handler![
            agent_spawn,
            agent_resume,
            agent_context_usage,
            agent_sync_events,
            agent_write,
            agent_kill,
            agent_resize,
            agent_set_session_config,
            agent_rename,
            agent_provider_list,
            agent_provider_diagnostic,
            agent_provider_catalog,
            agent_create,
            agent_resume_structured,
            agent_snapshot,
            agent_start_turn,
            agent_interrupt_turn,
            agent_set_config,
            agent_respond_permission,
            agent_close_structured,
            agent_list_structured,
            sessions_list,
            sessions_delete,
            setting_get,
            setting_set,
            workspace_root,
            create_project,
            list_projects,
            delete_project,
            rename_project,
            runtime_list_available,
            usage_fetch,
            usage_check,
            slash::agent_list_slash_items,
            slash::agent_list_slash_children,
            fs_read,
            fs_write,
            fs_list,
            fs_create_file,
            fs_create_dir,
            fs_paste,
            fs_delete,
            fs_rename,
            git_status,
            git_init,
            git_repo_info,
            git_stage,
            git_unstage,
            git_discard,
            git_discard_all,
            git_commit,
            git_branch,
            git_branches,
            git_checkout,
            git_log,
            git_show,
            git_show_commit,
            git_create_branch,
            git_switch_new,
            git_delete_branch,
            git_checkout_commit,
            git_pull,
            git_push,
            git_clone,
            notify,
            system_stats,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            // PTY bridge: prefer the user-level daemon, spawn one if needed,
            // fall back in-process only when no other owner can be proven (§8).
            let bridge = bridge::PtyBridge::start();
            bridge.attach_app(handle.clone());
            bridge.start_event_loop();
            app.manage(bridge.clone());
            log::info!("PTY bridge mode: {}", bridge.mode());
            // Resource sampler: every 3 s, sample each agent's process tree and
            // emit `resource://sample` (DevPlan §10). Runs against the bridge so
            // it reads live PIDs whether they live in-process or in the daemon.
            let resource = app
                .state::<Arc<resource::ResourceMonitor>>()
                .inner()
                .clone();
            resource::start_sampler(bridge, resource, handle);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |app_handle, event| {
            match event {
                // App quit (Phase 4 §9.4): DETACH, don't kill. The daemon and its
                // sessions keep running across GUI restarts; a reconnecting GUI
                // re-attaches to the same `(daemon_instance_id, agent_id,
                // generation, pid)`. In-process fallback has no daemon to keep
                // PTYs alive, so `detach` kills them there (same as the old
                // teardown — sessions stay `running` in the DB and resume next
                // launch).
                tauri::RunEvent::ExitRequested { .. } => {
                    let bridge = app_handle.state::<Arc<bridge::PtyBridge>>();
                    bridge.detach();
                }
                _ => {}
            }
        });
}
