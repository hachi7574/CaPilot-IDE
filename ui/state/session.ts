import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useStore, RestoredSession } from "./store";
import { notify } from "./notify";

/** One journaled lifecycle event returned by `agent_sync_events` (§6.2). */
interface JournalEvent {
  seq: number;
  ts: number;
  agent_id: string;
  kind: "exited" | "removed" | "hook_status" | string;
  exit_code?: number | null;
  status?: string | null;
}

/**
 * On app start, restore persisted sessions from sqlite so they survive a
 * restart (DevPlan §6.3). Restored sessions have no live channel yet — the
 * XTermPanel resumes them on tab open.
 *
 * Ended (`done`) sessions are kept in the store so the sidebar's "已结束" group
 * can surface them, but they are NOT re-opened as tabs and never re-promoted to
 * active — reopening a finished conversation is an explicit sidebar action.
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
              workspace_id: rec.workspace_id,
              project: rec.project,
              runtime: rec.runtime,
              status,
              title: rec.title,
              cwd: rec.cwd,
              pid: null,
              mode: rec.mode,
              speed: rec.speed,
              model: rec.model,
              // Session-creation timestamp: anchors the sidebar `tm-time` count-up
              // to the real session start (survives restart). The old code passed
              // `updated_at`, which a stale `.agent-meta.json` / keep-alive could
              // bump forward — the label then looked wrong.
              createdAt: rec.created_at,
            },
            null,
            rec.created_at
          );
          // Ended sessions are recoverable from the sidebar but are not
          // auto-reopened as tabs.
          if (status === "done") continue;
          s.addTabSilent({
            id: rec.id,
            type: "agent",
            agentId: rec.id,
            title: rec.title || rec.runtime,
          });
        }
        s.setSessionsRestored();
      })
      .catch(() => {
        // Backend not ready: the lookup still settled, so reveal the genuine
        // empty state and let the user create a session manually.
        if (!cancelled) useStore.getState().setSessionsRestored();
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
 * - `agent://hook-status` — a status-hook transition (working→idle etc.) from
 *   the daemon's status monitor; drives the same store path as the 1s polling.
 *
 * Every live event carries the journal `event_seq`, which advances the replay
 * watermark. After the listeners are live we call `agent_sync_events` to pull
 * everything journaled while the GUI was offline (natural exits, delete-mode
 * removals, hook transitions) and apply it in order — nothing that happened
 * while we were away is lost (§6.2/§9.4).
 */
export function useAgentEvents() {
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    // High-water mark of journaled lifecycle events already applied to the
    // store. Starts at 0 (this JS module lives for one GUI launch; the daemon
    // journal is per-daemon-incarnation and bounded).
    let appliedSeq = 0;

    const applyReplay = (events: JournalEvent[]) => {
      const s = useStore.getState();
      for (const ev of events) {
        appliedSeq = Math.max(appliedSeq, ev.seq);
        // Only touch agents we know about — the DB restore already reflects
        // done/removed state, so replay only fills the offline window and can
        // never resurrect a long-gone session.
        if (!s.agents.has(ev.agent_id)) continue;
        switch (ev.kind) {
          case "exited":
            s.updateAgentStatus(ev.agent_id, "done");
            break;
          case "removed":
            s.closeTab(ev.agent_id);
            s.removeAgent(ev.agent_id);
            break;
          case "hook_status":
            if (ev.status) {
              // Silent: journal replay after GUI restart must not chime/flash
              // historical working→idle edges as live completions.
              s.setHookStatus(
                ev.agent_id,
                { status: ev.status, ts: ev.ts },
                { silent: true }
              );
            }
            break;
        }
      }
    };

    // listen() resolves asynchronously — cleanup may run before the promise
    // settles, so a late listener must drop itself instead of leaking.
    Promise.all([
      listen<{ id: string; exit_code: number; event_seq: number }>(
        "agent://exited",
        (e) => {
          appliedSeq = Math.max(appliedSeq, e.payload.event_seq ?? 0);
          // Natural exit, record kept (marked done): the open tab stays (grayed),
          // its dead channel stays attached so the last output remains visible.
          // Reopening from the sidebar "已结束" group drops the channel and resumes.
          useStore.getState().updateAgentStatus(e.payload.id, "done");
        }
      ),
      listen<{ id: string; event_seq: number }>("agent://removed", (e) => {
        appliedSeq = Math.max(appliedSeq, e.payload.event_seq ?? 0);
        const s = useStore.getState();
        s.closeTab(e.payload.id);
        s.removeAgent(e.payload.id);
      }),
      // Fast-exit safety net: a session that ended in an immediately-after-spawn
      // boot failure emits this alongside the normal exited/removed event so the
      // reason reaches the user instead of the terminal silently vanishing.
      listen<{ id: string; message: string }>("agent://exit-diagnostic", (e) => {
        notify("dsh 启动失败", e.payload.message);
      }),
      listen<{
        id: string;
        status: string;
        ts: number;
        event_seq: number;
      }>("agent://hook-status", (e) => {
        appliedSeq = Math.max(appliedSeq, e.payload.event_seq ?? 0);
        const s = useStore.getState();
        if (s.agents.has(e.payload.id)) {
          s.setHookStatus(e.payload.id, {
            status: e.payload.status,
            ts: e.payload.ts,
          });
        }
      }),
    ])
      .then(([u1, u2, u3]) => {
        if (cancelled) {
          u1();
          u2();
          u3();
          return;
        }
        unlisten = () => {
          u1();
          u2();
          u3();
        };
        // Replay AFTER the listeners are live: events that race this call are
        // applied by the listeners (idempotent handlers), and their `event_seq`
        // bumps the watermark, so the response can't double-deliver.
        invoke<{ last_seq: number; events: JournalEvent[] }>(
          "agent_sync_events",
          { lastSeq: appliedSeq }
        )
          .then((r) => {
            if (!cancelled) applyReplay(r.events ?? []);
          })
          .catch(() => {
            // Backend not ready — ignore.
          });
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
