pub mod bash;
pub mod claude;

use crate::agent_runtime::adapter::AgentRuntimeAdapter;

/// Registry: map a runtime id to its adapter implementation.
pub fn get_adapter(runtime: &str) -> Box<dyn AgentRuntimeAdapter> {
    match runtime {
        "bash" => Box::new(bash::BashAdapter::new("bash", true)),
        "bash-rc" => Box::new(bash::BashAdapter::new("bash-rc", false)),
        // Default to claude for any other/unknown id.
        _ => Box::new(claude::ClaudeAdapter::new()),
    }
}

/// All known runtime ids (for detection lists). The minimal `--norc` "bash"
/// runtime stays resolvable in `get_adapter` (for resuming older sessions) but
/// is no longer offered as a new terminal — users get the full 正常 bash.
pub fn known_runtimes() -> &'static [&'static str] {
    &["claude", "bash-rc"]
}
