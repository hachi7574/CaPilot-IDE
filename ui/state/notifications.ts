import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useStore } from "./store";
import { notify } from "./notify";

/**
 * Fires system notifications for background events:
 * - New worker report (smart-return completion) → 汇报就绪 / 任务失败
 * - ESP BLE connection drop → ESP 断连
 *
 * Worker "done" is surfaced through `orchestration://report` (the dispatcher
 * only emits worker busy/idle/offline states, not a done/failed terminal
 * state), so we watch the `reports` array for newly appended reports.
 *
 * Honors the 系统通知 toggle via `notify()`.
 */
export function useNotifications() {
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let unsub: (() => void) | undefined;
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

    // New worker report appended to the store → system notification.
    unsub = useStore.subscribe((state, prev) => {
      const latest = state.reports[0];
      const prevLatest = prev.reports[0];
      if (latest && latest.ts !== prevLatest?.ts) {
        const title = latest.level === "failure" ? "任务失败" : "汇报就绪";
        const summary = (latest.summary ?? "").trim() || "(无摘要)";
        notify(title, `${latest.worker} · ${summary.slice(0, 80)}`);
      }
    });

    init();
    return () => {
      cancelled = true;
      unlisten?.();
      unsub?.();
    };
  }, []);
}
