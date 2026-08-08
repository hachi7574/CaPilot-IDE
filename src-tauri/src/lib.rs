mod agent_runtime;
pub mod esp;
mod orchestration;
mod persistence;
mod resource;

use agent_runtime::adapter::{AgentRole, AgentSession, AgentInfo, PermissionMode, Speed};
use agent_runtime::pty::{OnExit, PtyManager};
use agent_runtime::runtimes::{get_adapter, known_runtimes};
use esp::manager::EspManager;
use orchestration::dispatcher::CascadeMode;
use orchestration::Dispatcher;
use persistence::{agent_dir, ensure_project, project_dir, read_agent_meta, write_agent_meta, AgentMeta, AgentSessionRecord, Persistence, DEFAULT_PROJECT};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::ipc::Channel;
use tauri::{Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn parse_role(s: &str) -> AgentRole {
    match s {
        "master" => AgentRole::Master,
        "worker" => AgentRole::Worker,
        _ => AgentRole::Standalone,
    }
}

fn role_str(role: &AgentRole) -> &'static str {
    match role {
        AgentRole::Master => "master",
        AgentRole::Worker => "worker",
        AgentRole::Standalone => "standalone",
    }
}

fn parse_speed(s: &str) -> Speed {
    match s {
        "high" => Speed::High,
        "mid" => Speed::Mid,
        "fast" => Speed::Fast,
        _ => Speed::Auto,
    }
}

fn parse_mode(s: &str) -> PermissionMode {
    match s {
        "auto" => PermissionMode::Auto,
        "yolo" => PermissionMode::Yolo,
        _ => PermissionMode::Ask,
    }
}

fn mode_str(mode: &PermissionMode) -> &'static str {
    match mode {
        PermissionMode::Ask => "ask",
        PermissionMode::Auto => "auto",
        PermissionMode::Yolo => "yolo",
    }
}

fn speed_str(speed: &Speed) -> &'static str {
    match speed {
        Speed::High => "high",
        Speed::Mid => "mid",
        Speed::Fast => "fast",
        Speed::Auto => "auto",
    }
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
                Component::ParentDir | Component::CurDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("Invalid project name".to_string());
    }
    Ok(())
}

/// Prepend `~/CaPilot/bin` to PATH so every agent shell can invoke the
/// `capilot` orchestration shim (DevPlan §5.2).
fn capilot_path_env() -> Vec<(String, String)> {
    let home = std::env::var("HOME").unwrap_or_default();
    let bin_dir = std::path::PathBuf::from(&home).join("CaPilot").join("bin");
    match std::env::var("PATH") {
        Ok(path) => vec![("PATH".to_string(), format!("{}:{}", bin_dir.display(), path))],
        Err(_) => vec![("PATH".to_string(), bin_dir.to_string_lossy().to_string())],
    }
}

// ── Agent commands ──────────────────────────────────────────────

/// Settings KV key: what to do when a session's process exits on its own.
/// `"keep"` (default) marks it done (recoverable from the sidebar "已结束"
/// group, never auto-restored as a tab); `"delete"` removes the record entirely.
const SESSION_END_MODE_KEY: &str = "session_end_mode";

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

