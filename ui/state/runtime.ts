import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { RuntimeInfo, useStore } from "./store";

/** Load standalone runtime capabilities used by onboarding, settings, and the
 * Composer model/thinking/permission controls. */
export function useRuntimeSync() {
  useEffect(() => {
    let cancelled = false;

    invoke<RuntimeInfo[]>("runtime_list_available")
      .then((runtimes) => {
        if (!cancelled) {
          useStore.getState().setRuntimes(Array.isArray(runtimes) ? runtimes : []);
        }
      })
      .catch(() => {
        // Backend may still be starting; keep the empty state without crashing.
      });

    return () => {
      cancelled = true;
    };
  }, []);
}
