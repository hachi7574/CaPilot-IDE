//! Central gate for git subprocesses: project-root allow-listing plus bounded
//! concurrency and start rate. Uses tokio's existing Semaphore dependency.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

const MAX_CONCURRENT: usize = 8;
const MAX_STARTS_PER_SECOND: usize = 64;

struct GitGate {
    permits: Semaphore,
    starts: Mutex<VecDeque<Instant>>,
}

static GATE: LazyLock<GitGate> = LazyLock::new(|| GitGate {
    permits: Semaphore::new(MAX_CONCURRENT),
    starts: Mutex::new(VecDeque::new()),
});

fn allowed_roots() -> Vec<PathBuf> {
    // Workspace layout + every registered custom project root. Intentionally
    // narrower than `path_is_allowed` (which also admits all of $HOME) — git
    // ops should stay scoped to CaPilot projects, not arbitrary home paths.
    let workspace = crate::persistence::workspace_root();
    let mut roots = workspace.canonicalize().into_iter().collect::<Vec<_>>();
    if let Ok(entries) = std::fs::read_dir(&workspace) {
        for entry in entries.flatten().filter(|entry| entry.path().is_dir()) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(root) = crate::persistence::custom_project_root(&name) {
                // Keep non-canonical form as a fallback so a briefly-missing
                // folder still prefix-matches (mirrors path_is_allowed).
                let root = root.canonicalize().unwrap_or(root);
                if !roots
                    .iter()
                    .any(|r| crate::persistence::path_is_within(&root, r)
                        && crate::persistence::path_is_within(r, &root))
                {
                    roots.push(root);
                }
            }
        }
    }
    roots
}

pub fn validate_repo(repo: &str) -> Result<PathBuf, String> {
    let resolved = Path::new(repo)
        .canonicalize()
        .map_err(|error| format!("Invalid repo path: {error}"))?;
    #[cfg(test)]
    let test_root_allowed =
        crate::persistence::path_is_within(&resolved, &std::env::temp_dir());
    #[cfg(not(test))]
    let test_root_allowed = false;
    if !resolved.is_dir()
        || (!test_root_allowed
            && !allowed_roots()
                .iter()
                .any(|root| crate::persistence::path_is_within(&resolved, root)))
    {
        return Err("repo path is outside CaPilot project roots".to_string());
    }
    Ok(resolved)
}

fn wait_for_rate_slot() {
    loop {
        let now = Instant::now();
        let mut starts = GATE.starts.lock().unwrap_or_else(|p| p.into_inner());
        while starts
            .front()
            .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(1))
        {
            starts.pop_front();
        }
        if starts.len() < MAX_STARTS_PER_SECOND {
            starts.push_back(now);
            return;
        }
        let wait =
            Duration::from_secs(1).saturating_sub(now.duration_since(*starts.front().unwrap()));
        drop(starts);
        std::thread::sleep(wait.min(Duration::from_millis(20)));
    }
}

