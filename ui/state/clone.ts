import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useStore } from "./store";
import { spawnAgent } from "./agentActions";
import { notify } from "./notify";
import { t } from "../i18n";

/**
 * Reconciles background `git_clone` results with the store.
 *
 * `git_clone` returns the moment the clone starts; the actual clone runs in the
 * backend and reports on `git://cloned` / `git://clone-error`. This hook turns
 * those events into store updates:
 *  - cloned      → clear the "正在克隆中" flag, record the root, auto-open a
 *                  terminal in the freshly cloned project;
 *  - clone-error → drop the placeholder project, surface the git error.
 *
 * The frontend mints the clone id and calls `beginClone` *before* `invoke`, so a
 * fast clone cannot deliver its completion event before the id is tracked. As a
 * belt-and-suspenders, completion is also matched by project name: any pending
 * entry for `name` is cleared even if the id was somehow missing. A late event
 * after webview reload (no pending entry at all) just reconciles the project
 * list idempotently and never auto-spawns / auto-drops.
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
            const wasPending =
              id in s.pendingClones ||
              Object.values(s.pendingClones).includes(name);
            // Always clear pending for this id *and* name so a raced beginClone
            // can't leave the sidebar stuck on "正在克隆中".
            s.finishClone(id);
            s.finishCloneByName(name);
            s.setProjectRoots({ [name]: root });
            if (!wasPending) {
              // Late event (webview reload mid-clone): just surface the project.
              s.addProject(name, root);
              return;
            }
            // Auto-open a fresh agent terminal in the clone (best-effort).
            spawnAgent(name).catch((e) =>
              console.error(t("clone.autoOpenFailed"), e)
            );
          }
        );
        const unErr = await listen<{ id: string; name: string; error: string }>(
          "git://clone-error",
          (event) => {
            const { id, name, error } = event.payload;
            const s = useStore.getState();
            const wasPending =
              id in s.pendingClones ||
              Object.values(s.pendingClones).includes(name);
            s.finishClone(id);
            s.finishCloneByName(name);
            // Only drop / notify for clones this session started. A stale error
            // after reload must not delete a project the user may have fixed.
            if (!wasPending) return;
            s.removeProject(name);
            notify(t("clone.failed"), error);
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
