import { useEffect } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { useStore, UpdateStatus } from "./store";
import { notify } from "./notify";

/**
 * App self-update slice (docs/version-update-design.md).
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
    useStore.setState({
      updateStatus: res.available ? "available" : "up-to-date",
      updateLatest: res.latestVersion,
      updateNotes: res.notes,
      updateInstallable: res.installable,
      updateError: null,
    });

    // Announce a freshly-discovered update once per app launch (dedup by
    // version so re-checks don't spam the notification).
    if (
      opts.notifyOnFound &&
      res.available &&
      res.latestVersion &&
      res.latestVersion !== s.updateNotifiedVersion
    ) {
      useStore.setState({ updateNotifiedVersion: res.latestVersion });
      notify(
        "CaPilot 有新版本",
        `v${res.latestVersion} 已可用，可在 设置 → 关于 中升级。`
      );
    }
  } catch (e) {
    useStore.setState({
      updateStatus: "error",
      updateError: String(e),
    });
  }
}

/** Download + install the pending update, streaming 0..1 progress to the store.
 *  On success the backend relaunches the app, so this typically never resolves. */
export async function downloadAndInstall(): Promise<void> {
  const s = useStore.getState();
  if (s.updateDownloading) return;

  useStore.setState({ updateDownloading: true, updateProgress: 0 });
  const channel = new Channel<number>();
  channel.onmessage = (p) => {
    const progress = typeof p === "number" ? Math.min(1, Math.max(0, p)) : 0;
    useStore.setState({ updateProgress: progress });
  };

  try {
    await invoke("update_download_and_install", { onProgress: channel });
    // App was relaunched on success — this line usually never runs.
    useStore.setState({ updateDownloading: false, updateProgress: null });
  } catch (e) {
    useStore.setState({
      updateDownloading: false,
      updateProgress: null,
      updateStatus: "error",
      updateError: String(e),
    });
  }
}
