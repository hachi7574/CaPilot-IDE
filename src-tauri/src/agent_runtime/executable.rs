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

    // Absolute / relative path with a separator: trust the caller and only
    // classify whether it needs a cmd wrap. Bare names like `pwsh.exe` are
    // NOT paths — they must go through PATH+PATHEXT search below. Treating
    // them as paths previously made a missing PowerShell 7 look "resolved"
    // and ConPTY then failed with CreateProcess error 2.
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
        // Windows path-with-separator that does not exist yet — still return it
        // so callers can surface a path-specific error; probe will fail.
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

/// Run `<name> --version` and return a compact version token (`5.3.9`, not the
/// GNU bash banner). `None` when the binary is missing, fails, times out, or
/// prints nothing useful.
pub fn cli_version(name: &str) -> Option<String> {
    let out = run_cli(name, &["--version"], CLI_PROBE_TIMEOUT)?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.trim().lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    Some(short_version(line))
}

/// Pull `1.2.3` / `1.2.3-beta` out of a `--version` banner. Falls back to the
/// first whitespace token with a leading `v` stripped.
pub fn short_version(raw: &str) -> String {
    let s = raw.trim();
    if let Some(v) = first_semver(s) {
        return v;
    }
    s.split_whitespace()
        .next()
        .unwrap_or(s)
        .trim_start_matches(|c: char| c == 'v' || c == 'V')
        .to_string()
}

fn first_semver(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            let mut dots = 0;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            while i < bytes.len() && bytes[i] == b'.' {
                dots += 1;
                i += 1;
                if i >= bytes.len() || !bytes[i].is_ascii_digit() {
                    break;
                }
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if dots >= 1 {
                let mut end = i;
                if i < bytes.len() && (bytes[i] == b'-' || bytes[i] == b'+') {
                    i += 1;
                    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'-') {
                        i += 1;
                    }
                    end = i;
                }
                let token = &s[start..end];
                if token.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    return Some(token.to_string());
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Run `name` with `args` under the probe timeout. Used by adapters that need
/// more than `--version` (auth status, model lists).
pub fn run_cli(name: &str, args: &[&str], timeout: Duration) -> Option<std::process::Output> {
    let cmd = command_for(name, args)?;
    run_cmd_timeout(cmd, timeout)
}

// ── internals ───────────────────────────────────────────────────────────

fn looks_like_path(name: &str) -> bool {
    // Separators (or a Windows drive-absolute form) mean the caller named a
    // filesystem location. A bare `tool.exe` is a PATH lookup key, not a path —
    // CreateProcess still needs PATHEXT/PATH resolution for those.
    name.contains('/')
        || name.contains('\\')
        || Path::new(name).is_absolute()
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

    // Match cmd.exe / CreateProcess PATHEXT order: try `name.COM`, `name.EXE`,
    // `name.BAT`, `name.CMD`, … BEFORE the extensionless `name`.
    //
    // npm global installs on Windows drop three siblings in %APPDATA%\npm:
    //   claude       ← Unix `#!/bin/sh` shim (NOT a PE)
    //   claude.cmd   ← real Windows entry
    //   claude.ps1
    // Preferring the bare name first made Settings report Claude as missing:
    // CreateProcess on the sh script fails with ERROR_BAD_EXE_FORMAT (193),
    // and cli_available() treated that as "not installed".
    let mut names: Vec<String> = Vec::with_capacity(exts.len() + 1);
    let already_has_ext = name
        .rsplit(['/', '\\'])
        .next()
        .is_some_and(|base| base.contains('.'));
    if already_has_ext {
        names.push(name.to_string());
    } else {
        for ext in &exts {
            names.push(format!("{name}{ext}"));
        }
        // Bare name last — only useful for true extensionless PE renames.
        names.push(name.to_string());
    }

    let path_var = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_var) {
        if !dir.as_os_str().is_empty() && !dir.is_dir() {
            continue;
        }
        for n in &names {
            let candidate = dir.join(n);
            if let Some(resolved) = accept_windows_candidate(&candidate) {
                return Some(resolved);
            }
        }
    }

    // WinGet per-user packages (npm-less CLIs sometimes land here).
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let packages = PathBuf::from(local)
            .join("Microsoft")
            .join("WinGet")
            .join("Packages");
        if packages.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&packages) {
                for entry in entries.flatten() {
                    let dir = entry.path();
                    if !dir.is_dir() {
                        continue;
                    }
                    for n in &names {
                        let candidate = dir.join(n);
                        if let Some(resolved) = accept_windows_candidate(&candidate) {
                            return Some(resolved);
                        }
                    }
                }
            }
        }
    }

    // npm global prefix (normally already on PATH, but desktop-launched apps
    // sometimes miss %APPDATA%\npm even when `npm i -g` put shims there).
    if let Ok(appdata) = std::env::var("APPDATA") {
        let npm = PathBuf::from(appdata).join("npm");
        if npm.is_dir() {
            for n in &names {
                let candidate = npm.join(n);
                if let Some(resolved) = accept_windows_candidate(&candidate) {
                    return Some(resolved);
                }
            }
        }
    }

    None
}

