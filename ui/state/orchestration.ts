import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useStore, WorkerInfo, WorkerReport } from "./store";

/**
 * Keeps orchestration state (worker pool, smart-return toggle, master reports)
 * in sync with the Rust dispatcher.
 * - Fetches `worker_status` and `smart_return_get` on mount
 * - Subscribes to `orchestration://event` (worker state) and
 *   `orchestration://report` (smart-return reports)
 */
export function useOrchestrationSync() {
  useEffect(() => {
    let unlisteners: (() => void)[] = [];

    const init = async () => {
      const s = useStore.getState();

      try {
        const infos = await invoke<WorkerInfo[]>("worker_status");
        s.setWorkerInfos(infos ?? []);
      } catch {
        // Backend not ready yet
      }
      try {
        const enabled = await invoke<boolean>("smart_return_get");
        s.setSmartReturn(enabled);
      } catch {
        // ignore
      }

      try {
        const runtimeInfos = await invoke("runtime_list_available");
        s.setRuntimes(runtimeInfos as never);
      } catch {
        // ignore
      }

      unlisteners.push(
        await listen("orchestration://event", (event) => {
          const payload = event.payload as WorkerInfo;
          if (payload?.id) useStore.getState().upsertWorkerInfo(payload);
        })
      );
      unlisteners.push(
        await listen("orchestration://report", (event) => {
          const payload = event.payload as WorkerReport;
          if (payload?.summary) useStore.getState().addReport(payload);
        })
      );
    };

    init();
    return () => unlisteners.forEach((fn) => fn());
  }, []);
}

/** Toggle the smart-return setting in the Rust dispatcher. */
export async function setSmartReturn(enabled: boolean): Promise<void> {
  useStore.getState().setSmartReturn(enabled);
  try {
    await invoke("smart_return_set", { enabled });
  } catch {
    // ignore
  }
}

/** Mark an agent as a worker (or remove from the pool). */
export async function setAgentRole(id: string, role: "worker" | "standalone" | "master"): Promise<void> {
  try {
    await invoke("agent_set_role", { id, role });
    const s = useStore.getState();
    const a = s.agents.get(id);
    if (a) s.addAgent({ ...a, role }, null);
  } catch (e) {
    console.error("agent_set_role failed:", e);
  }
}
