//! ACP (Agent Client Protocol) transport — dual-track peer to the PTY path.
//!
//! Runtime ids use the `acp:` prefix (e.g. `acp:opencode`). Callers MUST route
//! through [`is_acp_runtime`] **before** [`crate::agent_runtime::runtimes::get_adapter`],
//! which would otherwise fall through to ClaudeAdapter for unknown ids (DEF-001).

pub mod bridge;
pub mod descriptor;
pub mod events;
pub mod fs_sandbox;
pub mod host;
pub mod permission;
pub mod registry;

pub use bridge::AcpBridge;
pub use descriptor::{AcpAgentDescriptor, AcpAgentsFile};
pub use events::{AcpEvent, AcpEventEnvelope};
pub use host::AcpHostError;

/// True when `runtime` is an ACP transport id (`acp:<name>`).
pub fn is_acp_runtime(runtime: &str) -> bool {
    runtime.starts_with("acp:")
}

/// Strip the `acp:` prefix. Returns `None` when the id is not ACP.
pub fn strip_acp_prefix(runtime: &str) -> Option<&str> {
    runtime.strip_prefix("acp:")
}

/// Build the full runtime id from a descriptor short id.
pub fn acp_runtime_id(descriptor_id: &str) -> String {
    format!("acp:{descriptor_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_acp_runtime_prefix_only() {
        assert!(is_acp_runtime("acp:opencode"));
        assert!(is_acp_runtime("acp:gemini"));
        assert!(!is_acp_runtime("opencode"));
        assert!(!is_acp_runtime("claude"));
        assert!(!is_acp_runtime("acp"));
        assert!(!is_acp_runtime("acp"));
        assert!(!is_acp_runtime("ACP:opencode"));
    }

    #[test]
    fn strip_and_build_roundtrip() {
        assert_eq!(strip_acp_prefix("acp:opencode"), Some("opencode"));
        assert_eq!(strip_acp_prefix("opencode"), None);
        assert_eq!(acp_runtime_id("opencode"), "acp:opencode");
    }
}
