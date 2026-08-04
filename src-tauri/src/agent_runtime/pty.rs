use crate::agent_runtime::adapter::{AgentError, AgentId, AgentInfo, AgentStatus};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::ipc::Channel;
use tokio::sync::Mutex;

/// Wrapper around a running PTY child process
struct PtyChild {
    pid: u32,
    writer: Box<dyn Write + Send>,
    reader_handle: Option<tokio::task::JoinHandle<()>>,
}

/// Manages all PTY sessions. Stored as Tauri managed state.
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
    pub async fn spawn(
        &self,
        agent_id: AgentId,
        cmd: &str,
        args: &[String],
        cwd: &PathBuf,
        rows: u16,
        cols: u16,
        on_data: Channel<Vec<u8>>,
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

        // Build command
        let mut command = CommandBuilder::new(cmd);
        for arg in args {
            command.arg(arg);
        }
        command.cwd(cwd);

        // Spawn the child via the slave (stdin/stdout/stderr connected to slave)
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| AgentError::PtyError(e.to_string()))?;
        let pid = child.process_id().unwrap_or(0);

        // Spawn a blocking reader task (PTY reads are blocking I/O)
        let reader_handle = tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF — child exited
                    Ok(n) => {
                        if on_data.send(buf[..n].to_vec()).is_err() {
                            break; // Channel closed (frontend disconnected)
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut children = self.children.lock().await;
        children.insert(
            agent_id.clone(),
            PtyChild {
                pid,
                writer,
                reader_handle: Some(reader_handle),
            },
        );

        // Keep the child alive in background
        std::mem::forget(child);

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
    pub async fn write(&self, agent_id: &str, data: &[u8]) -> Result<(), AgentError> {
        let mut children = self.children.lock().await;
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

    /// Resize an agent's PTY (stub — requires master fd for TIOCSWINSZ).
    pub async fn resize(&self, _agent_id: &str, _rows: u16, _cols: u16) -> Result<(), AgentError> {
        // PTY resize requires the master file descriptor, which is consumed during spawn.
        // Will be implemented later via stored raw fd + TIOCSWINSZ ioctl.
        Ok(())
    }

    /// Kill an agent's PTY process.
    pub async fn kill(&self, agent_id: &str) -> Result<(), AgentError> {
        let mut children = self.children.lock().await;
        if let Some(mut child) = children.remove(agent_id) {
            if let Some(handle) = child.reader_handle.take() {
                handle.abort();
            }
            drop(child);
        }
        Ok(())
    }
}
