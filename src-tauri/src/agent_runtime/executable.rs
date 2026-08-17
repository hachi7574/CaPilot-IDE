//! Windows-aware CLI executable resolution (Paseo-style).
//!
//! Agent CLIs installed via npm/pnpm on Windows are almost always `name.cmd`
//! shims, not bare PE binaries. ConPTY / `CreateProcess` (what `portable_pty`
//! uses) does **not** apply `PATHEXT` and cannot launch `.cmd`/`.bat` directly —
//! so detection and PTY spawn must:
//!
//! 1. Resolve a bare name to a real path (`claude` → `…\claude.cmd`)
//! 2. Wrap script shims as `cmd.exe /d /s /c "…"` for both probes and PTY
//!
//! Unix keeps the simple PATH lookup; the helpers are no-ops beyond that.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use super::adapter::{ensure_cli_path, run_cmd_timeout, CLI_PROBE_TIMEOUT};

/// A CLI binary located on disk (or a bare name that already works on PATH).
#[derive(Debug, Clone)]
pub struct ResolvedExe {
    pub path: PathBuf,
    /// True when CreateProcess/PTY cannot exec this path directly (`.cmd`/`.bat`).
    pub needs_cmd_wrap: bool,
}

/// Process-wide cache: bare name → resolved path. Invalidated never — install
/// layout is stable for a CaPilot session; restart picks up new installs.
fn resolve_cache() -> &'static Mutex<std::collections::HashMap<String, Option<ResolvedExe>>> {
    static CACHE: std::sync::OnceLock<Mutex<std::collections::HashMap<String, Option<ResolvedExe>>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Resolve `name` (bare command or absolute/relative path) to a runnable file.
///
/// On Windows walks `PATH` + `PATHEXT` (default `.COM;.EXE;.BAT;.CMD`). On Unix
/// only the bare name / exact path is tried (same as historical `cli_available`).
pub fn resolve_executable(name: &str) -> Option<ResolvedExe> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }

    // Absolute / relative path with a separator or known extension: trust the
    // caller and only classify whether it needs a cmd wrap.
    if looks_like_path(name) {
        let path = PathBuf::from(name);
        if path.is_file() || cfg!(not(windows)) {
            // On Unix, allow bare relative names like `./tool` even if we cannot
            // stat yet (spawn will fail with a clear OS error).
            return Some(ResolvedExe {
                needs_cmd_wrap: needs_cmd_wrap(&path),
                path,
            });
        }
        // Windows path that does not exist yet — still return it so callers can
        // surface a path-specific error; probe will fail.
        if path.extension().is_some() {
            return Some(ResolvedExe {
                needs_cmd_wrap: needs_cmd_wrap(&path),
                path,
            });
        }
        return None;
    }

    {
        let cache = resolve_cache().lock().ok()?;
        if let Some(hit) = cache.get(name) {
            return hit.clone();
        }
    }

    ensure_cli_path();
    let resolved = resolve_bare_uncached(name);

    if let Ok(mut cache) = resolve_cache().lock() {
        cache.insert(name.to_string(), resolved.clone());
    }
    resolved
}

/// Build a fully-argued `Command` for `name`. Prefer this over
/// `Command::new(name)` for any Windows-safe adapter spawn (including long-lived
/// stdio children such as codex app-server).
pub fn command_for(name: &str, args: &[&str]) -> Option<Command> {
    let resolved = resolve_executable(name)?;
    let owned: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
    let mut cmd = if resolved.needs_cmd_wrap {
        let (program, base_args) = wrap_with_cmd(&resolved.path, &owned);
        let mut c = Command::new(program);
        c.args(base_args);
        c
    } else {
        let mut c = Command::new(&resolved.path);
        c.args(&owned);
        c
    };
    hide_windows_console(&mut cmd);
    Some(cmd)
}

/// Rewrite `(program, args)` so a PTY/`CreateProcess` spawn can actually start
/// the process. Bare names are resolved; `.cmd`/`.bat` become
/// `cmd.exe /d /s /c "…"`.
///
/// If resolution fails the original pair is returned unchanged (spawn fails
/// with the OS error, same as before).
pub fn prepare_pty_launch(program: &str, args: &[String]) -> (String, Vec<String>) {
    let Some(resolved) = resolve_executable(program) else {
        return (program.to_string(), args.to_vec());
    };
    if resolved.needs_cmd_wrap {
        return wrap_with_cmd(&resolved.path, args);
    }
    let prog = path_to_command_string(&resolved.path);
    (prog, args.to_vec())
}

