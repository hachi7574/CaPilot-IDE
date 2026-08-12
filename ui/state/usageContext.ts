import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useStore, AgentInfo, AgentUsage, HookStatus, effectiveAgentStatus } from "./store";

/** Poll interval for per-agent live context-window occupancy. The `agent://usage`
 *  push event is the primary low-latency source; polling covers runtimes (e.g.
 *  OMP) whose live context is only exposed on demand. ~3s per the design doc. */
const POLL_MS = 3000;

/** Runtimes whose adapter can supply live context-window occupancy. claude,
 *  codex, and opencode implement `context_usage`; bash falls back to the trait
 *  default `None` and must never show a ring or be polled. */
const CONTEXT_USAGE_RUNTIMES = new Set(["claude", "codex", "opencode"]);

/** Whether an agent's runtime exposes a context-window meter at all. */
export function supportsContextUsage(runtime: string | undefined): boolean {
  return !!runtime && CONTEXT_USAGE_RUNTIMES.has(runtime);
}

/**
 * Decide whether an agent counts as "running/initializing" for the context
 * meter — the state where missing data shows a loading ring and polling stays
 * active. Reuses the store's `effectiveAgentStatus` derivation so the meter and
 * the poller agree on the same notion.
 *
 * `active` is deliberately pinned to `true`: the 2s activity window exists for
 * the tab strip, but a long-running quiet task (e.g. a 10s thinking phase)
 * must keep its loading ring and keep being polled. The rest of
 * `effectiveAgentStatus` still applies: `done`/`failed` are terminal, a
 * restored-but-unresumed session (no PTY channel) is `dormant` → idle, and
 * `busy`/`waiting_input` are authoritative regardless of connectivity.
 */
export function isContextMeterActive(
  agent: AgentInfo | undefined,
  connected: boolean,
  hook?: HookStatus | null
): boolean {
  if (!agent) return false;
  // Runtimes without a `context_usage` implementation (bash terminals) never
  // render a meter — don't keep them "active" (which would spin the ring).
  if (!supportsContextUsage(agent.runtime)) return false;
  // Explicit terminal/lifecycle states are authoritative: an `idle` record is
  // idle regardless of connectivity (missing data then renders nothing).
  if (agent.status === "done" || agent.status === "failed" || agent.status === "idle") {
    return false;
  }
  const status = effectiveAgentStatus(agent, connected, /* active */ true, hook);
  return status === "running" || status === "waiting_input";
}

/**
 * Live context-window usage sync (docs/context-window-usage.md).
 *
 * Three sources reconciled into `agent.last_usage`:
 *  - an immediate poll when the composer's active agent changes (opening or
 *    switching to an agent tab) — the meter populates without waiting for the
 *    next scheduled tick;
 *  - `agent://usage` push events (backend `agent_context_usage` emits them; the
 *    provider adapter produces a fresh sample during a turn / at completion);
 *  - a ~3s poll of `agent_context_usage` for every running/initializing agent,
 *    with one final sample on the running→idle transition, then polling stops
 *    while the agent is idle/dormant/done/failed.
 *
 * Agents without a cwd are skipped (no real session to ask the backend about).
 * Listeners and timers are cleaned up on unmount; `cancelled` guards late
 * async resolutions (StrictMode double-mount / backend-not-ready).
 */
export function useContextUsageSync() {
  // The composer renders the meter only for the active tab's agent. Whenever
  // that agent changes (opening / switching to an agent tab), poll it once
  // immediately instead of waiting up to POLL_MS for the next scheduled tick —
  // otherwise a freshly opened agent keeps its loading ring for up to 3s even
  // though its transcript already carries usage data.
  const activeAgentId = useStore((s) => {
    const tab = s.tabs.find((t) => t.id === s.activeTabId);
    return tab?.agentId;
  });

  useEffect(() => {
    if (!activeAgentId) return;
    const agent = useStore.getState().agents.get(activeAgentId);
    if (!agent || !agent.cwd || !supportsContextUsage(agent.runtime)) return;
    let cancelled = false;
    invoke<AgentUsage | null>("agent_context_usage", { id: activeAgentId })
      .then((usage) => {
        if (!cancelled) useStore.getState().updateAgentUsage(activeAgentId, usage);
      })
      .catch(() => {
        // Backend not ready or no such session — keep the last value.
      });
    return () => {
      cancelled = true;
    };
  }, [activeAgentId]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    // Last-seen "active" flag per agent id — detects the running→idle edge so
    // one final poll captures the settled usage before polling stops.
    let wasActive = new Map<string, boolean>();

    const pollOne = async (id: string) => {
      try {
        const usage = await invoke<AgentUsage | null>("agent_context_usage", {
          id,
        });
        if (!cancelled) useStore.getState().updateAgentUsage(id, usage);
      } catch {
        // Backend not ready or no such session — keep the last value.
      }
    };

    const tick = () => {
      const s = useStore.getState();
      const nextActive = new Map<string, boolean>();
      for (const [id, agent] of s.agents) {
        const connected = s.agentChannels.has(id);
        const active = isContextMeterActive(agent, connected, s.hookStatus.get(id));
        nextActive.set(id, active);
        if (!agent.cwd || !supportsContextUsage(agent.runtime)) continue;
        if (active) {
          void pollOne(id);
        } else if (wasActive.get(id)) {
          // running → idle transition: one final sample, then stop polling.
          void pollOne(id);
        }
      }
      wasActive = nextActive;
    };

    const init = async () => {
      try {
        const un = await listen<{ id: string; usage: AgentUsage | null }>(
          "agent://usage",
          (event) => {
            if (cancelled) return;
            const { id, usage } = event.payload;
            useStore.getState().updateAgentUsage(id, usage);
          }
        );
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

    void init();
    tick(); // immediate first poll (catches already-running sessions)
    const timer = setInterval(tick, POLL_MS);
    return () => {
      cancelled = true;
      unlisten?.();
      clearInterval(timer);
    };
  }, []);
}
