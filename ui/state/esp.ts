import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useStore } from "./store";

/**
 * Hook to keep ESP status in sync with the Rust backend.
 * - Fetches current status on mount
 * - Subscribes to `esp://event` for live updates
 * Call `connectEsp` / `disconnectEsp` from UI actions.
 */
export function useEspSync() {
  useEffect(() => {
    let unlisten: (() => void) | undefined;

    const init = async () => {
      try {
        const status = await invoke("esp_status");
        useStore.getState().setEspStatus(status as never);
      } catch {
        // Backend not ready yet — ignore.
      }
      unlisten = await listen("esp://event", (event) => {
        const payload = event.payload as any;
        if (!payload) return;
        const s = useStore.getState();
        switch (payload.event) {
          case "connected":
            s.setEspStatus({
              connected: true,
              name: payload.name ?? null,
              address: payload.address ?? null,
            });
            break;
          case "disconnected":
            s.setEspStatus({
              connected: false,
              battery_pct: null,
              battery_mv: null,
              last_seen_ms: null,
            });
            break;
          case "telemetry":
            s.setEspStatus({
              connected: true,
              battery_pct: payload.battery_pct ?? null,
              battery_mv: payload.battery_mv ?? null,
              last_seen_ms: Date.now(),
            });
            break;
        }
      });
    };

    init();
    return () => unlisten?.();
  }, []);
}

/** Connect to the ESP over BLE. */
export async function connectEsp(): Promise<string | null> {
  const s = useStore.getState();
  s.setEspConnecting(true);
  try {
    await invoke("esp_connect");
    return null;
  } catch (e) {
    return String(e);
  } finally {
    useStore.getState().setEspConnecting(false);
  }
}

/** Disconnect from the ESP. */
export async function disconnectEsp(): Promise<void> {
  try {
    await invoke("esp_disconnect");
  } catch {
    // ignore
  }
}
