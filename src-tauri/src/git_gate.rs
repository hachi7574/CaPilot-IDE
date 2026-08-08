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
    let workspace = crate::persistence::workspace_root();
    let mut roots = workspace.canonicalize().into_iter().collect::<Vec<_>>();
    if let Ok(entries) = std::fs::read_dir(&workspace) {
        for entry in entries.flatten().filter(|entry| entry.path().is_dir()) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(root) = crate::persistence::custom_project_root(&name) {
                if let Ok(root) = root.canonicalize() {
                    roots.push(root);
                }
            }
        }
    }
    if let Some(capilot) = workspace.parent() {
        roots.extend(capilot.join("Master").canonicalize());
    }
    roots
}

pub fn validate_repo(repo: &str) -> Result<PathBuf, String> {
    let resolved = Path::new(repo)
        .canonicalize()
        .map_err(|error| format!("Invalid repo path: {error}"))?;
    #[cfg(test)]
    let test_root_allowed = resolved.starts_with(std::env::temp_dir());
    #[cfg(not(test))]
    let test_root_allowed = false;
    if !resolved.is_dir()
        || (!test_root_allowed && !allowed_roots().iter().any(|root| resolved.starts_with(root)))
    {
        return Err("repo path is outside CaPilot project roots".to_string());
    }
    Ok(resolved)
}

fn wait_for_rate_slot() {
    loop {
        let now = Instant::now();
        let mut starts = GATE.starts.lock().unwrap();
        while starts.front().is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(1)) {
            starts.pop_front();
        }
        if starts.len() < MAX_STARTS_PER_SECOND {
            starts.push_back(now);
            return;
        }
        let wait = Duration::from_secs(1).saturating_sub(now.duration_since(*starts.front().unwrap()));
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
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|error| format!("git failed: {error}"))
}

pub fn clone_into(url: &str, target: &Path) -> Result<Output, String> {
    let parent = target.parent().ok_or_else(|| "clone target has no parent".to_string())?;
    let parent = parent.canonicalize().map_err(|error| format!("invalid clone parent: {error}"))?;
    let home = std::env::var("HOME").map(PathBuf::from).map_err(|_| "HOME not set".to_string())?;
    if !parent.is_dir() || !parent.starts_with(home) {
        return Err("clone target is outside the user home".to_string());
    }
    let name = target.file_name().ok_or_else(|| "invalid clone target".to_string())?;
    let _permit = acquire();
    wait_for_rate_slot();
    Command::new("git")
        .arg("clone")
        .arg("--")
        .arg(url)
        .arg(parent.join(name))
        .output()
        .map_err(|error| format!("git failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_limits_are_frozen() {
        assert_eq!(MAX_CONCURRENT, 8);
        assert_eq!(MAX_STARTS_PER_SECOND, 64);
    }
}
