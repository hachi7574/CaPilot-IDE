import { useEffect } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { useStore, UpdateStatus } from "./store";
import { notify } from "./notify";

/**
 * App self-update slice.
 *
 * `useUpdateSync` runs once at app mount: it reads the real app version, loads
 * the 启动时自动检查更新 preference, and (when enabled) schedules a background
 * check a few seconds later so startup isn't delayed by a slow update server.
 */
export function useUpdateSync() {
  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    (async () => {
      // Real version straight from the bundle (single source: Cargo.toml).
      try {
        const v = await getVersion();
        if (!cancelled) useStore.setState({ currentVersion: v });
      } catch {
        // Not running under Tauri (plain vite) — keep null.
      }

      // Load the auto-check preference (default on when unset).
      let autoCheck = true;
      try {
        const raw = await invoke<string | null>("setting_get", {
          key: "auto_check_update",
        });
        if (raw !== null) autoCheck = raw !== "false";
      } catch {
        // Backend not ready — keep default.
      }

      // Load the completion-chime preference (default on when unset).
      let soundOn = true;
      try {
        const raw = await invoke<string | null>("setting_get", {
          key: "sound_enabled",
        });
        if (raw !== null) soundOn = raw !== "false";
      } catch {
        // Backend not ready — keep default.
      }
      if (cancelled) return;
      useStore.setState({ autoCheckUpdate: autoCheck, soundEnabled: soundOn });
      if (!autoCheck) return;

      // Background check a few seconds after mount. Never blocks startup; a
      // failure just flips the status to "error" (visible in Settings → 关于).
      timer = setTimeout(() => {
        if (!cancelled) checkForUpdate({ notifyOnFound: true }).catch(() => {});
      }, 3000);
    })();

    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, []);
}

interface CheckOptions {
  /** Surface a desktop notification when a new version is found. Startup uses
   *  this; manual "检查更新" clicks rely on the inline UI and pass nothing. */
  notifyOnFound?: boolean;
}

/** Ask the backend for the latest version and fold the result into the store.
 *  Idempotent: a check already in flight (or a download in progress) is a no-op. */
export async function checkForUpdate(opts: CheckOptions = {}): Promise<void> {
  const s = useStore.getState();
  if (s.updateStatus === "checking" || s.updateDownloading) return;

  useStore.setState({ updateStatus: "checking", updateError: null });
  try {
    const res = await invoke<UpdateStatus>("update_check");
    // A newer release cancels any prior "稍后" dismiss for an older tag so the
    // in-app banner can reappear for the new version.
    const prevDismissed = useStore.getState().updatePromptDismissedVersion;
    const dismissStillValid =
      res.available &&
      res.latestVersion &&
      prevDismissed &&
      prevDismissed === res.latestVersion
        ? prevDismissed
        : res.available
          ? null
          : prevDismissed;
    useStore.setState({
      updateStatus: res.available ? "available" : "up-to-date",
      updateLatest: res.latestVersion,
      updateNotes: res.notes,
      updateInstallable: res.installable,
      updateError: null,
      updateCheckedAt: Date.now(),
      // Prefer the server's current version when our bootstrap hasn't landed.
      currentVersion: res.currentVersion || useStore.getState().currentVersion,
      updatePromptDismissedVersion: dismissStillValid,
    });

    // Secondary system notification (often silent on Linux/Wayland). The
    // primary surface is the in-app UpdatePrompt, which watches updateStatus.
    if (
      opts.notifyOnFound &&
      res.available &&
      res.latestVersion &&
      res.latestVersion !== s.updateNotifiedVersion
    ) {
      useStore.setState({ updateNotifiedVersion: res.latestVersion });
      notify(
        "CaPilot 有新版本",
        `v${res.latestVersion} 已可用 — 可在应用内提示中升级，或打开 设置 → 关于。`
      );
    }
  } catch (e) {
    useStore.setState({
      updateStatus: "error",
      updateError: String(e),
      updateCheckedAt: Date.now(),
    });
  }
}

/** Download + install the pending update, streaming progress to the store.
 *  On success the backend relaunches the app, so this typically never resolves.
 *
 *  Progress channel encoding (from Rust):
 *  - `0..1`  fraction when Content-Length is known
 *  - `>= 2`  `2 + bytesDownloaded` when length is unknown (show MB instead) */
export async function downloadAndInstall(): Promise<void> {
  const s = useStore.getState();
  if (s.updateDownloading) return;

  useStore.setState({
    updateDownloading: true,
    updateProgress: 0,
    updateBytesDownloaded: 0,
  });
  const channel = new Channel<number>();
  channel.onmessage = (p) => {
    if (typeof p !== "number" || Number.isNaN(p)) return;
    if (p >= 2) {
      // Indeterminate: absolute bytes encoded as 2 + bytes.
      useStore.setState({
        updateProgress: null,
        updateBytesDownloaded: Math.max(0, Math.round(p - 2)),
      });
      return;
    }
    useStore.setState({
      updateProgress: Math.min(1, Math.max(0, p)),
      updateBytesDownloaded: null,
    });
  };

  try {
    await invoke("update_download_and_install", { onProgress: channel });
    // App was relaunched on success — this line usually never runs.
    useStore.setState({
      updateDownloading: false,
      updateProgress: null,
      updateBytesDownloaded: null,
    });
  } catch (e) {
    useStore.setState({
      updateDownloading: false,
      updateProgress: null,
      updateBytesDownloaded: null,
      updateStatus: "error",
      updateError: String(e),
    });
  }
}
