//! Default interactive system shell (not Git Bash).
//!
//! - **Unix**: `$SHELL` when set and runnable, else `bash`, else `/bin/sh`
//! - **Windows**: `pwsh` (PowerShell 7+) when on PATH, else `ComSpec` / `cmd.exe`
//!
//! Agent CLIs do **not** go through this adapter — they spawn via
//! [`crate::agent_runtime::executable`]. This runtime is only the user's
//! terminal tab (project "+", file-tree "在此打开终端", quick-start commands).

use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, ModelInfo, PermissionModeInfo, ThinkingOptionInfo,
};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub struct ShellAdapter;

impl ShellAdapter {
    pub fn new() -> Self {
        Self
    }

    fn resolve() -> Option<ResolvedShell> {
        static CACHED: OnceLock<Option<ResolvedShell>> = OnceLock::new();
        CACHED.get_or_init(resolve_shell).clone()
    }
}

#[derive(Debug, Clone)]
struct ResolvedShell {
    /// Executable path or bare name for CreateProcess / portable_pty.
    program: PathBuf,
    /// Extra argv before the interactive session (e.g. pwsh `-NoLogo`).
    args: Vec<String>,
    /// Short label for Settings / tab titles.
    label: &'static str,
}

fn resolve_shell() -> Option<ResolvedShell> {
    crate::agent_runtime::adapter::ensure_cli_path();

    #[cfg(windows)]
    {
        return resolve_shell_windows();
    }
    #[cfg(not(windows))]
    {
        resolve_shell_unix()
    }
}

#[cfg(windows)]
fn resolve_shell_windows() -> Option<ResolvedShell> {
    // Prefer PowerShell 7+ when the user installed it — closer to a modern
    // interactive shell than Windows PowerShell 5.1 / cmd.
    if let Some(pwsh) = resolve_on_path(&["pwsh.exe", "pwsh"]) {
        return Some(ResolvedShell {
            program: pwsh,
            args: vec!["-NoLogo".into()],
            label: "PowerShell",
        });
    }

    // ComSpec is the documented default interactive shell for the machine.
    let comspec = std::env::var_os("ComSpec")
        .or_else(|| std::env::var_os("COMSPEC"))
        .map(PathBuf::from)
        .filter(|p| p.as_os_str().len() > 0);

    if let Some(cmd) = comspec {
        if cmd.is_file() || bare_runs(&cmd) {
            return Some(ResolvedShell {
                program: cmd,
                // Interactive ConPTY session — no /C or /K; bare cmd.exe is the
                // usual interactive host (same as Windows Terminal default).
                args: vec![],
                label: "cmd",
            });
        }
    }

    // Last resort hard paths (stripped env on desktop shortcuts).
    for candidate in [
        r"C:\Windows\System32\cmd.exe",
        r"C:\WINDOWS\system32\cmd.exe",
    ] {
        let p = PathBuf::from(candidate);
        if p.is_file() {
            return Some(ResolvedShell {
                program: p,
                args: vec![],
                label: "cmd",
            });
        }
    }
    None
}

#[cfg(not(windows))]
fn resolve_shell_unix() -> Option<ResolvedShell> {
    if let Ok(shell) = std::env::var("SHELL") {
        let p = PathBuf::from(&shell);
        if !shell.is_empty() && (p.is_file() || bare_runs(&p)) {
            return Some(ResolvedShell {
                program: p,
                args: vec![],
                label: "shell",
            });
        }
    }
    for name in ["bash", "zsh", "sh"] {
        if crate::agent_runtime::adapter::cli_available(name) {
            return Some(ResolvedShell {
                program: PathBuf::from(name),
                args: vec![],
                label: "shell",
            });
        }
    }
    for path in ["/bin/bash", "/bin/sh", "/usr/bin/bash", "/usr/bin/sh"] {
        let p = PathBuf::from(path);
        if p.is_file() {
            return Some(ResolvedShell {
                program: p,
                args: vec![],
                label: "shell",
            });
        }
    }
    None
}

