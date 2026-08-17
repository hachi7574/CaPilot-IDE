import { useEffect, useRef, useState } from "react";
import { quitWithDaemonMode } from "../../state/exitDaemon";

interface ExitDaemonDialogProps {
  onCancel: () => void;
}

/**
 * Shown when the user clicks the window × and Settings → 退出时后台终端 is
 * still "每次询问". Two actions + a "记住我的选择" checkbox; a note points at
 * Settings for later changes.
 */
export function ExitDaemonDialog({ onCancel }: ExitDaemonDialogProps) {
  const [remember, setRemember] = useState(false);
  const [busy, setBusy] = useState(false);
  const keepRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    keepRef.current?.focus();
  }, []);

  const choose = async (mode: "keep" | "kill") => {
    if (busy) return;
    setBusy(true);
    try {
      await quitWithDaemonMode(mode, remember);
      // window.close() usually tears us down before this resolves.
    } catch {
      setBusy(false);
    }
  };

  return (
    <div
      className="permission-confirm-overlay"
      role="presentation"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget && !busy) onCancel();
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape" && !busy) {
          event.preventDefault();
          onCancel();
        }
      }}
    >
      <section
        className="permission-confirm-dialog exit-daemon-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="exit-daemon-title"
        aria-describedby="exit-daemon-desc"
      >
        <div id="exit-daemon-title" className="permission-confirm-title">
          关闭 CaPilot
        </div>
        <p id="exit-daemon-desc" className="exit-daemon-desc">
          后台仍可能有 agent 终端在运行（由守护进程托管）。关闭窗口时要如何处理？
        </p>
        <ul className="exit-daemon-bullets">
          <li>
            <b>保留后台终端</b> — 守护进程与会话继续运行，下次打开可接上
          </li>
          <li>
            <b>结束后台终端</b> — 关闭守护进程并结束所有 agent（更干净，任务会中断）
          </li>
        </ul>
        <label className="exit-daemon-remember">
          <input
            type="checkbox"
            checked={remember}
            disabled={busy}
            onChange={(e) => setRemember(e.target.checked)}
          />
          <span>记住我的选择，下次不再询问</span>
        </label>
        <p className="exit-daemon-hint">
          之后仍可在 <b>设置 → 通用</b> 中的「退出时后台终端」随时更改。
        </p>
        <div className="permission-confirm-actions exit-daemon-actions">
          <button
            type="button"
            className="permission-confirm-btn"
            disabled={busy}
            onClick={onCancel}
          >
            取消
          </button>
          <button
            type="button"
            className="permission-confirm-btn danger"
            disabled={busy}
            onClick={() => void choose("kill")}
          >
            结束后台终端
          </button>
          <button
            ref={keepRef}
            type="button"
            className="permission-confirm-btn primary"
            disabled={busy}
            onClick={() => void choose("keep")}
          >
            保留后台终端
          </button>
        </div>
      </section>
    </div>
  );
}