/// Wrap the natural-exit bookkeeping into an `OnExit` for `PtyManager.spawn`.
/// Fired only on natural exit (EOF / read error); intentional kills never reach
/// it. Reads the session-end setting fresh each time, so a settings change
/// applies without an app restart.
fn build_on_exit(
    persistence: Arc<Persistence>,
    dispatcher: Arc<Dispatcher>,
    app: tauri::AppHandle,
) -> OnExit {
    Arc::new(move |agent_id, exit_code| {
        dispatcher.worker_ended_naturally(&agent_id, exit_code, &app);
        let is_master = persistence
            .db()
            .lock()
            .ok()
            .and_then(|db| db.get(&agent_id).ok().flatten())
            .is_some_and(|record| record.role == "master");
        // Poisoned lock / read error → default to "keep" so a session is never
        // silently dropped because of a transient DB failure.
        let keep = persistence
            .db()
            .lock()
            .map(|db| {
                db.get_setting(SESSION_END_MODE_KEY)
                    .ok()
                    .flatten()
                    .as_deref()
                    != Some("delete")
            })
            .unwrap_or(true);
        if keep {
            if let Ok(db) = persistence.db().lock() {
                let _ = db.update_status(&agent_id, "done", now_ms());
            }
            // Keep the per-agent meta in sync (dual-write convention) so a
            // stale `.agent-meta.json` never shows a finished session as running.
            let project = persistence
                .db()
                .lock()
                .ok()
                .and_then(|db| db.get(&agent_id).ok().flatten())
                .map(|rec| rec.project);
            if let Some(project) = project {
                if let Ok(mut meta) = read_agent_meta(&project, &agent_id) {
                    meta.status = "done".to_string();
                    meta.updated_at = now_ms();
                    let _ = write_agent_meta(&project, &meta);
                }
            }
            let _ = app.emit(
                "agent://exited",
                AgentExited {
                    id: agent_id.clone(),
                    exit_code,
                },
            );
            if is_master {
                dispatcher.cascade_master(&agent_id, CascadeMode::Keep, &app);
            }
        } else {
            if is_master {
                dispatcher.cascade_master(&agent_id, CascadeMode::Delete, &app);
            }
            // Delete mode: read the row's CURRENT project (not a value captured
            // at spawn — a project rename moves the agent dir, and the stale
            // name would leave the new dir orphaned).
            let project = persistence
                .db()
                .lock()
                .ok()
                .and_then(|db| db.get(&agent_id).ok().flatten())
                .map(|rec| rec.project)
                .unwrap_or_default();
            if let Ok(db) = persistence.db().lock() {
                let _ = db.delete(&agent_id);
            }
            let dir = agent_dir(&project, &agent_id);
            if dir.starts_with(persistence::workspace_root()) && dir.exists() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            let _ = app.emit("agent://removed", AgentRemoved { id: agent_id });
        }
    })
}

/// Shared spawn path used by `agent_spawn` (new) and `agent_resume` (restored).
#[allow(clippy::too_many_arguments)]
fn build_and_spawn(
    pty: &Arc<PtyManager>,
    persistence: &Arc<Persistence>,
    dispatcher: &Arc<Dispatcher>,
    app: &tauri::AppHandle,
    id: &str,
    project: &str,
    role: AgentRole,
    runtime: &str,
    resume: bool,
    resume_key: Option<String>,
    model: Option<String>,
    speed: &str,
    mode: &str,
    cwd: PathBuf,
    on_data: Channel<Vec<u8>>,
) -> Result<AgentInfo, String> {
    let adapter = get_adapter(runtime);
    if !adapter.is_available() {
        return Err(format!("Runtime '{}' is not available", runtime));
    }

    let session = AgentSession {
        id: id.to_string(),
        runtime: runtime.to_string(),
        mode: parse_mode(mode),
        speed: parse_speed(speed),
        model,
        cwd: cwd.clone(),
        context_dir: cwd.clone(),
        role: role.clone(),
        rows: 24,
        cols: 80,
        resume_key: resume_key.clone(),
    };

    let (cmd, mut args) = adapter
        .spawn_interactive(&session)
        .map_err(|e| format!("Failed to build command: {}", e))?;
    // Resume an existing conversation in the same context dir — only when the
    // caller asked for a resume (restored session / runtime switch). A brand-new
    // spawn stays fresh so it can never hijack the newest session in a shared
    // cwd (e.g. two claude terminals in one custom-rooted project).
    let resume_args = if resume { adapter.resume_args(&session) } else { vec![] };
    let detected_key = (!resume_args.is_empty())
        .then(|| resume_args.last().cloned().filter(|s| s != "--resume"))
        .flatten();
    if !resume_args.is_empty() {
        args.extend(resume_args);
    }

    let mut info = pty
        .spawn(
            id.to_string(),
            &cmd,
            &args,
            &cwd,
            24,
            80,
            on_data,
            Some(build_on_exit(
                persistence.clone(),
                dispatcher.clone(),
                app.clone(),
            )),
            &capilot_path_env(),
        )
        .map_err(|e| e.to_string())?;
    info.runtime = runtime.to_string();
    info.role = role.clone();
    info.mode = mode_str(&session.mode).to_string();
    info.speed = speed_str(&session.speed).to_string();
    info.model = session.model.clone();
    // New terminals are named after a random TICA cat breed (布偶 / 奥西 / …)
    // so they're friendly and memorable; the pinned master keeps its role label.
    if role == AgentRole::Master {
        info.title = format!("{}@{}", adapter.name(), role_str(&role));
    } else {
        info.title = agent_runtime::cat_breeds::next_breed().to_string();
    }

    // Persist metadata + session (best-effort; PTY already running).
    let now = now_ms();
    // The stored key is the provider session to continue on the next launch.
    // Fresh spawns have no session yet — agent_spawn's background capture fills
    // it in shortly after; resume carries the explicit key or the detected one.
    let persisted_key = session
        .resume_key
        .clone()
        .or_else(|| detected_key.clone());
    let meta = AgentMeta {
        id: id.to_string(),
        role: role_str(&role).to_string(),
        runtime: runtime.to_string(),
        resume_key: persisted_key.clone(),
        status: "running".to_string(),
        cwd: cwd.clone(),
        title: info.title.clone(),
        mode: mode.to_string(),
        speed: speed.to_string(),
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
        project: project.to_string(),
        role: role_str(&role).to_string(),
        runtime: runtime.to_string(),
        resume_key: persisted_key,
        cwd: cwd.clone(),
        title: info.title.clone(),
        status: "running".to_string(),
        mode: mode.to_string(),
        speed: speed.to_string(),
        model: session.model.clone(),
        created_at: now,
        updated_at: now,
    };
    if let Ok(db) = persistence.db().lock() {
        if let Err(e) = db.insert(&record) {
            log::warn!("failed to persist session {id}: {e}");
        }
    }

    match role {
        AgentRole::Worker => dispatcher.register_worker(id),
        AgentRole::Master => dispatcher.set_master(Some(id.to_string())),
        _ => {}
    }

    Ok(info)
}

