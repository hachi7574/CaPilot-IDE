pub mod bash;
pub mod claude;
pub mod codex;
pub mod dsh;
pub mod generic;
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
            "opencode",
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
        &[
            "claude", "codex", "opencode", "dsh", "pi", "shell", "bash-rc",
        ]
    }
}

/// All known runtime ids (for detection lists).
///
/// - `shell` — OS default interactive terminal (auto: pwsh/cmd on Windows, $SHELL on Unix)
/// - `powershell` / `cmd` — Windows-only in the detection list (still resolvable via
///   `get_adapter` on any platform so older sessions can resume)
/// - `bash-rc` — Git Bash / system bash (optional on Windows)
/// - first-class agent CLIs (including opencode), then v1 generic CLIs from
///   [`V1_RUNTIMES`]
///
/// The minimal `--norc` "bash" runtime stays resolvable in `get_adapter`
/// (for resuming older sessions) but is not offered as a new terminal.
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
        assert!(ids.contains(&"opencode"));
        assert!(ids.contains(&"mistral-vibe"));
    }

    /// Orca's TUI agent catalog minus `claude-agent-teams` (an Orca launch mode,
    /// not a CLI). CaPilot must know every id so a PATH hit can surface in Settings.
    #[test]
    fn known_runtimes_cover_orca_tui_agents() {
        let ids = known_runtimes();
        for id in [
            "claude",
            "openclaude",
            "codex",
            "autohand",
            "ante",
            "trae",
            "opencode",
            "mimo-code",
            "pi",
            "omp",
            "prime-agent",
            "gemini",
            "antigravity",
            "aider",
            "goose",
            "amp",
            "kilo",
            "kiro",
            "crush",
            "aug",
            "cline",
            "codebuff",
            "command-code",
            "continue",
            "cursor",
            "droid",
            "kimi",
            "mistral-vibe",
            "qwen-code",
            "rovo",
            "hermes",
            "openclaw",
            "copilot",
            "grok",
            "devin",
        ] {
            assert!(ids.contains(&id), "missing Orca agent {id}");
        }
        assert!(!ids.contains(&"claude-agent-teams"));
    }

    /// PATH presence (not `--version`) is what Orca counts. Every Orca detect
    /// binary that is actually installed must report available.
    #[test]
    fn installed_orca_detect_binaries_are_available() {
        crate::agent_runtime::adapter::ensure_cli_path();
        let pairs: &[(&str, &str)] = &[
            ("claude", "claude"),
            ("codex", "codex"),
            ("opencode", "opencode"),
            ("trae", "traecli"),
            ("gemini", "gemini"),
            ("aider", "aider"),
            ("kilo", "kilo"),
            ("kiro", "kiro-cli"),
            ("crush", "crush"),
            ("aug", "auggie"),
            ("cline", "cline"),
            ("codebuff", "codebuff"),
            ("command-code", "command-code"),
            ("continue", "cn"),
            ("kimi", "kimi"),
            ("qwen-code", "qwen"),
            ("hermes", "hermes"),
            ("copilot", "copilot"),
            ("omp", "omp"),
            ("openclaude", "openclaude"),
            ("pi", "pi"),
            ("cursor", "cursor-agent"),
            ("grok", "grok"),
            ("amp", "amp"),
            ("droid", "droid"),
            ("goose", "goose"),
            ("antigravity", "agy"),
            ("mistral-vibe", "vibe"),
        ];
        let mut on_path = 0usize;
        for (id, bin) in pairs {
            if !crate::agent_runtime::adapter::cli_available(bin) {
                continue;
            }
            on_path += 1;
            assert!(
                get_adapter(id).is_available(),
                "{id} ({bin}) is on PATH but CaPilot reports unavailable"
            );
        }
        let _ = on_path;
    }
}
