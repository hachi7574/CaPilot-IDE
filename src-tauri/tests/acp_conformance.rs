//! ACP v1 conformance test (architecture §8.1, Phase 2 acceptance).
//!
//! Drives the real [`AcpClient`] against the deterministic `fake-acp-agent`
//! binary (a real subprocess speaking NDJSON JSON-RPC over stdio): create →
//! prompt → tool-call stream → `session/request_permission` → resolve → turn
//! complete. This is the Phase 2 acceptance proof — a real turn containing a
//! tool call *and* a permission round-trip, with no xterm and no model API.
//!
//! The fake agent is a `[[bin]]` target, resolved here via
//! `CARGO_BIN_EXE_fake-acp-agent` (set by Cargo for integration tests).

use capilot_ide_lib::agent_provider::acp::{opencode_profile, AcpClient, AcpProfile};
use capilot_ide_lib::agent_provider::manager::{AgentManager, NewAgentRequest, ResumeAgentRequest};
use capilot_ide_lib::agent_provider::traits::AgentClient;
use capilot_ide_lib::agent_provider::types::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Collects sequenced agent events so the test can observe the whole turn.
#[derive(Default)]
struct EventLog {
    events: Mutex<Vec<(u64, AgentEvent)>>,
}

impl capilot_ide_lib::agent_provider::manager::AgentEventObserver for EventLog {
    fn on_agent_event(&self, _agent_id: &str, seq: u64, event: &AgentEvent) {
        self.events.lock().unwrap().push((seq, event.clone()));
    }
}

