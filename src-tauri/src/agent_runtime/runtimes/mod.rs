pub mod bash;
pub mod claude;

use crate::agent_runtime::adapter::AgentRuntimeAdapter;

/// Registry: map a runtime id to its adapter implementation.
pub fn get_adapter(runtime: &str) -> Box<dyn AgentRuntimeAdapter> {
    match runtime {
        "bash" => Box::new(bash::BashAdapter::new()),
        // Default to claude for any other/unknown id.
        _ => Box::new(claude::ClaudeAdapter::new()),
    }
}

/// All known runtime ids (for detection lists).
pub fn known_runtimes() -> &'static [&'static str] {
    &["claude", "bash"]
}
