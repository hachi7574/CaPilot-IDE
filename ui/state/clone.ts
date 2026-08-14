import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useStore } from "./store";
import { createDefaultAgent } from "./structuredAgent";
import { notify } from "./notify";

/**
 * Reconciles background `git_clone` results with the store.
 *
 * `git_clone` now returns the moment the clone starts; the actual clone runs in
 * the backend and reports on `git://cloned` / `git://clone-error`. This hook
 * turns those events into store updates:
 *  - cloned      → clear the "正在克隆中" flag, record the root, auto-open a
 *                  terminal in the freshly cloned project;
 *  - clone-error → drop the placeholder project, surface the git error.
 *
 * Only clones initiated in THIS session (ids present in `pendingClones`) get the
 * auto-open / drop treatment — a late event arriving after a webview reload just
 * reconciles the project list idempotently.
 */
export function useCloneEvents() {
  useEffect(() => {
    let unlisten: Array<() => void> = [];
    // StrictMode double-mount guard: `listen()` resolves after cleanup, so a
    // late listener must drop itself instead of leaking into the second mount.
    let cancelled = false;

    const init = async () => {
      try {
        const unDone = await listen<{ id: string; name: string; root: string }>(
          "git://cloned",
          (event) => {
            const { id, name, root } = event.payload;
            const s = useStore.getState();
            if (!(id in s.pendingClones)) {
              // Late event (webview reload mid-clone): just surface the project.
              s.addProject(name, root);
              return;
            }
            s.finishClone(id);
            s.setProjectRoots({ [name]: root });
            // Auto-open a fresh agent session in the clone (best-effort; Phase 5
            // default = structured backend).
            createDefaultAgent(name).catch((e) =>
              console.error("自动打开终端失败:", e)
            );
          }
        );
        const unErr = await listen<{ id: string; name: string; error: string }>(
          "git://clone-error",
          (event) => {
            const { id, name, error } = event.payload;
            const s = useStore.getState();
            if (!(id in s.pendingClones)) return;
            s.finishClone(id);
            s.removeProject(name);
            notify("Git 克隆失败", error);
          }
        );
        if (cancelled) {
          unDone();
          unErr();
        } else {
          unlisten = [unDone, unErr];
        }
      } catch {
        // Backend not ready yet — ignore.
      }
    };

    init();
    return () => {
      cancelled = true;
      unlisten.forEach((u) => u());
    };
  }, []);
}
