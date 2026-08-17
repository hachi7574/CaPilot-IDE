//! Interactive system shells (not Git Bash — see [`super::bash`]).
//!
//! Runtime ids:
//! - **`shell`** — OS default (auto): `$SHELL` on Unix; on Windows
//!   `pwsh` → Windows PowerShell → `ComSpec`/`cmd.exe`. Kept for older
//!   sessions and as a generic fallback.
//! - **`powershell`** — PowerShell 7+ (`pwsh`) when present, else Windows
//!   PowerShell 5.1. Windows-only in the new-terminal picker.
//! - **`cmd`** — `cmd.exe` via `ComSpec` / System32. Windows-only in the
//!   new-terminal picker.
//!
//! Agent CLIs do **not** go through this adapter — they spawn via
//! [`crate::agent_runtime::executable`]. These runtimes are only the user's
//! terminal tab (project "+", file-tree "在此打开终端", quick-start commands).

use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, ModelInfo, PermissionModeInfo, ThinkingOptionInfo,
};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Which binary family this adapter resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShellKind {
    /// Auto-detect the platform default interactive shell.
    Auto,
    /// PowerShell 7+ or Windows PowerShell 5.1 only.
    PowerShell,
    /// cmd.exe only.
    Cmd,
}

pub struct ShellAdapter {
    id: &'static str,
    kind: ShellKind,
}

impl ShellAdapter {
    /// Auto OS shell (`shell` runtime id).
    pub fn new() -> Self {
        Self {
            id: "shell",
            kind: ShellKind::Auto,
        }
    }

    /// Explicit PowerShell (`powershell` runtime id).
    pub fn powershell() -> Self {
        Self {
            id: "powershell",
            kind: ShellKind::PowerShell,
        }
    }

    /// Explicit cmd.exe (`cmd` runtime id).
    pub fn cmd() -> Self {
        Self {
            id: "cmd",
            kind: ShellKind::Cmd,
        }
    }

