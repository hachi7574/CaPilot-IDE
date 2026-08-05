use crate::agent_runtime::adapter::{AgentError, AgentId, AgentInfo, AgentStatus};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::ipc::Channel;

/// Monotonic counter used to give each spawn attempt a unique token so that a
/// stale `kill()` can cancel only its *own* in-flight spawn (Bug 4).
static NEXT_SPAWN_TOKEN: AtomicU64 = AtomicU64::new(0);

/// Wrapper around a running PTY child process
struct PtyChild {
    #[allow(dead_code)]
    pid: u32,
    /// The master PTY handle — kept alive so we can resize (TIOCSWINSZ) later.
    master: Box<dyn MasterPty + Send>,
    /// Writer into the PTY (frontend input). Wrapped in a per-agent Mutex so a
    /// blocking `write_all` never holds the global `children` lock (Bug 2).
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// The spawned child process. Stored so `kill()` actually terminates it
    /// (the old code leaked it via `std::mem::forget`).
    child: Box<dyn Child + Send + Sync>,
    /// Background reader task streaming PTY output over the Tauri Channel.
    reader_handle: Option<tokio::task::JoinHandle<()>>,
}

/// RAII guard that keeps `agent_id` in `PtyManager.spawning` for the duration
/// of a spawn, so concurrent spawn/resume calls for the same agent are
/// serialized (Bug 4). Removes the marker on drop (every exit path).
struct SpawnGuard {
    spawning: Arc<Mutex<HashMap<AgentId, u64>>>,
    id: AgentId,
    token: u64,
}

impl Drop for SpawnGuard {
    fn drop(&mut self) {
        if let Ok(mut spawning) = self.spawning.lock() {
            // Only remove our own marker; a newer spawn may have taken the slot.
            if spawning.get(&self.id) == Some(&self.token) {
                spawning.remove(&self.id);
            }
        }
    }
}

/// Remove a PTY entry from the map and reap (wait) the child process. Safe to
/// call more than once: if the entry is already gone (e.g. `kill()` won the
/// race), this is a no-op. The map lock is only held for the `remove` — the
/// blocking `wait()` runs lock-free so other PTY ops are never stalled.
fn reap_and_remove(children: &Arc<Mutex<HashMap<AgentId, PtyChild>>>, id: &AgentId) {
    let pc = children.lock().unwrap().remove(id);
    if let Some(pc) = pc {
        // Drop the rest of the entry (master, writer, reader handle), then
        // reap the child to avoid a zombie process.
        let PtyChild { child, .. } = pc;
        let mut child = child;
        let _ = child.wait();
    }
}

/// Manages all PTY sessions. Stored as Tauri managed state (behind an Arc).
///
/// Uses a `std::sync::Mutex` with synchronous methods so both async Tauri
/// commands and the (sync) orchestrator socket handler can use it without
/// holding locks across await points.
pub struct PtyManager {
    children: Arc<Mutex<HashMap<AgentId, PtyChild>>>,
    /// Agent ids with a spawn currently in flight → the unique token of that
    /// spawn attempt. Used to serialize concurrent spawn/resume (Bug 4).
    spawning: Arc<Mutex<HashMap<AgentId, u64>>>,
}

impl PtyManager {
    pub fn new() -> Self {
        Self {
            children: Arc::new(Mutex::new(HashMap::new())),
            spawning: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Spawn a command in a new PTY and stream output via a Tauri Channel.
    pub fn spawn(
        &self,
        agent_id: AgentId,
        cmd: &str,
        args: &[String],
        cwd: &PathBuf,
        rows: u16,
        cols: u16,
        on_data: Channel<Vec<u8>>,
        env_overrides: &[(String, String)],
    ) -> Result<AgentInfo, AgentError> {
        // Serialize concurrent spawn/resume for the same agent (Bug 4). The
        // token distinguishes each spawn attempt so `kill()` can cancel only
        // the spawn it was aimed at.
        let token = NEXT_SPAWN_TOKEN.fetch_add(1, Ordering::Relaxed);
        {
            let mut spawning = self.spawning.lock().unwrap();
            if spawning.contains_key(&agent_id) {
                return Err(AgentError::PtyError(format!(
                    "spawn in progress for agent {}",
                    agent_id
                )));
            }
            spawning.insert(agent_id.clone(), token);
        }
        let _guard = SpawnGuard {
            spawning: self.spawning.clone(),
            id: agent_id.clone(),
            token,
        };

        let pty_system = native_pty_system();
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = pty_system
            .openpty(size)
            .map_err(|e| AgentError::PtyError(e.to_string()))?;

        // Extract reader and writer from the master BEFORE spawning via slave
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| AgentError::PtyError(e.to_string()))?;
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(
            pair.master
                .take_writer()
                .map_err(|e| AgentError::PtyError(e.to_string()))?,
        ));
        // Keep the master handle for resize
        let master = pair.master;

        // Build command
        let mut command = CommandBuilder::new(cmd);
        for arg in args {
            command.arg(arg);
        }
        command.cwd(cwd);
        for (k, v) in env_overrides {
            command.env(k, v);
        }

        // Spawn the child via the slave (stdin/stdout/stderr connected to slave)
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| AgentError::PtyError(e.to_string()))?;
        let pid = child.process_id().unwrap_or(0);
        // Clone a killer so the reader task can terminate the process if the
        // frontend channel is dropped without an explicit kill.
        let mut killer = child.clone_killer();

