//! Contexts workspace model + persistence.
//!
//! Layout (per DevPlan §2.2):
//! ```text
//! ~/CaPilot/workspaces/<project>/
//! ├─ context/               # shared context
//! ├─ agents/<agent-id>/     # per-agent workspace (PTY cwd)
//! │  └─ .agent-meta.json    # role / runtime / resume_key / status
//! └─ sessions.db            # sqlite
//! ```

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
    #[serde(default)]
    pub requires_attention: bool,
    #[serde(default)]
    pub attention_reason: Option<String>,
    pub project: String,
    pub role: String, // master | worker | standalone
    pub runtime: String,
    pub resume_key: Option<String>,
    pub cwd: PathBuf,
    pub title: String,
    pub status: String, // idle | running | busy | done | failed
    /// Permission mode at spawn ("ask" | "auto" | "yolo"), persisted so a
    /// resumed session keeps the composer's choice.
    pub mode: String,
    /// Speed tier at spawn ("high" | "mid" | "fast" | "auto").
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
    #[serde(default)]
    pub requires_attention: bool,
    #[serde(default)]
    pub attention_reason: Option<String>,
    pub role: String,
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

// ── Workspace layout helpers ────────────────────────────────────

pub fn workspace_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join("CaPilot").join("workspaces")
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

/// Write `.agent-meta.json` into `dir` (which is created if missing). Shared by
/// `write_agent_meta` and the custom project-root path (git-cloned / local
/// folder projects whose agents live under `<root>/agents/<id>`).
pub fn write_agent_meta_to_dir(dir: &std::path::Path, meta: &AgentMeta) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(".agent-meta.json");
    let json = serde_json::to_vec_pretty(meta).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

pub fn write_agent_meta(project: &str, meta: &AgentMeta) -> std::io::Result<()> {
    write_agent_meta_to_dir(&agent_dir(project, &meta.id), meta)
}

