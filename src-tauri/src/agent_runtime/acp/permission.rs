//! Permission request state for ACP `session/request_permission`.
//!
//! MVP policy: always surface to the UI (CaPilot mode `ask`). auto/yolo later.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Outcome the frontend (or auto-policy) chooses.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOutcome {
    /// Allow with the given option id from the agent's option list.
    Allow { option_id: String },
    /// Reject once.
    Reject { option_id: Option<String> },
    /// User dismissed / cancelled.
    Cancelled,
}

/// One outstanding agent→client permission request.
#[derive(Debug, Clone)]
pub struct PendingPermission {
    pub agent_id: String,
    /// JSON-RPC id the agent used for the request (number or string as string).
    pub request_id: String,
    pub tool_call_id: Option<String>,
    pub summary: String,
}

/// Thread-safe map of pending permission requests keyed by `(agent_id, request_id)`.
#[derive(Default, Clone)]
pub struct PermissionBoard {
    inner: Arc<Mutex<HashMap<(String, String), PendingPermission>>>,
}

impl PermissionBoard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, p: PendingPermission) {
        if let Ok(mut g) = self.inner.lock() {
            g.insert((p.agent_id.clone(), p.request_id.clone()), p);
        }
    }

    pub fn take(&self, agent_id: &str, request_id: &str) -> Option<PendingPermission> {
        self.inner
            .lock()
            .ok()
            .and_then(|mut g| g.remove(&(agent_id.to_string(), request_id.to_string())))
    }

    pub fn clear_agent(&self, agent_id: &str) {
        if let Ok(mut g) = self.inner.lock() {
            g.retain(|(aid, _), _| aid != agent_id);
        }
    }
}
