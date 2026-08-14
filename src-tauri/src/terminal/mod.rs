//! TerminalService — the PTY host (architecture §3.1, §4.1, §13).
//!
//! Agent and Terminal are separate concepts: an Agent is a structured session
//! (timeline/events/permissions, owned by `agent_provider`), while a Terminal is
//! a byte-stream process. This module is the home of the terminal half: bash /
//! zsh, workspace scripts and interactive auth/diagnostic terminals all run as
//! PTYs owned by [`TerminalService`].
//!
//! The resident daemon owns one [`TerminalService`] and serves the terminal
//! surface (`terminal_create` / `terminal_attach` / `terminal_write` /
//! `terminal_resize` / `terminal_kill` / `terminal_list`, architecture §13)
//! through the framed protocol in `daemon/protocol.rs`. The GUI bridge
//! (`bridge.rs`) is the client; the in-process fallback drives [`PtyCore`]
//! directly.
//!
//! [`PtyCore`] is the Tauri-independent PTY lifecycle core (spawn / write /
//! resize / kill / reap, generation checks, natural-exit callbacks, the
//! atomic live-slot budget). It never touches the frontend store, the
//! canonical timeline, or the structured agent manager.

pub mod pty_core;

pub use pty_core::{OnExit, OutputSink, PtyCore, SinkError, SinkResult};

use crate::agent_runtime::adapter::{AgentError, AgentId, AgentInfo};
use std::path::PathBuf;
use std::sync::Arc;

/// The daemon's terminal facade over [`PtyCore`] (architecture §4.1). Owns the
/// PTY set; structured agent sessions are deliberately NOT routed through it —
/// they live in the `AgentManager` and never use raw PTY writes.
pub struct TerminalService {
    pty: Arc<PtyCore>,
}

impl Default for TerminalService {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalService {
    pub fn new() -> Self {
        Self {
            pty: Arc::new(PtyCore::new()),
        }
    }

    /// Spawn a command in a new PTY and stream output to `sink` (§13
    /// `terminal_create`). Same semantics as [`PtyCore::spawn`], including the
    /// atomic live-session cap.
    pub fn spawn(
        &self,
        agent_id: AgentId,
        cmd: &str,
        args: &[String],
        cwd: &PathBuf,
        rows: u16,
        cols: u16,
        sink: Arc<dyn OutputSink>,
        on_exit: Option<OnExit>,
        env_overrides: &[(String, String)],
    ) -> Result<AgentInfo, AgentError> {
        self.pty.spawn(
            agent_id,
            cmd,
            args,
            cwd,
            rows,
            cols,
            sink,
            on_exit,
            env_overrides,
        )
    }

    /// Write bytes to a terminal's PTY master (§13 `terminal_write`). Generation
    /// checks are enforced by [`PtyCore`].
    pub fn write(&self, agent_id: &str, data: &[u8]) -> Result<(), AgentError> {
        self.pty.write(agent_id, data)
    }

    pub fn resize(&self, agent_id: &str, rows: u16, cols: u16) -> Result<(), AgentError> {
        self.pty.resize(agent_id, rows, cols)
    }

    pub fn kill(&self, agent_id: &str) -> Result<(), AgentError> {
        self.pty.kill(agent_id)
    }

    /// Kill every live terminal (§13 `terminal_kill`; used at daemon shutdown).
    pub fn kill_all(&self) {
        self.pty.kill_all();
    }

    /// Live (agent_id, pid) pairs — the daemon's liveness authority for `List`.
    pub fn pids(&self) -> Vec<(String, u32)> {
        self.pty.pids()
    }

    /// Current spawn generation for `agent_id`, if live (§4.2 generation guard).
    pub fn generation(&self, agent_id: &str) -> Option<u64> {
        self.pty.generation(agent_id)
    }

    /// The underlying PTY core (used by the daemon for subscriber wiring).
    pub fn core(&self) -> &PtyCore {
        &self.pty
    }
}
