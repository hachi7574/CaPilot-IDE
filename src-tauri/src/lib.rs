mod agent_runtime;

use agent_runtime::adapter::{AgentRole, AgentRuntimeAdapter, AgentSession, PermissionMode, Speed};
use agent_runtime::pty::PtyManager;
use agent_runtime::runtimes::claude::ClaudeAdapter;
use std::path::PathBuf;
use tauri::ipc::Channel;

// ── Agent commands ──────────────────────────────────────────────

#[tauri::command]
async fn agent_spawn(
    state: tauri::State<'_, PtyManager>,
    runtime: String,
    cwd: String,
    role: String,
    on_data: Channel<Vec<u8>>,
) -> Result<agent_runtime::adapter::AgentInfo, String> {
    let agent_id = uuid::Uuid::new_v4().to_string();
    let cwd = PathBuf::from(&cwd);

    let role = match role.as_str() {
        "master" => AgentRole::Master,
        "worker" => AgentRole::Worker,
        _ => AgentRole::Standalone,
    };

    let session = AgentSession {
        id: agent_id.clone(),
        runtime: runtime.clone(),
        mode: PermissionMode::Ask,
        speed: Speed::Auto,
        cwd: cwd.clone(),
        context_dir: cwd.clone(),
        role: role.clone(),
        rows: 24,
        cols: 80,
    };

    // Get the adapter for this runtime
    let adapter = ClaudeAdapter::new();

    if !adapter.is_available() {
        return Err(format!("Runtime '{}' is not available", runtime));
    }

    let (cmd, args) = adapter.spawn_interactive(&session)
        .map_err(|e| format!("Failed to build command: {}", e))?;

    state.spawn(agent_id.clone(), &cmd, &args, &cwd, 24, 80, on_data).await
        .map(|mut info| {
            info.runtime = runtime;
            info.role = role;
            info
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn agent_write(
    state: tauri::State<'_, PtyManager>,
    id: String,
    data: String,
) -> Result<(), String> {
    // Append newline as the Enter key sends the message
    let payload = format!("{}\r\n", data);
    state.write(&id, payload.as_bytes()).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn agent_kill(
    state: tauri::State<'_, PtyManager>,
    id: String,
) -> Result<(), String> {
    state.kill(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn agent_resize(
    state: tauri::State<'_, PtyManager>,
    id: String,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    state.resize(&id, rows, cols).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn runtime_list_available() -> Vec<agent_runtime::adapter::RuntimeInfo> {
    let adapter = ClaudeAdapter::new();
    vec![agent_runtime::adapter::RuntimeInfo {
        id: adapter.id().to_string(),
        name: adapter.name().to_string(),
        available: adapter.is_available(),
        authenticated: adapter.is_authenticated(),
        models: adapter.list_models(),
    }]
}

// ── Filesystem commands ─────────────────────────────────────────

#[tauri::command]
async fn fs_read(path: String, _app: tauri::AppHandle) -> Result<String, String> {
    // Path whitelist: must be within the workspace
    let resolved = std::path::Path::new(&path)
        .canonicalize()
        .map_err(|e| format!("Invalid path: {}", e))?;

    // Basic check: reject paths that escape the home directory
    let home = std::env::var("HOME").map_err(|e| format!("HOME not set: {}", e))?;
    if !resolved.starts_with(&home) {
        return Err("Path escapes allowed directories".to_string());
    }

    std::fs::read_to_string(&resolved)
        .map_err(|e| format!("Failed to read file: {}", e))
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

    std::fs::write(&resolved, &content)
        .map_err(|e| format!("Failed to write file: {}", e))
}

#[tauri::command]
async fn fs_list(dir: String) -> Result<Vec<String>, String> {
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
        entries.push(entry.file_name().to_string_lossy().to_string());
    }
    Ok(entries)
}

// ── App entry point ─────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(tauri_plugin_log::Builder::new().build())
        .plugin(tauri_plugin_single_instance::init(|_app, _args, _cwd| {}))
        .plugin(tauri_plugin_process::init())
        .manage(PtyManager::new())
        .invoke_handler(tauri::generate_handler![
            agent_spawn,
            agent_write,
            agent_kill,
            agent_resize,
            runtime_list_available,
            fs_read,
            fs_write,
            fs_list,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
