//! Shared provider contract test (architecture §8 acceptance).
//!
//! The Phase 4 acceptance requirement: "Direct 和 ACP Provider 通过同一
//! contract test". This file runs the *identical* scenario body against two
//! deterministic subprocess providers — the ACP `fake-acp-agent` and the Codex
//! Direct `fake-codex-app-server` — and asserts the same canonical timeline,
//! permission round-trip, interrupt-exactly-once semantics, resume identity,
//! and foreign-handle rejection. A provider that passes the contract is
//! interchangeable at the UI layer: no Provider ID branching needed for core
//! interactions.
//!
//! The fakes are `[[bin]]` targets resolved via `CARGO_BIN_EXE_*` (set by Cargo
//! for integration tests). Both speak NDJSON JSON-RPC 2.0 over stdio.

use capilot_ide_lib::agent_provider::acp::{AcpClient, AcpProfile};
use capilot_ide_lib::agent_provider::direct::claude::{ClaudeClient, ClaudeProfile};
use capilot_ide_lib::agent_provider::direct::{CodexClient, CodexProfile};
use capilot_ide_lib::agent_provider::manager::{
    AgentEventObserver, AgentManager, AgentSnapshot, NewAgentRequest, ResumeAgentRequest,
};
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

impl AgentEventObserver for EventLog {
    fn on_agent_event(&self, _agent_id: &str, seq: u64, event: &AgentEvent) {
        self.events.lock().unwrap().push((seq, event.clone()));
    }
}

fn next_turn() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("t{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// One concrete provider under test, plus the per-provider facts the contract
/// body must observe (native tool item id/name, backend kind).
struct ProviderSpec {
    provider_id: &'static str,
    backend_kind: &'static str,
    tool_id: &'static str,
    tool_name: &'static str,
    client: Arc<dyn AgentClient>,
}

fn fake_acp_spec() -> ProviderSpec {
    ProviderSpec {
        provider_id: "fake-acp",
        backend_kind: "acp",
        tool_id: "fcall-1",
        tool_name: "bash",
        client: Arc::new(AcpClient::new(AcpProfile {
            provider_id: "fake-acp".into(),
            command: vec![env!("CARGO_BIN_EXE_fake-acp-agent").to_string()],
            env: vec![],
        })),
    }
}

fn fake_codex_spec() -> ProviderSpec {
    ProviderSpec {
        provider_id: "fake-codex",
        backend_kind: "direct",
        tool_id: "fcx-call-1",
        tool_name: "shell",
        client: Arc::new(CodexClient::new(CodexProfile {
            provider_id: "fake-codex".into(),
            command: vec![env!("CARGO_BIN_EXE_fake-codex-app-server").to_string()],
            env: vec![],
        })),
    }
}

/// The Claude adapter is a thin wrapper over the Codex session machinery (its
/// sidecar speaks the identical wire schema), so it must pass the same contract
/// against the same deterministic fake server.
fn fake_claude_spec() -> ProviderSpec {
    ProviderSpec {
        provider_id: "fake-claude",
        backend_kind: "direct",
        tool_id: "fcx-call-1",
        tool_name: "shell",
        client: Arc::new(ClaudeClient::new(ClaudeProfile {
            provider_id: "fake-claude".into(),
            command: vec![env!("CARGO_BIN_EXE_fake-codex-app-server").to_string()],
            env: vec![],
        })),
    }
}

fn manager_with(spec: &ProviderSpec, log: &Arc<EventLog>) -> Arc<AgentManager> {
    let manager = Arc::new(AgentManager::new());
    manager.subscribe(log.clone());
    manager.register_provider(spec.client.clone());
    manager
}

fn create(
    rt: &tokio::runtime::Runtime,
    manager: &Arc<AgentManager>,
    spec: &ProviderSpec,
) -> AgentSnapshot {
    rt.block_on(manager.clone().create_agent(NewAgentRequest {
        agent_id: "a1".into(),
        provider_id: spec.provider_id.into(),
        backend_kind: spec.backend_kind.into(),
        workspace_id: None,
        cwd: std::env::temp_dir(),
        model: None,
        config: vec![],
    }))
    .expect("create_agent")
}

// ── Scenario: full turn with tool call + permission ─────────────

fn run_full_turn(rt: &tokio::runtime::Runtime, spec: &ProviderSpec) {
    let log = Arc::new(EventLog::default());
    let manager = manager_with(spec, &log);

    let snap = create(rt, &manager, spec);
    assert_eq!(snap.agent.status, AgentStatus::Idle);
    assert!(snap.agent.capabilities.permissions);
    assert!(snap.agent.capabilities.session_resume);
    assert!(snap.agent.capabilities.structured_tools);
    assert_eq!(snap.agent.backend_kind, spec.backend_kind);

    // Start a turn; the fake streams tool + permission + messages.
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

    // Poll for the permission request, resolve with `allow_once`, and wait for
    // the turn to complete — the full permission round-trip.
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
                assert_eq!(req.actions.len(), 4, "four normalized actions");
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
    assert!(permission_seen, "the fake never requested permission");

    // Snapshot assertions: canonical timeline, completed tool with output,
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
    assert_eq!(assistant_text, "Hello world", "deltas appended to one item");

    let tool = snap
        .timeline
        .iter()
        .find(|i| i.item_id() == spec.tool_id)
        .expect("tool call item");
    match tool {
        TimelineItem::ToolCall(t) => {
            assert_eq!(t.status, ToolCallStatus::Completed);
            assert!(t.tool_output.is_some(), "completed tool carries output");
            assert_eq!(t.tool_name, spec.tool_name);
        }
        other => panic!("unexpected item {other:?}"),
    }

    assert_eq!(snap.agent.status, AgentStatus::Idle);
    assert_eq!(snap.agent.last_event_seq, snap.last_seq);

    let usage_seen = log
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|(_, ev)| matches!(ev, AgentEvent::ContextUsageUpdated(_)));
    assert!(usage_seen, "usage update should map to ContextUsageUpdated");

    rt.block_on(manager.close_agent("a1")).expect("close_agent");
    let snap = manager.snapshot("a1").expect("snapshot after close");
    assert_eq!(snap.agent.status, AgentStatus::Closed);
}

