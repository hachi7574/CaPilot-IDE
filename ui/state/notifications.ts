import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { notify } from "./notify";

/**
 * Fires a system notification when the ESP BLE connection drops.
 *
 * Honors the 系统通知 toggle via `notify()`.
 */
export function useNotifications() {
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    // StrictMode double-mount guard: `listen()` resolves after cleanup, so a
    // late listener must drop itself instead of leaking into the second mount.
    let cancelled = false;

    // ESP drop → system notification.
    const init = async () => {
      const un = await listen("esp://event", (event) => {
        const payload = event.payload as any;
        if (payload?.event === "disconnected") {
          const reason =
            payload.reason && String(payload.reason).trim()
              ? `原因：${payload.reason}`
              : "BLE 连接已断开";
          notify("ESP 断连", reason);
        }
      });
      if (cancelled) {
        un();
      } else {
        unlisten = un;
      }
    };

    init();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
