//! Best-effort whole-tree process teardown.
//!
//! Closing a PTY session or timing out a CLI probe must not leave orphan
//! grandchildren (node wrappers, agent subprocesses, shells started with
//! `cmd /c`). portable_pty's `Child::kill` only targets the direct child
//! (`TerminateProcess` on Windows, SIGHUP/SIGKILL on Unix) — descendants
//! keep running.
//!
//! - **Windows**: `taskkill /PID <pid> /T /F` (tree kill). Hidden console via
//!   `CREATE_NO_WINDOW` so a desktop GUI never flashes a console window.
//! - **Unix**: `kill(-pid, SIGKILL)` for the process group when the child is
//!   a session/group leader (PTY slaves and probes that called `setsid`),
//!   then `kill(pid, SIGKILL)` as a fallback for the root itself.

/// Kill `pid` and every descendant best-effort. No-op for pid 0 / unknown.
/// Never returns an error to callers — teardown is best-effort and must not
/// block session cleanup when the OS already reaped the tree.
pub fn kill_process_tree(pid: u32) {
    if pid == 0 {
        return;
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        // CREATE_NO_WINDOW: avoid a brief console flash when CaPilot is a GUI app.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut cmd = Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        let _ = cmd.status();
    }

    #[cfg(unix)]
    {
        let pid_i = pid as i32;
        if pid_i > 0 {
            unsafe {
                // Negative pid = process group. Harmless ESRCH when the child is
                // not a group leader; the direct kill below still runs.
                libc::kill(-pid_i, libc::SIGKILL);
                libc::kill(pid_i, libc::SIGKILL);
            }
        }
    }
}
