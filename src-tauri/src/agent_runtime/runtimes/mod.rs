pub mod bash;
pub mod claude;
pub mod codex;
pub mod dsh;
pub mod opencode;

use crate::agent_runtime::adapter::AgentRuntimeAdapter;

/// Registry: map a runtime id to its adapter implementation.
pub fn get_adapter(runtime: &str) -> Box<dyn AgentRuntimeAdapter> {
    match runtime {
        "bash" => Box::new(bash::BashAdapter::new("bash", true)),
        "bash-rc" => Box::new(bash::BashAdapter::new("bash-rc", false)),
        "codex" => Box::new(codex::CodexAdapter::new()),
        "opencode" => Box::new(opencode::OpenCodeAdapter::new()),
        "dsh" => Box::new(dsh::DshAdapter::new()),
        // Default to claude for any other/unknown id.
        _ => Box::new(claude::ClaudeAdapter::new()),
    }
}

/// All known runtime ids (for detection lists). The minimal `--norc` "bash"
/// runtime and opencode stay resolvable in `get_adapter` (for resuming older
/// sessions) but are no longer offered as a new terminal.
pub fn known_runtimes() -> &'static [&'static str] {
    &["claude", "codex", "dsh", "bash-rc"]
}
