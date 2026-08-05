import { invoke } from "@tauri-apps/api/core";

/** Persisted preference: system notifications on/off (default on). */
const NOTIFICATIONS_KEY = "capilot.notifications";

/**
 * Show a system notification via the Rust `notify` command.
 * Honors the 系统通知 toggle in Settings — when disabled, this is a no-op.
 * Never throws: notification backend unavailable (headless/linux) is ignored.
 */
export async function notify(title: string, body: string): Promise<void> {
  try {
    if (localStorage.getItem(NOTIFICATIONS_KEY) === "0") return;
  } catch {
    // ignore storage errors
  }
  try {
    await invoke("notify", { title, body });
  } catch {
    // Notification backend not available — ignore.
  }
}
