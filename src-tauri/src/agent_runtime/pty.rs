use crate::agent_runtime::adapter::{AgentError, AgentId, AgentInfo, AgentStatus};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::ipc::Channel;

/// Monotonic counter used to give each spawn attempt a unique token so that a
/// stale `kill()` can cancel only its *own* in-flight spawn (Bug 4).
static NEXT_SPAWN_TOKEN: AtomicU64 = AtomicU64::new(0);

/// Natural-exit callback: `(agent_id, exit_code)`. Fired by the reader task when
/// the child exits on its own (EOF / read error). Intentional kills (`kill()`,
/// channel-gone) clear/suppress it so a session that was deliberately stopped is
/// never misreported as a natural "done".
pub type OnExit = Arc<dyn Fn(String, i32) + Send + Sync>;

/// Wrapper around a running PTY child process
struct PtyChild {
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
    /// Natural-exit callback (see `OnExit`). `None` after an intentional kill.
    on_exit: Option<OnExit>,
    /// Set once the exit was intentional (kill / channel-gone), so the reader
    /// never fires `on_exit` for a deliberately stopped session.
    killed: Arc<AtomicBool>,
    /// Monotonic generation for this entry — the spawn token that created it.
    /// The reader task compares the live map entry's generation to its own
    /// before reaping on EOF/read-error, so a stale reader left over from a
    /// `kill()` + respawn never removes the NEW child's entry (Bug 5).
    generation: u64,
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

/// Remove a PTY entry from the map and reap (wait) the child process, returning
/// its exit code (-1 if unknown). Safe to call more than once: if the entry is
/// already gone (e.g. `kill()` won the race), this is a no-op. The map lock is
/// only held for the `remove` — the blocking `wait()` runs lock-free so other
/// PTY ops are never stalled.
fn reap_and_remove(children: &Arc<Mutex<HashMap<AgentId, PtyChild>>>, id: &AgentId) -> i32 {
    let pc = children.lock().unwrap().remove(id);
    if let Some(pc) = pc {
        // Drop the rest of the entry (master, writer, reader handle), then
        // reap the child to avoid a zombie process.
        let PtyChild { child, .. } = pc;
        let mut child = child;
        child.wait().map(|s| s.exit_code() as i32).unwrap_or(-1)
    } else {
        -1
    }
}

/// True if the live map entry for `id` is the one this reader started — i.e.
/// its generation still equals the token the reader captured at spawn time.
///
/// A stale reader can outlive its entry: `handle.abort()` cannot cancel a
/// `spawn_blocking` task blocked in `reader.read()`, so after a
/// `kill()`-then-respawn (e.g. `agent_resume` / `agent_switch_runtime`) the old
/// reader eventually sees EOF and would otherwise `reap_and_remove` the NEW
/// child's entry. Guarding with the generation keeps the new process alive and
/// lets the stale reader just clean itself up by returning (Bug 5).
fn is_own_entry(
    children: &Arc<Mutex<HashMap<AgentId, PtyChild>>>,
    id: &AgentId,
    generation: u64,
) -> bool {
    children
        .lock()
        .unwrap()
        .get(id)
        .map(|c| c.generation == generation)
        .unwrap_or(false)
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
        on_exit: Option<OnExit>,
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

        // The reader task needs the same `killed`/`on_exit` that go into the
        // map entry, plus this spawn's unique token as its *generation*. Capture
        // them up front (clones) so a concurrent `kill()` + respawn between the
        // map insert and the reader starting can never make the reader bind
        // itself to a DIFFERENT (newer) entry (Bug 5).
        let generation = token;
        let killed = Arc::new(AtomicBool::new(false));
        let reader_killed = killed.clone();
        let reader_on_exit = on_exit.clone();

        // Cancel-check + insert happen under ONE lock acquisition, so there is
        // no window where `kill()` can remove the `spawning` marker between the
        // check and the insert and still have the live child land in the map
        // (Bug 4 TOCTOU). The entry stores this spawn's token as `generation`,
        // so later readers / stale readers can tell which spawn owns it.
        {
            let spawning = self.spawning.lock().unwrap();
            let mut children = self.children.lock().unwrap();
            if spawning.get(&agent_id) != Some(&token) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AgentError::PtyError(format!(
                    "spawn cancelled for agent {}",
                    agent_id
                )));
            }
            // Insert into the map BEFORE starting the reader task, so the
            // reader's EOF / channel-close cleanup can always find (and reap)
            // the entry. (Bug 1 — without this, a fast-exiting child would
            // leave a stale entry.)
            children.insert(
                agent_id.clone(),
                PtyChild {
                    pid,
                    master,
                    writer,
                    child,
                    reader_handle: None,
                    on_exit,
                    killed,
                    generation,
                },
            );
        }

        // Spawn a blocking reader task (PTY reads are blocking I/O). On EOF,
        // read error, or channel close it removes the map entry and reaps the
        // child (Bug 1 + Bug 3), so no zombie and no stale state — but only if
        // the live entry is still this reader's own generation (Bug 5). Natural
        // exit (EOF / read error) also fires `on_exit` so the session lifecycle
        // can be persisted; intentional kills and channel-gone never do.
        let children_clone = self.children.clone();
        let reader_agent_id = agent_id.clone();
        let reader_handle = tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        // EOF — child exited. Remove + reap only our own entry
                        // (a stale reader must not tear down a respawned child),
                        // then report the natural exit so the session row can be
                        // finalized.
                        if is_own_entry(&children_clone, &reader_agent_id, generation) {
                            let exit_code = reap_and_remove(&children_clone, &reader_agent_id);
                            if !reader_killed.load(Ordering::SeqCst) {
                                if let Some(cb) = &reader_on_exit {
                                    cb(reader_agent_id.clone(), exit_code);
                                }
                            }
                        }
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
                            // below returns promptly. This is an intentional
                            // teardown, so `on_exit` is suppressed.
                            reader_killed.store(true, Ordering::SeqCst);
                            let _ = killer.kill();
                            if is_own_entry(&children_clone, &reader_agent_id, generation) {
                                reap_and_remove(&children_clone, &reader_agent_id);
                            }
                            break;
                        }
                    }
                    Err(_) => {
                        // Read error (e.g. master closed) — remove + reap only
                        // our own entry, then report the natural exit like EOF.
                        if is_own_entry(&children_clone, &reader_agent_id, generation) {
                            let exit_code = reap_and_remove(&children_clone, &reader_agent_id);
                            if !reader_killed.load(Ordering::SeqCst) {
                                if let Some(cb) = &reader_on_exit {
                                    cb(reader_agent_id.clone(), exit_code);
                                }
                            }
                        }
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
            workspace_id: None,
            project: None,
            runtime: String::new(),
            role: crate::agent_runtime::adapter::AgentRole::Standalone,
            status: AgentStatus::Running,
            title: String::new(),
            cwd: cwd.clone(),
            pid: Some(pid),
            mode: String::new(),
            speed: String::new(),
            model: None,
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

        // Remove the entry up front. The reader (if it wins the race to EOF)
        // will see its own generation is gone and skip `reap_and_remove`
        // (Bug 5), so a subsequent respawn's entry is never torn down.
        let pc = self.children.lock().unwrap().remove(agent_id);
        if let Some(mut pc) = pc {
            if let Some(handle) = pc.reader_handle.take() {
                // Best-effort: aborting cannot cancel a `spawn_blocking` task
                // blocked in `reader.read()`, but it marks the task cancelled
                // so it cleans up once the blocking read returns. The real
                // protection for a respawned entry is the generation check in
                // the reader (Bug 5).
                handle.abort();
            }
            // Intentional kill — the reader (if it wins the race to EOF) must
            // not fire `on_exit` and misreport this as a natural session end.
            pc.killed.store(true, Ordering::SeqCst);
            pc.on_exit = None;
            let _ = pc.child.kill();
            // Reap to avoid a zombie (Bug 1).
            let _ = pc.child.wait();
        }
        Ok(())
    }

    /// Kill every live PTY (used on app quit so no agent process is orphaned).
    /// Same semantics as `kill`: intentional teardown, so `on_exit` is
    /// suppressed and the session rows stay `running` (recoverable next launch).
    pub fn kill_all(&self) {
        let ids: Vec<AgentId> = self.children.lock().unwrap().keys().cloned().collect();
        for id in ids {
            let _ = self.kill(&id);
        }
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
