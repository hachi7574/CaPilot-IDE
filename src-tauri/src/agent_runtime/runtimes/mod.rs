pub mod bash;
pub mod generic;
pub mod claude;
pub mod codex;
pub mod dsh;
pub mod opencode;
pub mod pi;
pub mod shell;

use crate::agent_runtime::adapter::AgentRuntimeAdapter;
use generic::V1_RUNTIMES;

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
        "claude" => Box::new(claude::ClaudeAdapter::new()),
        // v1 CLIs + unknown ids. Unknown never silently spawn Claude.
        other => Box::new(generic::GenericCliAdapter::for_id(other)),
    }
}

fn first_class_runtimes() -> &'static [&'static str] {
    #[cfg(windows)]
    {
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
    #[cfg(not(windows))]
    {
        &["claude", "codex", "dsh", "pi", "shell", "bash-rc"]
    }
}

/// All known runtime ids (for detection lists).
///
/// - `shell` — OS default interactive terminal (auto: pwsh/cmd on Windows, $SHELL on Unix)
/// - `powershell` / `cmd` — Windows-only in the detection list (still resolvable via
///   `get_adapter` on any platform so older sessions can resume)
/// - `bash-rc` — Git Bash / system bash (optional on Windows)
/// - first-class agent CLIs, then v1 generic CLIs from [`V1_RUNTIMES`]
///
/// The minimal `--norc` "bash" runtime and opencode stay resolvable in
/// `get_adapter` (for resuming older sessions) but are not offered as new
/// terminals.
pub fn known_runtimes() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = first_class_runtimes().to_vec();
    for spec in V1_RUNTIMES {
        if !ids.contains(&spec.id) {
            ids.push(spec.id);
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codebuddy_is_generic_not_claude() {
        let adapter = get_adapter("codebuddy");
        assert_eq!(adapter.id(), "codebuddy");
        assert_eq!(adapter.name(), "CodeBuddy");
        assert!(adapter.list_models().is_empty());
        assert!(!adapter.supports_resume());
    }

    #[test]
    fn unknown_id_is_generic_not_claude() {
        let adapter = get_adapter("not-a-real-cli");
        assert_eq!(adapter.id(), "not-a-real-cli");
        assert_ne!(adapter.id(), "claude");
    }

    #[test]
    fn known_runtimes_include_v1_and_first_class() {
        let ids = known_runtimes();
        assert!(ids.contains(&"codebuddy"));
        assert!(ids.contains(&"claude"));
        assert!(ids.contains(&"trae"));
        assert!(ids.contains(&"hermes"));
        assert!(ids.contains(&"cursor"));
        assert!(!ids.contains(&"opencode"));
    }
}
