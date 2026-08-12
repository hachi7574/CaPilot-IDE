import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { RuntimeUsage, useStore } from "./store";

/** Poll interval for the status-bar usage readout (mirrors the reference doc's
 *  DEFAULT_POLL_MS). Refreshes on window focus and after settings changes too. */
const POLL_MS = 15 * 60 * 1000;
const USAGE_RUNTIMES = ["codex", "opencode"];

/** Load remaining-usage for every runtime enabled under Settings → 已安装 → ⚙ →
 *  用量统计, and keep `store.usageState` fresh for the status bar.
 *
 *  The backend owns the fetch config (`usage_config`); the renderer only reads
 *  the enable flags here, so the opencode auth cookie never crosses IPC. */
export function useUsageSync() {
  const revision = useStore((s) => s.usageRevision);

  useEffect(() => {
    let cancelled = false;

    const refresh = async () => {
      let enabled: Record<string, boolean> = {};
      try {
        const raw = await invoke<string | null>("setting_get", {
          key: "usage_enabled",
        });
        if (raw) enabled = JSON.parse(raw);
      } catch {
        // Backend not ready yet — the interval retries.
      }

      for (const rt of USAGE_RUNTIMES) {
        if (!enabled[rt]) {
          const s = useStore.getState();
          if (s.usageState[rt]) {
            s.setUsage(rt, {
              runtime: rt,
              available: false,
              error: null,
              plan_type: null,
              windows: [],
              checked_at: 0,
            });
          }
          continue;
        }
        invoke<RuntimeUsage>("usage_fetch", { runtime: rt })
          .then((usage) => {
            if (!cancelled) useStore.getState().setUsage(rt, usage);
          })
          .catch(() => {
            // Transient failure — keep the last value; the next poll retries.
          });
      }
    };

    refresh();
    const timer = setInterval(refresh, POLL_MS);
    window.addEventListener("focus", refresh);
    return () => {
      cancelled = true;
      clearInterval(timer);
      window.removeEventListener("focus", refresh);
    };
  }, [revision]);
}
