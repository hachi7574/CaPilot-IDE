//! Frontend-facing ACP event DTOs (camelCase over the wire).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One event pushed for a single agent session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AcpEvent {
    SessionStarted {
        #[serde(rename = "sessionId")]
        session_id: String,
        capabilities: Value,
    },
    MessageChunk {
        #[serde(rename = "messageId", skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        text: String,
        /// `agent` | `user` | `thought`
        #[serde(default = "default_role")]
        role: String,
    },
    ToolCall {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        status: String,
    },
    ToolCallUpdate {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Plan {
        entries: Vec<Value>,
    },
    Usage {
        used: u64,
        size: u64,
    },
    PermissionRequest {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "toolCallId", skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        raw: Option<Value>,
    },
    TurnDone {
        #[serde(rename = "stopReason")]
        stop_reason: String,
    },
    Status {
        status: String,
    },
    Error {
        message: String,
    },
    Stderr {
        line: String,
    },
}

fn default_role() -> String {
    "agent".to_string()
}

/// Envelope emitted on the Tauri event bus (`acp://event`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpEventEnvelope {
    pub agent_id: String,
    #[serde(flatten)]
    pub event: AcpEvent,
}

/// Sink for host → UI (or test) events.
pub trait AcpEventSink: Send + Sync {
    fn emit(&self, agent_id: &str, event: AcpEvent);
}

/// Collecting sink for unit/integration tests.
#[derive(Default)]
pub struct VecEventSink {
    pub events: std::sync::Mutex<Vec<(String, AcpEvent)>>,
}

impl AcpEventSink for VecEventSink {
    fn emit(&self, agent_id: &str, event: AcpEvent) {
        if let Ok(mut g) = self.events.lock() {
            g.push((agent_id.to_string(), event));
        }
    }
}

impl VecEventSink {
    pub fn snapshot(&self) -> Vec<(String, AcpEvent)> {
        self.events.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn wait_for(
        &self,
        pred: impl Fn(&AcpEvent) -> bool,
        timeout: std::time::Duration,
    ) -> Option<AcpEvent> {
        let start = std::time::Instant::now();
        loop {
            {
                let g = self.events.lock().ok()?;
                if let Some((_, ev)) = g.iter().rev().find(|(_, e)| pred(e)) {
                    return Some(ev.clone());
                }
            }
            if start.elapsed() > timeout {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}