#[tauri::command]
async fn agent_spawn(
    pty: tauri::State<'_, Arc<PtyManager>>,
    persistence: tauri::State<'_, Arc<Persistence>>,
    dispatcher: tauri::State<'_, Arc<Dispatcher>>,
    app: tauri::AppHandle,
    runtime: String,
    role: String,
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
    // Session cap: a compromised frontend (or runaway automation) must not be
    // able to spawn unbounded PTYs and exhaust the machine.
    const MAX_LIVE_SESSIONS: usize = 64;
    if pty.live_count() >= MAX_LIVE_SESSIONS {
        return Err(format!(
            "会话数已达上限 ({MAX_LIVE_SESSIONS})，请先关闭部分终端"
        ));
    }
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

    let role = parse_role(&role);

    // PTY working directory: custom-rooted agents open a terminal directly in
    // the project root (cloned repo / picked folder); workspace projects keep
    // the per-agent dir so each session's context stays isolated. Master-group
    // terminals use a short top-level dir (~/CaPilot/Master) so the bash prompt
    // isn't a long workspaces/master/agents/<uuid> path.
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
        None if role == AgentRole::Master => project_dir(&project),
        None if project == "master" => {
            let dir = persistence::workspace_root()
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join("Master");
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("Failed to create master terminal dir: {}", e))?;
            dir
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
    let cwd_for_capture = cwd.clone();
    let info = build_and_spawn(
        pty.inner(),
        persistence.inner(),
        dispatcher.inner(),
        &app,
        &agent_id,
        &project,
        role,
        &runtime,
        resume,
        resume_key,
        model,
        &speed.unwrap_or_else(|| "auto".to_string()),
        &mode.unwrap_or_else(|| "ask".to_string()),
        cwd,
        on_data,
    )?;

    // Best-effort: capture the provider session id the freshly-started process
    // creates, so a later restart can resume this exact conversation instead of
    // re-detecting (which would collide when several agents share one cwd, e.g.
    // a custom-rooted project). Runs in the background so spawn returns at once.
    if get_adapter(&runtime).supports_resume() {
        let persistence = persistence.inner().clone();
        let project = project.clone();
        let agent_id = agent_id.clone();
        let runtime = runtime.clone();
        tokio::spawn(async move {
            let adapter = get_adapter(&runtime);
            for _ in 0..5 {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                // If the session was deleted mid-poll, stop before rewriting
                // .agent-meta.json (which would recreate the removed dir).
                let still_exists = persistence
                    .db()
                    .lock()
                    .ok()
                    .and_then(|db| db.get(&agent_id).ok().flatten())
                    .is_some();
                if !still_exists {
                    break;
                }
                if let Some(key) = adapter.capture_resume_key(&cwd_for_capture) {
                    if let Ok(db) = persistence.db().lock() {
                        let _ = db.update_resume_key(&agent_id, &key, now_ms());
                    }
                    if let Ok(mut meta) = read_agent_meta(&project, &agent_id) {
                        meta.resume_key = Some(key);
                        meta.updated_at = now_ms();
                        let _ = write_agent_meta(&project, &meta);
                    }
                    break;
                }
            }
        });
    }

    Ok(info)
}

