import { useEffect, useRef, useState } from "react";
import { quitWithDaemonMode } from "../../state/exitDaemon";
import { useT } from "../../i18n";

interface ExitDaemonDialogProps {
  onCancel: () => void;
}

/**
 * Shown when the user clicks the window ×, Settings → exit-daemon mode is still
 * "ask", and at least one agent still has a live PTY. Empty / all-dormant /
 * all-ended sessions skip this dialog (see `handleTitlebarClose`). Two actions
 * + a "remember" checkbox; a note points at Settings for later changes.
 */
export function ExitDaemonDialog({ onCancel }: ExitDaemonDialogProps) {
  const t = useT();
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
          {t("exitDialog.title")}
        </div>
        <p id="exit-daemon-desc" className="exit-daemon-desc">
          {t("exitDialog.desc")}
        </p>
        <ul className="exit-daemon-bullets">
          <li>
            <b>{t("exitDialog.keepBulletTitle")}</b> — {t("exitDialog.keepBulletBody")}
          </li>
          <li>
            <b>{t("exitDialog.killBulletTitle")}</b> — {t("exitDialog.killBulletBody")}
          </li>
        </ul>
        <label className="exit-daemon-remember">
          <input
            type="checkbox"
            checked={remember}
            disabled={busy}
            onChange={(e) => setRemember(e.target.checked)}
          />
          <span>{t("exitDialog.remember")}</span>
        </label>
        <p className="exit-daemon-hint">{t("exitDialog.hint")}</p>
        <div className="permission-confirm-actions exit-daemon-actions">
          <button
            type="button"
            className="permission-confirm-btn"
            disabled={busy}
            onClick={onCancel}
          >
            {t("common.cancel")}
          </button>
          <button
            type="button"
            className="permission-confirm-btn danger"
            disabled={busy}
            onClick={() => void choose("kill")}
          >
            {t("exitDialog.kill")}
          </button>
          <button
            ref={keepRef}
            type="button"
            className="permission-confirm-btn primary"
            disabled={busy}
            onClick={() => void choose("keep")}
          >
            {t("exitDialog.keep")}
          </button>
        </div>
      </section>
    </div>
  );
}
