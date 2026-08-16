//! AcpBridge — process table parallel to PtyBridge for ACP sessions.

use super::descriptor::{self, AcpAgentDescriptor};
use super::events::{AcpEvent, AcpEventSink};
use super::host::{self, AcpHostError, AcpSessionHandle, AcpSessionStatus};
use super::permission::{PermissionBoard, PermissionOutcome};
use super::{acp_runtime_id, is_acp_runtime, strip_acp_prefix};
use crate::agent_runtime::adapter::{AgentInfo, AgentStatus};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

/// Tauri event name for ACP session events.
pub const ACP_EVENT: &str = "acp://event";

/// Forwards host events onto the Tauri event bus.
pub struct TauriEventSink {
    app: AppHandle,
}

impl TauriEventSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl AcpEventSink for TauriEventSink {
    fn emit(&self, agent_id: &str, event: AcpEvent) {
        let envelope = super::events::AcpEventEnvelope {
            agent_id: agent_id.to_string(),
            event,
        };
        let _ = self.app.emit(ACP_EVENT, &envelope);
    }
}

struct SessionEntry {
    handle: Arc<AcpSessionHandle>,
    info: AgentInfo,
}

/// In-process registry of live ACP sessions.
pub struct AcpBridge {
    sessions: Mutex<HashMap<String, SessionEntry>>,
    permissions: PermissionBoard,
    /// Optional app handle — set after Tauri starts so events can emit.
    app: Mutex<Option<AppHandle>>,
}