#[tauri::command]
async fn agent_resume(
    pty: tauri::State<'_, Arc<PtyManager>>,
    persistence: tauri::State<'_, Arc<Persistence>>,
    dispatcher: tauri::State<'_, Arc<Dispatcher>>,
    app: tauri::AppHandle,
    id: String,
    on_data: Channel<Vec<u8>>,
) -> Result<AgentInfo, String> {
    let record = {
        let db = persistence.db().lock().unwrap();
        db.get(&id).map_err(|e| e.to_string())?
    };
    let Some(rec) = record else {
        return Err(format!("Session not found: {}", id));
    };

    // Kill any leftover PTY before re-spawning.
    pty.kill(&id).map_err(|e| e.to_string())?;
    let role = parse_role(&rec.role);
    build_and_spawn(
        pty.inner(),
        persistence.inner(),
        dispatcher.inner(),
        &app,
        &id,
        &rec.project,
        role,
        &rec.runtime,
        true, // resume — continue the stored/detected conversation
        rec.resume_key.clone(),
        rec.model.clone(),
        &rec.speed,
        &rec.mode,
        rec.cwd.clone(),
        on_data,
    )
}

#[tauri::command]
async fn agent_write(
    pty: tauri::State<'_, Arc<PtyManager>>,
    id: String,
    data: String,
    raw: Option<bool>,
) -> Result<(), String> {
    // DevPlan §4.2: composer send = pty_write(文本 + \r) — Enter submits the TUI
    // input line. `raw: true` is used by the xterm panel for keystroke passthrough.
    let payload = if raw.unwrap_or(false) {
        data
    } else {
        format!("{}\r", data)
    };
    pty.write(&id, payload.as_bytes()).map_err(|e| e.to_string())
}

#[tauri::command]
async fn agent_kill(
    pty: tauri::State<'_, Arc<PtyManager>>,
    persistence: tauri::State<'_, Arc<Persistence>>,
    id: String,
) -> Result<(), String> {
    pty.kill(&id).map_err(|e| e.to_string())?;
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
    pty: tauri::State<'_, Arc<PtyManager>>,
    id: String,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    pty.resize(&id, rows, cols).map_err(|e| e.to_string())
}

/// Switch a tab's runtime: kill old PTY → respawn same context dir with the
/// new runtime → resume session history (DevPlan §4.8).
#[tauri::command]
async fn agent_switch_runtime(
    pty: tauri::State<'_, Arc<PtyManager>>,
    persistence: tauri::State<'_, Arc<Persistence>>,
    dispatcher: tauri::State<'_, Arc<Dispatcher>>,
    app: tauri::AppHandle,
    id: String,
    runtime: String,
    on_data: Channel<Vec<u8>>,
) -> Result<AgentInfo, String> {
    let record = {
        let db = persistence.db().lock().unwrap();
        db.get(&id).map_err(|e| e.to_string())?
    };
    let Some(rec) = record else {
        return Err(format!("Session not found: {}", id));
    };

    // Validate the target runtime BEFORE killing the old PTY so a failed
    // switch can't brick the session (DevPlan §4.8).
    let adapter = get_adapter(&runtime);
    if !adapter.is_available() {
        return Err(format!(
            "Cannot switch to '{}': runtime not available",
            runtime
        ));
    }

    pty.kill(&id).map_err(|e| e.to_string())?;
    {
        let db = persistence.db().lock().unwrap();
        db.update_runtime(&id, &runtime, now_ms()).map_err(|e| e.to_string())?;
    }
    let project = rec.project.clone();
    if let Ok(mut meta) = read_agent_meta(&project, &id) {
        meta.runtime = runtime.clone();
        meta.updated_at = now_ms();
        let _ = write_agent_meta(&project, &meta);
    }

    let role = parse_role(&rec.role);
    build_and_spawn(
        pty.inner(),
        persistence.inner(),
        dispatcher.inner(),
        &app,
        &id,
        &project,
        role,
        &runtime,
        true, // runtime switch resumes session history in the same context dir
        rec.resume_key.clone(),
        rec.model.clone(),
        &rec.speed,
        &rec.mode,
        rec.cwd.clone(),
        on_data,
    )
}

