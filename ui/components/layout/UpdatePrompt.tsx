import { useStore } from "../../state/store";
import { downloadAndInstall } from "../../state/update";
import { Icon } from "../Icon";

/**
 * In-app prompt when a newer release is available.
 *
 * Startup only used a system notification before — on Linux/Wayland that is
 * often silent or blocked, so users never saw the update. This banner is the
 * primary surface; the system notification remains a secondary cue.
 */
export function UpdatePrompt() {
  const status = useStore((s) => s.updateStatus);
  const latest = useStore((s) => s.updateLatest);
  const current = useStore((s) => s.currentVersion);
  const notes = useStore((s) => s.updateNotes);
  const installable = useStore((s) => s.updateInstallable);
  const downloading = useStore((s) => s.updateDownloading);
  const progress = useStore((s) => s.updateProgress);
  const bytesDownloaded = useStore((s) => s.updateBytesDownloaded);
  const dismissed = useStore((s) => s.updatePromptDismissedVersion);

  if (status !== "available" || !latest) return null;
  if (dismissed && dismissed === latest) return null;

  const pct =
    downloading && progress != null ? Math.round(progress * 100) : null;
  const bytesLabel =
    downloading && progress == null && bytesDownloaded != null
      ? `${(bytesDownloaded / (1024 * 1024)).toFixed(1)} MB`
      : null;

  return (
    <div
      className="update-prompt"
      role="status"
      aria-live="polite"
      data-downloading={downloading || undefined}
    >
      <div className="update-prompt-icon" aria-hidden>
        <Icon name="download" size={16} />
      </div>
      <div className="update-prompt-body">
        <div className="update-prompt-title">
          发现新版本 v{latest}
          {current ? (
            <span className="update-prompt-current">（当前 v{current}）</span>
          ) : null}
        </div>
        {notes ? (
          <div className="update-prompt-notes" title={notes}>
            {notes.split("\n").find((l) => l.trim()) ?? ""}
          </div>
        ) : (
          <div className="update-prompt-notes">可下载并安装更新</div>
        )}
        {pct != null ? (
          <div className="update-prompt-progress">
            <div className="update-prompt-progress-track">
              <div
                className="update-prompt-progress-bar"
                style={{ width: `${pct}%` }}
              />
            </div>
            <span>{pct}%</span>
          </div>
        ) : bytesLabel ? (
          <div className="update-prompt-progress">
            <span>已下载 {bytesLabel}</span>
          </div>
        ) : null}
      </div>
      <div className="update-prompt-actions">
        <button
          type="button"
          className="update-prompt-install"
          onClick={() => downloadAndInstall()}
          disabled={!installable || downloading}
          title={
            installable
              ? "下载并安装新版本"
              : "开发构建不支持自动安装，请使用发布包"
          }
        >
          {downloading ? "下载中…" : "立即更新"}
        </button>
        <button
          type="button"
          className="update-prompt-later"
          onClick={() =>
            useStore.setState({ updatePromptDismissedVersion: latest })
          }
          disabled={downloading}
        >
          稍后
        </button>
      </div>
    </div>
  );
}