impl AcpBridge {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            sessions: Mutex::new(HashMap::new()),
            permissions: PermissionBoard::new(),
            app: Mutex::new(None),
        })
    }

    pub fn bind_app(&self, app: AppHandle) {
        if let Ok(mut g) = self.app.lock() {
            *g = Some(app);
        }
    }

    fn sink(&self) -> Arc<dyn AcpEventSink> {
        if let Ok(g) = self.app.lock() {
            if let Some(app) = g.as_ref() {
                return Arc::new(TauriEventSink::new(app.clone()));
            }
        }
        // Fallback: drop events (tests / pre-bind).
        Arc::new(NullSink)
    }

    /// Start an ACP session for `runtime` (`acp:…`).
    pub fn start(
        &self,
        id: &str,
        runtime: &str,
        cwd: &Path,
        title: &str,
        mode: &str,
        speed: &str,
        model: Option<String>,
        workspace_id: Option<String>,
        project: Option<String>,
        resume_key: Option<&str>,
        desc_override: Option<AcpAgentDescriptor>,
    ) -> Result<AgentInfo, AcpHostError> {
        if !is_acp_runtime(runtime) {
            return Err(AcpHostError::Message(format!(
                "not an ACP runtime: {runtime}"
            )));
        }
        let short = strip_acp_prefix(runtime).unwrap_or(runtime);
        let desc = match desc_override {
            Some(d) => d,
            None => descriptor::find_descriptor(short).ok_or_else(|| {
                AcpHostError::Message(format!("unknown ACP agent descriptor: {short}"))
            })?,
        };
        if !descriptor::command_available(&desc.command) {
            return Err(AcpHostError::Message(format!(
                "ACP agent command not found on PATH: {}",
                desc.command
            )));
        }

        let sink = self.sink();
        let handle = host::start_session(
            id,
            runtime,
            &desc,
            cwd,
            resume_key,
            sink,
            self.permissions.clone(),
        )?;
        let handle = Arc::new(handle);

        let acp_sid = handle.acp_session_id();
        let info = AgentInfo {
            id: id.to_string(),
            workspace_id,
            project,
            runtime: runtime.to_string(),
            status: AgentStatus::Idle,
            title: title.to_string(),
            cwd: cwd.to_path_buf(),
            pid: None,
            mode: mode.to_string(),
            speed: speed.to_string(),
            model,
            last_usage: None,
        };

        // Stash resume key (= acp session id) is returned to caller via info;
        // persistence layer writes it separately.
        let _ = acp_sid;

        if let Ok(mut g) = self.sessions.lock() {
            g.insert(
                id.to_string(),
                SessionEntry {
                    handle,
                    info: info.clone(),
                },
            );
        }
        Ok(info)
    }

    /// Convenience: start with an explicit descriptor (tests / mock).
    pub fn start_with_descriptor(
        &self,
        id: &str,
        desc: AcpAgentDescriptor,
        cwd: &Path,
        resume_key: Option<&str>,
    ) -> Result<AgentInfo, AcpHostError> {
        let runtime = acp_runtime_id(&desc.id);
        self.start(
            id,
            &runtime,
            cwd,
            "acp-test",
            "ask",
            "auto",
            None,
            None,
            None,
            resume_key,
            Some(desc),
        )
    }

    pub fn prompt(&self, id: &str, text: &str) -> Result<String, AcpHostError> {
        let handle = self.handle(id)?;
        // DEF-007: emit running as soon as the turn starts so the UI Stop
        // button does not depend solely on the frontend optimistic busy flag.
        self.sink().emit(
            id,
            AcpEvent::Status {
                status: "running".into(),
            },
        );
        // Long timeout: real models can be slow; mock is instant.
        let result = handle.prompt(text, Duration::from_secs(300));
        // On hard failure before turn_done, drop back to idle so the strip
        // does not stick on 运行中. Successful turns already emit idle via
        // handle_line(stopReason).
        if result.is_err() {
            self.sink().emit(
                id,
                AcpEvent::Status {
                    status: "idle".into(),
                },
            );
        }
        result
    }

    /// Fire-and-forget prompt on a background thread (Tauri command path).
    pub fn prompt_async(self: &Arc<Self>, id: String, text: String) {
        let bridge = Arc::clone(self);
        std::thread::Builder::new()
            .name(format!("acp-prompt-{id}"))
            .spawn(move || {
                if let Err(e) = bridge.prompt(&id, &text) {
                    // Error already emitted as turn failure inside host when possible.
                    log::warn!("acp_prompt {id}: {e}");
                    if let Ok(g) = bridge.app.lock() {
                        if let Some(app) = g.as_ref() {
                            let env = super::events::AcpEventEnvelope {
                                agent_id: id,
                                event: AcpEvent::Error {
                                    message: e.to_string(),
                                },
                            };
                            let _ = app.emit(ACP_EVENT, &env);
                        }
                    }
                }
            })
            .ok();
    }

    /// Cancel in-flight turn — **notification only** (DEF-002).
    pub fn cancel(&self, id: &str) -> Result<(), AcpHostError> {
        self.handle(id)?.cancel()
    }

    pub fn respond_permission(
        &self,
        id: &str,
        request_id: &str,
        outcome: PermissionOutcome,
    ) -> Result<(), AcpHostError> {
        // Drop from board (best-effort).
        let _ = self.permissions.take(id, request_id);
        self.handle(id)?.respond_permission(request_id, outcome)
    }

    pub fn kill(&self, id: &str) -> Result<(), AcpHostError> {
        let handle = {
            let mut g = self
                .sessions
                .lock()
                .map_err(|_| AcpHostError::Message("lock poisoned".into()))?;
            g.remove(id).map(|e| e.handle)
        };
        if let Some(h) = handle {
            h.kill()?;
            self.permissions.clear_agent(id);
            Ok(())
        } else {
            Err(AcpHostError::Message(format!("ACP session not found: {id}")))
        }
    }

    pub fn contains(&self, id: &str) -> bool {
        self.sessions
            .lock()
            .map(|g| g.contains_key(id))
            .unwrap_or(false)
    }

    pub fn status(&self, id: &str) -> Option<AcpSessionStatus> {
        let g = self.sessions.lock().ok()?;
        g.get(id).map(|e| e.handle.status())
    }

    pub fn acp_session_id(&self, id: &str) -> Option<String> {
        let g = self.sessions.lock().ok()?;
        g.get(id).and_then(|e| e.handle.acp_session_id())
    }

    pub fn info(&self, id: &str) -> Option<AgentInfo> {
        let g = self.sessions.lock().ok()?;
        g.get(id).map(|e| {
            let mut info = e.info.clone();
            info.status = match e.handle.status() {
                AcpSessionStatus::Connecting | AcpSessionStatus::Running => AgentStatus::Running,
                AcpSessionStatus::WaitingPermission => AgentStatus::WaitingInput,
                AcpSessionStatus::Ready => AgentStatus::Idle,
                AcpSessionStatus::Failed => AgentStatus::Failed,
                AcpSessionStatus::Done => AgentStatus::Done,
            };
            info
        })
    }

    fn handle(&self, id: &str) -> Result<Arc<AcpSessionHandle>, AcpHostError> {
        let g = self
            .sessions
            .lock()
            .map_err(|_| AcpHostError::Message("lock poisoned".into()))?;
        g.get(id)
            .map(|e| Arc::clone(&e.handle))
            .ok_or_else(|| AcpHostError::Message(format!("ACP session not found: {id}")))
    }
}

