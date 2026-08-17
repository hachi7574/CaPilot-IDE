import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

/**
 * What happens to the PTY daemon / live agent terminals when the GUI exits.
 *
 * - `ask`  — show a chooser on close (default when unset);
 * - `keep` — detach only; daemon + agent PTYs keep running across restarts;
 * - `kill` — shut down the daemon and kill every live PTY.
 *
 * Persisted under the settings KV key `exit_daemon_mode` (allow-listed in Rust).
 */
export type ExitDaemonMode = "ask" | "keep" | "kill";

export const EXIT_DAEMON_MODE_KEY = "exit_daemon_mode";

/** Normalize a raw settings value. Anything unknown falls back to `ask`. */
export function parseExitDaemonMode(
  raw: string | null | undefined
): ExitDaemonMode {
  if (raw === "keep" || raw === "kill" || raw === "ask") return raw;
  return "ask";
}

export async function loadExitDaemonMode(): Promise<ExitDaemonMode> {
  try {
    const raw = await invoke<string | null>("setting_get", {
      key: EXIT_DAEMON_MODE_KEY,
    });
    return parseExitDaemonMode(raw);
  } catch {
    return "ask";
  }
}

export async function saveExitDaemonMode(mode: ExitDaemonMode): Promise<void> {
  await invoke("setting_set", {
    key: EXIT_DAEMON_MODE_KEY,
    value: mode,
  }).catch(() => {});
}

/**
 * Apply the quit policy and close the window.
 *
 * 1. Stage a one-shot override via `app_prepare_exit` so ExitRequested does
 *    the right thing even when the user did not check "记住选择".
 * 2. If `remember`, also persist `keep`/`kill` so future closes skip the dialog.
 * 3. `window.close()` → Rust `ExitRequested` consumes the override / setting.
 */
export async function quitWithDaemonMode(
  mode: "keep" | "kill",
  remember: boolean
): Promise<void> {
  await invoke("app_prepare_exit", { mode }).catch(async () => {
    // Older binary without the command: sticky write so this quit is still
    // correct (remember-semantics degrade to always-remember on that build).
    await saveExitDaemonMode(mode);
  });
  if (remember) {
    await saveExitDaemonMode(mode);
  }
  await getCurrentWindow().close();
}

/**
 * Shared close-button handler for the titlebar × in LeftSidebar / TabBar.
 *
 * - setting `ask` (default) → caller should open the dialog (returns `"ask"`);
 * - setting `keep`/`kill` → quit immediately with that policy (returns the mode).
 */
export async function handleTitlebarClose(
  openAskDialog: () => void
): Promise<"ask" | "keep" | "kill"> {
  const mode = await loadExitDaemonMode();
  if (mode === "ask") {
    openAskDialog();
    return "ask";
  }
  await quitWithDaemonMode(mode, false);
  return mode;
}