fn next_turn() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("t{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

#[test]
fn acp_full_turn_with_tool_call_and_permission() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let manager = Arc::new(AgentManager::new());
    let log = Arc::new(EventLog::default());
    manager.subscribe(log.clone());
    manager.register_provider(Arc::new(AcpClient::new(AcpProfile {
        provider_id: "fake-acp".into(),
        command: vec![env!("CARGO_BIN_EXE_fake-acp-agent").to_string()],
        env: vec![],
    })));

    // Create — the fake agent handshakes and emits SessionReady.
    let snap = rt
        .block_on(manager.clone().create_agent(NewAgentRequest {
            agent_id: "a1".into(),
            provider_id: "fake-acp".into(),
            backend_kind: "acp".into(),
            workspace_id: None,
            cwd: std::env::temp_dir(),
            model: None,
            config: vec![],
        }))
        .expect("create_agent");
    assert_eq!(snap.agent.status, AgentStatus::Idle);
    assert!(snap.agent.capabilities.permissions);
    assert!(snap.agent.capabilities.session_resume);
    assert!(snap.agent.capabilities.structured_tools);
    assert_eq!(snap.agent.backend_kind, "acp");

    // Start a turn; the fake agent streams tool_call + permission + messages.
    let turn = next_turn();
    let turn_id = rt
        .block_on(manager.start_turn(
            "a1",
            AgentPrompt {
                client_message_id: format!("cm-{turn}"),
                content: vec![PromptContent::Text {
                    text: "run date".into(),
                }],
            },
        ))
        .expect("start_turn");
    assert!(!turn_id.is_empty(), "start_turn must return a turn id");

    // Poll for the permission request, resolve it with `allow_once`, and wait
    // for the turn to complete. The fake agent blocks the turn on our answer,
    // so this asserts the full permission round-trip.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut permission_seen = false;
    loop {
        let events = log.events.lock().unwrap().clone();
        if !permission_seen {
            if let Some((_, AgentEvent::PermissionRequested(req))) = events
                .iter()
                .find(|(_, ev)| matches!(ev, AgentEvent::PermissionRequested(_)))
            {
                permission_seen = true;
                assert_eq!(req.actions.len(), 4);
                assert!(req.actions.iter().any(|a| a.id == "allow_once"));
                assert!(req.actions.iter().any(|a| a.id == "reject_always"));
                rt.block_on(manager.respond_to_permission("a1", &req.id, "allow_once"))
                    .expect("respond_to_permission");
            }
        }
        if events
            .iter()
            .any(|(_, ev)| matches!(ev, AgentEvent::TurnCompleted(_)))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for turn completion; events so far: {events:#?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(permission_seen, "the agent never requested permission");

    // Snapshot assertions: canonical timeline, completed tool call with output,
    // accumulated assistant text, no dangling permissions, idle status.
    let snap = manager.snapshot("a1").expect("snapshot");
    assert!(
        snap.pending_permissions.is_empty(),
        "permission must be resolved"
    );

    let user_msgs = snap
        .timeline
        .iter()
        .filter(|i| matches!(i, TimelineItem::UserMessage(_)))
        .count();
    assert_eq!(
        user_msgs, 1,
        "exactly one user message, from the client side"
    );

    let assistant_text: String = snap
        .timeline
        .iter()
        .filter_map(|i| match i {
            TimelineItem::AssistantMessage(m) => Some(m.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        assistant_text, "Hello world",
        "both chunks appended to one item"
    );

    let tool = snap
        .timeline
        .iter()
        .find(|i| i.item_id() == "fcall-1")
        .expect("tool call item");
    match tool {
        TimelineItem::ToolCall(t) => {
            assert_eq!(t.status, ToolCallStatus::Completed);
            assert!(t.tool_output.is_some(), "completed tool carries output");
            assert_eq!(t.tool_name, "bash");
        }
        other => panic!("unexpected item {other:?}"),
    }

    // The turn completed cleanly: status idle, turn id recorded.
    assert_eq!(snap.agent.status, AgentStatus::Idle);
    assert_eq!(snap.agent.last_event_seq, snap.last_seq);

    // Usage update flowed through as a context-usage event.
    let usage_seen = log
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|(_, ev)| matches!(ev, AgentEvent::ContextUsageUpdated(_)));
    assert!(usage_seen, "usage_update should map to ContextUsageUpdated");

    // Close the session: graceful session/close + SessionClosed event.
    rt.block_on(manager.close_agent("a1")).expect("close_agent");
    let snap = manager.snapshot("a1").expect("snapshot after close");
    assert_eq!(snap.agent.status, AgentStatus::Closed);
}

#[test]
fn acp_interrupt_cancels_turn() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let manager = Arc::new(AgentManager::new());
    let log = Arc::new(EventLog::default());
    manager.subscribe(log.clone());
    manager.register_provider(Arc::new(AcpClient::new(AcpProfile {
        provider_id: "fake-acp".into(),
        command: vec![env!("CARGO_BIN_EXE_fake-acp-agent").to_string()],
        env: vec![],
    })));

    rt.block_on(manager.clone().create_agent(NewAgentRequest {
        agent_id: "a2".into(),
        provider_id: "fake-acp".into(),
        backend_kind: "acp".into(),
        workspace_id: None,
        cwd: std::env::temp_dir(),
        model: None,
        config: vec![],
    }))
    .expect("create_agent");

    rt.block_on(manager.start_turn(
        "a2",
        AgentPrompt {
            client_message_id: "cm-2".into(),
            content: vec![PromptContent::Text {
                text: "run date".into(),
            }],
        },
    ))
    .expect("start_turn");

    // Let the fake agent reach the permission request, then cancel.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let cancelled = log
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|(_, ev)| matches!(ev, AgentEvent::PermissionRequested(_)));
        if cancelled {
            break;
        }
        assert!(Instant::now() < deadline, "permission never requested");
        std::thread::sleep(Duration::from_millis(50));
    }

    rt.block_on(manager.interrupt("a2")).expect("interrupt");

    // The session/prompt response arrives with stopReason "cancelled"; the
    // adapter must not double-emit a terminal event.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let events = log.events.lock().unwrap().clone();
        let cancellations = events
            .iter()
            .filter(|(_, ev)| matches!(ev, AgentEvent::TurnCancelled(_)))
            .count();
        assert!(Instant::now() < deadline, "turn was never cancelled");
        if cancellations >= 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let events = log.events.lock().unwrap().clone();
    let cancellations = events
        .iter()
        .filter(|(_, ev)| matches!(ev, AgentEvent::TurnCancelled(_)))
        .count();
    let completions = events
        .iter()
        .filter(|(_, ev)| matches!(ev, AgentEvent::TurnCompleted(_)))
        .count();
    assert_eq!(cancellations, 1, "exactly one TurnCancelled");
    assert_eq!(completions, 0, "no TurnCompleted after a cancel");

    rt.block_on(manager.close_agent("a2")).expect("close_agent");
}

