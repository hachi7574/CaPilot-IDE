/**
 * Global `acp://event` subscription. Events are filtered by `agentId` inside
 * the store so multi-tab sessions never cross-contaminate.
 */
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useStore } from "./store";
import type { AcpEventPayload } from "./acpTypes";

export function useAcpEvents() {
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    listen<AcpEventPayload>("acp://event", (e) => {
      const payload = e.payload;
      if (!payload || typeof payload !== "object") return;
      const agentId = (payload as { agentId?: string }).agentId;
      if (!agentId) return;
      useStore.getState().applyAcpEvent(payload);
    })
      .then((fn) => {
        if (cancelled) {
          fn();
          return;
        }
        unlisten = fn;
      })
      .catch(() => {
        // Backend / webview not ready — ignore.
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);
}
