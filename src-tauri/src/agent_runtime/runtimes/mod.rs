pub mod bash;
pub mod claude;
pub mod codex;
pub mod dsh;
pub mod opencode;
pub mod pi;
pub mod shell;

use crate::agent_runtime::adapter::AgentRuntimeAdapter;

/// Registry: map a runtime id to its adapter implementation.
pub fn get_adapter(runtime: &str) -> Box<dyn AgentRuntimeAdapter> {
    match runtime {
        "shell" => Box::new(shell::ShellAdapter::new()),
        "powershell" => Box::new(shell::ShellAdapter::powershell()),
        "cmd" => Box::new(shell::ShellAdapter::cmd()),
        "bash" => Box::new(bash::BashAdapter::new("bash", true)),
        "bash-rc" => Box::new(bash::BashAdapter::new("bash-rc", false)),
        "codex" => Box::new(codex::CodexAdapter::new()),
        "opencode" => Box::new(opencode::OpenCodeAdapter::new()),
        "dsh" => Box::new(dsh::DshAdapter::new()),
        "pi" => Box::new(pi::PiAdapter::new()),
        // Default to claude for any other/unknown id.
        _ => Box::new(claude::ClaudeAdapter::new()),
    }
}

/// All known runtime ids (for detection lists).
///
/// - `shell` — OS default interactive terminal (auto: pwsh/cmd on Windows, $SHELL on Unix)
/// - `powershell` / `cmd` — explicit Windows shells (also probeable on Unix if pwsh exists)
/// - `bash-rc` — Git Bash / system bash (optional on Windows)
/// - agent CLIs
///
/// The minimal `--norc` "bash" runtime and opencode stay resolvable in
/// `get_adapter` (for resuming older sessions) but are not offered as new
/// terminals.
pub fn known_runtimes() -> &'static [&'static str] {
    &[
        "claude",
        "codex",
        "dsh",
        "pi",
        "shell",
        "powershell",
        "cmd",
        "bash-rc",
    ]
}