#[tauri::command]
async fn agent_set_role(
    persistence: tauri::State<'_, Arc<Persistence>>,
    dispatcher: tauri::State<'_, Arc<Dispatcher>>,
    id: String,
    role: String,
) -> Result<(), String> {
    let role = parse_role(&role);
    let role_s = role_str(&role).to_string();
    // The agent's own project (from its DB row) — agent metadata lives under
    // `workspaces/<project>/agents/<id>`, and the row's project is the truth.
    let project = {
        let db = persistence.db().lock().unwrap();
        db.get(&id)
            .map(|r| r.map(|rec| rec.project).unwrap_or_default())
            .map_err(|e| e.to_string())?
    };
    {
        let db = persistence.db().lock().unwrap();
        db.update_role(&id, &role_s, now_ms()).map_err(|e| e.to_string())?;
    }
    if let Ok(mut meta) = read_agent_meta(&project, &id) {
        meta.role = role_s.clone();
        meta.updated_at = now_ms();
        let _ = write_agent_meta(&project, &meta);
    }
    match role {
        AgentRole::Worker => dispatcher.register_worker(&id),
        AgentRole::Master => {
            dispatcher.set_master(Some(id.clone()));
            dispatcher.unregister_worker(&id);
        }
        AgentRole::Standalone => dispatcher.unregister_worker(&id),
    }
    Ok(())
}

/// Update a session's composer config (permission mode / speed / model).
/// Persists to the DB row + `.agent-meta.json` so the next `agent_resume` uses
/// the new values; the running PTY is intentionally NOT touched (no restart, no
/// interruption). Omitted fields keep their current value.
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
        let mode = match mode {
            Some(m) if matches!(m.as_str(), "ask" | "auto" | "yolo") => m,
            _ => rec.mode.clone(),
        };
        let speed = match speed {
            Some(s) if matches!(s.as_str(), "high" | "mid" | "fast" | "auto") => s,
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
    for entry in std::fs::read_dir(&root)
        .map_err(|e| format!("Failed to read workspaces dir: {}", e))?
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
        let project_root = persistence::custom_project_root(&name)
            .unwrap_or_else(|| root.join(&name));
        projects.push(ProjectInfo {
            root: project_root.to_string_lossy().to_string(),
            name,
        });
    }
    projects.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(projects)
}

/// Delete a project's workspace directory (`~/CaPilot/workspaces/<name>`) —
/// sessions, agent metadata, context. Called by the sidebar's "移除项目".
/// Custom-rooted projects only lose this metadata dir; their real folder
/// (picked / cloned) is never touched. The pinned master group is guarded.
#[tauri::command]
fn delete_project(name: String) -> Result<(), String> {
    if name == "master" {
        return Err("不能删除 master".to_string());
    }
    sanitize_project(&name)?;
    let dir = persistence::project_dir(&name);
    // Belt-and-braces: the resolved path must stay under the workspace root.
    if !dir.starts_with(persistence::workspace_root()) {
        return Err("非法路径".to_string());
    }
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("删除项目目录失败: {}", e))?;
    }
    Ok(())
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
    if old == "master" {
        return Err("不能重命名 master".to_string());
    }
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
    pty: tauri::State<'_, Arc<PtyManager>>,
    persistence: tauri::State<'_, Arc<Persistence>>,
    dispatcher: tauri::State<'_, Arc<Dispatcher>>,
    app: tauri::AppHandle,
    id: String,
) -> Result<(), String> {
    // Best-effort end to end: a failed kill (e.g. the PTY was already reaped by
    // the reader task) must not skip session cleanup, or the DB row survives and
    // the terminal resurrects on the next restart.
    let _ = pty.kill(&id);
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
    if rec.role == "master" {
        dispatcher.cascade_master(&id, CascadeMode::Delete, &app);
    }
    let dir = agent_dir(&rec.project, &id);
    // Belt-and-braces: the resolved dir must stay under the workspace root.
    if dir.starts_with(persistence::workspace_root()) && dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    if let Some(db) = persistence.db_tolerant() {
        let _ = db.delete(&id);
    }
    dispatcher.unregister_worker(&id);
    Ok(())
}

