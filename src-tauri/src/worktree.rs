//! Git worktree isolation primitives.
//!
//! Every operation here shells out to `git` through [`crate::git_gate`] so the
//! concurrency/rate gates still apply. The standard `git_gate::run` validates the
//! *source repo* root (which is a CaPilot project root); worktree targets live
//! OUTSIDE the project-root allow-list (sibling dirs of the repo), so path
//! safety is enforced here instead:
//!
//! - branch names go through [`crate::validate_branch_name`] (same hard rules as
//!   the Git panel);
//! - worktree directory names are sanitized (`[A-Za-z0-9._-]` only), never
//!   contain `/` or a `..` segment, and the final path is checked to stay under
//!   the repo's parent dir;
//! - the candidate loop re-checks live `git worktree list` state, so a name that
//!   races with another creator is skipped rather than corrupted.

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::git_gate;

/// One entry from `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktreeInfo {
    pub path: PathBuf,
    /// Checked-out branch name (`None` = detached HEAD).
    pub branch: Option<String>,
}

/// Outcome of a successful [`create_worktree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedWorktree {
    pub branch: String,
    pub path: PathBuf,
    /// The ref the new branch forks from (`None` when git's default HEAD was used).
    pub base_ref: Option<String>,
}

/// Max candidate suffix iterations before giving up (`foo-2` … `foo-100`).
const MAX_CANDIDATES: usize = 100;

// ── git primitives ───────────────────────────────────────────────