    fn resolve(&self) -> Option<ResolvedShell> {
        match self.kind {
            ShellKind::Auto => {
                static CACHED: OnceLock<Option<ResolvedShell>> = OnceLock::new();
                CACHED.get_or_init(resolve_shell_auto).clone()
            }
            ShellKind::PowerShell => {
                static CACHED: OnceLock<Option<ResolvedShell>> = OnceLock::new();
                CACHED.get_or_init(resolve_powershell).clone()
            }
            ShellKind::Cmd => {
                static CACHED: OnceLock<Option<ResolvedShell>> = OnceLock::new();
                CACHED.get_or_init(resolve_cmd).clone()
            }
        }
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

fn resolve_shell_auto() -> Option<ResolvedShell> {
    crate::agent_runtime::adapter::ensure_cli_path();

    #[cfg(windows)]
    {
        // Prefer PowerShell when present; fall back to cmd.
        resolve_powershell().or_else(resolve_cmd)
    }
    #[cfg(not(windows))]
    {
        resolve_shell_unix()
    }
}

/// PowerShell 7+ (`pwsh`) then Windows PowerShell 5.1.
fn resolve_powershell() -> Option<ResolvedShell> {
    crate::agent_runtime::adapter::ensure_cli_path();

    #[cfg(windows)]
    {
        // Prefer PowerShell 7+ when the user installed it — closer to a modern
        // interactive shell than Windows PowerShell 5.1. Must resolve to a
        // real on-disk binary: a bare `pwsh.exe` that is not on PATH is not a
        // hit (CreateProcess error 2).
        if let Some(pwsh) = resolve_on_path(&["pwsh.exe", "pwsh"]) {
            return Some(ResolvedShell {
                program: pwsh,
                args: vec!["-NoLogo".into()],
                label: "PowerShell",
            });
        }

        // Windows PowerShell 5.1 ships with Windows. Prefer the absolute
        // System32 path so a stripped PATH (daemon DETACHED_PROCESS) still
        // finds it.
        for candidate in [
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            r"C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe",
        ] {
            let p = PathBuf::from(candidate);
            if p.is_file() {
                return Some(ResolvedShell {
                    program: p,
                    args: vec!["-NoLogo".into()],
                    label: "Windows PowerShell",
                });
            }
        }
        if let Some(ps) = resolve_on_path(&["powershell.exe", "powershell"]) {
            return Some(ResolvedShell {
                program: ps,
                args: vec!["-NoLogo".into()],
                label: "Windows PowerShell",
            });
        }
        return None;
    }
    #[cfg(not(windows))]
    {
        // Rare but useful when pwsh is installed on macOS/Linux.
        if let Some(pwsh) = resolve_on_path_unix(&["pwsh"]) {
            return Some(ResolvedShell {
                program: pwsh,
                args: vec!["-NoLogo".into()],
                label: "PowerShell",
            });
        }
        None
    }
}

/// cmd.exe via ComSpec / System32 hard paths.
fn resolve_cmd() -> Option<ResolvedShell> {
    crate::agent_runtime::adapter::ensure_cli_path();

    #[cfg(windows)]
    {
        // ComSpec is the documented default interactive shell for the machine.
        let comspec = std::env::var_os("ComSpec")
            .or_else(|| std::env::var_os("COMSPEC"))
            .map(PathBuf::from)
            .filter(|p| p.as_os_str().len() > 0);

        if let Some(cmd) = comspec {
            if cmd.is_file() || bare_runs(&cmd) {
                return Some(ResolvedShell {
                    program: cmd,
                    // Interactive ConPTY session — no /C or /K; bare cmd.exe is
                    // the usual interactive host (same as Windows Terminal).
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
        return None;
    }
    #[cfg(not(windows))]
    {
        None
    }
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

fn missing_shell_message(kind: ShellKind) -> String {
    match kind {
        ShellKind::PowerShell => {
            #[cfg(windows)]
            {
                "未检测到 PowerShell。请确认已安装 PowerShell 7（pwsh）或系统自带的 Windows PowerShell。"
                    .to_string()
            }
            #[cfg(not(windows))]
            {
                "未检测到 PowerShell（pwsh）。".to_string()
            }
        }
        ShellKind::Cmd => {
            "未检测到 cmd.exe。请确认 ComSpec 指向有效的 cmd.exe。".to_string()
        }
        ShellKind::Auto => {
            #[cfg(windows)]
            {
                "未检测到系统 shell（cmd / PowerShell）。请确认 ComSpec 指向有效的 cmd.exe。"
                    .to_string()
            }
            #[cfg(not(windows))]
            {
                "未检测到系统 shell。请设置 $SHELL 或安装 bash。".to_string()
            }
        }
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
            // Only accept a real filesystem hit. A bare unresolved name must not
            // become the interactive shell — ConPTY can't PATH-search the way
            // cmd.exe does and fails with error 2.
            if !r.path.is_file() {
                continue;
            }
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

#[cfg(not(windows))]
fn resolve_on_path_unix(names: &[&str]) -> Option<PathBuf> {
    for name in names {
        if crate::agent_runtime::adapter::cli_available(name) {
            return Some(PathBuf::from(name));
        }
    }
    None
}

impl AgentRuntimeAdapter for ShellAdapter {
    fn id(&self) -> &str {
        self.id
    }

    fn name(&self) -> &str {
        match self.resolve() {
            Some(r) => r.label,
            None => match self.kind {
                ShellKind::PowerShell => "PowerShell",
                ShellKind::Cmd => "CMD",
                ShellKind::Auto => "Shell",
            },
        }
    }

    fn is_available(&self) -> bool {
        self.resolve().is_some()
    }

    fn is_authenticated(&self) -> bool {
        true
    }

    fn version(&self) -> Option<String> {
        let r = self.resolve()?;
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
        if self.resolve().is_some() {
            None
        } else {
            Some(missing_shell_message(self.kind))
        }
    }

    fn spawn_interactive(&self, _session: &AgentSession) -> Result<(String, Vec<String>), String> {
        let r = self
            .resolve()
            .ok_or_else(|| missing_shell_message(self.kind))?;
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
    fn resolve_auto_finds_a_shell() {
        assert!(
            ShellAdapter::new().resolve().is_some(),
            "CI/dev hosts must have a system shell"
        );
    }

    #[test]
    #[cfg(windows)]
    fn resolve_powershell_or_cmd_on_windows() {
        // At least one of the explicit Windows shells must resolve.
        assert!(
            ShellAdapter::powershell().resolve().is_some()
                || ShellAdapter::cmd().resolve().is_some(),
            "Windows CI/dev hosts must have PowerShell or cmd"
        );
    }

    #[test]
    #[cfg(windows)]
    fn resolve_cmd_finds_cmd_on_windows() {
        assert!(
            ShellAdapter::cmd().resolve().is_some(),
            "Windows always ships cmd.exe"
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