#[test]
fn acp_resume_opens_persisted_session() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let manager = Arc::new(AgentManager::new());
    manager.register_provider(Arc::new(AcpClient::new(AcpProfile {
        provider_id: "fake-acp".into(),
        command: vec![env!("CARGO_BIN_EXE_fake-acp-agent").to_string()],
        env: vec![],
    })));

    let snap = rt
        .block_on(manager.clone().create_agent(NewAgentRequest {
            agent_id: "a3".into(),
            provider_id: "fake-acp".into(),
            backend_kind: "acp".into(),
            workspace_id: None,
            cwd: std::env::temp_dir(),
            model: None,
            config: vec![],
        }))
        .expect("create_agent");
    let handle = snap.agent.persistence.clone().expect("persistence handle");

    // Resume from the handle into a fresh agent id.
    let resumed = rt
        .block_on(manager.clone().resume_agent(ResumeAgentRequest {
            agent_id: "a3r".into(),
            handle: handle.clone(),
            cwd: std::env::temp_dir(),
            model: None,
            config: vec![],
        }))
        .expect("resume_agent");
    assert_eq!(resumed.agent.status, AgentStatus::Idle);
    assert_eq!(resumed.agent.provider_id, "fake-acp");
    assert_eq!(
        resumed
            .agent
            .persistence
            .as_ref()
            .map(|h| &h.runtime_session_id),
        Some(&handle.runtime_session_id)
    );

    rt.block_on(manager.close_agent("a3"))
        .expect("close original");
    rt.block_on(manager.close_agent("a3r"))
        .expect("close resumed");
}

#[test]
fn acp_rejects_foreign_provider_handle() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let manager = Arc::new(AgentManager::new());
    manager.register_provider(Arc::new(AcpClient::new(AcpProfile {
        provider_id: "fake-acp".into(),
        command: vec![env!("CARGO_BIN_EXE_fake-acp-agent").to_string()],
        env: vec![],
    })));

    let foreign = PersistenceHandle {
        provider_id: "some-other-provider".into(),
        runtime_session_id: "fsess-1".into(),
        native_handle: None,
        metadata: None,
    };
    let err = rt
        .block_on(manager.clone().resume_agent(ResumeAgentRequest {
            agent_id: "a4".into(),
            handle: foreign,
            cwd: std::env::temp_dir(),
            model: None,
            config: vec![],
        }))
        .expect_err("foreign handle must be rejected");
    // The manager resolves the client by handle.provider_id and refuses a
    // provider that is not registered (§10.3: a different provider is a
    // handoff, not a resume).
    assert!(
        err.to_string().contains("provider not registered"),
        "unexpected error: {err}"
    );
}

/// Live smoke test for the first real provider (`opencode acp`, §8.1). Runs only
/// when invoked explicitly (`cargo test --test acp_conformance -- --ignored`)
/// because it spawns the user's installed `opencode`. Costs no model tokens: it
/// exercises only `initialize` → `session/new` → catalog → `session/close`.
#[test]
#[ignore = "requires a local `opencode` binary"]
fn acp_live_opencode_catalog() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let client = AcpClient::new(opencode_profile());
    rt.block_on(async {
        if !client.is_available().await.expect("availability probe") {
            eprintln!("skipping: `opencode` not on PATH");
            return;
        }
        let catalog = client
            .fetch_catalog(std::env::temp_dir().as_path())
            .await
            .expect("live opencode catalog");
        assert!(!catalog.models.is_empty(), "catalog must list models");
        assert!(catalog.capabilities.structured_tools);
    });
}