// ── Settings KV commands ─────────────────────────────────────────

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
    const ALLOWED: &[&str] = &[SESSION_END_MODE_KEY];
    if !ALLOWED.contains(&key.as_str()) {
        return Err(format!("unknown setting key: {}", key));
    }
    let db = persistence.db().lock().unwrap();
    db.set_setting(&key, &value).map_err(|e| e.to_string())
}

// ── Orchestration commands ──────────────────────────────────────

#[tauri::command]
async fn worker_status(
    dispatcher: tauri::State<'_, Arc<Dispatcher>>,
) -> Result<Vec<orchestration::dispatcher::WorkerInfo>, String> {
    Ok(dispatcher.workers_list())
}

#[tauri::command]
async fn smart_return_set(
    dispatcher: tauri::State<'_, Arc<Dispatcher>>,
    enabled: bool,
) -> Result<(), String> {
    dispatcher.set_smart_return(enabled);
    Ok(())
}

#[tauri::command]
async fn smart_return_get(
    dispatcher: tauri::State<'_, Arc<Dispatcher>>,
) -> Result<bool, String> {
    Ok(dispatcher.smart_return_enabled())
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
        });
    }
    out
}

/// Models a runtime can offer — powers the composer `[模型↑]` switcher
/// (DevPlan §3.2).
#[tauri::command]
async fn runtime_models(runtime: String) -> Vec<agent_runtime::adapter::ModelInfo> {
    get_adapter(&runtime).list_models()
}