struct NullSink;
impl AcpEventSink for NullSink {
    fn emit(&self, _agent_id: &str, _event: AcpEvent) {}
}

/// Resolve cwd for tests.
#[allow(dead_code)]
pub fn test_cwd() -> PathBuf {
    std::env::temp_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_runtime::acp::events::{AcpEvent, VecEventSink};
    use crate::agent_runtime::acp::host;
    use std::collections::HashMap;
    use std::time::Duration;

    fn mock_descriptor() -> AcpAgentDescriptor {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/mock_acp_agent.py");
        AcpAgentDescriptor {
            id: "mock".to_string(),
            name: "Mock ACP".to_string(),
            command: "python3".to_string(),
            args: vec![fixture.to_string_lossy().to_string()],
            env: HashMap::new(),
            cwd_mode: "session".to_string(),
            icon: None,
            enabled: true,
        }
    }

    #[test]
    fn mock_prompt_streams_chunks_and_end_turn() {
        let sink = Arc::new(VecEventSink::default());
        let perms = PermissionBoard::new();
        let cwd = std::env::temp_dir();
        let handle = host::start_session(
            "test-agent-1",
            "acp:mock",
            &mock_descriptor(),
            &cwd,
            None,
            sink.clone() as Arc<dyn AcpEventSink>,
            perms,
        )
        .expect("start mock");

        let stop = handle
            .prompt("hello world", Duration::from_secs(10))
            .expect("prompt");
        assert_eq!(stop, "end_turn");

        let events = sink.snapshot();
        let chunks: Vec<_> = events
            .iter()
            .filter_map(|(_, e)| match e {
                AcpEvent::MessageChunk { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            chunks.iter().any(|t| t.contains("echo:") || t.contains("hello")),
            "expected message chunks, got {events:?}"
        );
        assert!(
            events
                .iter()
                .any(|(_, e)| matches!(e, AcpEvent::TurnDone { stop_reason } if stop_reason == "end_turn")),
            "expected turn_done end_turn"
        );

        handle.kill().ok();
    }

    #[test]
    fn cancel_is_notification_shaped_no_hang() {
        // Cancel on idle session should not error hard; notification is fire-and-forget.
        let sink = Arc::new(VecEventSink::default());
        let perms = PermissionBoard::new();
        let cwd = std::env::temp_dir();
        let handle = host::start_session(
            "test-agent-cancel",
            "acp:mock",
            &mock_descriptor(),
            &cwd,
            None,
            sink as Arc<dyn AcpEventSink>,
            perms,
        )
        .expect("start");
        // No in-flight prompt — cancel still sends notification without id.
        handle.cancel().expect("cancel notification");
        handle.kill().ok();
    }

    #[test]
    fn bridge_start_prompt_kill() {
        let bridge = AcpBridge::new();
        let cwd = std::env::temp_dir();
        let info = bridge
            .start_with_descriptor("br-1", mock_descriptor(), &cwd, None)
            .expect("start");
        assert_eq!(info.runtime, "acp:mock");
        assert!(bridge.contains("br-1"));
        let stop = bridge.prompt("br-1", "br-hi").expect("prompt");
        assert_eq!(stop, "end_turn");
        let sid = bridge.acp_session_id("br-1");
        assert_eq!(sid.as_deref(), Some("sess_mock_1"));
        bridge.kill("br-1").expect("kill");
        assert!(!bridge.contains("br-1"));
        let _ = info;
    }
}
