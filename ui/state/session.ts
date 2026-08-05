import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useStore, RestoredSession } from "./store";

/**
 * On app start, restore persisted sessions from sqlite so they survive a
 * restart (DevPlan §6.3). Restored sessions have no live channel yet — the
 * XTermPanel resumes them on tab open.
 */
export function useSessionRestore() {
  useEffect(() => {
    // StrictMode double-mount guard: the sessions_list invoke resolves after
    // the first mount's cleanup, so the restore would run twice and re-add /
    // re-subscribe. Dropping late resolutions keeps the restore single-shot.
    let cancelled = false;
    invoke<RestoredSession[]>("sessions_list")
      .then((sessions) => {
        if (cancelled) return;
        const s = useStore.getState();
        for (const rec of sessions ?? []) {
          s.addAgent(
            {
              id: rec.id,
              runtime: rec.runtime,
              role: rec.role,
              status: (rec.status as never) || "idle",
              title: rec.title,
              cwd: rec.cwd,
              pid: null,
            },
            null
          );
          if (rec.role === "master") s.setMasterAgentId(rec.id);
          s.addTabSilent({
            id: rec.id,
            type: "agent",
            agentId: rec.id,
            title: rec.title || `${rec.runtime}@${rec.role}`,
          });
        }
      })
      .catch(() => {
        // Backend not ready — ignore.
      });
    return () => {
      cancelled = true;
    };
  }, []);
}
