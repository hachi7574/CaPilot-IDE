//! Contexts workspace model + persistence.
//!
//! Workspace layout:
//! ```text
//! ~/CaPilot/workspaces/<project>/
//! ├─ context/               # shared context
//! ├─ agents/<agent-id>/     # per-agent workspace (PTY cwd)
//! │  └─ .agent-meta.json    # runtime / resume_key / status
//! └─ sessions.db            # sqlite
//! ```

use crate::lifecycle_journal::{LifecycleEventKind, LifecycleJournal};
use crate::session_store::{NaturalExit, SessionStore};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Default project used when none is supplied by the frontend.
pub const DEFAULT_PROJECT: &str = "default";

// ── Data model ──────────────────────────────────────────────────

/// A persisted agent session row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSessionRecord {
    pub id: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    pub project: String,
    pub runtime: String,
    pub resume_key: Option<String>,
    pub cwd: PathBuf,
    pub title: String,
    pub status: String, // idle | running | busy | done | failed
    /// Permission mode at spawn ("ask" | "auto" | "yolo"), persisted so a
    /// resumed session keeps the composer's choice.
    pub mode: String,
    /// Provider-specific thinking/effort option id.
    pub speed: String,
    /// Selected model id at spawn (None = runtime default).
    pub model: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Contents of `agents/<id>/.agent-meta.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMeta {
    pub id: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    pub runtime: String,
    pub resume_key: Option<String>,
    pub status: String,
    pub cwd: PathBuf,
    pub title: String,
    pub mode: String,
    pub speed: String,
    pub model: Option<String>,
    pub updated_at: i64,
}

/// A registered git worktree isolation workspace. One row per live worktree
/// created via `worktree_create` (or adopted by startup reconciliation).
/// `id` is derived from the pair `(repo, path)`; `instance_id` is minted fresh
/// per creation so reusing a path never inherits stale state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeMeta {
    pub id: String,
    /// Source repo root (absolute path).
    pub repo: String,
    /// Worktree folder (absolute path).
    pub path: PathBuf,
    /// Checked-out branch name.
    pub branch: String,
    /// Fork base (`main` / `origin/main` / …).
    pub base_ref: Option<String>,
    /// Optional parent workspace (stored only; no lineage logic in v1).
    pub parent_id: Option<String>,
    /// Fresh UUID minted at each creation.
    pub instance_id: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Derive the stable `worktrees.id` for a `(repo, path)` pair. Using the full
/// absolute strings keeps ids unique across repos and collisions-safe even if a
/// path contains the separator.
pub fn worktree_id(repo: &str, path: &std::path::Path) -> String {
    format!("{repo}::{}", path.display())
}

// ── Workspace layout helpers ────────────────────────────────────

/// Resolve the current user's home directory cross-platform.
///
/// Order:
/// 1. `HOME` — Unix default; also honored when tests / Git Bash set it on Windows
/// 2. `USERPROFILE` — Windows default
/// 3. `HOMEDRIVE` + `HOMEPATH` — Windows fallback
///
/// Production code must use this instead of bare `std::env::var("HOME")`.
/// Windows GUI processes typically have no `HOME`, only `USERPROFILE`.
pub fn user_home() -> Result<PathBuf, String> {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Ok(PathBuf::from(home));
        }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if !profile.is_empty() {
            return Ok(PathBuf::from(profile));
        }
    }
    if let (Ok(drive), Ok(path)) = (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH")) {
        let combined = format!("{drive}{path}");
        if !combined.is_empty() {
            return Ok(PathBuf::from(combined));
        }
    }
    Err("user home directory is not set (HOME/USERPROFILE)".into())
}

/// Like [`user_home`], but falls back to a writable temp dir instead of erroring.
/// Used for layout helpers that historically defaulted to `/tmp` on Unix.
pub fn user_home_or_tmp() -> PathBuf {
    user_home().unwrap_or_else(|_| {
        if let Ok(tmp) = std::env::var("TEMP").or_else(|_| std::env::var("TMP")) {
            if !tmp.is_empty() {
                return PathBuf::from(tmp);
            }
        }
        #[cfg(windows)]
        {
            return PathBuf::from(r"C:\Temp");
        }
        #[cfg(not(windows))]
        {
            PathBuf::from("/tmp")
        }
    })
}

pub fn workspace_root() -> PathBuf {
    user_home_or_tmp().join("CaPilot").join("workspaces")
}

pub fn project_dir(project: &str) -> PathBuf {
    workspace_root().join(project)
}

/// True when a project dir contains only scaffold (`.git`, empty `agents/`,
/// `context/`, `sessions.db`) — no real agents or user files. Used to safely drop
/// the legacy "default" project dir after its sessions DB was migrated up.
fn is_pure_scaffold(dir: &std::path::Path) -> bool {
    let Ok(mut entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.all(|entry| {
        let Ok(entry) = entry else { return false };
        let name = entry.file_name();
        match name.to_str() {
            Some(".git") | Some("context") | Some("sessions.db") => true,
            Some("agents") => entry
                .path()
                .read_dir()
                .map(|mut d| d.next().is_none())
                .unwrap_or(false),
            _ => false,
        }
    })
}

/// Create the contexts workspace layout for a project. Also `git init`s the
/// project root so the Git panel has a real repository to read.
pub fn ensure_project(project: &str) -> std::io::Result<PathBuf> {
    let dir = project_dir(project);
    std::fs::create_dir_all(dir.join("context"))?;
    std::fs::create_dir_all(dir.join("agents"))?;
    // git init if not already a repo (best-effort; the git panel depends on it)
    if !dir.join(".git").exists() {
        let _ = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .output();
    }
    Ok(dir)
}

pub fn agent_dir(project: &str, agent_id: &str) -> PathBuf {
    project_dir(project).join("agents").join(agent_id)
}

/// Sidecar dir for hook-reported agent status (`~/CaPilot/status/`). Claude Code
/// lifecycle hooks (injected per-session via `--settings`, see the claude
/// adapter) write one JSON file per agent here; the frontend polls it to drive
/// the accurate 运行中/空闲 split. App-owned, never inside a project workspace.
pub fn status_dir() -> PathBuf {
    user_home_or_tmp().join("CaPilot").join("status")
}

/// The per-agent status sidecar path (`~/CaPilot/status/<agent_id>.json`).
pub fn status_file(agent_id: &str) -> PathBuf {
    status_dir().join(format!("{agent_id}.json"))
}

/// Persist a custom project root (picked folder / git clone) to
/// `~/CaPilot/workspaces/<name>/project.json`. Written at create/clone time so
/// the root survives even with zero agents (agent-meta recovery needs one).
pub fn write_project_root(name: &str, root: &std::path::Path) -> std::io::Result<()> {
    let dir = project_dir(name);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("project.json"),
        serde_json::json!({ "root": root }).to_string(),
    )
}

fn persisted_project_root(name: &str) -> Option<PathBuf> {
    let data = std::fs::read_to_string(project_dir(name).join("project.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    v.get("root").and_then(|r| r.as_str()).map(PathBuf::from)
}

/// Recover a custom-rooted project's real on-disk root from its agent metadata.
///
/// Custom-rooted projects (git-cloned / picked folder) host their session
/// metadata under `~/CaPilot/workspaces/<name>/agents/<id>`, but each agent's
/// `cwd` points at the real project root. When an agent's cwd is NOT its own
/// workspace-scoped dir (nor the workspace project dir), that cwd is the root to
/// surface — so after a restart the sidebar restores the correct root instead of
/// the empty workspace dir. A persisted `project.json` root (written at
/// create/clone) takes precedence so the root survives a project with no agents.
pub fn custom_project_root(name: &str) -> Option<PathBuf> {
    if let Some(root) = persisted_project_root(name) {
        return Some(root);
    }
    let agents_dir = project_dir(name).join("agents");
    let entries = std::fs::read_dir(&agents_dir).ok()?;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        if let Ok(data) = std::fs::read(dir.join(".agent-meta.json")) {
            if let Ok(meta) = serde_json::from_slice::<AgentMeta>(&data) {
                if meta.cwd != dir && meta.cwd != project_dir(name) {
                    return Some(meta.cwd);
                }
            }
        }
    }
    None
}

// ── .agent-meta.json ────────────────────────────────────────────

/// Per-agent cross-process advisory lock for `.agent-meta.json`.
///
/// The GUI process and (later) the PTY daemon can both update the same sidecar
/// (title/status/runtime). A `std::sync::Mutex` would only serialize threads
/// inside ONE process, so we lock a per-agent file (`agents/<id>/.agent-meta.lock`)
/// with `flock` (Unix) / `LockFileEx` (Windows). Held for the duration of a
/// read-modify-write; the lock is released on drop (closing the fd drops the
/// flock).
///
/// Callers must re-read the meta from disk INSIDE the lock and only change the
/// target fields — a stale value captured before the lock could clobber a
/// concurrent writer's update.
pub struct AgentMetaGuard {
    _file: std::fs::File,
}

impl AgentMetaGuard {
    /// Take the exclusive per-agent lock, creating the agent dir if needed.
    pub fn lock(project: &str, agent_id: &str) -> std::io::Result<Self> {
        let dir = agent_dir(project, agent_id);
        std::fs::create_dir_all(&dir)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(".agent-meta.lock"))?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(Self { _file: file })
    }
}

/// Write `.agent-meta.json` into `dir` (which is created if missing). Shared by
/// `write_agent_meta` and the custom project-root path (git-cloned / local
/// folder projects whose agents live under `<root>/agents/<id>`).
///
/// Written atomically (same-directory temp file + rename) so a concurrent reader
/// or a crash mid-write can never observe a truncated/partial sidecar.
pub fn write_agent_meta_to_dir(dir: &std::path::Path, meta: &AgentMeta) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(".agent-meta.json");
    let json = serde_json::to_vec_pretty(meta).map_err(std::io::Error::other)?;
    let tmp = dir.join(".agent-meta.json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        std::io::Write::write_all(&mut f, &json)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn write_agent_meta(project: &str, meta: &AgentMeta) -> std::io::Result<()> {
    write_agent_meta_to_dir(&agent_dir(project, &meta.id), meta)
}

pub fn read_agent_meta(project: &str, agent_id: &str) -> std::io::Result<AgentMeta> {
    let path = agent_dir(project, agent_id).join(".agent-meta.json");
    let data = std::fs::read(path)?;
    Ok(serde_json::from_slice(&data)?)
}

/// Default (empty) `AgentMeta` used when a sidecar is missing but a DB row
/// exists — startup repair recreates the sidecar from the DB source of truth.
/// Also the seed for `update_agent_meta`'s locked read-modify-write.
#[allow(dead_code)] // exercised by tests; becomes the shared SessionStore core in Phase 1d
fn empty_agent_meta(project: &str, agent_id: &str) -> AgentMeta {
    AgentMeta {
        id: agent_id.to_string(),
        workspace_id: None,
        runtime: String::new(),
        resume_key: None,
        status: "idle".to_string(),
        cwd: agent_dir(project, agent_id),
        title: String::new(),
        mode: "ask".to_string(),
        speed: "auto".to_string(),
        model: None,
        updated_at: 0,
    }
}

/// Locked read-modify-write of an agent's `.agent-meta.json`.
///
/// Takes the per-agent cross-process lock, re-reads the CURRENT meta from disk
/// (a missing sidecar starts from `empty_agent_meta`), applies `f`, then writes
/// atomically. Prevents the two-writers-clobbering-each-other race the design
/// calls out: neither the GUI nor a daemon should re-apply a stale
/// read-modify-write over the other's title/status/runtime change.
#[allow(dead_code)] // exercised by tests; becomes the shared SessionStore core in Phase 1d
pub fn update_agent_meta<F>(project: &str, agent_id: &str, f: F) -> std::io::Result<AgentMeta>
where
    F: FnOnce(&mut AgentMeta),
{
    let _guard = AgentMetaGuard::lock(project, agent_id)?;
    let mut meta = read_agent_meta(project, agent_id)
        .unwrap_or_else(|_| empty_agent_meta(project, agent_id));
    f(&mut meta);
    write_agent_meta(project, &meta)?;
    Ok(meta)
}

// ── SQLite sessions DB ──────────────────────────────────────────

/// Idempotent column migration: adds `column_def` (a `name TYPE ...` fragment)
/// to `table` only when the column is missing — so pre-existing DBs pick up new
/// columns without touching existing rows.
fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    column_def: &str,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let cols: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<_, _>>()?;
    if !cols.iter().any(|c| c == column) {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column_def};"))?;
    }
    Ok(())
}

/// True for SQLITE_BUSY / SQLITE_LOCKED — the two errors a cross-process writer
/// can hit when another connection briefly holds the database lock. Used to
/// retry operations that SQLite's busy handler does not cover (notably the
/// `journal_mode=WAL` conversion, which must not fail just because another
/// process is starting up at the same time).
fn is_sqlite_busy(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ffi::ErrorCode::DatabaseBusy,
                ..
            },
            _
        ) | rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ffi::ErrorCode::DatabaseLocked,
                ..
            },
            _
        )
    )
}

pub struct SessionsDb {
    conn: Connection,
}

impl SessionsDb {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        // Cross-process concurrency: the GUI process and (later) the PTY daemon
        // both open `sessions.db`. WAL lets one writer proceed while other
        // connections keep reading, and busy_timeout makes concurrent writers
        // wait instead of failing instantly with SQLITE_BUSY. `query_row`
        // executes each PRAGMA and reads back its result row, so both are
        // verified, not just set.
        //
        // busy_timeout is set BEFORE journal_mode: converting a fresh DB to WAL
        // takes an exclusive lock, so under multi-process contention the
        // conversion would otherwise fail with SQLITE_BUSY the moment another
        // connection touches the file (the timeout must already be installed for
        // the conversion to wait its turn).
        let _timeout: i64 = conn.query_row("PRAGMA busy_timeout=5000", [], |r| r.get(0))?;
        // SQLite does NOT apply the busy handler to `journal_mode` changes, so a
        // concurrently-starting process can still fail the conversion instantly
        // even with busy_timeout set. Retry a bounded number of times: on a
        // freshly-created DB the winner's conversion is done in microseconds, so
        // the losers just need to briefly wait their turn. Established WAL DBs
        // (the normal case — the GUI creates the DB once, the daemon opens it
        // later) return "wal" immediately without needing the exclusive lock.
        let mut wal: String = String::new();
        for attempt in 0..10 {
            match conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0)) {
                Ok(m) => {
                    wal = m;
                    break;
                }
                Err(e) if is_sqlite_busy(&e) && attempt < 9 => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => return Err(e),
            }
        }
        if !wal.eq_ignore_ascii_case("wal") {
            // WAL unsupported (exotic filesystem / read-only store). Fail loudly
            // rather than silently running in DELETE mode under cross-process
            // contention.
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(1),
                Some("journal_mode did not become WAL".to_string()),
            ));
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id         TEXT PRIMARY KEY,
                workspace_id TEXT,
                project    TEXT NOT NULL,
                runtime    TEXT NOT NULL,
                resume_key TEXT,
                cwd        TEXT NOT NULL,
                title      TEXT NOT NULL DEFAULT '',
                status     TEXT NOT NULL DEFAULT 'idle',
                mode       TEXT NOT NULL DEFAULT 'ask',
                speed      TEXT NOT NULL DEFAULT 'auto',
                model      TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS worktrees (
                id          TEXT PRIMARY KEY,
                repo        TEXT NOT NULL,
                path        TEXT NOT NULL,
                branch      TEXT NOT NULL,
                base_ref    TEXT,
                parent_id   TEXT,
                instance_id TEXT NOT NULL,
                created_at  INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL
            );",
        )?;
        // Older builds stored hierarchy and handoff state. It is intentionally
        // discarded; SQLite's bundled version supports DROP COLUMN.
        let legacy_columns = {
            let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
            let columns = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<String>, _>>()?;
            columns
        };
        for column in ["role", "requires_attention", "attention_reason"] {
            if legacy_columns.iter().any(|existing| existing == column) {
                conn.execute_batch(&format!("ALTER TABLE sessions DROP COLUMN {column};"))?;
            }
        }
        // Migrate pre-existing DBs (created before mode/speed/model existed):
        // ALTER TABLE only adds a missing column, so old rows default cleanly.
        ensure_column(
            &conn,
            "sessions",
            "mode",
            "mode TEXT NOT NULL DEFAULT 'ask'",
        )?;
        ensure_column(
            &conn,
            "sessions",
            "speed",
            "speed TEXT NOT NULL DEFAULT 'auto'",
        )?;
        ensure_column(&conn, "sessions", "model", "model TEXT")?;
        ensure_column(&conn, "sessions", "workspace_id", "workspace_id TEXT")?;
        Ok(Self { conn })
    }

    /// Read a persisted app setting, or None when unset.
    pub fn get_setting(&self, key: &str) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
    }

    /// Upsert a persisted app setting.
    pub fn set_setting(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn insert(&self, s: &AgentSessionRecord) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO sessions
                (id, workspace_id, project, runtime, resume_key, cwd, title, status, mode, speed, model, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(id) DO UPDATE SET
                workspace_id=excluded.workspace_id, project=excluded.project, runtime=excluded.runtime,
                resume_key=excluded.resume_key, cwd=excluded.cwd, title=excluded.title,
                status=excluded.status, mode=excluded.mode, speed=excluded.speed,
                model=excluded.model, updated_at=excluded.updated_at",
            params![
                s.id,
                s.workspace_id,
                s.project,
                s.runtime,
                s.resume_key,
                s.cwd.to_string_lossy(),
                s.title,
                s.status,
                s.mode,
                s.speed,
                s.model,
                s.created_at,
                s.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn update_status(&self, id: &str, status: &str, updated_at: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status, updated_at, id],
        )?;
        Ok(())
    }

    pub fn update_runtime(&self, id: &str, runtime: &str, updated_at: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET runtime = ?1, updated_at = ?2 WHERE id = ?3",
            params![runtime, updated_at, id],
        )?;
        Ok(())
    }

    pub fn update_title(&self, id: &str, title: &str, updated_at: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, updated_at, id],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn update_resume_key(
        &self,
        id: &str,
        resume_key: &str,
        updated_at: i64,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET resume_key = ?1, updated_at = ?2 WHERE id = ?3",
            params![resume_key, updated_at, id],
        )?;
        Ok(())
    }

    /// Update a session's mode/speed/model (per-session composer config). Only
    /// updates the DB + timestamps — the running PTY is left untouched.
    pub fn update_config(
        &self,
        id: &str,
        mode: &str,
        speed: &str,
        model: Option<&str>,
        updated_at: i64,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET mode = ?1, speed = ?2, model = ?3, updated_at = ?4 WHERE id = ?5",
            params![mode, speed, model, updated_at, id],
        )?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> rusqlite::Result<Option<AgentSessionRecord>> {
        self.conn
            .query_row(
                "SELECT id, workspace_id, project, runtime, resume_key, cwd, title, status, mode, speed, model, created_at, updated_at
                 FROM sessions WHERE id = ?1",
                params![id],
                Self::row_to_session,
            )
            .optional()
    }

    pub fn list_all(&self) -> rusqlite::Result<Vec<AgentSessionRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, workspace_id, project, runtime, resume_key, cwd, title, status, mode, speed, model, created_at, updated_at FROM sessions ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], Self::row_to_session)?;
        rows.collect()
    }

    pub fn delete(&self, id: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn delete_project(&self, project: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM sessions WHERE project = ?1",
            params![project],
        )?;
        Ok(())
    }

    /// Rewrite a project's sessions after its workspace dir was renamed: update
    /// the `project` column, and rewrite `cwd` for default-rooted sessions
    /// (cwd inside the old workspace prefix). Custom-rooted cwds are untouched.
    pub fn rename_project(
        &self,
        old: &str,
        new: &str,
        old_prefix: &str,
        new_prefix: &str,
    ) -> rusqlite::Result<()> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, cwd FROM sessions WHERE project = ?1")?;
        let rows: Vec<(String, String)> = stmt
            .query_map(params![old], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(Result::ok)
            .collect();
        for (id, cwd) in rows {
            let new_cwd = if cwd.starts_with(old_prefix) {
                format!("{}{}", new_prefix, &cwd[old_prefix.len()..])
            } else {
                cwd
            };
            self.conn.execute(
                "UPDATE sessions SET project = ?1, cwd = ?2 WHERE id = ?3",
                params![new, new_cwd, id],
            )?;
        }
        Ok(())
    }

    // ── worktrees (isolation workspaces) ─────────────────────────

    /// Insert (or replace) a worktree row.
    pub fn insert_worktree(&self, w: &WorktreeMeta) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO worktrees
                (id, repo, path, branch, base_ref, parent_id, instance_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                repo=excluded.repo, path=excluded.path, branch=excluded.branch,
                base_ref=excluded.base_ref, parent_id=excluded.parent_id,
                instance_id=excluded.instance_id, created_at=excluded.created_at,
                updated_at=excluded.updated_at",
            params![
                w.id,
                w.repo,
                w.path.to_string_lossy(),
                w.branch,
                w.base_ref,
                w.parent_id,
                w.instance_id,
                w.created_at,
                w.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn get_worktree(&self, id: &str) -> rusqlite::Result<Option<WorktreeMeta>> {
        self.conn
            .query_row(
                "SELECT id, repo, path, branch, base_ref, parent_id, instance_id, created_at, updated_at
                 FROM worktrees WHERE id = ?1",
                params![id],
                Self::row_to_worktree,
            )
            .optional()
    }

    /// Find a worktree by its on-disk path (used by remove-by-path and
    /// reconciliation, where the id pair is not readily available).
    pub fn find_worktree_by_path(&self, path: &str) -> rusqlite::Result<Option<WorktreeMeta>> {
        self.conn
            .query_row(
                "SELECT id, repo, path, branch, base_ref, parent_id, instance_id, created_at, updated_at
                 FROM worktrees WHERE path = ?1",
                params![path],
                Self::row_to_worktree,
            )
            .optional()
    }

    /// Every registered worktree (all repos).
    pub fn list_worktrees(&self) -> rusqlite::Result<Vec<WorktreeMeta>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, path, branch, base_ref, parent_id, instance_id, created_at, updated_at
             FROM worktrees ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], Self::row_to_worktree)?;
        rows.collect()
    }

    /// Worktrees belonging to one source repo (for startup reconciliation
    /// against `git worktree list`).
    pub fn list_worktrees_for_repo(&self, repo: &str) -> rusqlite::Result<Vec<WorktreeMeta>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo, path, branch, base_ref, parent_id, instance_id, created_at, updated_at
             FROM worktrees WHERE repo = ?1 ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map(params![repo], Self::row_to_worktree)?;
        rows.collect()
    }

    pub fn delete_worktree(&self, id: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM worktrees WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Drop every worktree row for a repo (used when a source project is
    /// deleted wholesale).
    pub fn delete_worktrees_for_repo(&self, repo: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM worktrees WHERE repo = ?1",
            params![repo],
        )?;
        Ok(())
    }

    fn row_to_worktree(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorktreeMeta> {
        Ok(WorktreeMeta {
            id: row.get(0)?,
            repo: row.get(1)?,
            path: PathBuf::from(row.get::<_, String>(2)?),
            branch: row.get(3)?,
            base_ref: row.get(4)?,
            parent_id: row.get(5)?,
            instance_id: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    }

    fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentSessionRecord> {
        Ok(AgentSessionRecord {
            id: row.get(0)?,
            workspace_id: row.get(1)?,
            project: row.get(2)?,
            runtime: row.get(3)?,
            resume_key: row.get(4)?,
            cwd: PathBuf::from(row.get::<_, String>(5)?),
            title: row.get(6)?,
            status: row.get(7)?,
            mode: row.get(8)?,
            speed: row.get(9)?,
            model: row.get(10)?,
            created_at: row.get(11)?,
            updated_at: row.get(12)?,
        })
    }
}

// ── Managed state ───────────────────────────────────────────────

/// Shared persistence handle for the GUI. Holds a [`SessionStore`] (the
/// Tauri-independent facade over the sessions DB + sidecars, shared with the
/// PTY daemon) plus the [`LifecycleJournal`]. The natural-exit path goes through
/// [`Self::apply_natural_exit`] so the GUI fallback exercises the same code the
/// daemon will use (§6.1) — only the Tauri `emit` stays in the caller.
pub struct Persistence {
    store: SessionStore,
    journal: LifecycleJournal,
}

impl Persistence {
    /// Open the sessions store. Sessions live in a SINGLE top-level database
    /// (`~/CaPilot/sessions.db`) — not inside a per-project (or "default")
    /// workspace dir — so no scaffold project is created just for persistence.
    /// A legacy `workspaces/default/sessions.db` (the old global store) is
    /// migrated up once, then its empty scaffold dir is removed.
    pub fn open() -> std::io::Result<Self> {
        let ca_pilot = workspace_root()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("CaPilot"));
        std::fs::create_dir_all(&ca_pilot)?;
        let db_path = ca_pilot.join("sessions.db");
        // Migrate the old global sessions DB out of the "default" project dir.
        let legacy = workspace_root().join(DEFAULT_PROJECT).join("sessions.db");
        if !db_path.exists() && legacy.exists() {
            let _ = std::fs::copy(&legacy, &db_path);
        }
        // Drop the legacy "default" scaffold dir (migrated sessions DB above) so
        // it stops showing as a project — only when it's pure scaffold (no real
        // agents / user files), so no user data is ever lost.
        let legacy_dir = workspace_root().join(DEFAULT_PROJECT);
        if is_pure_scaffold(&legacy_dir) {
            let _ = std::fs::remove_dir_all(&legacy_dir);
        }
        let store = SessionStore::from_base(ca_pilot)?;
        Ok(Self {
            store,
            journal: LifecycleJournal::new(),
        })
    }

    pub fn db(&self) -> &Mutex<SessionsDb> {
        self.store.db()
    }

    /// Lock the sessions DB, tolerating a poisoned mutex (a panic while holding
    /// the lock marks it poisoned; `unwrap()` would then panic on every command).
    /// Returns None only if the lock is currently held by a panicked holder that
    /// never released — practically never. Callers should fall back gracefully.
    pub fn db_tolerant(&self) -> Option<std::sync::MutexGuard<'_, SessionsDb>> {
        self.store.db_tolerant()
    }

    /// The Tauri-independent store (for daemon-style direct access).
    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    /// The lifecycle event log (Phase 4 replays it after offline gaps).
    pub fn journal(&self) -> &LifecycleJournal {
        &self.journal
    }

    /// Apply the natural-exit policy (`session_end_mode`) and record the
    /// lifecycle event. The caller (GUI bridge) uses the returned [`NaturalExit`]
    /// to emit the matching Tauri event; the daemon calls
    /// [`SessionStore::apply_natural_exit`] directly.
    pub fn apply_natural_exit(&self, agent_id: &str, exit_code: i32) -> NaturalExit {
        let outcome = self.store.apply_natural_exit(agent_id);
        let (kind, payload) = if outcome.deleted {
            (LifecycleEventKind::Removed, None)
        } else {
            (
                LifecycleEventKind::Exited,
                Some(serde_json::json!({ "exit_code": exit_code })),
            )
        };
        self.journal.record(agent_id, kind, payload);
        outcome
    }
}

/// Startup repair: `sessions.db` is the source of truth for session metadata;
/// each `.agent-meta.json` is a derivable copy. Recreate any missing (or
/// unparseable) sidecar from its DB row so a hand-deleted / corrupted meta file
/// can't make a restored session look unowned. Idempotent — only writes when the
/// file is absent or invalid, and it takes the per-agent cross-process lock so a
/// concurrently-running daemon isn't clobbered. Returns the number of files
/// recreated.
pub fn repair_agent_meta(db: &SessionsDb) -> std::io::Result<usize> {
    let records = db.list_all().map_err(std::io::Error::other)?;
    let mut repaired = 0usize;
    for rec in records {
        // Lock first, then re-check existence inside the lock: a daemon may have
        // just written it. Writing over a fresh concurrent write would be a lost
        // update.
        let _guard = AgentMetaGuard::lock(&rec.project, &rec.id)?;
        if read_agent_meta(&rec.project, &rec.id).is_err() {
            let meta = AgentMeta {
                id: rec.id.clone(),
                workspace_id: rec.workspace_id.clone(),
                runtime: rec.runtime.clone(),
                resume_key: rec.resume_key.clone(),
                status: rec.status.clone(),
                cwd: rec.cwd.clone(),
                title: rec.title.clone(),
                mode: rec.mode.clone(),
                speed: rec.speed.clone(),
                model: rec.model.clone(),
                updated_at: rec.updated_at,
            };
            write_agent_meta(&rec.project, &meta)?;
            repaired += 1;
        }
    }
    Ok(repaired)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> AgentSessionRecord {
        AgentSessionRecord {
            id: "abc".into(),
            workspace_id: Some("wks_test".into()),
            project: "test".into(),
            runtime: "claude".into(),
            resume_key: Some("k1".into()),
            cwd: PathBuf::from("/tmp/w/agents/abc"),
            title: "布偶".into(),
            status: "running".into(),
            mode: "yolo".into(),
            speed: "fast".into(),
            model: Some("claude-opus-5".into()),
            created_at: 1,
            updated_at: 2,
        }
    }

    fn sample_worktree(repo: &str, path: &str, branch: &str) -> WorktreeMeta {
        WorktreeMeta {
            id: worktree_id(repo, std::path::Path::new(path)),
            repo: repo.to_string(),
            path: PathBuf::from(path),
            branch: branch.to_string(),
            base_ref: Some("main".to_string()),
            parent_id: None,
            instance_id: uuid::Uuid::new_v4().simple().to_string(),
            created_at: 10,
            updated_at: 11,
        }
    }

    #[test]
    fn db_insert_list_update() {
        let path = std::env::temp_dir().join(format!("capilot-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = SessionsDb::open(&path).unwrap();
        db.insert(&sample()).unwrap();
        let all = db.list_all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].resume_key.as_deref(), Some("k1"));
        assert_eq!(all[0].workspace_id.as_deref(), Some("wks_test"));
        // mode/speed/model survive the roundtrip.
        assert_eq!(all[0].mode, "yolo");
        assert_eq!(all[0].speed, "fast");
        assert_eq!(all[0].model.as_deref(), Some("claude-opus-5"));

        db.update_status("abc", "done", 99).unwrap();
        let got = db.get("abc").unwrap().unwrap();
        assert_eq!(got.status, "done");
        assert_eq!(got.updated_at, 99);

        db.delete("abc").unwrap();
        assert!(db.get("abc").unwrap().is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn delete_project_uses_persisted_identity_not_cwd() {
        let path = std::env::temp_dir().join(format!(
            "capilot-project-delete-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let db = SessionsDb::open(&path).unwrap();
        let mut session = sample();
        session.project = "legacy".into();
        session.cwd = PathBuf::from("/tmp/Legacy");
        db.insert(&session).unwrap();

        db.delete_project("other").unwrap();
        assert!(db.get("abc").unwrap().is_some());
        db.delete_project("legacy").unwrap();
        assert!(db.get("abc").unwrap().is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn meta_roundtrip() {
        let dir = std::env::temp_dir().join(format!("capilot-meta-{}", std::process::id()));
        let target = dir.join("agents").join("x");
        std::fs::create_dir_all(&target).unwrap();
        let meta = AgentMeta {
            id: "x".into(),
            workspace_id: Some("wks_meta".into()),
            runtime: "claude".into(),
            resume_key: None,
            status: "running".into(),
            cwd: target.clone(),
            title: "t".into(),
            mode: "ask".into(),
            speed: "auto".into(),
            model: None,
            updated_at: 5,
        };
        let json = serde_json::to_vec_pretty(&meta).unwrap();
        std::fs::write(target.join(".agent-meta.json"), json).unwrap();
        let read: AgentMeta =
            serde_json::from_slice(&std::fs::read(target.join(".agent-meta.json")).unwrap())
                .unwrap();
        assert_eq!(read.id, "x");
        assert_eq!(read.mode, "ask");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_kv_roundtrip() {
        let path = std::env::temp_dir().join(format!("capilot-settings-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = SessionsDb::open(&path).unwrap();
        // Unset → None.
        assert_eq!(db.get_setting("session_end_mode").unwrap(), None);
        // Upsert + read back.
        db.set_setting("session_end_mode", "delete").unwrap();
        assert_eq!(
            db.get_setting("session_end_mode").unwrap().as_deref(),
            Some("delete")
        );
        // Upsert overwrites.
        db.set_setting("session_end_mode", "keep").unwrap();
        assert_eq!(
            db.get_setting("session_end_mode").unwrap().as_deref(),
            Some("keep")
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn update_config_persists_per_session_values() {
        let path = std::env::temp_dir().join(format!("capilot-cfg-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = SessionsDb::open(&path).unwrap();
        db.insert(&sample()).unwrap();

        // Change mode + model, keep speed untouched (None).
        db.update_config("abc", "yolo", "auto", Some("claude-opus-5"), 99)
            .unwrap();
        let got = db.get("abc").unwrap().unwrap();
        assert_eq!(got.mode, "yolo");
        assert_eq!(got.speed, "auto");
        assert_eq!(got.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(got.updated_at, 99);

        // Clearing model back to default.
        db.update_config("abc", "ask", "fast", None, 100).unwrap();
        let got = db.get("abc").unwrap().unwrap();
        assert_eq!(got.mode, "ask");
        assert_eq!(got.speed, "fast");
        assert_eq!(got.model, None);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn worktree_crud_roundtrip() {
        let path = std::env::temp_dir().join(format!("capilot-wt-crud-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = SessionsDb::open(&path).unwrap();

        let w1 = sample_worktree("/repo/a", "/repo/a-ft", "ft");
        let w2 = sample_worktree("/repo/a", "/repo/a-ft-2", "ft-2");
        let w3 = sample_worktree("/repo/b", "/repo/b-x", "x");

        db.insert_worktree(&w1).unwrap();
        db.insert_worktree(&w2).unwrap();
        db.insert_worktree(&w3).unwrap();

        // get by id
        let got = db.get_worktree(&w1.id).unwrap().unwrap();
        assert_eq!(got.branch, "ft");
        assert_eq!(got.repo, "/repo/a");
        assert_eq!(got.path, PathBuf::from("/repo/a-ft"));

        // find by path
        assert_eq!(
            db.find_worktree_by_path("/repo/a-ft-2").unwrap().unwrap().branch,
            "ft-2"
        );

        // list all + per-repo
        assert_eq!(db.list_worktrees().unwrap().len(), 3);
        let repo_a = db.list_worktrees_for_repo("/repo/a").unwrap();
        assert_eq!(repo_a.len(), 2);
        assert_eq!(db.list_worktrees_for_repo("/repo/b").unwrap().len(), 1);

        // delete one
        db.delete_worktree(&w2.id).unwrap();
        assert!(db.get_worktree(&w2.id).unwrap().is_none());
        assert_eq!(db.list_worktrees().unwrap().len(), 2);

        // delete a whole repo's rows
        db.delete_worktrees_for_repo("/repo/a").unwrap();
        assert_eq!(db.list_worktrees_for_repo("/repo/a").unwrap().len(), 0);
        assert_eq!(db.list_worktrees().unwrap().len(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn old_schema_db_is_migrated() {
        // Simulate a pre-mode/speed/model DB (created by an older build): open()
        // must add the missing columns and keep old rows readable.
        let path = std::env::temp_dir().join(format!("capilot-legacy-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE sessions (
                    id         TEXT PRIMARY KEY,
                    project    TEXT NOT NULL,
                    role       TEXT NOT NULL,
                    runtime    TEXT NOT NULL,
                    resume_key TEXT,
                    cwd        TEXT NOT NULL,
                    title      TEXT NOT NULL DEFAULT '',
                    status     TEXT NOT NULL DEFAULT 'idle',
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );",
            )
            .unwrap();
        }
        let db = SessionsDb::open(&path).unwrap();
        // Old rows (none here) would default to ask/auto; new inserts carry values.
        assert_eq!(db.list_all().unwrap().len(), 0);
        db.insert(&sample()).unwrap();
        let got = db.get("abc").unwrap().unwrap();
        assert_eq!(got.mode, "yolo");
        assert_eq!(got.speed, "fast");
        assert_eq!(got.model.as_deref(), Some("claude-opus-5"));
        let _ = std::fs::remove_file(&path);
    }

    // ── Phase 1c: persistence hardening ─────────────────────────────

    /// WAL + busy_timeout are what make `sessions.db` safe to open from two
    /// processes (GUI + daemon): one writer proceeds while the other waits
    /// instead of failing with SQLITE_BUSY. Verify both PRAGMAs actually took
    /// effect (set + read back), not just that they ran without error.
    #[test]
    fn db_open_enables_wal_and_busy_timeout() {
        let path = std::env::temp_dir().join(format!("capilot-wal-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
        {
            let db = SessionsDb::open(&path).unwrap();
            let mode: String = db
                .conn
                .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .unwrap();
            assert_eq!(mode.to_ascii_lowercase(), "wal");
            let timeout: i64 = db
                .conn
                .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
                .unwrap();
            assert_eq!(timeout, 5000);
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    /// Atomic write = same-directory temp + rename. A crash mid-write can never
    /// leave a truncated sidecar visible; the temp file must not survive.
    #[test]
    fn meta_write_is_atomic_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("capilot-meta-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let meta = AgentMeta {
            id: "a1".into(),
            workspace_id: None,
            runtime: "claude".into(),
            resume_key: None,
            status: "running".into(),
            cwd: dir.clone(),
            title: "t".into(),
            mode: "ask".into(),
            speed: "auto".into(),
            model: None,
            updated_at: 7,
        };
        write_agent_meta_to_dir(&dir, &meta).unwrap();
        let target = dir.join(".agent-meta.json");
        assert!(target.exists());
        assert!(!dir.join(".agent-meta.json.tmp").exists());
        let read: AgentMeta = serde_json::from_slice(&std::fs::read(target).unwrap()).unwrap();
        assert_eq!(read.id, "a1");
        assert_eq!(read.updated_at, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `update_agent_meta` re-reads inside the lock, so concurrent writers each
    /// apply on top of the latest value. Two threads appending suffixes must
    /// produce a title with ALL suffixes — a lost update would drop some.
    #[test]
    fn update_agent_meta_serializes_concurrent_writers() {
        let _guard = crate::agent_runtime::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let home = std::env::temp_dir().join(format!("capilot-meta-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::env::set_var("HOME", &home);
        let _ = update_agent_meta("proj", "agent1", |m| m.title = "base".into());

        let handles: Vec<_> = (0..2)
            .map(|t| {
                std::thread::spawn(move || {
                    for i in 0..5 {
                        update_agent_meta("proj", "agent1", |m| {
                            m.title = format!("{}-t{t}i{i}", m.title);
                            m.updated_at += 1;
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let meta = read_agent_meta("proj", "agent1").unwrap();
        // Every writer's counter increment survived — no clobbered RMW.
        assert_eq!(meta.updated_at, 10);
        assert_eq!(meta.title.matches("-t0i").count(), 5);
        assert_eq!(meta.title.matches("-t1i").count(), 5);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Genuine cross-process stress: 4 child test binaries concurrently
    /// read-modify-write the same `.agent-meta.json`. The flock-based guard is
    /// per-open-file-description, so it serializes across processes (this is the
    /// exact GUI-vs-daemon race the design brief calls out). Without the lock,
    /// the 40 increments would collapse into a much lower count.
    #[test]
    fn two_process_meta_lock_prevents_lost_updates() {
        const WORKER: &str = "CAPILOT_META_WORKER";
        if std::env::var(WORKER).is_ok() {
            // Re-invoked test binary, running only THIS test. Do the locked
            // writes and return — never spawn grandchildren.
            for i in 0..10 {
                update_agent_meta("proj", "agent2", |m| {
                    m.title = format!("{}-w{i}", m.title);
                    m.updated_at += 1;
                })
                .unwrap();
            }
            return;
        }
        let _guard = crate::agent_runtime::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let home = std::env::temp_dir().join(format!("capilot-meta-2proc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::env::set_var("HOME", &home);
        let _ = update_agent_meta("proj", "agent2", |m| m.title = "base".into());

        let exe = std::env::current_exe().unwrap();
        // Substring filter (NO `--exact`): libtest registers test names without
        // the crate prefix (`persistence::tests::…`), while `module_path!()`
        // includes it (`capilot_ide_lib::persistence::tests::…`), so `--exact`
        // would match 0 tests and the workers would silently no-op.
        let filter = "two_process_meta_lock_prevents_lost_updates";
        let mut children = Vec::new();
        for _ in 0..4 {
            children.push(
                std::process::Command::new(&exe)
                    .arg(filter)
                    .env(WORKER, "1")
                    .env("HOME", &home)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
        }
        for mut c in children {
            assert!(c.wait().unwrap().success(), "meta worker exited non-zero");
        }
        let meta = read_agent_meta("proj", "agent2").unwrap();
        assert_eq!(meta.updated_at, 40);
        assert_eq!(meta.title.matches("-w").count(), 40);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Cross-process stress on `sessions.db` itself: 4 child processes each run
    /// insert/update/delete transactions against the same SQLite file. With WAL
    /// + busy_timeout, no worker may panic with SQLITE_BUSY and the DB must stay
    /// intact. Any `SQLITE_BUSY` inside a worker turns into a non-zero exit.
    #[test]
    fn two_process_db_write_stress_no_sqlite_busy() {
        const DB_WORKER: &str = "CAPILOT_DB_WORKER";
        if std::env::var(DB_WORKER).is_ok() {
            let home = PathBuf::from(std::env::var("HOME").unwrap());
            let db = SessionsDb::open(&home.join("CaPilot").join("stress.db")).unwrap();
            let wid = std::env::var("CAPILOT_DB_WORKER_ID").unwrap();
            for i in 0..20 {
                let mut s = sample();
                s.id = format!("{wid}-{i}");
                s.project = "stress".into();
                db.insert(&s).unwrap();
                db.update_status(&s.id, "done", 100 + i).unwrap();
                db.delete(&s.id).unwrap();
            }
            return;
        }
        let _guard = crate::agent_runtime::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let home = std::env::temp_dir().join(format!("capilot-db-2proc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::env::set_var("HOME", &home);
        std::fs::create_dir_all(home.join("CaPilot")).unwrap();
        // Establish the DB in WAL mode ONCE before the workers start. This
        // mirrors production (the GUI creates `sessions.db` first; the daemon
        // opens an existing WAL DB later) and keeps the stress test focused on
        // the acceptance criterion: concurrent read/write of an established DB,
        // not the one-time journal-mode conversion.
        let _pre = SessionsDb::open(&home.join("CaPilot").join("stress.db")).unwrap();

        let exe = std::env::current_exe().unwrap();
        // Substring filter (NO `--exact`) — see the meta lock test above for why
        // `--exact` + `module_path!()` silently matches 0 tests in the child.
        let filter = "two_process_db_write_stress_no_sqlite_busy";
        let mut children = Vec::new();
        for w in 0..4 {
            children.push(
                std::process::Command::new(&exe)
                    .arg(filter)
                    .env(DB_WORKER, "1")
                    .env("CAPILOT_DB_WORKER_ID", format!("w{w}"))
                    .env("HOME", &home)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
        }
        for mut c in children {
            assert!(c.wait().unwrap().success(), "db worker exited non-zero");
        }
        // All worker rows were deleted by their own transactions; re-open and
        // confirm the DB survived intact and still accepts writes.
        let db = SessionsDb::open(&home.join("CaPilot").join("stress.db")).unwrap();
        assert_eq!(db.list_all().unwrap().len(), 0);
        db.insert(&sample()).unwrap();
        assert_eq!(db.list_all().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Startup repair recreates a missing/corrupt `.agent-meta.json` from the DB
    /// row (source of truth) and is idempotent — it must not rewrite a sidecar a
    /// concurrently-running daemon just wrote.
    #[test]
    fn repair_agent_meta_recreates_missing_sidecar() {
        let _guard = crate::agent_runtime::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let home = std::env::temp_dir().join(format!("capilot-repair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::env::set_var("HOME", &home);

        let path = home.join("CaPilot").join("sessions.db");
        std::fs::create_dir_all(home.join("CaPilot")).unwrap();
        let _ = std::fs::remove_file(&path);
        let db = SessionsDb::open(&path).unwrap();
        let mut session = sample();
        session.project = "rp".into();
        session.cwd = agent_dir("rp", &session.id);
        db.insert(&session).unwrap();
        // Simulate a hand-deleted / truncated sidecar.
        let sidecar_dir = agent_dir("rp", "abc");
        std::fs::create_dir_all(&sidecar_dir).unwrap();
        std::fs::write(sidecar_dir.join(".agent-meta.json"), b"not json").unwrap();

        let repaired = repair_agent_meta(&db).unwrap();
        assert_eq!(repaired, 1);
        let meta = read_agent_meta("rp", "abc").unwrap();
        assert_eq!(meta.id, "abc");
        assert_eq!(meta.title, session.title);
        assert_eq!(meta.runtime, session.runtime);
        assert_eq!(meta.cwd, session.cwd);
        assert_eq!(meta.updated_at, session.updated_at);

        // Idempotent: a second pass repairs nothing.
        assert_eq!(repair_agent_meta(&db).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&home);
    }
}