fn missing_shell_message() -> String {
    #[cfg(windows)]
    {
        "未检测到系统 shell（cmd / PowerShell）。请确认 ComSpec 指向有效的 cmd.exe。".to_string()
    }
    #[cfg(not(windows))]
    {
        "未检测到系统 shell。请设置 $SHELL 或安装 bash。".to_string()
    }
}

fn bare_runs(path: &Path) -> bool {
    let mut c = std::process::Command::new(path);
    // cmd.exe /C ver  is cheap; pwsh --version; unix shells --version.
    #[cfg(windows)]
    {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if name.contains("cmd") {
            c.args(["/C", "ver"]);
        } else {
            c.arg("-NoLogo").arg("-Command").arg("exit 0");
        }
    }
    #[cfg(not(windows))]
    {
        c.arg("--version");
    }
    crate::agent_runtime::adapter::run_cmd_timeout(
        c,
        crate::agent_runtime::adapter::CLI_PROBE_TIMEOUT,
    )
    .is_some_and(|o| o.status.success())
}

#[cfg(windows)]
fn resolve_on_path(names: &[&str]) -> Option<PathBuf> {
    for name in names {
        if let Some(r) = crate::agent_runtime::executable::resolve_executable(name) {
            if !r.needs_cmd_wrap {
                // Prefer real PE binaries for the interactive shell itself.
                return Some(r.path);
            }
            // .cmd shim for pwsh is fine — prepare_pty_launch will wrap it.
            return Some(r.path);
        }
    }
    None
}

impl AgentRuntimeAdapter for ShellAdapter {
    fn id(&self) -> &str {
        "shell"
    }

    fn name(&self) -> &str {
        match Self::resolve() {
            Some(r) => r.label,
            None => "Shell",
        }
    }

    fn is_available(&self) -> bool {
        Self::resolve().is_some()
    }

    fn is_authenticated(&self) -> bool {
        true
    }

    fn version(&self) -> Option<String> {
        let r = Self::resolve()?;
        let prog = r.program.to_str()?;
        // Best-effort; cmd's version line is noisy — surface the label instead.
        if r.label == "cmd" {
            return Some("cmd".into());
        }
        crate::agent_runtime::adapter::cli_version(prog).or_else(|| Some(r.label.to_string()))
    }

    fn list_models(&self) -> Vec<ModelInfo> {
        vec![]
    }

    fn list_permission_modes(&self) -> Vec<PermissionModeInfo> {
        vec![]
    }

    fn list_thinking_options(&self) -> Vec<ThinkingOptionInfo> {
        vec![]
    }

    fn preflight(&self) -> Option<String> {
        if Self::resolve().is_some() {
            None
        } else {
            Some(missing_shell_message())
        }
    }

    fn spawn_interactive(&self, _session: &AgentSession) -> Result<(String, Vec<String>), String> {
        let r = Self::resolve().ok_or_else(missing_shell_message)?;
        let cmd = r
            .program
            .to_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| r.program.to_string_lossy().into_owned());
        Ok((cmd, r.args))
    }

    fn resume_args(&self, _session: &AgentSession) -> Vec<String> {
        vec![]
    }

    fn speed_args(&self, _speed: &str) -> Vec<String> {
        vec![]
    }

    fn mode_args(&self, _mode: &str) -> Vec<String> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_finds_a_shell() {
        assert!(
            ShellAdapter::resolve().is_some(),
            "CI/dev hosts must have a system shell"
        );
    }

    #[test]
    fn spawn_interactive_returns_program() {
        let a = ShellAdapter::new();
        let session = AgentSession {
            id: "t".into(),
            runtime: "shell".into(),
            mode: "ask".into(),
            speed: "auto".into(),
            model: None,
            cwd: std::env::temp_dir(),
            context_dir: std::env::temp_dir(),
            rows: 24,
            cols: 80,
            resume_key: None,
        };
        let (cmd, _args) = a.spawn_interactive(&session).expect("shell");
        assert!(!cmd.is_empty());
    }
}