pub fn read_agent_meta(project: &str, agent_id: &str) -> std::io::Result<AgentMeta> {
    let path = agent_dir(project, agent_id).join(".agent-meta.json");
    let data = std::fs::read(path)?;
    Ok(serde_json::from_slice(&data)?)
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

pub struct SessionsDb {
    conn: Connection,
}

impl SessionsDb {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id         TEXT PRIMARY KEY,
                workspace_id TEXT,
                requires_attention INTEGER NOT NULL DEFAULT 0,
                attention_reason TEXT,
                project    TEXT NOT NULL,
                role       TEXT NOT NULL,
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
            );",
        )?;
        // Migrate pre-existing DBs (created before mode/speed/model existed):
        // ALTER TABLE only adds a missing column, so old rows default cleanly.
        ensure_column(&conn, "sessions", "mode", "mode TEXT NOT NULL DEFAULT 'ask'")?;
        ensure_column(&conn, "sessions", "speed", "speed TEXT NOT NULL DEFAULT 'auto'")?;
        ensure_column(&conn, "sessions", "model", "model TEXT")?;
        ensure_column(&conn, "sessions", "workspace_id", "workspace_id TEXT")?;
        ensure_column(&conn, "sessions", "requires_attention", "requires_attention INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&conn, "sessions", "attention_reason", "attention_reason TEXT")?;
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
                (id, workspace_id, requires_attention, attention_reason, project, role, runtime, resume_key, cwd, title, status, mode, speed, model, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             ON CONFLICT(id) DO UPDATE SET
                workspace_id=excluded.workspace_id, requires_attention=excluded.requires_attention,
                attention_reason=excluded.attention_reason, project=excluded.project, role=excluded.role, runtime=excluded.runtime,
                resume_key=excluded.resume_key, cwd=excluded.cwd, title=excluded.title,
                status=excluded.status, mode=excluded.mode, speed=excluded.speed,
                model=excluded.model, updated_at=excluded.updated_at",
            params![
                s.id,
                s.workspace_id,
                s.requires_attention,
                s.attention_reason,
                s.project,
                s.role,
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

    pub fn update_attention(&self, id: &str, reason: Option<&str>, updated_at: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET requires_attention = ?1, attention_reason = ?2, updated_at = ?3 WHERE id = ?4",
            params![reason.is_some(), reason, updated_at, id],
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

    pub fn update_role(&self, id: &str, role: &str, updated_at: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sessions SET role = ?1, updated_at = ?2 WHERE id = ?3",
            params![role, updated_at, id],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn update_resume_key(&self, id: &str, resume_key: &str, updated_at: i64) -> rusqlite::Result<()> {
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
                "SELECT id, workspace_id, requires_attention, attention_reason, project, role, runtime, resume_key, cwd, title, status, mode, speed, model, created_at, updated_at
                 FROM sessions WHERE id = ?1",
                params![id],
                Self::row_to_session,
            )
            .optional()
    }

    pub fn list_all(&self) -> rusqlite::Result<Vec<AgentSessionRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, workspace_id, requires_attention, attention_reason, project, role, runtime, resume_key, cwd, title, status, mode, speed, model, created_at, updated_at FROM sessions ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], Self::row_to_session)?;
        rows.collect()
    }

    pub fn delete(&self, id: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
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

    fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentSessionRecord> {
        Ok(AgentSessionRecord {
            id: row.get(0)?,
            workspace_id: row.get(1)?,
            requires_attention: row.get(2)?,
            attention_reason: row.get(3)?,
            project: row.get(4)?,
            role: row.get(5)?,
            runtime: row.get(6)?,
            resume_key: row.get(7)?,
            cwd: PathBuf::from(row.get::<_, String>(8)?),
            title: row.get(9)?,
            status: row.get(10)?,
            mode: row.get(11)?,
            speed: row.get(12)?,
            model: row.get(13)?,
            created_at: row.get(14)?,
            updated_at: row.get(15)?,
        })
    }
}

// ── Managed state ───────────────────────────────────────────────

/// Shared persistence handle. Holds the single sessions DB.
/// `rusqlite::Connection` is not Sync, so the DB sits behind a Mutex.
pub struct Persistence {
    db: Mutex<SessionsDb>,
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
        let db = SessionsDb::open(&db_path).map_err(std::io::Error::other)?;
        Ok(Self {
            db: Mutex::new(db),
        })
    }

    pub fn db(&self) -> &Mutex<SessionsDb> {
        &self.db
    }

    /// Lock the sessions DB, tolerating a poisoned mutex (a panic while holding
    /// the lock marks it poisoned; `unwrap()` would then panic on every command).
    /// Returns None only if the lock is currently held by a panicked holder that
    /// never released — practically never. Callers should fall back gracefully.
    pub fn db_tolerant(&self) -> Option<std::sync::MutexGuard<'_, SessionsDb>> {
        self.db.lock().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> AgentSessionRecord {
        AgentSessionRecord {
            id: "abc".into(),
            workspace_id: Some("wks_test".into()),
            requires_attention: false,
            attention_reason: None,
            project: "test".into(),
            role: "worker".into(),
            runtime: "claude".into(),
            resume_key: Some("k1".into()),
            cwd: PathBuf::from("/tmp/w/agents/abc"),
            title: "Claude@worker".into(),
            status: "running".into(),
            mode: "yolo".into(),
            speed: "fast".into(),
            model: Some("claude-opus-5".into()),
            created_at: 1,
            updated_at: 2,
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
        assert!(!all[0].requires_attention);
        assert_eq!(all[0].attention_reason, None);
        // mode/speed/model survive the roundtrip.
        assert_eq!(all[0].mode, "yolo");
        assert_eq!(all[0].speed, "fast");
        assert_eq!(all[0].model.as_deref(), Some("claude-opus-5"));

        db.update_status("abc", "done", 99).unwrap();
        db.update_attention("abc", Some("finished"), 100).unwrap();
        let got = db.get("abc").unwrap().unwrap();
        assert_eq!(got.status, "done");
        assert!(got.requires_attention);
        assert_eq!(got.attention_reason.as_deref(), Some("finished"));
        assert_eq!(got.updated_at, 100);

        db.delete("abc").unwrap();
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
            requires_attention: false,
            attention_reason: None,
            role: "master".into(),
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
            serde_json::from_slice(&std::fs::read(target.join(".agent-meta.json")).unwrap()).unwrap();
        assert_eq!(read.id, "x");
        assert_eq!(read.role, "master");
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
}
