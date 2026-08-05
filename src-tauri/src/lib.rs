mod agent_runtime;
pub mod esp;
mod orchestration;
mod persistence;

use agent_runtime::adapter::{AgentRole, AgentSession, AgentInfo, PermissionMode, Speed};
use agent_runtime::pty::PtyManager;
use agent_runtime::runtimes::{get_adapter, known_runtimes};
use esp::manager::EspManager;
use orchestration::Dispatcher;
use persistence::{agent_dir, ensure_project, project_dir, read_agent_meta, write_agent_meta, AgentMeta, AgentSessionRecord, Persistence, DEFAULT_PROJECT};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::ipc::Channel;

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

/// Shared spawn path used by `agent_spawn` (new) and `agent_resume` (restored).
#[allow(clippy::too_many_arguments)]
fn build_and_spawn(
    pty: &Arc<PtyManager>,
    persistence: &Persistence,
    dispatcher: &Dispatcher,
    id: &str,
    project: &str,
    role: AgentRole,
    runtime: &str,
    resume_key: Option<String>,
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
        mode: PermissionMode::Ask,
        speed: Speed::Auto,
        cwd: cwd.clone(),
        context_dir: cwd.clone(),
        role: role.clone(),
        rows: 24,
        cols: 80,
    };

    let (cmd, mut args) = adapter
        .spawn_interactive(&session)
        .map_err(|e| format!("Failed to build command: {}", e))?;
    // Resume an existing conversation in the same context dir. Claude detects
    // the most recent session in cwd; other runtimes use the stored key.
    let resume_args = adapter.resume_args(&session);
    let detected_key = if resume_args.is_empty() {
        None
    } else {
        resume_args.last().cloned().filter(|s| s != "--resume")
    };
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
            &capilot_path_env(),
        )
        .map_err(|e| e.to_string())?;
    info.runtime = runtime.to_string();
    info.role = role.clone();
    info.title = format!("{}@{}", adapter.name(), role_str(&role));

    // Persist metadata + session (best-effort; PTY already running).
    let now = now_ms();
    let persisted_key = resume_key.clone().or_else(|| detected_key.clone());
    let meta = AgentMeta {
        id: id.to_string(),
        role: role_str(&role).to_string(),
        runtime: runtime.to_string(),
        resume_key: persisted_key.clone(),
        status: "running".to_string(),
        cwd: cwd.clone(),
        title: info.title.clone(),
        updated_at: now,
    };
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
    runtime: String,
    role: String,
    project: String,
    resume_key: Option<String>,
    on_data: Channel<Vec<u8>>,
) -> Result<AgentInfo, String> {
    let agent_id = uuid::Uuid::new_v4().to_string();
    let project = if project.is_empty() {
        persistence.project().to_string()
    } else {
        project
    };
    ensure_project(&project).map_err(|e| format!("Failed to init workspace: {}", e))?;

    let role = parse_role(&role);
    let cwd = if role == AgentRole::Master {
        project_dir(&project)
    } else {
        let dir = agent_dir(&project, &agent_id);
        std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create agent dir: {}", e))?;
        dir
    };

    build_and_spawn(
        pty.inner(),
        persistence.inner(),
        dispatcher.inner(),
        &agent_id,
        &project,
        role,
        &runtime,
        resume_key,
        cwd,
        on_data,
    )
}

#[tauri::command]
async fn agent_resume(
    pty: tauri::State<'_, Arc<PtyManager>>,
    persistence: tauri::State<'_, Arc<Persistence>>,
    dispatcher: tauri::State<'_, Arc<Dispatcher>>,
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
        &id,
        &rec.project,
        role,
        &rec.runtime,
        rec.resume_key.clone(),
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
    if let Ok(db) = persistence.db().lock() {
        let _ = db.update_status(&id, "idle", now_ms());
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
        &id,
        &project,
        role,
        &runtime,
        rec.resume_key.clone(),
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
    {
        let db = persistence.db().lock().unwrap();
        db.update_role(&id, &role_s, now_ms()).map_err(|e| e.to_string())?;
    }
    let project = persistence.project().to_string();
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

#[tauri::command]
async fn sessions_list(
    persistence: tauri::State<'_, Arc<Persistence>>,
) -> Result<Vec<AgentSessionRecord>, String> {
    let db = persistence.db().lock().unwrap();
    db.list_all().map_err(|e| e.to_string())
}

#[tauri::command]
async fn workspace_root(
    persistence: tauri::State<'_, Arc<Persistence>>,
) -> Result<String, String> {
    Ok(project_dir(persistence.project()).to_string_lossy().to_string())
}

#[tauri::command]
async fn sessions_delete(
    pty: tauri::State<'_, Arc<PtyManager>>,
    persistence: tauri::State<'_, Arc<Persistence>>,
    dispatcher: tauri::State<'_, Arc<Dispatcher>>,
    id: String,
) -> Result<(), String> {
    pty.kill(&id).map_err(|e| e.to_string())?;
    let project = persistence.project().to_string();
    let dir = agent_dir(&project, &id);
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    {
        let db = persistence.db().lock().unwrap();
        db.delete(&id).map_err(|e| e.to_string())?;
    }
    dispatcher.unregister_worker(&id);
    Ok(())
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
    let resolved = std::path::Path::new(&path)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&path));
    let home = std::env::var("HOME").map_err(|e| format!("HOME not set: {}", e))?;
    if !resolved.starts_with(&home) {
        return Err("Path escapes allowed directories".to_string());
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

// ── Git commands ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct GitEntry {
    pub index: String,
    pub worktree: String,
    pub path: String,
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
            raw_path[arrow + 4..].to_string()
        } else {
            raw_path.to_string()
        };
        entries.push(GitEntry {
            index: index.to_string(),
            worktree: worktree.to_string(),
            path,
        });
    }
    entries
}

#[tauri::command]
async fn git_status(dir: String) -> Result<Vec<GitEntry>, String> {
    let out = std::process::Command::new("git")
        .args(["-C", &dir, "status", "--porcelain"])
        .output()
        .map_err(|e| format!("git failed: {}", e))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git error (not a repository?): {}", err.trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(parse_porcelain(&text))
}

#[cfg(test)]
mod tests {
    use super::parse_porcelain;

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

// ── App entry point ─────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let pty = Arc::new(PtyManager::new());
    let persistence = Arc::new(
        Persistence::open(DEFAULT_PROJECT).expect("Failed to init persistence"),
    );
    let dispatcher = Arc::new(Dispatcher::new(pty.clone(), persistence.clone()));
    dispatcher.refresh_workers();
    // Install the `capilot` PATH shim (best-effort).
    match orchestration::shim::install_shim() {
        Ok(p) => log::info!("capilot shim installed at {}", p.display()),
        Err(e) => log::warn!("failed to install capilot shim: {e}"),
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_log::Builder::new().build())
        .plugin(tauri_plugin_single_instance::init(|_app, _args, _cwd| {}))
        .plugin(tauri_plugin_process::init())
        .manage(pty)
        .manage(persistence)
        .manage(dispatcher.clone())
        .manage(EspManager::new())
        .invoke_handler(tauri::generate_handler![
            agent_spawn,
            agent_resume,
            agent_write,
            agent_kill,
            agent_resize,
            agent_switch_runtime,
            agent_set_role,
            sessions_list,
            sessions_delete,
            workspace_root,
            worker_status,
            smart_return_set,
            smart_return_get,
            runtime_list_available,
            fs_read,
            fs_write,
            fs_list,
            git_status,
            esp_connect,
            esp_disconnect,
            esp_status,
            esp_send,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            dispatcher.start(handle);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
