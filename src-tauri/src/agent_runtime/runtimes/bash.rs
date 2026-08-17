use crate::agent_runtime::adapter::{
    AgentRuntimeAdapter, AgentSession, ModelInfo, PermissionModeInfo, ThinkingOptionInfo,
};
use std::path::PathBuf;
use std::sync::OnceLock;

/// Shell runtime in two flavours:
/// - `"bash"` (norc: true) — minimal shell, skips `~/.bashrc` (clean, fast).
/// - `"bash-rc"` (norc: false) — full interactive bash that sources the user's
///   `~/.bashrc`, so the prompt / aliases / PATH match the system terminal.
///
/// On Windows this is **Git Bash** (optional). The default new-terminal entry is
/// the OS shell (`shell` runtime), not bash — see [`super::shell`].
pub struct BashAdapter {
    id: &'static str,
    norc: bool,
}

impl BashAdapter {
    pub fn new(id: &'static str, norc: bool) -> Self {
        Self { id, norc }
    }

    /// Resolve the bash binary once per process.
    ///
    /// Order:
    /// 1. Well-known Git for Windows install paths (absolute `bash.exe`) —
    ///    preferred on Windows so we never pick a random WSL/MSYS bash.
    /// 2. `bash` on PATH (after [`ensure_cli_path`] has prepended Git bins)
    ///
    /// Returning an absolute path on Windows avoids a later PATH race and makes
    /// spawn failures point at a real file the user can inspect.
    fn resolve_bash() -> Option<PathBuf> {
        static CACHED: OnceLock<Option<PathBuf>> = OnceLock::new();
        CACHED
            .get_or_init(|| {
                // Make sure Git\bin is visible before the PATH probe.
                crate::agent_runtime::adapter::ensure_cli_path();

                #[cfg(windows)]
                {
                    // Prefer an absolute Git-for-Windows bash.exe. Bare `bash` on
                    // PATH can resolve to unrelated tools; absolute path also
                    // survives PATH mutations between probe and PTY spawn.
                    if let Some(p) = find_windows_bash_exe() {
                        if bash_runs(&p) {
                            return Some(p);
                        }
                        // File exists but --version failed (locked / broken
                        // install). Still return it — spawn will surface the
                        // real OS error rather than "not detected".
                        if p.is_file() {
                            return Some(p);
                        }
                    }
                }

                if crate::agent_runtime::adapter::cli_available("bash") {
                    // Prefer a real resolved path over the bare name so ConPTY
                    // does not re-search PATH later.
                    if let Some(r) =
                        crate::agent_runtime::executable::resolve_executable("bash")
                    {
                        if r.path.is_file() {
                            return Some(r.path);
                        }
                    }
                    return Some(PathBuf::from("bash"));
                }

                #[cfg(not(windows))]
                {
                    for path in ["/bin/bash", "/usr/bin/bash"] {
                        let p = PathBuf::from(path);
                        if p.is_file() && bash_runs(&p) {
                            return Some(p);
                        }
                    }
                }

                None
            })
            .clone()
    }

    fn unavailable_message() -> String {
        #[cfg(windows)]
        {
            "未检测到 bash。请安装 Git for Windows（勾选 Git Bash），\
             或把 bash.exe 所在目录加入 PATH 后重启 CaPilot。\
             常见路径：C:\\Program Files\\Git\\bin\\bash.exe"
                .to_string()
        }
        #[cfg(not(windows))]
        {
            "未检测到 bash。请安装 bash 并确保它在 PATH 中，然后重启 CaPilot。"
                .to_string()
        }
    }
}

/// True when `path --version` exits 0 within the CLI probe timeout.
fn bash_runs(path: &std::path::Path) -> bool {
    let mut c = std::process::Command::new(path);
    c.arg("--version");
    crate::agent_runtime::executable::hide_windows_console(&mut c);
    crate::agent_runtime::adapter::run_cmd_timeout(
        c,
        crate::agent_runtime::adapter::CLI_PROBE_TIMEOUT,
    )
    .is_some_and(|o| o.status.success())
}

/// Probe well-known Git for Windows layouts for `bash.exe`.
#[cfg(windows)]
fn find_windows_bash_exe() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    for dir in crate::agent_runtime::adapter::windows_git_bin_dirs() {
        candidates.push(dir.join("bash.exe"));
    }

    // Hard-coded last-resort paths (cover installs where env vars are stripped).
    for root in [
        r"C:\Program Files\Git",
        r"C:\Program Files (x86)\Git",
        r"C:\Git",
        // Common non-C: portable / custom installs.
        r"D:\Git",
        r"E:\Git",
        r"A:\Git",
    ] {
        candidates.push(PathBuf::from(root).join(r"bin\bash.exe"));
        candidates.push(PathBuf::from(root).join(r"usr\bin\bash.exe"));
    }

    // Prefer bin\bash.exe (the small launcher) over usr\bin when both exist.
    let mut bin_hit = None;
    let mut usr_hit = None;
    for p in candidates {
        if !p.is_file() {
            continue;
        }
        let s = p.to_string_lossy().to_ascii_lowercase();
        if s.ends_with(r"\bin\bash.exe") && !s.contains(r"\usr\bin\") {
            bin_hit = Some(p);
            break;
        }
        if usr_hit.is_none() {
            usr_hit = Some(p);
        }
    }
    bin_hit.or(usr_hit)
}

impl AgentRuntimeAdapter for BashAdapter {
    fn id(&self) -> &str {
        self.id
    }

    fn name(&self) -> &str {
        if self.norc {
            "Bash"
        } else {
            #[cfg(windows)]
            {
                "Git Bash"
            }
            #[cfg(not(windows))]
            {
                "Bash (rc)"
            }
        }
    }

    fn is_available(&self) -> bool {
        Self::resolve_bash().is_some()
    }

    fn is_authenticated(&self) -> bool {
        true
    }

    fn version(&self) -> Option<String> {
        let bin = Self::resolve_bash()?;
        // `bash --version`'s first line is "GNU bash, version 5.2.15(1)-release …".
        if let Some(s) = bin.to_str() {
            crate::agent_runtime::adapter::cli_version(s)
        } else if bash_runs(&bin) {
            // Non-UTF8 path that still runs — surface a generic label.
            Some("bash".to_string())
        } else {
            None
        }
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
        if Self::resolve_bash().is_some() {
            None
        } else {
            Some(Self::unavailable_message())
        }
    }

    fn spawn_interactive(&self, _session: &AgentSession) -> Result<(String, Vec<String>), String> {
        let bin = Self::resolve_bash().ok_or_else(Self::unavailable_message)?;
        // `--norc` for the minimal shell; the full variant just runs bash
        // (interactive), which sources /etc/bash.bashrc + ~/.bashrc (or the
        // Git Bash equivalents under Windows).
        let args = if self.norc {
            vec!["--norc".to_string()]
        } else {
            vec![]
        };
        // Use the resolved path (may be absolute on Windows) so spawn does not
        // re-depend on a later PATH mutation.
        let cmd = bin
            .to_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "bash".to_string());
        Ok((cmd, args))
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
    fn resolve_bash_finds_system_bash_on_unix() {
        #[cfg(unix)]
        {
            assert!(
                BashAdapter::resolve_bash().is_some(),
                "unix CI / dev boxes must have bash on PATH"
            );
        }
    }

    #[test]
    fn unavailable_message_mentions_install_hint() {
        let msg = BashAdapter::unavailable_message();
        assert!(msg.contains("bash"), "{msg}");
        #[cfg(windows)]
        assert!(msg.contains("Git"), "{msg}");
    }
}
