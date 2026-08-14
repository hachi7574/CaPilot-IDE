//! Phase 1 acceptance tests (architecture §14): using a fake provider, the
//! AgentManager can create a session, stream the timeline, cancel, request +
//! resolve a permission, and resume a snapshot from a persisted handle.

use super::fake::{default_provider, full_capabilities, FakeProvider};
use super::manager::{AgentEventObserver, AgentManager, NewAgentRequest, ResumeAgentRequest};
use super::types::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

fn new_request(agent_id: &str, provider_id: &str) -> NewAgentRequest {
    NewAgentRequest {
        agent_id: agent_id.into(),
        provider_id: provider_id.into(),
        backend_kind: "acp".into(),
        workspace_id: Some("wks-1".into()),
        cwd: PathBuf::from("/tmp/w/proj"),
        model: Some("fake-model".into()),
        config: vec![],
    }
}

fn sample_permission(agent_id: &str, id: &str) -> PermissionRequest {
    PermissionRequest {
        id: id.into(),
        agent_id: agent_id.into(),
        kind: PermissionKind::ToolCall,
        title: "Edit file".into(),
        description: Some("Allow Write('/tmp/w/proj/README.md')".into()),
        subject: PermissionSubject {
            kind: PermissionKind::FileChange,
            title: "Write".into(),
            description: None,
            icon: None,
        },
        actions: vec![
            PermissionAction {
                id: "allow".into(),
                label: "允许".into(),
                behavior: PermissionBehavior::Allow,
            },
            PermissionAction {
                id: "deny".into(),
                label: "拒绝".into(),
                behavior: PermissionBehavior::Deny,
            },
        ],
    }
}

#[test]
fn create_session_ready_and_snapshot() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let provider = default_provider();
        let manager: Arc<AgentManager> = Arc::new(AgentManager::new());
        manager.register_provider(provider.clone());

        let snap = manager
            .create_agent(new_request("a1", "fake"))
            .await
            .unwrap();
        assert_eq!(snap.agent.provider_id, "fake");
        assert_eq!(snap.agent.backend_kind, "acp");
        assert_eq!(snap.agent.status, AgentStatus::Idle);
        assert_eq!(snap.agent.capabilities, full_capabilities());
        assert!(snap.agent.persistence.is_some());
        assert_eq!(snap.timeline.len(), 0);
        assert!(snap.pending_permissions.is_empty());

        // The provider's SessionReady was routed through the manager sink.
        assert_eq!(provider.session_count(), 1);
        let snap2 = manager.snapshot("a1").unwrap();
        assert_eq!(snap2.agent.status, AgentStatus::Idle);
    });
}

#[test]
fn stream_timeline_upserts_by_item_id() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let provider = default_provider();
        let manager: Arc<AgentManager> = Arc::new(AgentManager::new());
        manager.register_provider(provider.clone());
        manager
            .create_agent(new_request("a1", "fake"))
            .await
            .unwrap();

        let session = provider.session("rsession-0").unwrap(); // first created
        session.emit(AgentEvent::Timeline(TimelineEvent::Started {
            item: TimelineItem::UserMessage(MessageItem {
                item_id: "u1".into(),
                role: MessageRole::User,
                text: "hi".into(),
                created_at: 1,
                metadata: None,
            }),
        }));
        session.emit(AgentEvent::Timeline(TimelineEvent::Started {
            item: TimelineItem::AssistantMessage(MessageItem {
                item_id: "a1".into(),
                role: MessageRole::Assistant,
                text: "Let me ".into(),
                created_at: 2,
                metadata: None,
            }),
        }));
        session.emit(AgentEvent::Timeline(TimelineEvent::Appended {
            item_id: "a1".into(),
            text_delta: "look".into(),
        }));
        session.emit(AgentEvent::Timeline(TimelineEvent::Replaced {
            item: TimelineItem::AssistantMessage(MessageItem {
                item_id: "a1".into(),
                role: MessageRole::Assistant,
                text: "Let me look".into(),
                created_at: 2,
                metadata: None,
            }),
        }));

        let snap = manager.snapshot("a1").unwrap();
        assert_eq!(snap.timeline.len(), 2);
        assert_eq!(snap.timeline[0].item_id(), "u1");
        assert_eq!(snap.timeline[1].item_id(), "a1");
    });
}

