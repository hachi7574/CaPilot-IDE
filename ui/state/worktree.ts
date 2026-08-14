import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useStore, type WorktreeMeta } from "./store";
import { spawnAgent } from "./agentActions";

/**
 * Keeps the sidebar's git worktree registry in sync with the backend.
 *
 * The `worktrees` table is persisted in `~/CaPilot/sessions.db`; on mount we
 * pull the full list (`worktree_list_all`) so branch badges survive a restart.
 * Then `worktree://created` / `worktree://removed` events apply incrementally:
 *  - created → register the meta, surface the backend-created project shell,
 *              and auto-open a terminal in the fresh worktree (best-effort);
 *  - removed → drop the project shell + worktree locally. The backend already
 *              killed sessions, deleted the shell and ran `git worktree remove`,
 *              so the event handler must NOT call `delete_project` / remove again.
 */
export function useWorktreeEvents() {
  useEffect(() => {
    let unlisten: Array<() => void> = [];
    // StrictMode double-mount guard, same discipline as useCloneEvents.
    let cancelled = false;

    const init = async () => {
      try {
        const list = await invoke<WorktreeMeta[]>("worktree_list_all");
        if (!cancelled) useStore.getState().setWorktrees(list);
      } catch {
        // Backend not ready yet — ignore; events will reconcile when they arrive.
      }
    };

    const initEvents = async () => {
      try {
        const unCreated = await listen<{ meta: WorktreeMeta; name: string }>(
          "worktree://created",
          (event) => {
            const { meta, name } = event.payload;
            const s = useStore.getState();
            s.addWorktree(meta, name);
            s.setFocusedProject(name);
            spawnAgent(name).catch((e) =>
              console.error("自动打开工作区终端失败:", e)
            );
          }
        );
        const unRemoved = await listen<{ path: string; name: string }>(
          "worktree://removed",
          (event) => {
            useStore.getState().removeWorktreeLocal(event.payload.path);
          }
        );
        if (cancelled) {
          unCreated();
          unRemoved();
        } else {
          unlisten = [unCreated, unRemoved];
        }
      } catch {
        // Backend not ready yet — ignore.
      }
    };

    init();
    initEvents();
    return () => {
      cancelled = true;
      unlisten.forEach((u) => u());
    };
  }, []);
}