// ── Scenario: interrupt cancels the turn exactly once ───────────

fn run_interrupt(rt: &tokio::runtime::Runtime, spec: &ProviderSpec) {
    let log = Arc::new(EventLog::default());
    let manager = manager_with(spec, &log);
    create(rt, &manager, spec);

    rt.block_on(manager.start_turn(
        "a1",
        AgentPrompt {
            client_message_id: "cm-2".into(),
            content: vec![PromptContent::Text {
                text: "run date".into(),
            }],
        },
    ))
    .expect("start_turn");

    // Let the fake reach the permission request, then cancel.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if log
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|(_, ev)| matches!(ev, AgentEvent::PermissionRequested(_)))
        {
            break;
        }
        assert!(Instant::now() < deadline, "permission never requested");
        std::thread::sleep(Duration::from_millis(50));
    }

    rt.block_on(manager.interrupt("a1")).expect("interrupt");

    // The adapter must emit exactly one terminal event: the cancel wins over
    // any racing completion.
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

    rt.block_on(manager.close_agent("a1")).expect("close_agent");
}

// ── Scenario: resume reopens the same native session id ─────────

fn run_resume(rt: &tokio::runtime::Runtime, spec: &ProviderSpec) {
    let manager = manager_with(spec, &Arc::new(EventLog::default()));
    let snap = create(rt, &manager, spec);
    let handle = snap.agent.persistence.clone().expect("persistence handle");

    let resumed = rt
        .block_on(manager.clone().resume_agent(ResumeAgentRequest {
            agent_id: "a1r".into(),
            handle: handle.clone(),
            cwd: std::env::temp_dir(),
            model: None,
            config: vec![],
        }))
        .expect("resume_agent");
    assert_eq!(resumed.agent.status, AgentStatus::Idle);
    assert_eq!(resumed.agent.provider_id, spec.provider_id);
    assert_eq!(
        resumed
            .agent
            .persistence
            .as_ref()
            .map(|h| &h.runtime_session_id),
        Some(&handle.runtime_session_id),
        "resume must keep the same native session id"
    );

    rt.block_on(manager.close_agent("a1"))
        .expect("close original");
    rt.block_on(manager.close_agent("a1r"))
        .expect("close resumed");
}

// ── Scenario: a foreign provider handle is rejected ─────────────

fn run_foreign_handle(rt: &tokio::runtime::Runtime, spec: &ProviderSpec) {
    let manager = manager_with(spec, &Arc::new(EventLog::default()));
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

// ── The shared contract: each scenario runs against BOTH providers ─

#[test]
fn contract_full_turn_with_tool_and_permission() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for spec in [fake_acp_spec(), fake_codex_spec(), fake_claude_spec()] {
        eprintln!("== contract full turn: {}", spec.provider_id);
        run_full_turn(&rt, &spec);
    }
}

#[test]
fn contract_interrupt_cancels_turn_exactly_once() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for spec in [fake_acp_spec(), fake_codex_spec(), fake_claude_spec()] {
        eprintln!("== contract interrupt: {}", spec.provider_id);
        run_interrupt(&rt, &spec);
    }
}

#[test]
fn contract_resume_keeps_runtime_identity() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for spec in [fake_acp_spec(), fake_codex_spec(), fake_claude_spec()] {
        eprintln!("== contract resume: {}", spec.provider_id);
        run_resume(&rt, &spec);
    }
}