#[test]
fn start_turn_cancel_and_status_transitions() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let provider = default_provider();
        let manager: Arc<AgentManager> = Arc::new(AgentManager::new());
        manager.register_provider(provider.clone());
        manager
            .create_agent(new_request("a1", "fake"))
            .await
            .unwrap();
        let session = provider.session("rsession-0").unwrap();

        let turn = manager
            .start_turn(
                "a1",
                AgentPrompt {
                    client_message_id: "m1".into(),
                    content: vec![PromptContent::Text {
                        text: "build it".into(),
                    }],
                },
            )
            .await
            .unwrap();
        assert!(turn.starts_with("turn-"));
        assert_eq!(
            manager.snapshot("a1").unwrap().agent.status,
            AgentStatus::Running
        );

        // A provider work event keeps running; completion returns to idle.
        session.emit(AgentEvent::Timeline(TimelineEvent::Started {
            item: TimelineItem::ToolCall(ToolCallItem {
                item_id: "t1".into(),
                tool_name: "bash".into(),
                tool_input: Some(serde_json::json!({"cmd": "echo hi"})),
                tool_output: None,
                status: ToolCallStatus::Running,
                created_at: 3,
                metadata: None,
            }),
        }));
        assert_eq!(
            manager.snapshot("a1").unwrap().agent.status,
            AgentStatus::Running
        );

        manager.interrupt("a1").await.unwrap();
        assert_eq!(
            manager.snapshot("a1").unwrap().agent.status,
            AgentStatus::Idle
        );
    });
}

#[test]
fn permission_request_resolve_and_single_resolution() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let provider = default_provider();
        let manager: Arc<AgentManager> = Arc::new(AgentManager::new());
        manager.register_provider(provider.clone());
        manager
            .create_agent(new_request("a1", "fake"))
            .await
            .unwrap();
        let session = provider.session("rsession-0").unwrap();

        session.emit(AgentEvent::PermissionRequested(sample_permission(
            "a1", "p1",
        )));
        let snap = manager.snapshot("a1").unwrap();
        assert_eq!(snap.agent.status, AgentStatus::WaitingPermission);
        assert_eq!(snap.pending_permissions.len(), 1);

        // Undeclared action rejected.
        let err = manager
            .respond_to_permission("a1", "p1", "not-declared")
            .await
            .unwrap_err();
        assert_eq!(err.code(), "invalid_argument");
        // Declared action resolves.
        manager
            .respond_to_permission("a1", "p1", "allow")
            .await
            .unwrap();
        let snap = manager.snapshot("a1").unwrap();
        assert!(snap.pending_permissions.is_empty());
        assert_eq!(snap.agent.status, AgentStatus::Running);

        // Second resolution fails: not found.
        let err = manager
            .respond_to_permission("a1", "p1", "allow")
            .await
            .unwrap_err();
        assert_eq!(err.code(), "permission_not_found");
    });
}

#[test]
fn resume_replays_timeline_from_persistence_handle() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let provider = default_provider();
        let manager: Arc<AgentManager> = Arc::new(AgentManager::new());
        manager.register_provider(provider.clone());
        manager
            .create_agent(new_request("a1", "fake"))
            .await
            .unwrap();
        let session = provider.session("rsession-0").unwrap();

        // Build a timeline + an unresolved permission, then close (releases the
        // runtime but keeps the record + handle).
        session.emit(AgentEvent::Timeline(TimelineEvent::Started {
            item: TimelineItem::UserMessage(MessageItem {
                item_id: "u1".into(),
                role: MessageRole::User,
                text: "original prompt".into(),
                created_at: 1,
                metadata: None,
            }),
        }));
        session.emit(AgentEvent::PermissionRequested(sample_permission(
            "a1", "p1",
        )));
        let handle = manager
            .snapshot("a1")
            .unwrap()
            .agent
            .persistence
            .clone()
            .unwrap();
        manager.close_agent("a1").await.unwrap();
        assert!(session.is_closed(), "close must release the fake runtime");
        assert_eq!(
            manager.snapshot("a1").unwrap().agent.status,
            AgentStatus::Closed
        );

        // Resume from the handle: the provider replays recorded events so the
        // manager rebuilds the timeline and pending permission.
        manager
            .resume_agent(ResumeAgentRequest {
                agent_id: "a1".into(),
                handle: handle.clone(),
                cwd: PathBuf::from("/tmp/w/proj"),
                model: None,
                config: vec![],
            })
            .await
            .unwrap();

        let snap = manager.snapshot("a1").unwrap();
        // Replay ends with the permission still unresolved → WaitingPermission.
        assert_eq!(snap.agent.status, AgentStatus::WaitingPermission);
        assert_eq!(snap.timeline.len(), 1);
        assert_eq!(snap.timeline[0].item_id(), "u1");
        assert_eq!(snap.pending_permissions.len(), 1);
        assert_eq!(snap.pending_permissions[0].id, "p1");
    });
}