// ── Filesystem commands ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct FsEntryBrief {
    pub name: String,
    pub is_dir: bool,
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
    let read_dir = std::fs::read_dir(&resolved)
        .map_err(|e| format!("Failed to read directory: {}", e))?;
    for entry in read_dir {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Failed to read file type: {}", e))?;
        entries.push(FsEntryBrief {
            name: entry.file_name().to_string_lossy().to_string(),
            is_dir: file_type.is_dir(),
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
fn copy_dir_recursive(
    src: &std::path::Path,
    dest: &std::path::Path,
) -> std::io::Result<()> {
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
    let parent = raw.parent().ok_or_else(|| "无效路径：无父目录".to_string())?;
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
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git failed: {}", e))?;
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
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    let home_path = std::path::PathBuf::from(&home);
    let resolved = std::path::Path::new(repo)
        .canonicalize()
        .map_err(|e| format!("Invalid repo path: {}", e))?;
    if !resolved.starts_with(&home_path) || !resolved.is_dir() {
        return Err("repo path escapes allowed directories".to_string());
    }
    Ok(resolved)
}

/// Stream-count lines in a file without loading it into memory (a huge untracked
/// file would otherwise be read whole just to report `+N`). Capped at 1M lines so
/// a pathological file can't stall `git_status`.
fn count_lines(path: &std::path::Path) -> i32 {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return 0;
    };
    std::io::BufReader::new(file).lines().take(1_000_000).count() as i32
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
    pub hash: String,
    pub subject: String,
    pub author: String,
    pub ts: i64,
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

/// Parse `git log --pretty=format:%h%x1f%s%x1f%an%x1f%ct` output. Each commit
/// is one line; `%x1f` (unit separator) delimits hash/subject/author/ts.
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
        let subject = parts.next().unwrap_or("").trim().to_string();
        let author = parts.next().unwrap_or("").trim().to_string();
        let ts = parts.next().unwrap_or("0").trim().parse::<i64>().unwrap_or(0);
        entries.push(GitLogEntry {
            hash,
            subject,
            author,
            ts,
        });
    }
    entries
}

#[tauri::command]
async fn git_status(dir: String) -> Result<Vec<GitEntry>, String> {
    let text = git_run(&dir, &["status", "--porcelain"])?;
    let mut entries = parse_porcelain(&text);

    // Per-file line counts: staged (--cached) + unstaged diffs.
    let mut counts: std::collections::HashMap<String, (i32, i32)> =
        std::collections::HashMap::new();
    for args in [&["diff", "--cached", "--numstat"][..], &["diff", "--numstat"][..]] {
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

/// Files changed in the HEAD commit — the Git panel's "已提交的更改" group.
/// `git diff-tree` lists them with their change char (A/M/D). Fails on a repo
/// with no commits yet; the frontend treats that as an empty list.
#[tauri::command]
async fn git_committed(repo: String) -> Result<Vec<GitEntry>, String> {
    let text = git_run(
        &repo,
        &["diff-tree", "--no-commit-id", "--name-status", "-r", "HEAD"],
    )?;
    Ok(parse_name_status(&text))
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
    let is_repo = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
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
            std::fs::remove_file(&p)
                .map_err(|e| format!("删除未跟踪文件失败 {}: {}", f, e))?;
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

/// Recent commit history (short hash, subject, author, timestamp) for the Git
/// panel's "提交历史" section (DevPlan §7.4B). Defaults to the latest 20.
#[tauri::command]
async fn git_log(repo: String, count: Option<i32>) -> Result<Vec<GitLogEntry>, String> {
    let n = count.unwrap_or(20).clamp(1, 200).to_string();
    let text = git_run(
        &repo,
        &[
            "log",
            "--pretty=format:%h%x1f%s%x1f%an%x1f%ct",
            "-n",
            n.as_str(),
        ],
    )?;
    Ok(parse_log(&text))
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
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .arg("show")
        .arg(&spec)
        .output()
        .map_err(|e| format!("git failed: {}", e))?;
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
        return Err("不支持的 Git 地址（仅支持 http://、https://、ssh://、git@、git://）".to_string());
    }
    Ok(())
}

/// Clone a remote git repository into `<parent_dir>/<name>`. The parent dir must
/// already exist; the URL is validated and passed via `Command::arg` (no shell)
/// after `--`, so a `-`-prefixed URL is never treated as a flag. Runs off the
/// async runtime so a slow network clone doesn't block other IPC. On success
/// returns the clone dir.
#[tauri::command]
async fn git_clone(url: String, name: String, parent_dir: String) -> Result<String, String> {
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
    let target_for_cmd = target.clone();
    let out = tauri::async_runtime::spawn_blocking(move || {
        std::process::Command::new("git")
            .arg("clone")
            .arg("--")
            .arg(&url)
            .arg(&target_for_cmd)
            .output()
    })
    .await
    .map_err(|e| format!("git clone 任务失败: {}", e))?
    .map_err(|e| format!("git 启动失败: {}", e))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git clone 失败: {}", err.trim()));
    }
    // Persist the cloned folder as the project root so terminals open there.
    let _ = persistence::write_project_root(&name, &target);
    Ok(target.to_string_lossy().to_string())
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
        create_project, delete_project, git_run, parse_branches, parse_log,
        parse_name_status, parse_porcelain, persistence, rename_project,
        rename_project_inner,
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
    fn parses_log_lines() {
        let text =
            "abc1234\u{1f}Fix crash on startup\u{1f}Alice\u{1f}1712345678\nd111111\u{1f}Add docs\u{1f}Bob\u{1f}1712345600\n";
        let log = parse_log(text);
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].hash, "abc1234");
        assert_eq!(log[0].subject, "Fix crash on startup");
        assert_eq!(log[0].author, "Alice");
        assert_eq!(log[0].ts, 1712345678);
        assert_eq!(log[1].hash, "d111111");
        assert_eq!(log[1].subject, "Add docs");
        assert_eq!(log[1].author, "Bob");
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
    // reads the process-global HOME, and parallel tests would clobber each other.
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn custom_project_root_survives_without_agents() {
        let _guard = HOME_LOCK.lock().unwrap();
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
        let _guard = HOME_LOCK.lock().unwrap();
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

        delete_project("proj".into()).unwrap();
        // Workspace metadata dir is gone, the custom root folder is untouched.
        assert!(!ws.exists());
        assert!(root_folder.exists());

        // master is guarded.
        assert!(delete_project("master".into()).is_err());

        std::env::remove_var("HOME");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn persistence_open_creates_no_default_project() {
        let _guard = HOME_LOCK.lock().unwrap();
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
            "-c", "user.name=test",
            "-c", "user.email=test@test.dev",
            "commit", "-q", "-m", "init",
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
        let _guard = HOME_LOCK.lock().unwrap();
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
            role: "worker".into(),
            runtime: "claude".into(),
            resume_key: None,
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
            project: "oldproj".into(),
            role: "worker".into(),
            runtime: "claude".into(),
            resume_key: None,
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
}

// ── Notification command ────────────────────────────────────────

/// Show a system notification (worker done / ESP drop / report ready).
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

// ── ESP commands ────────────────────────────────────────────────

#[tauri::command]
async fn esp_connect(
    state: tauri::State<'_, EspManager>,
    app: tauri::AppHandle,
) -> Result<esp::transport::EspStatus, String> {
    state.connect_ble(app).await.map_err(|e| e.to_string())?;
    Ok(state.status().await)
}

#[tauri::command]
async fn esp_disconnect(state: tauri::State<'_, EspManager>) -> Result<(), String> {
    state.disconnect().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn esp_status(state: tauri::State<'_, EspManager>) -> Result<esp::transport::EspStatus, String> {
    Ok(state.status().await)
}

#[tauri::command]
async fn esp_send(
    state: tauri::State<'_, EspManager>,
    payload: String,
) -> Result<(), String> {
    state.send_command(payload.as_bytes()).await.map_err(|e| e.to_string())
}

// ── Resource monitor commands ──────────────────────────────────

/// Buffered per-agent CPU/MEM history (oldest → newest) for the curve overlay.
#[tauri::command]
fn resource_history(
    monitor: tauri::State<'_, Arc<resource::ResourceMonitor>>,
    agent_id: String,
) -> Vec<resource::HistoryPoint> {
    monitor.history(&agent_id)
}

/// System-wide CPU + memory for the Computer Status panel.
#[derive(serde::Serialize)]
struct SystemStats {
    cpu_pct: f32,
    mem_used: u64,
    mem_total: u64,
}

#[tauri::command]
fn system_stats(
    monitor: tauri::State<'_, Arc<resource::ResourceMonitor>>,
) -> SystemStats {
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let pty = Arc::new(PtyManager::new());
    // Clone kept for the exit handler so the app can kill every agent PTY on
    // quit (no orphaned claude/bash), without touching their session rows.
    let pty_killer = pty.clone();
    let persistence = Arc::new(
        Persistence::open().expect("Failed to init persistence"),
    );
    let dispatcher = Arc::new(Dispatcher::new(pty.clone(), persistence.clone()));
    dispatcher.refresh_workers();
    let resource = Arc::new(resource::ResourceMonitor::new());
    // Install the `capilot` PATH shim (best-effort).
    match orchestration::shim::install_shim() {
        Ok(p) => log::info!("capilot shim installed at {}", p.display()),
        Err(e) => log::warn!("failed to install capilot shim: {e}"),
    }

    let _app = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_log::Builder::new().build())
        .plugin(tauri_plugin_single_instance::init(|_app, _args, _cwd| {}))
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(pty)
        .manage(persistence)
        .manage(dispatcher.clone())
        .manage(EspManager::new())
        .manage(resource)
        .invoke_handler(tauri::generate_handler![
            agent_spawn,
            agent_resume,
            agent_write,
            agent_kill,
            agent_resize,
            agent_switch_runtime,
            agent_set_role,
            agent_set_session_config,
            sessions_list,
            sessions_delete,
            setting_get,
            setting_set,
            workspace_root,
            create_project,
            list_projects,
            delete_project,
            rename_project,
            worker_status,
            smart_return_set,
            smart_return_get,
            runtime_list_available,
            runtime_models,
            fs_read,
            fs_write,
            fs_list,
            fs_create_file,
            fs_create_dir,
            fs_paste,
            fs_delete,
            fs_rename,
            git_status,
            git_committed,
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
            git_pull,
            git_push,
            git_clone,
            notify,
            esp_connect,
            esp_disconnect,
            esp_status,
            esp_send,
            resource_history,
            system_stats,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            dispatcher.start(handle.clone());
            // Resource sampler: every 3 s, sample each agent's process tree and
            // emit `resource://sample` (DevPlan §10).
            let pty = app.state::<Arc<PtyManager>>().inner().clone();
            let resource = app.state::<Arc<resource::ResourceMonitor>>().inner().clone();
            resource::start_sampler(pty, resource, handle);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |_app_handle, event| {
            match event {
                // App quit: kill every live PTY so no claude/bash is orphaned.
                // Intentional teardown → sessions stay `running` in the DB and
                // resume normally next launch.
                tauri::RunEvent::ExitRequested { .. } => {
                    pty_killer.kill_all();
                }
                _ => {}
            }
        });
}
