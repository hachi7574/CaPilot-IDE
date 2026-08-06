import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useStore, RestoredSession } from "./store";

/**
 * On app start, restore persisted sessions from sqlite so they survive a
 * restart (DevPlan §6.3). Restored sessions have no live channel yet — the
 * XTermPanel resumes them on tab open.
 *
 * Ended (`done`) sessions are kept in the store so the sidebar's "已结束" group
 * can surface them, but they are NOT re-opened as tabs and never re-promoted to
 * master — reopening a finished conversation is an explicit sidebar action.
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
          const status = (rec.status as never) || "idle";
          s.addAgent(
            {
              id: rec.id,
              runtime: rec.runtime,
              role: rec.role,
              status,
              title: rec.title,
              cwd: rec.cwd,
              pid: null,
              mode: rec.mode,
              speed: rec.speed,
              model: rec.model,
            },
            null,
            rec.updated_at
          );
          // Ended sessions are recoverable from the sidebar but are not
          // auto-reopened as tabs / not re-promoted to master.
          if (status === "done") continue;
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

/**
 * React to backend session-lifecycle events:
 * - `agent://exited`  — a session's process ended naturally and the record was
 *   kept (marked done). The tab stays open but grays out under "已结束".
 * - `agent://removed` — the "session ended → delete" setting removed the
 *   record; close the tab and drop the agent.
 */
export function useAgentEvents() {
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    // listen() resolves asynchronously — cleanup may run before the promise
    // settles, so a late listener must drop itself instead of leaking.
    Promise.all([
      listen<{ id: string; exit_code: number }>("agent://exited", (e) => {
        const s = useStore.getState();
        // Natural exit, record kept (marked done): the open tab stays (grayed),
        // its dead channel stays attached so the last output remains visible.
        // Reopening from the sidebar "已结束" group drops the channel and resumes.
        s.updateAgentStatus(e.payload.id, "done");
        // A finished master must vacate the composer's master slot: with a stale
        // masterAgentId the composer keeps agent_write-ing the dead process, and
        // the frontend `.catch(()=>{})` swallows the error → the user's input
        // silently vanishes.
        if (s.masterAgentId === e.payload.id) s.setMasterAgentId(null);
      }),
      listen<{ id: string }>("agent://removed", (e) => {
        const s = useStore.getState();
        s.closeTab(e.payload.id);
        s.removeAgent(e.payload.id);
      }),
    ])
      .then(([u1, u2]) => {
        if (cancelled) {
          u1();
          u2();
        } else {
          unlisten = () => {
            u1();
            u2();
          };
        }
      })
      .catch(() => {
        // Backend not ready — ignore.
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