#[test]
fn observer_receives_sequenced_events_in_order() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let provider = default_provider();
        let manager: Arc<AgentManager> = Arc::new(AgentManager::new());
        manager.register_provider(provider.clone());

        struct Recorder {
            seqs: Mutex<Vec<u64>>,
            last: AtomicU64,
        }
        impl AgentEventObserver for Recorder {
            fn on_agent_event(&self, _agent_id: &str, seq: u64, _event: &AgentEvent) {
                self.seqs.lock().unwrap().push(seq);
                self.last.store(seq, Ordering::SeqCst);
            }
        }
        let recorder = Arc::new(Recorder {
            seqs: Mutex::new(vec![]),
            last: AtomicU64::new(0),
        });
        manager.subscribe(recorder.clone());

        manager
            .create_agent(new_request("a1", "fake"))
            .await
            .unwrap();
        let session = provider.session("rsession-0").unwrap();
        session.emit(AgentEvent::Timeline(TimelineEvent::Started {
            item: TimelineItem::AssistantMessage(MessageItem {
                item_id: "a1".into(),
                role: MessageRole::Assistant,
                text: "x".into(),
                created_at: 1,
                metadata: None,
            }),
        }));

        let seqs = recorder.seqs.lock().unwrap();
        assert_eq!(seqs.len(), 2); // SessionReady + Timeline
        assert_eq!(seqs[0], 1);
        assert_eq!(seqs[1], 2);
        assert_eq!(recorder.last.load(Ordering::SeqCst), 2);
    });
}

#[test]
fn provider_switch_is_handoff_not_resume() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let provider = default_provider();
        let manager: Arc<AgentManager> = Arc::new(AgentManager::new());
        manager.register_provider(provider.clone());
        manager
            .create_agent(new_request("a1", "fake"))
            .await
            .unwrap();
        let handle = manager
            .snapshot("a1")
            .unwrap()
            .agent
            .persistence
            .clone()
            .unwrap();

        // Unregistered provider → not found.
        let mut wrong = handle.clone();
        wrong.provider_id = "ghost".into();
        let err = manager
            .resume_agent(ResumeAgentRequest {
                agent_id: "a1".into(),
                handle: wrong,
                cwd: PathBuf::from("/tmp"),
                model: None,
                config: vec![],
            })
            .await
            .unwrap_err();
        assert_eq!(err.code(), "provider_not_found");

        // A registered DIFFERENT provider cannot silently continue the same
        // native session — the manager rejects it as a handoff (§10.3).
        let provider2 = FakeProvider::new("other", full_capabilities());
        manager.register_provider(provider2);
        let mut wrong = handle;
        wrong.provider_id = "other".into();
        let err = manager
            .resume_agent(ResumeAgentRequest {
                agent_id: "a1".into(),
                handle: wrong,
                cwd: PathBuf::from("/tmp"),
                model: None,
                config: vec![],
            })
            .await
            .unwrap_err();
        assert_eq!(err.code(), "invalid_argument");

        // Same provider still resumes fine.
        let snap = manager
            .resume_agent(ResumeAgentRequest {
                agent_id: "a1".into(),
                handle: manager
                    .snapshot("a1")
                    .unwrap()
                    .agent
                    .persistence
                    .clone()
                    .unwrap(),
                cwd: PathBuf::from("/tmp"),
                model: None,
                config: vec![],
            })
            .await
            .unwrap();
        assert_eq!(snap.agent.provider_id, "fake");
    });
}
