/**
 * Path / shell helpers for injecting commands into the OS default shell.
 *
 * Backend paths use the host OS separator (`\` on Windows via `Path::display` /
 * `to_string_lossy`). File-tree joins historically used `/`; both forms are
 * accepted. Commands written into a live shell must match that shell's quoting
 * rules — PowerShell, cmd.exe, and POSIX shells disagree on quotes and `&&`.
 */

/** True when the host looks like Windows (Tauri desktop WebView). */
export function isWindowsHost(): boolean {
  if (typeof navigator === "undefined") return false;
  const p = (navigator.platform || "").toLowerCase();
  if (p.includes("win")) return true;
  const ua = (navigator.userAgent || "").toLowerCase();
  return ua.includes("windows");
}

/** Join directory + name with the host's preferred separator.
 *  `name` may itself contain `/` (git relative paths); those are rewritten to
 *  the host separator so tree keys stay consistent on Windows. */
export function joinPath(dir: string, name: string): string {
  if (!dir) return name;
  const sep = isWindowsHost() ? "\\" : "/";
  const trimmed = dir.replace(/[\\/]+$/, "");
  const rest = isWindowsHost()
    ? name.replace(/\//g, "\\").replace(/^\\+/, "")
    : name.replace(/^\/+/, "");
  if (!rest) return trimmed;
  return `${trimmed}${sep}${rest}`;
}

/** Parent directory of an absolute path (handles `/` and `\`). */
export function parentPath(path: string): string {
  const norm = path.replace(/\\/g, "/");
  const idx = norm.lastIndexOf("/");
  if (idx <= 0) {
    // `C:` drive root or bare name — keep the original drive prefix if present.
    if (/^[A-Za-z]:/.test(path) && idx === 2) {
      return path.slice(0, 3); // `C:\` or `C:/`
    }
    return idx === 0 ? norm.slice(0, 1) : "";
  }
  // Preserve original separator style when the input used backslashes.
  const parent = path.slice(0, idx);
  return parent || (isWindowsHost() ? path.slice(0, 3) : "/");
}

/** Final path segment. */
export function baseName(path: string): string {
  const norm = path.replace(/\\/g, "/");
  const idx = norm.lastIndexOf("/");
  return idx >= 0 ? norm.slice(idx + 1) : path;
}

/**
 * Detect the shell flavor that will interpret an injected command line.
 * Uses the live runtime label from Settings probes when available.
 */
export type ShellFlavor = "powershell" | "cmd" | "posix";

export function detectShellFlavor(
  runtimeId: string | undefined,
  runtimeName?: string | null
): ShellFlavor {
  const id = (runtimeId ?? "").toLowerCase();
  // Explicit bash / Git Bash sessions always speak POSIX.
  if (id === "bash" || id === "bash-rc" || id.startsWith("bash")) {
    return "posix";
  }
  // Explicit Windows shell runtime ids — prefer id over label.
  if (id === "powershell" || id === "pwsh") return "powershell";
  if (id === "cmd") return "cmd";
  if (!isWindowsHost()) return "posix";
  const label = (runtimeName ?? "").toLowerCase();
  if (label.includes("cmd")) return "cmd";
  // Default Windows OS shell is pwsh → ComSpec/cmd; prefer PowerShell when the
  // label says so, otherwise cmd (ComSpec fallback label is "cmd").
  if (label.includes("powershell") || label === "pwsh") return "powershell";
  if (label === "cmd") return "cmd";
  // Shell runtime on Windows with unknown label — PowerShell is preferred by
  // the adapter, so default to its quoting rules (cmd is the rarer fallback).
  if (id === "shell") return "powershell";
  return "posix";
}

/** True for plain interactive shells (OS shell / PowerShell / cmd / bash) —
 *  not agent CLIs. Used for command injection after spawn and file-tree actions. */
export function isShellRuntime(runtime: string | undefined | null): boolean {
  const id = (runtime ?? "").toLowerCase();
  return (
    id === "shell" ||
    id === "powershell" ||
    id === "pwsh" ||
    id === "cmd" ||
    id === "bash" ||
    id === "bash-rc" ||
    id.startsWith("bash")
  );
}

/** Quote a path/arg for the target shell. */
export function shellQuote(value: string, flavor: ShellFlavor): string {
  switch (flavor) {
    case "powershell": {
      // PowerShell single-quoted string: double embedded single quotes.
      return `'${value.replace(/'/g, "''")}'`;
    }
    case "cmd": {
      // cmd double-quotes; double embedded double-quotes.
      // Avoid bare `& | < >` by always quoting paths we inject.
      return `"${value.replace(/"/g, '""')}"`;
    }
    default: {
      // POSIX / bash: single-quote, close-escape-reopen for embedded quotes.
      return `'${value.replace(/'/g, `'\\''`)}'`;
    }
  }
}

/** `cd <dir>` then run `cmd` as one line the target shell understands. */
export function shellCdAndRun(
  dir: string,
  cmd: string,
  flavor: ShellFlavor
): string {
  const q = shellQuote(dir, flavor);
  switch (flavor) {
    case "powershell":
      // `;` sequences even if Set-Location fails (interactive injection).
      return `Set-Location ${q}; ${cmd}`;
    case "cmd":
      // `/d` also switches drive letters (plain `cd` does not).
      return `cd /d ${q} & ${cmd}`;
    default:
      return `cd ${q} && ${cmd}`;
  }
}

/** Just `cd` into a directory (folder → current terminal). */
export function shellCd(dir: string, flavor: ShellFlavor): string {
  const q = shellQuote(dir, flavor);
  switch (flavor) {
    case "powershell":
      return `Set-Location ${q}`;
    case "cmd":
      return `cd /d ${q}`;
    default:
      return `cd ${q}`;
  }
}

/**
 * Build a command that runs `fileName` from its own directory.
 * Returns null when the extension isn't a known runnable type and the file
 * isn't marked executable (Unix bit — always false on Windows backend).
 */
export function runCommandForFile(
  fileName: string,
  executable: boolean,
  flavor: ShellFlavor
): string | null {
  const dot = fileName.lastIndexOf(".");
  const ext = dot >= 0 ? fileName.slice(dot).toLowerCase() : "";
  // Relative form the shell resolves against its cwd (already cd'd to the dir).
  const rel =
    flavor === "cmd"
      ? `.\\${fileName}`
      : flavor === "powershell"
        ? `.\\${fileName}`
        : `./${fileName}`;
  const q = shellQuote(rel, flavor);

  switch (ext) {
    case ".py":
      // Windows Python launcher is typically `python`; Unix distros ship `python3`.
      return isWindowsHost() ? `python ${q}` : `python3 ${q}`;
    case ".ps1":
      return flavor === "cmd"
        ? `powershell -NoProfile -ExecutionPolicy Bypass -File ${q}`
        : flavor === "powershell"
          ? q // direct script invoke in pwsh
          : `pwsh -NoProfile -File ${q}`;
    case ".bat":
    case ".cmd":
      // cmd/PowerShell both accept quoted paths; POSIX needs cmd.exe.
      if (flavor === "posix") {
        return `cmd.exe /c ${q}`;
      }
      return q;
    case ".sh":
    case ".bash":
      // Prefer bash when the session is already bash; otherwise try bash then sh.
      return flavor === "posix" ? `bash ${q}` : `bash ${q}`;
    case ".js":
    case ".mjs":
    case ".cjs":
      return `node ${q}`;
    case ".ts":
    case ".tsx":
      return `npx tsx ${q}`;
    case ".rb":
      return `ruby ${q}`;
    case ".php":
      return `php ${q}`;
    case ".pl":
      return `perl ${q}`;
    case ".go":
      return `go run ${q}`;
    default:
      if (executable) return q;
      return null;
  }
}