/// List the repo's existing worktrees (path + checked-out branch). Includes the
/// main worktree. Used for dedup and startup reconciliation.
pub fn list_worktrees_in(repo: &Path) -> Result<Vec<GitWorktreeInfo>, String> {
    let out = run_in_repo(repo, &["worktree", "list", "--porcelain"])?;
    if !out.status.success() {
        return Err(format!(
            "git worktree list 失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(parse_worktree_list(&String::from_utf8_lossy(&out.stdout)))
}

/// Create a NEW branch `branch` in `repo` and check it out in a fresh worktree
/// at `path`, forked from `base` (or the repo's HEAD when `base` is `None`).
/// `--no-track` keeps the new branch from auto-tracking `base`.
pub fn add_worktree(
    repo: &Path,
    branch: &str,
    path: &Path,
    base: Option<&str>,
) -> Result<Output, String> {
    let path_str = path.to_string_lossy();
    let mut args: Vec<&str> = vec!["worktree", "add", "--no-track", "-b", branch, &path_str];
    if let Some(base) = base {
        args.push(base);
    }
    run_in_repo(repo, &args)
}

/// Check out an EXISTING local branch in a new worktree at `path`. The caller
/// must have verified the branch is not already checked out by another worktree
/// (git refuses a second checkout of the same branch).
pub fn add_worktree_existing(repo: &Path, branch: &str, path: &Path) -> Result<Output, String> {
    let path_str = path.to_string_lossy();
    run_in_repo(repo, &["worktree", "add", &path_str, branch])
}

/// Remove a worktree directory and its git metadata via `git worktree remove`
/// (NEVER a raw `rm -rf` — that would leave stale `.git/worktrees/<name>`
/// entries). Runs with `-C <path>` so it works even when the path is not a
/// CaPilot-registered project root. `force` allows removal despite uncommitted
/// changes / untracked files (the normal state of an AI worktree being dropped).
pub fn remove_worktree(path: &Path, force: bool) -> Result<(), String> {
    let path_str = path.to_string_lossy();
    let args: Vec<&str> = if force {
        vec!["worktree", "remove", "--force", &path_str]
    } else {
        vec!["worktree", "remove", &path_str]
    };
    let out = git_gate::run_raw(path, &args)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git worktree remove 失败: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Resolve the repo's default fork base. Prefers `origin/HEAD` (what a fresh
/// clone would check out), then the current branch, then any local branch.
/// Fails only for an empty repo with no remote — the caller reports that clearly.
pub fn resolve_default_branch(repo: &Path) -> Result<String, String> {
    // 1. `git symbolic-ref refs/remotes/origin/HEAD` → "refs/remotes/origin/main".
    let out = run_in_repo(repo, &["symbolic-ref", "refs/remotes/origin/HEAD"])?;
    if out.status.success() {
        let refname = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Some(branch) = refname.rsplit('/').next() {
            if !branch.is_empty() && branch != "HEAD" {
                return Ok(branch.to_string());
            }
        }
    }
    // 2. Current branch (a clone with no origin/HEAD still knows its HEAD).
    let out = run_in_repo(repo, &["branch", "--show-current"])?;
    if out.status.success() {
        let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !branch.is_empty() {
            return Ok(branch);
        }
    }
    // 3. Any local branch.
    let out = run_in_repo(repo, &["for-each-ref", "--format=%(refname:short)", "refs/heads"])?;
    if out.status.success() {
        if let Some(first) = String::from_utf8_lossy(&out.stdout).lines().next() {
            if !first.trim().is_empty() {
                return Ok(first.trim().to_string());
            }
        }
    }
    Err("无法探测默认分支：仓库为空或没有可用分支".to_string())
}

/// Enable `git push` to auto-create the remote branch on first push
/// (`push.autoSetupRemote true`), so a new worktree branch can be published with
/// a plain `git push`.
pub fn set_auto_setup_remote(repo: &Path) {
    let _ = run_in_repo(repo, &["config", "push.autoSetupRemote", "true"]);
}

// ── naming / sanitization ────────────────────────────────────────

/// Replace every character outside `[A-Za-z0-9._-]` with `-`, then trim leading
/// and trailing `-`/`.` so the result is a safe single path segment and a legal
/// branch name (`.` and `-` cannot lead a git ref).
pub fn sanitize_workspace_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    cleaned.trim_matches(|c| c == '-' || c == '.').to_string()
}

/// Reject a sanitized workspace name that could escape the intended directory:
/// empty, a bare `.`/`..`, any `..` sequence, or a `.`-prefixed (hidden) entry.
fn validate_workspace_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("工作区名称净化后为空".to_string());
    }
    if name == "." || name == ".." || name.contains("..") {
        return Err("工作区名称包含非法路径段".to_string());
    }
    if name.starts_with('.') {
        return Err("工作区名称不能以 . 开头".to_string());
    }
    if name.contains('/') {
        return Err("工作区名称不能包含 /".to_string());
    }
    Ok(())
}

/// Default worktree location: a sibling of the repo root
/// (`<repo_parent>/<repo_name>-<name>`), so the worktree is NOT inside a
/// git-tracked directory. The path is derived from the repo's canonical parent
/// and the single validated `name` segment, then re-checked to stay under the
/// parent (belt-and-braces against traversal).
pub fn compute_worktree_path(repo_root: &Path, name: &str) -> Result<PathBuf, String> {
    validate_workspace_name(name)?;
    let parent = repo_root
        .parent()
        .ok_or_else(|| "仓库根没有父目录".to_string())?
        .canonicalize()
        .map_err(|e| format!("无法解析仓库父目录: {e}"))?;
    let repo_name = repo_root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    let dir_name = format!("{repo_name}-{name}");
    let path = parent.join(&dir_name);
    if !path.starts_with(&parent) {
        return Err("工作区路径越界".to_string());
    }
    Ok(path)
}

// ── candidate loop ───────────────────────────────────────────────

/// Create a worktree for `name` in `repo`, deduping branch + path against live
/// `git worktree list` state. Tries `name`, `name-2`, `name-3`, … (≤ 100) and
/// returns the first (branch, path) pair it can actually check out.
///
/// - branch already exists but is FREE (not checked out anywhere) → reused via
///   [`add_worktree_existing`];
/// - branch exists and is checked out by another worktree → next candidate;
/// - path already exists → next candidate;
/// - git refuses (unwritable path, unborn HEAD, …) → next candidate.
pub fn create_worktree(
    repo: &Path,
    name: &str,
    base: Option<&str>,
) -> Result<CreatedWorktree, String> {
    let branch_base = sanitize_workspace_name(name);
    if branch_base.is_empty() {
        return Err("工作区名称净化后为空".to_string());
    }
    crate::validate_branch_name(&branch_base)?;
    validate_workspace_name(&branch_base)?;

    let repo_root = repo
        .canonicalize()
        .map_err(|e| format!("无效仓库路径: {e}"))?;
    let base_ref = match base {
        Some(b) if !b.trim().is_empty() => {
            // An EXPLICIT base must resolve — a typo'd ref would otherwise burn
            // all 100 candidates on the same failing `git worktree add`.
            let b = b.trim();
            if !ref_exists(&repo_root, b) {
                return Err(format!("分叉基点不存在: {b}"));
            }
            Some(b.to_string())
        }
        _ => match resolve_default_branch(&repo_root) {
            Ok(b) => Some(b),
            Err(e) => {
                log::warn!("resolve_default_branch({}): {e}", repo_root.display());
                None
            }
        },
    };

    for i in 1..=MAX_CANDIDATES {
        let candidate = if i == 1 {
            branch_base.clone()
        } else {
            format!("{branch_base}-{i}")
        };
        let path = match compute_worktree_path(&repo_root, &candidate) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("compute_worktree_path({candidate}): {e}");
                continue;
            }
        };
        if path.exists() {
            continue;
        }
        if branch_in_use(&repo_root, &candidate) {
            continue;
        }
        let branch_free_but_exists = branch_exists(&repo_root, &candidate);
        let result = if branch_free_but_exists {
            add_worktree_existing(&repo_root, &candidate, &path)
        } else {
            add_worktree(&repo_root, &candidate, &path, base_ref.as_deref())
        };
        match result {
            Ok(out) if out.status.success() => {
                set_auto_setup_remote(&repo_root);
                return Ok(CreatedWorktree {
                    branch: candidate,
                    path,
                    base_ref: base_ref.clone(),
                });
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                log::warn!("worktree add {candidate} 失败: {}", err.trim());
                if path.exists() {
                    // Best-effort rollback of the partial dir git may have left.
                    let _ = remove_worktree(&path, true);
                }
            }
            Err(e) => {
                log::warn!("worktree add {candidate} 启动失败: {e}");
            }
        }
    }
    Err("无法创建隔离工作区：候选名全部被占用或创建失败".to_string())
}

