import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { RuntimeInfo, useStore } from "./store";

/** Load standalone runtime capabilities used by onboarding, settings, and the
 * Composer model/thinking/permission controls. */
export function useRuntimeSync() {
  useEffect(() => {
    let cancelled = false;

    const load = () =>
      invoke<RuntimeInfo[]>("runtime_list_available")
        .then((runtimes) => {
          if (!cancelled) {
            useStore
              .getState()
              .setRuntimes(Array.isArray(runtimes) ? runtimes : []);
          }
        })
        .catch(() => {
          // Backend may still be starting; keep the empty state without crashing.
        });

    load();
    // One retry: first probe can race the backend / a cold CLI cache and leave
    // Settings empty even though new-terminal templates still work.
    const retry = window.setTimeout(() => {
      if (!cancelled && useStore.getState().runtimes.length === 0) {
        load();
      }
    }, 2500);

    return () => {
      cancelled = true;
      window.clearTimeout(retry);
    };
  }, []);
}