/// `true` when `<name> --version` exits 0 within [`CLI_PROBE_TIMEOUT`].
pub fn cli_available(name: &str) -> bool {
    run_cli(name, &["--version"], CLI_PROBE_TIMEOUT).is_some_and(|o| o.status.success())
}

/// Run `<name> --version` and return the trimmed first stdout line.
pub fn cli_version(name: &str) -> Option<String> {
    let out = run_cli(name, &["--version"], CLI_PROBE_TIMEOUT)?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.trim().lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Run `name` with `args` under the probe timeout. Used by adapters that need
/// more than `--version` (auth status, model lists).
pub fn run_cli(name: &str, args: &[&str], timeout: Duration) -> Option<std::process::Output> {
    let cmd = command_for(name, args)?;
    run_cmd_timeout(cmd, timeout)
}

// ── internals ───────────────────────────────────────────────────────────

fn looks_like_path(name: &str) -> bool {
    name.contains('/')
        || name.contains('\\')
        || Path::new(name).extension().is_some_and(|ext| {
            let e = ext.to_string_lossy();
            e.eq_ignore_ascii_case("exe")
                || e.eq_ignore_ascii_case("cmd")
                || e.eq_ignore_ascii_case("bat")
                || e.eq_ignore_ascii_case("com")
        })
}

fn needs_cmd_wrap(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
}

fn path_to_command_string(path: &Path) -> String {
    path.to_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[cfg(not(windows))]
fn resolve_bare_uncached(name: &str) -> Option<ResolvedExe> {
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(ResolvedExe {
                path: candidate,
                needs_cmd_wrap: false,
            });
        }
    }
    // Fall back to bare name so historical `Command::new("bash")` behaviour is
    // preserved when the binary exists but we could not stat it (race / non-UTF8).
    // Missing binaries still fail at the `--version` probe / PTY spawn.
    Some(ResolvedExe {
        path: PathBuf::from(name),
        needs_cmd_wrap: false,
    })
}

#[cfg(windows)]
fn resolve_bare_uncached(name: &str) -> Option<ResolvedExe> {
    resolve_bare_windows(name)
}

#[cfg(not(windows))]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(windows)]
fn resolve_bare_windows(name: &str) -> Option<ResolvedExe> {
    let pathext = std::env::var_os("PATHEXT")
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
    let exts: Vec<String> = pathext
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with('.') {
                s.to_string()
            } else {
                format!(".{s}")
            }
        })
        .collect();

    // Also try the bare name (no extension) first — covers real .exe renames
    // and Unix-style shims that somehow landed on a Windows PATH.
    let mut names: Vec<String> = Vec::with_capacity(exts.len() + 1);
    names.push(name.to_string());
    for ext in &exts {
        // Avoid `claude.exe.exe` if the user already typed an extension.
        if name
            .rsplit(['/', '\\'])
            .next()
            .is_some_and(|base| base.contains('.'))
        {
            break;
        }
        names.push(format!("{name}{ext}"));
    }

    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        if !dir.as_os_str().is_empty() && !dir.is_dir() {
            continue;
        }
        for n in &names {
            let candidate = dir.join(n);
            if candidate.is_file() {
                return Some(ResolvedExe {
                    needs_cmd_wrap: needs_cmd_wrap(&candidate),
                    path: candidate,
                });
            }
        }
    }

    // WinGet per-user packages (npm-less CLIs sometimes land here).
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let packages = PathBuf::from(local).join("Microsoft").join("WinGet").join("Packages");
        if packages.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&packages) {
                for entry in entries.flatten() {
                    let dir = entry.path();
                    if !dir.is_dir() {
                        continue;
                    }
                    for n in &names {
                        let candidate = dir.join(n);
                        if candidate.is_file() {
                            return Some(ResolvedExe {
                                needs_cmd_wrap: needs_cmd_wrap(&candidate),
                                path: candidate,
                            });
                        }
                    }
                }
            }
        }
    }

    None
}