fn acquire() -> tokio::sync::SemaphorePermit<'static> {
    loop {
        if let Ok(permit) = GATE.permits.try_acquire() {
            return permit;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub fn run(repo: &str, args: &[&str]) -> Result<Output, String> {
    let repo = validate_repo(repo)?;
    let _permit = acquire();
    wait_for_rate_slot();
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    // Git panel / status polls fire often; without CREATE_NO_WINDOW a GUI host
    // flashes an empty console for every `git status` / `git log`.
    crate::agent_runtime::executable::hide_windows_console(&mut cmd);
    cmd.output()
        .map_err(|error| format!("git failed: {error}"))
}

/// Run git in `path` with the same concurrency/rate gates as [`run`] but WITHOUT
/// the project-root allow-list. Reserved for targets that live outside the
/// standard roots by design — git worktree paths (siblings of a repo root) are
/// not in `allowed_roots()`. The caller MUST guarantee `path` was derived from a
/// validated repo root / registered worktree (see `worktree.rs`); this never
/// validates user-supplied paths on its own.
pub fn run_raw(path: &Path, args: &[&str]) -> Result<Output, String> {
    let _permit = acquire();
    wait_for_rate_slot();
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(path).args(args);
    crate::agent_runtime::executable::hide_windows_console(&mut cmd);
    cmd.output()
        .map_err(|error| format!("git failed: {error}"))
}

/// How long a background `git clone` may run before we kill it and surface a
/// timeout. Private-repo auth via Git Credential Manager is interactive and can
/// stall forever when the helper cannot show a prompt (GUI app, no TTY); the
/// timeout turns that into a clear error instead of a permanent "正在克隆中".
pub const CLONE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Clone `url` into `target` (which must not already exist). Disables the TTY
/// password prompt (`GIT_TERMINAL_PROMPT=0`), closes stdin, hides the Windows
/// console, and enforces [`CLONE_TIMEOUT`]. Credential Manager may still open a
/// GUI prompt when it can; if auth never completes the timeout fires. The
/// caller is responsible for removing a partial target on failure.
pub fn clone_into(url: &str, target: &Path) -> Result<Output, String> {
    use std::io::Read;
    use std::process::Stdio;

    let parent = target
        .parent()
        .ok_or_else(|| "clone target has no parent".to_string())?;
    let parent = parent
        .canonicalize()
        .map_err(|error| format!("invalid clone parent: {error}"))?;
    if !parent.is_dir() || !crate::persistence::path_is_allowed(&parent)? {
        return Err("clone target is not an allowed directory".to_string());
    }
    let name = target
        .file_name()
        .ok_or_else(|| "invalid clone target".to_string())?;
    // Windows canonicalize() yields `\\?\B:\...`. Git for Windows rejects that
    // prefix when creating the work tree ("Invalid argument"). Strip it so the
    // path we hand to `git clone` is a plain drive path.
    let dest = crate::persistence::strip_verbatim_prefix(&parent).join(name);
    let _permit = acquire();
    wait_for_rate_slot();

    let mut cmd = Command::new("git");
    // Never block on a hidden terminal password prompt. GCM may still pop a
    // GUI when credentials are missing; the timeout below is the backstop.
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .arg("clone")
        .arg("--")
        .arg(url)
        .arg(&dest);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = cmd
        .spawn()
        .map_err(|error| format!("git failed: {error}"))?;

    // Drain pipes on side threads so a chatty git/helper can't fill the OS
    // pipe buffer and deadlock while we poll for exit / timeout.
    let mut stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| "git stdout closed".to_string())?;
    let mut stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| "git stderr closed".to_string())?;
    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + CLONE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                // Best-effort kill. Pipe readers unblock once git dies; any
                // credential-helper grandchild may linger briefly.
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();
                return Err(format!(
                    "git clone 超时（超过 {} 秒）。若仓库为私有，请先在终端完成 GitHub 登录（gh auth login / git credential），再重试",
                    CLONE_TIMEOUT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => {
                let _ = child.kill();
                let _ = stdout_handle.join();
                let _ = stderr_handle.join();
                return Err(format!("git failed: {error}"));
            }
        }
    };

    let stdout = stdout_handle.join().unwrap_or_default();
    let stderr = stderr_handle.join().unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_limits_are_frozen() {
        assert_eq!(MAX_CONCURRENT, 8);
        assert_eq!(MAX_STARTS_PER_SECOND, 64);
    }

    #[test]
    fn clone_timeout_is_generous_but_finite() {
        // 10 minutes: long enough for a large repo over a slow link, short
        // enough that a hung credential prompt doesn't look like "forever".
        assert_eq!(CLONE_TIMEOUT, Duration::from_secs(600));
    }

    #[test]
    fn clone_into_rejects_forbidden_system_path() {
        // System dirs stay off-limits on every platform we ship.
        let target = std::path::Path::new(if cfg!(windows) {
            r"C:\Windows\Temp\capilot-clone-should-fail\repo"
        } else {
            "/etc/capilot-clone-should-fail/repo"
        });
        let err = clone_into("https://example.com/repo.git", target).unwrap_err();
        assert!(
            err.contains("not an allowed") || err.contains("invalid clone parent"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn strip_verbatim_prefix_turns_extended_path_into_drive_path() {
        // Regression: git clone failed with
        //   fatal: could not create work tree dir '\\?\B:\...': Invalid argument
        // because canonicalize() on Windows emits the \\?\ prefix.
        let stripped = crate::persistence::strip_verbatim_prefix(std::path::Path::new(
            r"\\?\B:\capilot_ide_git_test",
        ));
        assert_eq!(stripped, std::path::Path::new(r"B:\capilot_ide_git_test"));
        let stripped_unc = crate::persistence::strip_verbatim_prefix(std::path::Path::new(
            r"\\?\UNC\server\share\repo",
        ));
        assert_eq!(
            stripped_unc,
            std::path::Path::new(r"\\server\share\repo")
        );
    }
}