#[test]
fn contract_rejects_foreign_handle() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for spec in [fake_acp_spec(), fake_codex_spec(), fake_claude_spec()] {
        eprintln!("== contract foreign handle: {}", spec.provider_id);
        run_foreign_handle(&rt, &spec);
    }
}

/// Live smoke test for the real Codex `app-server` (architecture §8.2). Runs
/// only when invoked explicitly (`cargo test --test contract_conformance --
/// --ignored`) because it spawns the user's installed `codex`. Costs no model
/// tokens: it exercises only `initialize` → `thread/start` → catalog →
/// `thread/unsubscribe`.
#[test]
#[ignore = "requires a local `codex` binary"]
fn contract_live_codex_catalog() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let client = CodexClient::new(capilot_ide_lib::agent_provider::direct::codex_profile());
    rt.block_on(async {
        if !client.is_available().await.expect("availability probe") {
            eprintln!("skipping: `codex` not on PATH");
            return;
        }
        let catalog = client
            .fetch_catalog(std::env::temp_dir().as_path())
            .await
            .expect("live codex catalog");
        assert!(!catalog.models.is_empty(), "catalog must list models");
        assert!(catalog.capabilities.structured_tools);
    });
}

/// Live smoke for the Claude Agent SDK sidecar (architecture §8.1). Runs only
/// when invoked explicitly (`cargo test --test contract_conformance -- --ignored`)
/// because it needs a Node runtime and the sidecar's SDK install. Zero model
/// tokens: exercises only `initialize` → `thread/start` → catalog →
/// `thread/unsubscribe`.
#[test]
#[ignore = "requires the claude sidecar (node + @anthropic-ai/claude-agent-sdk)"]
fn contract_live_claude_catalog() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let client = ClaudeClient::new(capilot_ide_lib::agent_provider::direct::claude_profile());
    rt.block_on(async {
        if !client.is_available().await.expect("availability probe") {
            eprintln!("skipping: claude sidecar not available");
            return;
        }
        let catalog = client
            .fetch_catalog(std::env::temp_dir().as_path())
            .await
            .expect("live claude catalog");
        assert!(!catalog.models.is_empty(), "catalog must list models");
        assert!(catalog.capabilities.structured_tools);
        assert!(catalog.capabilities.permissions);
    });
}

/// Full live turn against the real Claude sidecar + Agent SDK. Runs only when
/// invoked explicitly because it costs model tokens. Exercises the whole
/// stack: Rust `ClaudeClient` → sidecar → SDK → back.
#[test]
#[ignore = "costs model tokens; requires the claude sidecar"]
fn contract_live_claude_turn() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let client = Arc::new(ClaudeClient::new(
        capilot_ide_lib::agent_provider::direct::claude_profile(),
    ));
    rt.block_on(async {
        if !client.is_available().await.expect("availability probe") {
            eprintln!("skipping: claude sidecar not available");
            return;
        }
        let manager = Arc::new(AgentManager::new());
        let log = Arc::new(EventLog::default());
        manager.subscribe(log.clone());
        manager.register_provider(client.clone());

        let snap = manager
            .create_agent(NewAgentRequest {
                agent_id: "a-claude-live".into(),
                provider_id: "claude".into(),
                backend_kind: "direct".into(),
                workspace_id: None,
                cwd: std::env::temp_dir(),
                model: Some("claude-haiku-4-5".into()),
                config: vec![],
            })
            .await
            .expect("create_agent");
        assert_eq!(snap.agent.status, AgentStatus::Idle);

        manager
            .start_turn(
                "a-claude-live",
                AgentPrompt {
                    client_message_id: "cm-live".into(),
                    content: vec![PromptContent::Text {
                        text: "Reply with the single word: pong.".into(),
                    }],
                },
            )
            .await
            .expect("start_turn");

        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            let events = log.events.lock().unwrap().clone();
            if events
                .iter()
                .any(|(_, ev)| matches!(ev, AgentEvent::TurnCompleted(_)))
            {
                break;
            }
            if events
                .iter()
                .any(|(_, ev)| matches!(ev, AgentEvent::TurnFailed(_)))
            {
                panic!("live claude turn failed: {events:#?}");
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for live claude turn: {events:#?}"
            );
            std::thread::sleep(Duration::from_millis(200));
        }
        let text: String = {
            let snap2 = manager.snapshot("a-claude-live").expect("snapshot");
            snap2
                .timeline
                .iter()
                .filter_map(|i| match i {
                    TimelineItem::AssistantMessage(m) => Some(m.text.as_str()),
                    _ => None,
                })
                .collect()
        };
        eprintln!("live claude assistant text: {text:?}");
        manager.close_agent("a-claude-live").await.expect("close");
        assert!(
            text.contains("pong"),
            "expected the model's reply to contain 'pong', got {text:?}"
        );
    });
}