/// `cmd.exe /d /s /c "<quoted script> <quoted args…>"`
fn wrap_with_cmd(script: &Path, args: &[String]) -> (String, Vec<String>) {
    let comspec = std::env::var("ComSpec")
        .or_else(|_| std::env::var("COMSPEC"))
        .unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".to_string());

    let mut line = quote_cmd_arg(&path_to_command_string(script));
    for a in args {
        line.push(' ');
        line.push_str(&quote_cmd_arg(a));
    }

    (
        comspec,
        vec![
            "/d".to_string(),
            "/s".to_string(),
            "/c".to_string(),
            line,
        ],
    )
}

/// Quote one argv token for inclusion inside `cmd.exe /s /c`'s command line.
///
/// Metacharacters that cmd would reinterpret (`& | < > ^ % ( ) ! "`) force
/// double-quoting; embedded `"` are doubled. Good enough for agent CLI flags
/// (`--model`, paths with spaces).
pub fn quote_cmd_arg(arg: &str) -> String {
    let needs_quote = arg.is_empty()
        || arg.chars().any(|c| {
            matches!(
                c,
                ' ' | '\t'
                    | '"'
                    | '&'
                    | '|'
                    | '<'
                    | '>'
                    | '^'
                    | '%'
                    | '('
                    | ')'
                    | '!'
                    | ','
                    | ';'
                    | '='
            )
        });
    if !needs_quote {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for c in arg.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    out
}

fn hide_windows_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = cmd;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_cmd_arg_leaves_simple_tokens() {
        assert_eq!(quote_cmd_arg("--version"), "--version");
        assert_eq!(quote_cmd_arg("claude"), "claude");
    }

    #[test]
    fn quote_cmd_arg_quotes_spaces_and_meta() {
        assert_eq!(quote_cmd_arg("a b"), "\"a b\"");
        assert_eq!(quote_cmd_arg("a&b"), "\"a&b\"");
        assert_eq!(quote_cmd_arg("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn wrap_with_cmd_uses_comspec_and_flags() {
        let (prog, args) = wrap_with_cmd(
            Path::new(r"C:\npm\claude.cmd"),
            &["--model".into(), "sonnet".into()],
        );
        assert!(
            prog.to_ascii_lowercase().contains("cmd.exe") || prog.eq_ignore_ascii_case("cmd"),
            "prog={prog}"
        );
        assert_eq!(args[0], "/d");
        assert_eq!(args[1], "/s");
        assert_eq!(args[2], "/c");
        assert!(args[3].contains("claude.cmd"), "{}", args[3]);
        assert!(args[3].contains("--model"), "{}", args[3]);
        assert!(args[3].contains("sonnet"), "{}", args[3]);
    }

    #[test]
    fn needs_cmd_wrap_detects_scripts() {
        assert!(needs_cmd_wrap(Path::new("claude.cmd")));
        assert!(needs_cmd_wrap(Path::new(r"C:\x\CLAUDE.CMD")));
        assert!(needs_cmd_wrap(Path::new("run.bat")));
        assert!(!needs_cmd_wrap(Path::new("claude.exe")));
        assert!(!needs_cmd_wrap(Path::new("claude")));
    }

    #[test]
    fn prepare_pty_launch_passthrough_on_unix_bare_name() {
        #[cfg(unix)]
        {
            let (prog, args) = prepare_pty_launch("bash", &["--norc".into()]);
            // bash resolves on unix CI; program should still be invokable.
            assert!(!prog.is_empty());
            assert_eq!(args, vec!["--norc".to_string()]);
            assert!(!prog.eq_ignore_ascii_case("cmd.exe"));
        }
    }

    #[test]
    fn cli_available_finds_bash_on_unix() {
        #[cfg(unix)]
        {
            assert!(cli_available("bash"));
            assert!(!cli_available("capilot-definitely-missing-binary-xyz"));
        }
    }

    #[test]
    fn looks_like_path_rules() {
        assert!(looks_like_path(r"C:\foo\claude.cmd"));
        assert!(looks_like_path("/usr/bin/claude"));
        assert!(looks_like_path("claude.exe"));
        assert!(!looks_like_path("claude"));
        assert!(!looks_like_path("opencode"));
    }
}