/// True when `branch` is checked out by ANY worktree (main included) — git
/// forbids a second worktree on the same branch.
fn branch_in_use(repo: &Path, branch: &str) -> bool {
    list_worktrees_in(repo)
        .map(|wts| wts.iter().any(|wt| wt.branch.as_deref() == Some(branch)))
        .unwrap_or(false)
}

/// True when a local branch named `branch` exists.
fn branch_exists(repo: &Path, branch: &str) -> bool {
    run_in_repo(repo, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// True when `ref_name` (any ref form: `main`, `origin/main`, `v1.0`, …)
/// resolves to a commit.
fn ref_exists(repo: &Path, ref_name: &str) -> bool {
    run_in_repo(repo, &["rev-parse", "--verify", "--quiet", ref_name])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ── helpers ──────────────────────────────────────────────────────

fn run_in_repo(repo: &Path, args: &[&str]) -> Result<Output, String> {
    git_gate::run(&repo.to_string_lossy(), args)
}

/// Parse `git worktree list --porcelain`:
/// ```text
/// worktree /abs/path
/// HEAD <sha>
/// branch refs/heads/<name>
///
/// worktree /abs/other
/// HEAD <sha>
/// detached
/// ```
fn parse_worktree_list(text: &str) -> Vec<GitWorktreeInfo> {
    let mut entries = Vec::new();
    let mut current: Option<GitWorktreeInfo> = None;
    for line in text.lines() {
        if line.trim().is_empty() {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(GitWorktreeInfo {
                path: PathBuf::from(path.trim()),
                branch: None,
            });
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            if let Some(entry) = current.as_mut() {
                entry.branch = Some(branch.trim().to_string());
            }
        } else if line.starts_with("detached") {
            if let Some(entry) = current.as_mut() {
                entry.branch = None;
            }
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    // ── temp-repo helpers ─────────────────────────────────────────

    /// Create a temp dir with a real git repo and one commit on `main`. The
    /// guard removes the whole tree on drop (even mid-test panics).
    fn temp_repo() -> (TempDirGuard, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "capilot-wt-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        run_git(&dir, &["init", "-q", "-b", "main"]);
        run_git(&dir, &["config", "user.email", "test@example.com"]);
        run_git(&dir, &["config", "user.name", "Test"]);
        std::fs::write(dir.join("README.md"), "hi").unwrap();
        run_git(&dir, &["add", "."]);
        run_git(&dir, &["commit", "-qm", "initial"]);
        (TempDirGuard(dir.clone()), dir)
    }

    struct TempDirGuard(PathBuf);
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn run_git(dir: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git should run")
    }

    // ── sanitize / path ───────────────────────────────────────────

    #[test]
    fn sanitize_replaces_invalid_and_trims() {
        assert_eq!(sanitize_workspace_name("My Feature!"), "My-Feature");
        assert_eq!(sanitize_workspace_name("  foo  "), "foo");
        assert_eq!(sanitize_workspace_name("---bar---"), "bar");
        assert_eq!(sanitize_workspace_name("a/b/c"), "a-b-c");
        assert_eq!(sanitize_workspace_name("中文"), ""); // non-ascii → '-' ×3, then trimmed → empty (invalid name)
        assert_eq!(sanitize_workspace_name("foo.bar"), "foo.bar");
    }

    #[test]
    fn validate_workspace_name_rejects_traversal() {
        assert!(validate_workspace_name("foo").is_ok());
        assert!(validate_workspace_name("foo-bar_2.x").is_ok());
        assert!(validate_workspace_name("").is_err());
        assert!(validate_workspace_name("..").is_err());
        assert!(validate_workspace_name(".").is_err());
        assert!(validate_workspace_name("a..b").is_err());
        assert!(validate_workspace_name(".hidden").is_err());
    }

    #[test]
    fn compute_worktree_path_is_sibling_and_under_parent() {
        let (_guard, repo) = temp_repo();
        let parent = repo.parent().unwrap();
        let path = compute_worktree_path(&repo, "foo").unwrap();
        assert!(path.starts_with(parent));
        assert_eq!(path.file_name().unwrap().to_str().unwrap(), format!("{}-foo", repo.file_name().unwrap().to_str().unwrap()));
    }

    // ── porcelain parsing ─────────────────────────────────────────

    #[test]
    fn parses_worktree_list_porcelain() {
        let text = "worktree /main\nHEAD 1111111\nbranch refs/heads/main\n\nworktree /wt/foo\nHEAD 2222222\nbranch refs/heads/foo\n\nworktree /wt/detached\nHEAD 3333333\ndetached\n";
        let entries = parse_worktree_list(text);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].path, PathBuf::from("/main"));
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert_eq!(entries[1].branch.as_deref(), Some("foo"));
        assert_eq!(entries[2].branch, None);
    }

    // ── integration: real git ─────────────────────────────────────

    #[test]
    fn list_includes_main_worktree() {
        let (_guard, repo) = temp_repo();
        let entries = list_worktrees_in(&repo).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
    }

    #[test]
    fn resolve_default_branch_falls_back_to_local_main() {
        let (_guard, repo) = temp_repo();
        // No remote configured → origin/HEAD missing → falls back to current
        // local branch "main".
        assert_eq!(resolve_default_branch(&repo).unwrap(), "main");
    }

    #[test]
    fn resolve_default_branch_prefers_origin_head() {
        let (_guard, repo) = temp_repo();
        // Simulate origin/HEAD pointing at main (what `git clone` leaves behind).
        run_git(&repo, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]);
        // Also set up origin/main so the ref exists.
        run_git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        assert_eq!(resolve_default_branch(&repo).unwrap(), "main");
    }

    #[test]
    fn create_worktree_makes_new_branch_and_dir() {
        let (_guard, repo) = temp_repo();
        let created = create_worktree(&repo, "feature-x", None).unwrap();
        assert_eq!(created.branch, "feature-x");
        assert!(created.path.exists());
        assert_eq!(created.base_ref.as_deref(), Some("main"));
        // `git worktree list` now sees both.
        let entries = list_worktrees_in(&repo).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.branch.as_deref() == Some("feature-x")));
        // The worktree's own checked-out branch matches.
        let head = run_git(&created.path, &["branch", "--show-current"]);
        assert_eq!(String::from_utf8_lossy(&head.stdout).trim(), "feature-x");
    }

    #[test]
    fn create_worktree_increments_suffix_when_branch_taken() {
        let (_guard, repo) = temp_repo();
        // Take "feature-x" first.
        let first = create_worktree(&repo, "feature-x", None).unwrap();
        assert_eq!(first.branch, "feature-x");
        // Second identical name → branch is now checked out → `feature-x-2`.
        let second = create_worktree(&repo, "feature-x", None).unwrap();
        assert_eq!(second.branch, "feature-x-2");
        assert!(second.path.exists());
        // Both dirs distinct.
        assert_ne!(first.path, second.path);
    }

    #[test]
    fn create_worktree_reuses_free_existing_branch() {
        let (_guard, repo) = temp_repo();
        // Create a branch without checking it out anywhere.
        run_git(&repo, &["branch", "ghost"]);
        let created = create_worktree(&repo, "ghost", None).unwrap();
        assert_eq!(created.branch, "ghost");
        // Branch was reused, not re-created with a suffix.
        assert_eq!(created.path.file_name().unwrap().to_str().unwrap(), format!("{}-ghost", repo.file_name().unwrap().to_str().unwrap()));
    }

    #[test]
    fn create_worktree_rejects_missing_explicit_base() {
        let (_guard, repo) = temp_repo();
        let err = create_worktree(&repo, "feature", Some("no-such-ref")).unwrap_err();
        assert!(err.contains("分叉基点不存在"), "{err}");
    }

    #[test]
    fn create_worktree_honors_explicit_base() {
        let (_guard, repo) = temp_repo();
        // A second branch to fork from.
        run_git(&repo, &["branch", "release"]);
        let created = create_worktree(&repo, "hotfix", Some("release")).unwrap();
        assert_eq!(created.base_ref.as_deref(), Some("release"));
    }

    #[test]
    fn remove_worktree_cleans_git_metadata() {
        let (_guard, repo) = temp_repo();
        let created = create_worktree(&repo, "doomed", None).unwrap();
        assert_eq!(list_worktrees_in(&repo).unwrap().len(), 2);
        remove_worktree(&created.path, true).unwrap();
        assert!(!created.path.exists());
        let entries = list_worktrees_in(&repo).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
    }
}
