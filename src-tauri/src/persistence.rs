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
    pub project: String,
    pub role: String, // master | worker | standalone
    pub runtime: String,
    pub resume_key: Option<String>,
    pub cwd: PathBuf,
    pub title: String,
    pub status: String, // idle | running | busy | done | failed
    pub created_at: i64,
    pub updated_at: i64,
}

/// Contents of `agents/<id>/.agent-meta.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMeta {
    pub id: String,
    pub role: String,
    pub runtime: String,
    pub resume_key: Option<String>,
    pub status: String,
    pub cwd: PathBuf,
    pub title: String,
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

#[allow(dead_code)]
pub fn sessions_db_path(project: &str) -> PathBuf {
    project_dir(project).join("sessions.db")
}

// ── .agent-meta.json ────────────────────────────────────────────

pub fn write_agent_meta(project: &str, meta: &AgentMeta) -> std::io::Result<()> {
    let dir = agent_dir(project, &meta.id);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(".agent-meta.json");
    let json = serde_json::to_vec_pretty(meta).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

pub fn read_agent_meta(project: &str, agent_id: &str) -> std::io::Result<AgentMeta> {
    let path = agent_dir(project, agent_id).join(".agent-meta.json");
    let data = std::fs::read(path)?;
    Ok(serde_json::from_slice(&data)?)
}

// ── SQLite sessions DB ──────────────────────────────────────────

pub struct SessionsDb {
    conn: Connection,
}

impl SessionsDb {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
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
        )?;
        Ok(Self { conn })
    }

    pub fn insert(&self, s: &AgentSessionRecord) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO sessions
                (id, project, role, runtime, resume_key, cwd, title, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                project=excluded.project, role=excluded.role, runtime=excluded.runtime,
                resume_key=excluded.resume_key, cwd=excluded.cwd, title=excluded.title,
                status=excluded.status, updated_at=excluded.updated_at",
            params![
                s.id,
                s.project,
                s.role,
                s.runtime,
                s.resume_key,
                s.cwd.to_string_lossy(),
                s.title,
                s.status,
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

    pub fn get(&self, id: &str) -> rusqlite::Result<Option<AgentSessionRecord>> {
        self.conn
            .query_row(
                "SELECT id, project, role, runtime, resume_key, cwd, title, status, created_at, updated_at
                 FROM sessions WHERE id = ?1",
                params![id],
                Self::row_to_session,
            )
            .optional()
    }

    pub fn list_all(&self) -> rusqlite::Result<Vec<AgentSessionRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, project, role, runtime, resume_key, cwd, title, status, created_at, updated_at FROM sessions ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], Self::row_to_session)?;
        rows.collect()
    }

    pub fn delete(&self, id: &str) -> rusqlite::Result<()> {
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        Ok(())
    }

    fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentSessionRecord> {
        Ok(AgentSessionRecord {
            id: row.get(0)?,
            project: row.get(1)?,
            role: row.get(2)?,
            runtime: row.get(3)?,
            resume_key: row.get(4)?,
            cwd: PathBuf::from(row.get::<_, String>(5)?),
            title: row.get(6)?,
            status: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    }
}

// ── Managed state ───────────────────────────────────────────────

/// Shared persistence handle. Holds the current project and its session DB.
/// `rusqlite::Connection` is not Sync, so the DB sits behind a Mutex.
pub struct Persistence {
    project: String,
    db: Mutex<SessionsDb>,
}

impl Persistence {
    pub fn open(project: &str) -> std::io::Result<Self> {
        let project = if project.is_empty() {
            DEFAULT_PROJECT.to_string()
        } else {
            project.to_string()
        };
        let dir = ensure_project(&project)?;
        let db = SessionsDb::open(&dir.join("sessions.db"))
            .map_err(std::io::Error::other)?;
        Ok(Self {
            project,
            db: Mutex::new(db),
        })
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn db(&self) -> &Mutex<SessionsDb> {
        &self.db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> AgentSessionRecord {
        AgentSessionRecord {
            id: "abc".into(),
            project: "test".into(),
            role: "worker".into(),
            runtime: "claude".into(),
            resume_key: Some("k1".into()),
            cwd: PathBuf::from("/tmp/w/agents/abc"),
            title: "Claude@worker".into(),
            status: "running".into(),
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

        db.update_status("abc", "done", 99).unwrap();
        let got = db.get("abc").unwrap().unwrap();
        assert_eq!(got.status, "done");
        assert_eq!(got.updated_at, 99);

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
            role: "master".into(),
            runtime: "claude".into(),
            resume_key: None,
            status: "running".into(),
            cwd: target.clone(),
            title: "t".into(),
            updated_at: 5,
        };
        let json = serde_json::to_vec_pretty(&meta).unwrap();
        std::fs::write(target.join(".agent-meta.json"), json).unwrap();
        let read: AgentMeta =
            serde_json::from_slice(&std::fs::read(target.join(".agent-meta.json")).unwrap()).unwrap();
        assert_eq!(read.id, "x");
        assert_eq!(read.role, "master");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
