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
            model.as_deref(),
            sink,
            self.permissions.clone(),
        )?;
        let handle = Arc::new(handle);

        let acp_sid = handle.acp_session_id();
        // Prefer caller model; else bootstrap effective (zen free / preferred).
        let effective_model = model.or_else(|| handle.last_model());
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
            model: effective_model,
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
        // Reflect rate-limit fallback model onto AgentInfo + panel error was
        // already humanized inside host.
        if let Some(m) = handle.last_model() {
            if let Ok(mut g) = self.sessions.lock() {
                if let Some(entry) = g.get_mut(id) {
                    if entry.info.model.as_deref() != Some(m.as_str()) {
                        entry.info.model = Some(m);
                    }
                }
            }
        }
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
                    // Surface humanized error on the panel (DEF-011: never silent).
                    let message = host::humanize_acp_error(&e.to_string());
                    log::warn!("acp_prompt {id}: {message}");
                    bridge.sink().emit(
                        &id,
                        AcpEvent::Error {
                            message: message.clone(),
                        },
                    );
                    bridge.sink().emit(
                        &id,
                        AcpEvent::Status {
                            status: "idle".into(),
                        },
                    );
                } else if let Some(m) = {
                    bridge
                        .sessions
                        .lock()
                        .ok()
                        .and_then(|g| g.get(&id).and_then(|e| e.handle.last_model()))
                } {
                    if let Ok(mut g) = bridge.sessions.lock() {
                        if let Some(entry) = g.get_mut(&id) {
                            if entry.info.model.as_deref() != Some(m.as_str()) {
                                entry.info.model = Some(m);
                            }
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

    /// Live model switch via `session/set_config_option` + update AgentInfo.model.
    pub fn set_model(&self, id: &str, model: &str) -> Result<(), AcpHostError> {
        let handle = self.handle(id)?;
        handle.set_model(model, Duration::from_secs(30))?;
        if let Ok(mut g) = self.sessions.lock() {
            if let Some(entry) = g.get_mut(id) {
                entry.info.model = Some(model.to_string());
            }
        }
        Ok(())
    }

    /// Live config option (effort / mode / …).
    pub fn set_config_option(
        &self,
        id: &str,
        config_id: &str,
        value: &str,
    ) -> Result<(), AcpHostError> {
        self.handle(id)?
            .set_config_option(config_id, value, Duration::from_secs(30))?;
        if config_id == "model" {
            if let Ok(mut g) = self.sessions.lock() {
                if let Some(entry) = g.get_mut(id) {
                    entry.info.model = Some(value.to_string());
                }
            }
        }
        Ok(())
    }

    /// Update cached AgentInfo.model without RPC (e.g. after bootstrap event).
    pub fn note_model(&self, id: &str, model: Option<String>) {
        if let Ok(mut g) = self.sessions.lock() {
            if let Some(entry) = g.get_mut(id) {
                entry.info.model = model;
            }
        }
    }

    pub fn respond_permission(
        &self,
        id: &str,
        request_id: &str,
        outcome: PermissionOutcome,
    ) -> Result<(), AcpHostError> {
        // Drop from board (best-effort).
        let _ = self.permissions.take(id, request_id);
        self.handle(id)?.respond_permission(request_id, outcome)?;
        // Resume turn UX: leave waiting_input → running until turn_done.
        self.sink().emit(
            id,
            AcpEvent::Status {
                status: "running".into(),
            },
        );
        Ok(())
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
        self.is_alive(id)
    }

    /// True only when the bridge owns a still-living ACP child for `id`.
    /// Dead children are dropped so a later resume can respawn cleanly.
    pub fn is_alive(&self, id: &str) -> bool {
        let mut g = match self.sessions.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        match g.get(id) {
            Some(entry) if entry.handle.is_alive() => true,
            Some(_) => {
                // Process exited — drop so FE resume / acp_prompt can respawn.
                if let Some(entry) = g.remove(id) {
                    let _ = entry.handle.kill();
                    self.permissions.clear_agent(id);
                    self.sink().emit(
                        id,
                        AcpEvent::Error {
                            message: "ACP agent 进程已退出，请重新发送以恢复会话".into(),
                        },
                    );
                    self.sink().emit(
                        id,
                        AcpEvent::Status {
                            status: "idle".into(),
                        },
                    );
                }
                false
            }
            None => false,
        }
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

    /// F11: when agent advertises loadSession, start with resume_key → session/load.
    #[test]
    fn bridge_resume_via_session_load() {
        let bridge = AcpBridge::new();
        let cwd = std::env::temp_dir();
        let info = bridge
            .start_with_descriptor(
                "br-resume",
                mock_descriptor(),
                &cwd,
                Some("sess_resumed_42"),
            )
            .expect("start with resume_key");
        assert_eq!(info.runtime, "acp:mock");
        let sid = bridge.acp_session_id("br-resume");
        assert_eq!(
            sid.as_deref(),
            Some("sess_resumed_42"),
            "session/load must adopt resume_key as sessionId"
        );
        let stop = bridge
            .prompt("br-resume", "after-load")
            .expect("prompt after load");
        assert_eq!(stop, "end_turn");
        bridge.kill("br-resume").expect("kill");
    }

    #[test]
    fn permission_request_allow_roundtrip() {
        use crate::agent_runtime::acp::permission::PermissionOutcome;
        let sink = Arc::new(VecEventSink::default());
        let perms = PermissionBoard::new();
        let cwd = std::env::temp_dir();
        let handle = Arc::new(
            host::start_session(
                "test-perm-allow",
                "acp:mock",
                &mock_descriptor(),
                &cwd,
                None,
                None,
                sink.clone() as Arc<dyn AcpEventSink>,
                perms,
            )
            .expect("start"),
        );

        let h2 = Arc::clone(&handle);
        let s2 = sink.clone();
        let joiner = std::thread::spawn(move || {
            // Wait for permission event, then allow.
            let ev = s2.wait_for(
                |e| matches!(e, AcpEvent::PermissionRequest { .. }),
                Duration::from_secs(5),
            );
            assert!(ev.is_some(), "expected PermissionRequest event");
            if let Some(AcpEvent::PermissionRequest { request_id, .. }) = ev {
                h2.respond_permission(
                    &request_id,
                    PermissionOutcome::Allow {
                        option_id: "allow-once".into(),
                    },
                )
                .expect("respond allow");
            }
        });

        let stop = handle
            .prompt("please need permission now", Duration::from_secs(10))
            .expect("prompt");
        assert_eq!(stop, "end_turn");
        joiner.join().expect("joiner");

        let events = sink.snapshot();
        assert!(
            events
                .iter()
                .any(|(_, e)| matches!(e, AcpEvent::PermissionRequest { .. })),
            "permission event missing: {events:?}"
        );
        assert!(
            events.iter().any(|(_, e)| matches!(
                e,
                AcpEvent::ToolCall { status, .. } if status == "completed"
            )),
            "expected completed tool after allow: {events:?}"
        );
        handle.kill().ok();
    }

    #[test]
    fn permission_request_reject_roundtrip() {
        use crate::agent_runtime::acp::permission::PermissionOutcome;
        let sink = Arc::new(VecEventSink::default());
        let perms = PermissionBoard::new();
        let cwd = std::env::temp_dir();
        let handle = Arc::new(
            host::start_session(
                "test-perm-reject",
                "acp:mock",
                &mock_descriptor(),
                &cwd,
                None,
                None,
                sink.clone() as Arc<dyn AcpEventSink>,
                perms,
            )
            .expect("start"),
        );

        let h2 = Arc::clone(&handle);
        let s2 = sink.clone();
        let joiner = std::thread::spawn(move || {
            let ev = s2.wait_for(
                |e| matches!(e, AcpEvent::PermissionRequest { .. }),
                Duration::from_secs(5),
            );
            assert!(ev.is_some(), "expected PermissionRequest");
            if let Some(AcpEvent::PermissionRequest { request_id, .. }) = ev {
                h2.respond_permission(
                    &request_id,
                    PermissionOutcome::Reject {
                        option_id: Some("reject-once".into()),
                    },
                )
                .expect("respond reject");
            }
        });

        let stop = handle
            .prompt("permission please reject", Duration::from_secs(10))
            .expect("prompt");
        assert_eq!(stop, "end_turn");
        joiner.join().expect("joiner");

        let events = sink.snapshot();
        assert!(
            events.iter().any(|(_, e)| matches!(
                e,
                AcpEvent::ToolCall { status, .. } if status == "failed"
            )),
            "expected failed tool after reject: {events:?}"
        );
        handle.kill().ok();
    }

    #[test]
    fn fs_read_under_cwd_ok_and_outside_denied() {
        let sink = Arc::new(VecEventSink::default());
        let perms = PermissionBoard::new();
        // Dedicated root so outside path is unambiguous.
        let root = std::env::temp_dir().join(format!(
            "acp-fs-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let inside = root.join("note.txt");
        std::fs::write(&inside, "sandbox-ok").unwrap();
        let outside = std::env::temp_dir().join(format!(
            "acp-fs-out-{}",
            std::process::id()
        ));
        std::fs::write(&outside, "secret").unwrap();

        let handle = host::start_session(
            "test-fs",
            "acp:mock",
            &mock_descriptor(),
            &root,
            None,
            None,
            sink.clone() as Arc<dyn AcpEventSink>,
            perms,
        )
        .expect("start");

        let inside_s = inside.to_string_lossy().to_string();
        let stop = handle
            .prompt(&format!("fsread:{inside_s}"), Duration::from_secs(10))
            .expect("prompt inside");
        assert_eq!(stop, "end_turn");

        let events = sink.snapshot();
        let chunks: Vec<_> = events
            .iter()
            .filter_map(|(_, e)| match e {
                AcpEvent::MessageChunk { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(
            chunks.iter().any(|t| t.contains("fs_ok:") && t.contains("sandbox-ok")),
            "expected fs_ok with content, got {chunks:?}"
        );

        // Outside path must be denied by sandbox.
        let outside_s = outside.to_string_lossy().to_string();
        let stop2 = handle
            .prompt(&format!("fsread:{outside_s}"), Duration::from_secs(10))
            .expect("prompt outside");
        assert_eq!(stop2, "end_turn");
        let events2 = sink.snapshot();
        let chunks2: Vec<_> = events2
            .iter()
            .filter_map(|(_, e)| match e {
                AcpEvent::MessageChunk { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(
            chunks2.iter().any(|t| t.contains("fs_err:") && t.contains("escapes")),
            "expected fs_err escapes, got {chunks2:?}"
        );

        handle.kill().ok();
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Product smoke against real `opencode acp` when available.
    /// Prefers zen free; host falls back to go on rate limit.
    #[test]
    fn opencode_acp_real_prompt_smoke() {
        if std::env::var("CAP_SKIP_OPENCODE_ACP").ok().as_deref() == Some("1") {
            eprintln!("skip: CAP_SKIP_OPENCODE_ACP=1");
            return;
        }
        // Resolve opencode on PATH.
        let has = std::process::Command::new("sh")
            .arg("-c")
            .arg("command -v opencode")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !has {
            eprintln!("skip: opencode not on PATH");
            return;
        }

        let bridge = AcpBridge::new();
        let cwd = std::env::temp_dir();
        let desc = AcpAgentDescriptor {
            id: "opencode".into(),
            name: "OpenCode ACP smoke".into(),
            command: "opencode".into(),
            args: vec!["acp".into()],
            env: HashMap::new(),
            cwd_mode: "session".into(),
            icon: None,
            enabled: true,
        };
        // Prefer go first for reliability when zen free is rate-limited in CI/dev;
        // user UI still defaults to zen free via registry is_default.
        let info = bridge
            .start(
                "oc-smoke-1",
                "acp:opencode",
                &cwd,
                "smoke",
                "ask",
                "low",
                Some("opencode-go/deepseek-v4-flash".into()),
                None,
                None,
                None,
                Some(desc),
            )
            .expect("start opencode acp");
        assert_eq!(info.runtime, "acp:opencode");
        let stop = bridge
            .prompt("oc-smoke-1", "Reply with exactly one word: pong")
            .expect("prompt must succeed");
        assert_eq!(stop, "end_turn");
        let live = bridge.info("oc-smoke-1").expect("info");
        eprintln!("opencode smoke model={:?}", live.model);
        bridge.kill("oc-smoke-1").ok();
    }
}