        // If `kill()` was called while this spawn was in flight, bail out now
        // and reap the child so a stale PTY never lands in the map (Bug 4).
        {
            let spawning = self.spawning.lock().unwrap();
            if spawning.get(&agent_id) != Some(&token) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AgentError::PtyError(format!(
                    "spawn cancelled for agent {}",
                    agent_id
                )));
            }
        }

        // Insert into the map BEFORE starting the reader task, so the reader's
        // EOF / channel-close cleanup can always find (and reap) the entry.
        // (Bug 1 — without this, a fast-exiting child would leave a stale entry.)
        {
            let mut children = self.children.lock().unwrap();
            children.insert(
                agent_id.clone(),
                PtyChild {
                    pid,
                    master,
                    writer,
                    child,
                    reader_handle: None,
                },
            );
        }

        // Spawn a blocking reader task (PTY reads are blocking I/O). On EOF,
        // read error, or channel close it removes the map entry and reaps the
        // child (Bug 1 + Bug 3), so no zombie and no stale state.
        let children_clone = self.children.clone();
        let reader_agent_id = agent_id.clone();
        let reader_handle = tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        // EOF — child exited. Remove + reap (Bug 1).
                        reap_and_remove(&children_clone, &reader_agent_id);
                        break;
                    }
                    Ok(n) => {
                        if on_data.send(buf[..n].to_vec()).is_err() {
                            // Tauri Channel::send errors when the channel is
                            // closed (frontend disconnected/saturated). There
                            // is no way to distinguish backpressure from close
                            // with the Tauri Channel API, so treat a send error
                            // as "channel gone": kill the child AND clean up
                            // (Bug 3). The child is killed first so `wait()`
                            // below returns promptly.
                            let _ = killer.kill();
                            reap_and_remove(&children_clone, &reader_agent_id);
                            break;
                        }
                    }
                    Err(_) => {
                        // Read error (e.g. master closed) — remove + reap.
                        reap_and_remove(&children_clone, &reader_agent_id);
                        break;
                    }
                }
            }
        });

        // Attach the reader handle now that the reader is actually running. If
        // the reader already cleaned up (instant EOF), the entry is gone and
        // the handle is simply dropped (detached).
        if let Some(child) = self.children.lock().unwrap().get_mut(&agent_id) {
            child.reader_handle = Some(reader_handle);
        }

        Ok(AgentInfo {
            id: agent_id,
            runtime: String::new(),
            role: crate::agent_runtime::adapter::AgentRole::Standalone,
            status: AgentStatus::Running,
            title: String::new(),
            cwd: cwd.clone(),
            pid: Some(pid),
        })
    }

    /// Write input to an agent's PTY.
    pub fn write(&self, agent_id: &str, data: &[u8]) -> Result<(), AgentError> {
        // Clone the per-agent writer Arc under the map lock (brief), then drop
        // the map lock before doing the blocking `write_all`. This keeps the
        // global `children` Mutex uncontended (Bug 2).
        let writer = {
            let children = self.children.lock().unwrap();
            let child = children
                .get(agent_id)
                .ok_or_else(|| AgentError::AgentNotFound(agent_id.to_string()))?;
            child.writer.clone()
        };
        let mut writer = writer.lock().unwrap();
        writer
            .write_all(data)
            .map_err(|e| AgentError::PtyError(e.to_string()))?;
        writer
            .flush()
            .map_err(|e| AgentError::PtyError(e.to_string()))?;
        Ok(())
    }

    /// Resize an agent's PTY via the stored master fd (TIOCSWINSZ).
    pub fn resize(&self, agent_id: &str, rows: u16, cols: u16) -> Result<(), AgentError> {
        let mut children = self.children.lock().unwrap();
        let child = children
            .get_mut(agent_id)
            .ok_or_else(|| AgentError::AgentNotFound(agent_id.to_string()))?;
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        child
            .master
            .resize(size)
            .map_err(|e| AgentError::PtyError(e.to_string()))
    }

    /// Kill an agent's PTY process and clean up.
    pub fn kill(&self, agent_id: &str) -> Result<(), AgentError> {
        // Cancel any in-flight spawn for this agent (Bug 4): removing the
        // marker makes that spawn's post-`spawn_command` check fail, so it
        // kills + reaps its own child instead of inserting a stale entry.
        self.spawning.lock().unwrap().remove(agent_id);

        let pc = self.children.lock().unwrap().remove(agent_id);
        if let Some(mut pc) = pc {
            if let Some(handle) = pc.reader_handle.take() {
                handle.abort();
            }
            let _ = pc.child.kill();
            // Reap to avoid a zombie (Bug 1).
            let _ = pc.child.wait();
        }
        Ok(())
    }

    /// True if a live PTY exists for the agent.
    pub fn is_alive(&self, agent_id: &str) -> bool {
        self.children.lock().unwrap().contains_key(agent_id)
    }

    /// Snapshot of live agent PIDs — `(agent_id, pid)`. Used by the resource
    /// monitor (§10) to sample whole process trees per agent.
    pub fn pids(&self) -> Vec<(String, u32)> {
        self.children
            .lock()
            .unwrap()
            .iter()
            .map(|(id, c)| (id.clone(), c.pid))
            .collect()
    }

    /// Number of live PTY sessions (for diagnostics).
    #[allow(dead_code)]
    pub fn live_count(&self) -> usize {
        self.children.lock().unwrap().len()
    }
}
