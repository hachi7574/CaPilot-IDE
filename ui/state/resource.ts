import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useStore, AgentResource, ResourcePoint } from "./store";

/**
 * Resource monitor sync (DevPlan §10).
 * Subscribes to `resource://sample` (emitted every second by the Rust sampler)
 * and merges each batch into the store's per-agent CPU/MEM snapshot + rolling
 * history for the sparkline curve.
 */
export function useResourceSync() {
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;

    const init = async () => {
      try {
        const un = await listen("resource://sample", (event) => {
          const payload = event.payload as AgentResource[];
          if (Array.isArray(payload)) {
            useStore.getState().applyResourceSample(payload);
          }
        });
        // Guard against StrictMode double-mount: if cleanup already ran while
        // the async listen() was pending, drop this late subscription.
        if (cancelled) {
          un();
        } else {
          unlisten = un;
        }
      } catch {
        // Backend not ready yet — ignore.
      }
    };

    init();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}

/** Fetch the backend-buffered history (oldest → newest) for the curve overlay. */
export async function fetchResourceHistory(agentId: string): Promise<ResourcePoint[]> {
  try {
    const points = await invoke<ResourcePoint[]>("resource_history", { agentId });
    return Array.isArray(points) ? points : [];
  } catch {
    return [];
  }
}

/** Format a byte count as a human-readable MEM string (e.g. "3.2GB"). */
export function fmtMem(bytes: number | undefined): string {
  if (bytes === undefined || Number.isNaN(bytes)) return "—";
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(2)}GB`;
  if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(1)}MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(0)}KB`;
  return `${bytes}B`;
}

/** Format a CPU% value for display. */
export function fmtCpu(cpu: number | undefined): string {
  if (cpu === undefined || Number.isNaN(cpu)) return "—";
  return `${Math.round(cpu)}%`;
}