/// Accept a PATH hit only when CreateProcess/ConPTY can actually launch it.
/// Skips npm's extensionless Unix shims (`#!/bin/sh`) that sit next to `.cmd`.
#[cfg(windows)]
fn accept_windows_candidate(candidate: &Path) -> Option<ResolvedExe> {
    if !candidate.is_file() {
        return None;
    }
    if needs_cmd_wrap(candidate) {
        return Some(ResolvedExe {
            needs_cmd_wrap: true,
            path: candidate.to_path_buf(),
        });
    }
    // Extensionless or .exe/.com: reject obvious non-PE scripts so the search
    // continues to `name.cmd` in the same directory.
    if is_unix_script_shim(candidate) {
        return None;
    }
    Some(ResolvedExe {
        needs_cmd_wrap: false,
        path: candidate.to_path_buf(),
    })
}

/// `true` when `path` looks like a text script with a shebang (npm's bare
/// `claude` shim), which CreateProcess cannot run (Win32 error 193).
#[cfg(windows)]
fn is_unix_script_shim(path: &Path) -> bool {
    // .cmd/.bat are handled via needs_cmd_wrap; never classify them as unix.
    if needs_cmd_wrap(path) {
        return false;
    }
    // Real PE binaries start with "MZ". Shebang scripts start with "#!".
    let mut buf = [0u8; 2];
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    use std::io::Read;
    match f.read(&mut buf) {
        Ok(2) if &buf == b"#!" => true,
        Ok(2) if &buf == b"MZ" => false,
        // No PE header and not shebang — if it has no Windows executable
        // extension, treat as non-launchable so PATHEXT siblings can win.
        Ok(_) => path
            .extension()
            .and_then(|e| e.to_str())
            .is_none_or(|e| {
                !(e.eq_ignore_ascii_case("exe")
                    || e.eq_ignore_ascii_case("com")
                    || e.eq_ignore_ascii_case("cmd")
                    || e.eq_ignore_ascii_case("bat"))
            }),
        _ => false,
    }
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

/// Suppress the extra console window flash when spawning short-lived probe
/// children from a GUI / DETACHED_PROCESS host on Windows.
pub fn hide_windows_console(cmd: &mut Command) {
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
    fn short_version_strips_gnu_bash_banner() {
        assert_eq!(
            short_version("GNU bash，版本 5.3.9(1)-release (x86_64-pc-linux-gnu)"),
            "5.3.9"
        );
        assert_eq!(short_version("codebuddy 2.127.0"), "2.127.0");
        assert_eq!(short_version("v0.56.0"), "0.56.0");
        assert_eq!(short_version("1.0.80"), "1.0.80");
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
        assert!(looks_like_path(r".\tools\claude.exe"));
        // Bare names — with or without extension — are PATH lookup keys, not
        // filesystem paths. `pwsh.exe` must search PATH, not be trusted as-is.
        assert!(!looks_like_path("claude.exe"));
        assert!(!looks_like_path("claude"));
        assert!(!looks_like_path("opencode"));
        assert!(!looks_like_path("pwsh.exe"));
    }

    /// npm on Windows writes a Unix `#!/bin/sh` shim next to `name.cmd`. The
    /// resolver must prefer the .cmd (CreateProcess-launchable) over the bare
    /// script, otherwise Settings reports the CLI as missing (error 193).
    #[cfg(windows)]
    #[test]
    fn windows_prefers_cmd_over_unix_npm_shim() {
        let _guard = crate::agent_runtime::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "capilot-npm-shim-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Extensionless Unix shim (what npm actually writes).
        std::fs::write(
            dir.join("fakectl"),
            "#!/bin/sh\nexec node \"$(dirname \"$0\")/cli.js\" \"$@\"\n",
        )
        .unwrap();
        // Real Windows entry.
        std::fs::write(
            dir.join("fakectl.cmd"),
            "@ECHO off\r\necho fakectl-ok\r\n",
        )
        .unwrap();

        // Prepend our dir so the search hits it first; keep the rest of PATH.
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let mut new_path = dir.as_os_str().to_owned();
        new_path.push(";");
        new_path.push(&old_path);
        std::env::set_var("PATH", &new_path);

        // Bust the process-wide resolve cache for this name.
        if let Ok(mut cache) = resolve_cache().lock() {
            cache.remove("fakectl");
        }

        let resolved = resolve_executable("fakectl").expect("should find fakectl.cmd");
        assert!(
            resolved
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case("fakectl.cmd")),
            "expected fakectl.cmd, got {:?}",
            resolved.path
        );
        assert!(resolved.needs_cmd_wrap);

        // Cleanup.
        std::env::set_var("PATH", old_path);
        if let Ok(mut cache) = resolve_cache().lock() {
            cache.remove("fakectl");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn is_unix_script_shim_detects_shebang() {
        let dir = std::env::temp_dir().join(format!(
            "capilot-shebang-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("tool");
        std::fs::write(&shim, "#!/usr/bin/env node\nconsole.log(1)\n").unwrap();
        assert!(is_unix_script_shim(&shim));
        let cmd = dir.join("tool.cmd");
        std::fs::write(&cmd, "@echo hi\r\n").unwrap();
        assert!(!is_unix_script_shim(&cmd));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
