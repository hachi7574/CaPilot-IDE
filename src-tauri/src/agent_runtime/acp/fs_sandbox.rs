//! Path sandbox for ACP client `fs/*` methods.
//!
//! All paths must resolve (after symlink canonicalization) under the session
//! `cwd` root. Writes are disabled in MVP regardless of path.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Max bytes returned by `fs/read_text_file` (defense in depth).
pub const MAX_READ_BYTES: u64 = 2 * 1024 * 1024; // 2 MiB

#[derive(Debug, thiserror::Error)]
pub enum FsSandboxError {
    #[error("path must be absolute: {0}")]
    NotAbsolute(String),
    #[error("path escapes session cwd: {0}")]
    OutsideRoot(String),
    #[error("path not found: {0}")]
    NotFound(String),
    #[error("not a regular file: {0}")]
    NotFile(String),
    #[error("file too large (>{MAX_READ_BYTES} bytes): {0}")]
    TooLarge(String),
    #[error("fs write is disabled")]
    WriteDisabled,
    #[error("io: {0}")]
    Io(#[from] io::Error),
}

/// Canonicalize `requested` and ensure it stays under `root` (also canonicalized).
///
/// - Relative paths are rejected (ACP requires absolute paths).
/// - Symlinks are resolved via `canonicalize` on existing components; for a path
///   whose final component does not yet exist, the parent is canonicalized and
///   the file name is re-joined (write path validation — still rejected by policy).
pub fn resolve_under_root(root: &Path, requested: &str) -> Result<PathBuf, FsSandboxError> {
    let req = PathBuf::from(requested);
    if !req.is_absolute() {
        return Err(FsSandboxError::NotAbsolute(requested.to_string()));
    }

    let root_canon = fs::canonicalize(root).map_err(|e| {
        FsSandboxError::Io(io::Error::new(
            e.kind(),
            format!("canonicalize session cwd {}: {e}", root.display()),
        ))
    })?;

    let resolved = if req.exists() {
        fs::canonicalize(&req)?
    } else {
        // Resolve parent + join name so we still catch symlink parents.
        let parent = req.parent().ok_or_else(|| {
            FsSandboxError::OutsideRoot(requested.to_string())
        })?;
        let name = req.file_name().ok_or_else(|| {
            FsSandboxError::OutsideRoot(requested.to_string())
        })?;
        if !parent.exists() {
            return Err(FsSandboxError::NotFound(requested.to_string()));
        }
        let parent_canon = fs::canonicalize(parent)?;
        parent_canon.join(name)
    };

    if !resolved.starts_with(&root_canon) {
        return Err(FsSandboxError::OutsideRoot(requested.to_string()));
    }
    Ok(resolved)
}

/// Read a UTF-8 (lossy) text file under `root`. Enforces size cap.
pub fn read_text_file(root: &Path, path: &str, line: Option<u32>, limit: Option<u32>) -> Result<String, FsSandboxError> {
    let resolved = resolve_under_root(root, path)?;
    let meta = fs::metadata(&resolved)?;
    if !meta.is_file() {
        return Err(FsSandboxError::NotFile(path.to_string()));
    }
    if meta.len() > MAX_READ_BYTES {
        return Err(FsSandboxError::TooLarge(path.to_string()));
    }
    let content = fs::read_to_string(&resolved).or_else(|_| {
        // Lossy fallback for non-UTF8.
        fs::read(&resolved).map(|b| String::from_utf8_lossy(&b).into_owned())
    })?;

    // Optional line slice (1-based start, `limit` lines) — ACP optional params.
    if line.is_some() || limit.is_some() {
        let start = line.unwrap_or(1).max(1) as usize;
        let lines: Vec<&str> = content.lines().collect();
        if start > lines.len() {
            return Ok(String::new());
        }
        let end = match limit {
            Some(n) => (start - 1 + n as usize).min(lines.len()),
            None => lines.len(),
        };
        Ok(lines[start - 1..end].join("\n"))
    } else {
        Ok(content)
    }
}

/// MVP: always reject writes.
pub fn write_text_file(_root: &Path, _path: &str, _content: &str) -> Result<(), FsSandboxError> {
    Err(FsSandboxError::WriteDisabled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "acp-fs-sandbox-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rejects_relative_path() {
        let root = tmp_root();
        let err = resolve_under_root(&root, "relative.txt").unwrap_err();
        assert!(matches!(err, FsSandboxError::NotAbsolute(_)));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn allows_file_under_root() {
        let root = tmp_root();
        let f = root.join("hello.txt");
        fs::write(&f, "hi").unwrap();
        let abs = f.to_string_lossy().to_string();
        let got = resolve_under_root(&root, &abs).unwrap();
        assert_eq!(got, fs::canonicalize(&f).unwrap());
        let text = read_text_file(&root, &abs, None, None).unwrap();
        assert_eq!(text, "hi");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_path_outside_root() {
        let root = tmp_root();
        let outside = std::env::temp_dir().join(format!(
            "acp-fs-outside-{}",
            std::process::id()
        ));
        fs::write(&outside, "x").unwrap();
        let abs = outside.to_string_lossy().to_string();
        let err = resolve_under_root(&root, &abs).unwrap_err();
        assert!(matches!(err, FsSandboxError::OutsideRoot(_)));
        let _ = fs::remove_file(&outside);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_symlink_escape() {
        let root = tmp_root();
        let outside = std::env::temp_dir().join(format!(
            "acp-fs-sym-target-{}",
            std::process::id()
        ));
        fs::write(&outside, "secret").unwrap();
        let link = root.join("escape");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            let abs = link.to_string_lossy().to_string();
            let err = resolve_under_root(&root, &abs).unwrap_err();
            assert!(
                matches!(err, FsSandboxError::OutsideRoot(_)),
                "expected OutsideRoot, got {err:?}"
            );
        }
        let _ = fs::remove_file(&outside);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn write_always_disabled() {
        let root = tmp_root();
        let f = root.join("w.txt");
        let abs = f.to_string_lossy().to_string();
        let err = write_text_file(&root, &abs, "nope").unwrap_err();
        assert!(matches!(err, FsSandboxError::WriteDisabled));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn line_limit_slice() {
        let root = tmp_root();
        let f = root.join("lines.txt");
        fs::write(&f, "a\nb\nc\nd\n").unwrap();
        let abs = f.to_string_lossy().to_string();
        let got = read_text_file(&root, &abs, Some(2), Some(2)).unwrap();
        assert_eq!(got, "b\nc");
        let _ = fs::remove_dir_all(&root);
    }
}
