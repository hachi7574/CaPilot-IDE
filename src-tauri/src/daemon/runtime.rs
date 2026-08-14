//! Daemon runtime paths, instance lock, token and instance identity (§4.1).
//!
//! Layout under the CaPilot base dir (`~/CaPilot`):
//!
//! ```text
//! run/
//!   capilot.sock    Unix socket (0600)
//!   token           random auth token (0600), written by the daemon at startup
//!   daemon.lock     OS advisory exclusive lock — the sole liveness/mutex proof
//!   instance.json   daemon_instance_id + pid + start_ts + protocol_version
//! ```
//!
//! A PID file alone can't be the mutex (stale, PID reuse); the OS exclusive
//! lock on `daemon.lock` is. The PID/instance file is diagnostic and enables
//! identity reconciliation (§6.2).

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::daemon::protocol::PROTOCOL_VERSION;

static NEXT_SOCKET_SUFFIX: AtomicU64 = AtomicU64::new(0);

/// Parent of `run/` — `~/CaPilot` by default.
pub fn daemon_base(base: &Path) -> PathBuf {
    base.to_path_buf()
}

pub fn run_dir(base: &Path) -> PathBuf {
    base.join("run")
}
pub fn socket_path(base: &Path) -> PathBuf {
    run_dir(base).join("capilot.sock")
}
pub fn token_path(base: &Path) -> PathBuf {
    run_dir(base).join("token")
}
pub fn lock_path(base: &Path) -> PathBuf {
    run_dir(base).join("daemon.lock")
}
pub fn instance_path(base: &Path) -> PathBuf {
    run_dir(base).join("instance.json")
}

/// A socket path for an in-test server. Tests run several daemons in parallel,
/// so each needs its own socket name in the same `run/` dir.
pub fn test_socket_path(base: &Path) -> PathBuf {
    let n = NEXT_SOCKET_SUFFIX.fetch_add(1, Ordering::Relaxed);
    run_dir(base).join(format!("capilot-test-{}.sock", n))
}

/// Create `base/run` with `0700` and confirm it is not world/group accessible.
/// The brief requires the socket's parent dir be `0700` (§4.1).
pub fn ensure_run_dir(base: &Path) -> io::Result<PathBuf> {
    let dir = run_dir(base);
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dir)?.permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(&dir, perms)?;
    }
    Ok(dir)
}

/// Generate a 32-hex-char random token (122 bits from uuid v4).
pub fn generate_token() -> String {
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    format!("{}{}", a.simple(), b.simple())[..32].to_string()
}

/// Write the token file with `0600`. Overwrites any previous token — a new
/// daemon invalidates stale clients (the previous daemon is gone by the lock).
pub fn write_token(base: &Path, token: &str) -> io::Result<()> {
    ensure_run_dir(base)?;
    let path = token_path(base);
    let mut f = File::create(&path)?;
    f.write_all(token.as_bytes())?;
    f.write_all(b"\n")?;
    f.flush()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = f.metadata()?.permissions();
        perms.set_mode(0o600);
        f.set_permissions(perms)?;
    }
    Ok(())
}

/// Read the daemon token.
pub fn read_token(base: &Path) -> io::Result<String> {
    let raw = std::fs::read_to_string(token_path(base))?;
    Ok(raw.trim().to_string())
}

/// Instance identity recorded at daemon startup (diagnostic + §6.2 identity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceInfo {
    pub daemon_instance_id: String,
    pub pid: u32,
    pub start_ts: i64,
    pub protocol_version: u32,
}

pub fn write_instance_info(base: &Path, info: &InstanceInfo) -> io::Result<()> {
    ensure_run_dir(base)?;
    let path = instance_path(base);
    let mut f = File::create(&path)?;
    f.write_all(
        serde_json::to_vec_pretty(info)
            .unwrap_or_default()
            .as_slice(),
    )?;
    f.flush()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = f.metadata()?.permissions();
        perms.set_mode(0o600);
        f.set_permissions(perms)?;
    }
    Ok(())
}

pub fn read_instance_info(base: &Path) -> Option<InstanceInfo> {
    let raw = std::fs::read_to_string(instance_path(base)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The OS-level exclusive lock that proves a single daemon owns the PTY set.
/// Dropping it (or process death) releases the lock — stale lock files are
/// harmless because only the flock matters, not the file's existence.
pub struct InstanceLock {
    file: File,
    #[allow(dead_code)] // kept alive for the whole daemon lifetime
    path: PathBuf,
}

impl InstanceLock {
    /// Try to acquire the exclusive instance lock.
    /// - `Ok(Some(lock))`: acquired — we are the only daemon.
    /// - `Ok(None)`: another daemon currently holds it.
    /// - `Err`: the lock file couldn't be opened (e.g. permissions).
    pub fn try_acquire(base: &Path) -> io::Result<Option<Self>> {
        ensure_run_dir(base)?;
        let path = lock_path(base);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&path)?;
        use fs2::FileExt;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self { file, path })),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e),
        }
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Allocate a fresh daemon instance id.
pub fn new_instance_id() -> String {
    format!("d_{}", uuid::Uuid::new_v4().simple())
}

/// Identity record for a freshly started daemon.
pub fn make_instance_info(instance_id: &str) -> InstanceInfo {
    InstanceInfo {
        daemon_instance_id: instance_id.to_string(),
        pid: std::process::id(),
        start_ts: now_ms(),
        protocol_version: PROTOCOL_VERSION,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_base(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "capilot_daemon_runtime_{}_{}_{}",
            tag,
            std::process::id(),
            now_ms()
        ))
    }

    #[test]
    fn run_dir_is_created_private() {
        let base = tmp_base("dir");
        let dir = ensure_run_dir(&base).unwrap();
        assert!(dir.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o077,
                0,
                "run dir must be 0700 (not group/world accessible)"
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn token_roundtrip_and_private_file() {
        let base = tmp_base("tok");
        let t = generate_token();
        assert_eq!(t.len(), 32);
        write_token(&base, &t).unwrap();
        assert_eq!(read_token(&base).unwrap(), t);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(token_path(&base))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "token file must be 0600");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn instance_lock_is_exclusive_and_released_on_drop() {
        let base = tmp_base("lock");
        let a = InstanceLock::try_acquire(&base).unwrap().unwrap();
        // Second acquisition fails while `a` is alive.
        assert!(InstanceLock::try_acquire(&base).unwrap().is_none());
        drop(a);
        // After drop the lock is released (simulates daemon exit).
        assert!(InstanceLock::try_acquire(&base).unwrap().is_some());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn instance_info_roundtrip() {
        let base = tmp_base("info");
        let info = make_instance_info("d_abc");
        write_instance_info(&base, &info).unwrap();
        let back = read_instance_info(&base).unwrap();
        assert_eq!(back.daemon_instance_id, "d_abc");
        assert_eq!(back.pid, std::process::id());
        assert_eq!(back.protocol_version, PROTOCOL_VERSION);
        let _ = std::fs::remove_dir_all(&base);
    }
}
