use crate::agent_runtime::adapter::{AgentError, AgentId, AgentInfo, AgentStatus};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::ipc::Channel;

/// Wrapper around a running PTY child process
struct PtyChild {
    #[allow(dead_code)]
    pid: u32,
    /// The master PTY handle — kept alive so we can resize (TIOCSWINSZ) later.
    master: Box<dyn MasterPty + Send>,
    /// Writer into the PTY (frontend input).
    writer: Box<dyn Write + Send>,
    /// The spawned child process. Stored so `kill()` actually terminates it
    /// (the old code leaked it via `std::mem::forget`).
    child: Box<dyn Child + Send + Sync>,
    /// Background reader task streaming PTY output over the Tauri Channel.
    reader_handle: Option<tokio::task::JoinHandle<()>>,
}

/// Manages all PTY sessions. Stored as Tauri managed state (behind an Arc).
///
/// Uses a `std::sync::Mutex` with synchronous methods so both async Tauri
/// commands and the (sync) orchestrator socket handler can use it without
/// holding locks across await points.
pub struct PtyManager {
    children: Arc<Mutex<HashMap<AgentId, PtyChild>>>,
}

impl PtyManager {
    pub fn new() -> Self {
        Self {
            children: Arc::new(Mutex::new(HashMap::new())),
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
        let writer: Box<dyn Write + Send> = pair
            .master
            .take_writer()
            .map_err(|e| AgentError::PtyError(e.to_string()))?;
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
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| AgentError::PtyError(e.to_string()))?;
        let pid = child.process_id().unwrap_or(0);
        // Clone a killer so the reader task can terminate the process if the
        // frontend channel is dropped without an explicit kill.
        let mut killer = child.clone_killer();

        // Spawn a blocking reader task (PTY reads are blocking I/O)
        let reader_handle = tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF — child exited
                    Ok(n) => {
                        if on_data.send(buf[..n].to_vec()).is_err() {
                            // Channel closed (frontend disconnected) — kill the child
                            let _ = killer.kill();
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut children = self.children.lock().unwrap();
        children.insert(
            agent_id.clone(),
            PtyChild {
                pid,
                master,
                writer,
                child,
                reader_handle: Some(reader_handle),
            },
        );

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
        let mut children = self.children.lock().unwrap();
        let child = children
            .get_mut(agent_id)
            .ok_or_else(|| AgentError::AgentNotFound(agent_id.to_string()))?;
        child
            .writer
            .write_all(data)
            .map_err(|e| AgentError::PtyError(e.to_string()))?;
        child
            .writer
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
        let mut children = self.children.lock().unwrap();
        if let Some(mut child) = children.remove(agent_id) {
            if let Some(handle) = child.reader_handle.take() {
                handle.abort();
            }
            let _ = child.child.kill();
            drop(child);
        }
        Ok(())
    }

    /// True if a live PTY exists for the agent.
    pub fn is_alive(&self, agent_id: &str) -> bool {
        self.children.lock().unwrap().contains_key(agent_id)
    }

    /// Number of live PTY sessions (for diagnostics).
    #[allow(dead_code)]
    pub fn live_count(&self) -> usize {
        self.children.lock().unwrap().len()
    }
}
