mod agent_runtime;
pub mod esp;
mod orchestration;
mod persistence;
mod resource;

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
use tauri::Manager;
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
    };

    let (cmd, mut args) = adapter
        .spawn_interactive(&session)
        .map_err(|e| format!("Failed to build command: {}", e))?;
    // Resume an existing conversation in the same context dir. An explicit
    // stored resume_key wins; otherwise fall back to adapter auto-detect.
    let resume_args = if let Some(key) = &resume_key {
        vec!["--resume".to_string(), key.clone()]
    } else {
        adapter.resume_args(&session)
    };
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
    model: Option<String>,
    speed: Option<String>,
    mode: Option<String>,
    // Custom project root (git-cloned / local-folder project). When provided,
    // the agent's cwd lives under this root instead of `workspace_root()/name`.
    project_root: Option<String>,
    on_data: Channel<Vec<u8>>,
) -> Result<AgentInfo, String> {
    let agent_id = uuid::Uuid::new_v4().to_string();
    let project = if project.is_empty() {
        persistence.project().to_string()
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
    // the per-agent dir so each session's context stays isolated.
    let cwd = match &project_root {
        Some(pr) => {
            let p = std::path::PathBuf::from(pr);
            std::fs::create_dir_all(&p)
                .map_err(|e| format!("Failed to create project root: {}", e))?;
            p.canonicalize()
                .map_err(|e| format!("Invalid project root: {}", e))?
        }
        None if role == AgentRole::Master => project_dir(&project),
        None => {
            let dir = agent_dir(&project, &agent_id);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("Failed to create agent dir: {}", e))?;
            dir
        }
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
        model,
        &speed.unwrap_or_else(|| "auto".to_string()),
        &mode.unwrap_or_else(|| "ask".to_string()),
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
        None,
        "auto",
        "ask",
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
        &id,
        &project,
        role,
        &runtime,
        rec.resume_key.clone(),
        None,
        "auto",
        "ask",
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

#[tauri::command]
async fn sessions_delete(
    pty: tauri::State<'_, Arc<PtyManager>>,
    persistence: tauri::State<'_, Arc<Persistence>>,
    dispatcher: tauri::State<'_, Arc<Dispatcher>>,
    id: String,
) -> Result<(), String> {
    // Best-effort end to end: a failed kill (e.g. the PTY was already reaped by
    // the reader task) must not skip session cleanup, or the DB row survives and
    // the terminal resurrects on the next restart.
    let _ = pty.kill(&id);
    let project = persistence.project().to_string();
    let dir = agent_dir(&project, &id);
    if dir.exists() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    {
        let db = persistence.db().lock().unwrap();
        let _ = db.delete(&id);
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

// ── Git commands ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct GitEntry {
    pub index: String,
    pub worktree: String,
    pub path: String,
    pub add: i32,
    pub del: i32,
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
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
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
            map.insert(path.to_string(), (a, d));
        }
    }
    map
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

/// Whether a directory is a git repo / has a remote / current branch. Powers the
/// Git panel's "未初始化 git" prompt and "无远程仓库" hint.
#[derive(Debug, Clone, Serialize)]
pub struct RepoInfo {
    pub is_repo: bool,
    pub has_remote: bool,
    pub branch: Option<String>,
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
    Ok(RepoInfo {
        is_repo,
        has_remote,
        branch,
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
    // `git ls-files -z -- <paths>` lists only the tracked ones; the rest are
    // untracked and get deleted from disk.
    let mut ls: Vec<&str> = vec!["ls-files", "-z", "--"];
    ls.extend(files.iter().map(String::as_str));
    let tracked_out = git_run(&repo, &ls).unwrap_or_default();
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
        git_run(&repo, &args)?;
    }
    for f in files.iter().filter(|f| !tracked.contains(*f)) {
        let p = std::path::Path::new(&repo).join(f);
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
    Ok(target.to_string_lossy().to_string())
}

#[tauri::command]
async fn git_push(repo: String) -> Result<(), String> {
    git_run(&repo, &["push"]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::{parse_branches, parse_log, parse_porcelain};

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
    let persistence = Arc::new(
        Persistence::open(DEFAULT_PROJECT).expect("Failed to init persistence"),
    );
    let dispatcher = Arc::new(Dispatcher::new(pty.clone(), persistence.clone()));
    dispatcher.refresh_workers();
    let resource = Arc::new(resource::ResourceMonitor::new());
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
            sessions_list,
            sessions_delete,
            workspace_root,
            create_project,
            list_projects,
            worker_status,
            smart_return_set,
            smart_return_get,
            runtime_list_available,
            runtime_models,
            fs_read,
            fs_write,
            fs_list,
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
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
