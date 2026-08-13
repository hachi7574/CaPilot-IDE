//! Tauri-independent session store (§6.1) — shared by the GUI fallback path and
//! (later) the PTY daemon.
//!
//! Both owners must persist session metadata through the same rules: SQLite
//! `sessions.db` is the source of truth, each `.agent-meta.json` is a derivable
//! copy protected by the per-agent cross-process lock, and a natural exit ends a
//! session with the user's `session_end_mode` policy. Bundling them here means
//! the daemon opens the exact same store as the GUI and cannot drift.
//!
//! `Persistence` (in `persistence.rs`) is the thin Tauri-managed wrapper over
//! this store; the daemon constructs a `SessionStore` directly.

use crate::persistence::{
    agent_dir, read_agent_meta, repair_agent_meta, workspace_root, write_agent_meta, SessionsDb,
};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Setting key controlling what happens when an agent exits naturally: anything
/// except `"delete"` keeps the row (status → `done`); `"delete"` removes the
/// row and the agent dir.
pub const SESSION_END_MODE_KEY: &str = "session_end_mode";

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Outcome of [`SessionStore::apply_natural_exit`]. The caller (GUI bridge /
/// daemon) uses this to emit the matching lifecycle event — no Tauri dependency
/// here.
#[derive(Debug, Clone)]
pub struct NaturalExit {
    /// The project the session belonged to (if the row was still present).
    pub project: Option<String>,
    /// True when `session_end_mode` is "delete" and the row + agent dir were
    /// removed; false when the session was kept (status → `done`).
    pub deleted: bool,
}

/// Shared, Tauri-independent persistence facade over the sessions DB and the
/// per-agent sidecar files.
pub struct SessionStore {
    /// Single sessions DB behind a Mutex (`rusqlite::Connection` is not Sync).
    db: Mutex<SessionsDb>,
    /// `~/CaPilot` — parent of `sessions.db` and of `workspaces/`.
    base: PathBuf,
}

impl SessionStore {
    /// Open (creating the parent dir if needed) the store at `base`, with the
    /// DB at `base/sessions.db`. WAL / `busy_timeout` / cross-process sidecar
    /// locking live in `SessionsDb` / `AgentMetaGuard`.
    pub fn from_base(base: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&base)?;
        let db_path = base.join("sessions.db");
        let db = SessionsDb::open(&db_path).map_err(std::io::Error::other)?;
        Ok(Self {
            db: Mutex::new(db),
            base,
        })
    }

    /// Open the store at the standard `~/CaPilot` location.
    pub fn open() -> std::io::Result<Self> {
        let base = workspace_root()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("CaPilot"));
        Self::from_base(base)
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    pub fn db(&self) -> &Mutex<SessionsDb> {
        &self.db
    }

    /// Lock the sessions DB, tolerating a poisoned mutex (a panic while holding
    /// the lock marks it poisoned; `unwrap()` would then panic on every
    /// command). Returns None only if the lock is currently held by a panicked
    /// holder that never released — practically never.
    pub fn db_tolerant(&self) -> Option<std::sync::MutexGuard<'_, SessionsDb>> {
        self.db.lock().ok()
    }

    /// Mark a session `done` on natural exit: update the DB row and sync the
    /// `.agent-meta.json` status. Best-effort like the pre-extraction code — a
    /// transient DB/sidecar failure must never turn a natural exit into a hard
    /// error.
    pub fn mark_done(&self, agent_id: &str) -> NaturalExit {
        let _ = self
            .db
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .update_status(agent_id, "done", now_ms());
        // Keep the per-agent meta in sync (dual-write convention) so a stale
        // `.agent-meta.json` never shows a finished session as running. The
        // project is read fresh so a project rename moves the correct dir.
        let project = self
            .db
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(agent_id)
            .ok()
            .flatten()
            .map(|rec| rec.project);
        if let Some(project) = &project {
            if let Ok(mut meta) = read_agent_meta(project, agent_id) {
                meta.status = "done".to_string();
                meta.updated_at = now_ms();
                let _ = write_agent_meta(project, &meta);
            }
        }
        NaturalExit {
            project,
            deleted: false,
        }
    }

    /// Delete a session (natural-exit delete mode): remove the DB row and the
    /// agent dir, when the dir is under the workspace root. Best-effort.
    pub fn delete_session(&self, agent_id: &str) -> NaturalExit {
        let project = self
            .db
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(agent_id)
            .ok()
            .flatten()
            .map(|rec| rec.project)
            .unwrap_or_default();
        let _ = self
            .db
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .delete(agent_id);
        let dir = agent_dir(&project, agent_id);
        if dir.starts_with(workspace_root()) && dir.exists() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        NaturalExit {
            project: Some(project),
            deleted: true,
        }
    }

    /// Apply the natural-exit policy (`session_end_mode`) for an agent that
    /// exited on its own. Re-reads the setting each call so a settings change
    /// applies without a restart.
    pub fn apply_natural_exit(&self, agent_id: &str) -> NaturalExit {
        let delete = self
            .db
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_setting(SESSION_END_MODE_KEY)
            .ok()
            .flatten()
            .as_deref()
            == Some("delete");
        if delete {
            self.delete_session(agent_id)
        } else {
            self.mark_done(agent_id)
        }
    }

    /// Startup repair: recreate any missing/corrupt `.agent-meta.json` from the
    /// DB row (source of truth). Idempotent. Returns the number of files
    /// recreated.
    pub fn repair(&self) -> std::io::Result<usize> {
        let db = self.db.lock().unwrap_or_else(|p| p.into_inner());
        repair_agent_meta(&db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::ENV_LOCK;
    use crate::persistence::{ensure_project, write_agent_meta, AgentMeta};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A temp HOME so `workspace_root()` points somewhere isolated. The env
    /// lock is shared crate-wide (persistence + runtimes touch the same
    /// process-global HOME), so tests never observe each other's env.
    fn with_isolated_home(f: impl FnOnce(PathBuf)) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = std::env::temp_dir().join(format!(
            "capilot_session_store_{}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&home).unwrap();
        let old = std::env::var("HOME").ok();
        std::env::set_var("HOME", &home);
        f(home.clone());
        match old {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    fn insert_row(store: &SessionStore, id: &str, project: &str) {
        let rec = crate::persistence::AgentSessionRecord {
            id: id.to_string(),
            workspace_id: None,
            project: project.to_string(),
            runtime: "claude".to_string(),
            resume_key: None,
            cwd: agent_dir(project, id),
            title: "t".to_string(),
            status: "running".to_string(),
            mode: "ask".to_string(),
            speed: "auto".to_string(),
            model: None,
            created_at: 1,
            updated_at: 1,
        };
        store
            .db()
            .lock()
            .unwrap()
            .insert(&rec)
            .expect("insert row");
    }

    fn sidecar(project: &str, id: &str) -> AgentMeta {
        read_agent_meta(project, id).expect("sidecar readable")
    }

    #[test]
    fn natural_exit_keep_marks_done_and_syncs_meta() {
        with_isolated_home(|home| {
            let store = SessionStore::from_base(home.join("CaPilot")).unwrap();
            ensure_project("proj").unwrap();
            insert_row(&store, "s1", "proj");
            let dir = agent_dir("proj", "s1");
            std::fs::create_dir_all(&dir).unwrap();
            write_agent_meta(
                "proj",
                &AgentMeta {
                    id: "s1".into(),
                    workspace_id: None,
                    runtime: "claude".into(),
                    resume_key: None,
                    status: "running".into(),
                    cwd: dir,
                    title: "t".into(),
                    mode: "ask".into(),
                    speed: "auto".into(),
                    model: None,
                    updated_at: 1,
                },
            )
            .unwrap();

            // Default mode (no `session_end_mode` setting) → keep.
            let outcome = store.apply_natural_exit("s1");
            assert!(!outcome.deleted);
            assert_eq!(outcome.project.as_deref(), Some("proj"));

            let row = store.db().lock().unwrap().get("s1").unwrap().unwrap();
            assert_eq!(row.status, "done");
            assert_eq!(sidecar("proj", "s1").status, "done");
        });
    }

    #[test]
    fn natural_exit_delete_removes_row_and_dir() {
        with_isolated_home(|home| {
            let store = SessionStore::from_base(home.join("CaPilot")).unwrap();
            ensure_project("proj").unwrap();
            insert_row(&store, "s2", "proj");
            let dir = agent_dir("proj", "s2");
            std::fs::create_dir_all(&dir).unwrap();
            write_agent_meta(
                "proj",
                &AgentMeta {
                    id: "s2".into(),
                    workspace_id: None,
                    runtime: "claude".into(),
                    resume_key: None,
                    status: "running".into(),
                    cwd: dir.clone(),
                    title: "t".into(),
                    mode: "ask".into(),
                    speed: "auto".into(),
                    model: None,
                    updated_at: 1,
                },
            )
            .unwrap();
            store
                .db()
                .lock()
                .unwrap()
                .set_setting(SESSION_END_MODE_KEY, "delete")
                .unwrap();

            let outcome = store.apply_natural_exit("s2");
            assert!(outcome.deleted);
            assert_eq!(outcome.project.as_deref(), Some("proj"));
            assert!(store.db().lock().unwrap().get("s2").unwrap().is_none());
            assert!(!dir.exists(), "agent dir must be removed in delete mode");
        });
    }

    #[test]
    fn repair_recreates_missing_sidecar() {
        with_isolated_home(|home| {
            let store = SessionStore::from_base(home.join("CaPilot")).unwrap();
            ensure_project("proj").unwrap();
            insert_row(&store, "s3", "proj");
            let dir = agent_dir("proj", "s3");
            std::fs::create_dir_all(&dir).unwrap();
            // No sidecar written — repair must recreate it from the DB row.
            assert_eq!(store.repair().unwrap(), 1);
            assert_eq!(sidecar("proj", "s3").status, "running");
            // Idempotent — second pass recreates nothing.
            assert_eq!(store.repair().unwrap(), 0);
        });
    }
}
