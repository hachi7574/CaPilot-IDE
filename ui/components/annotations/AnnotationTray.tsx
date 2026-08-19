import { useState } from "react";
import { createPortal } from "react-dom";
import {
  useAnnotations,
  buildFeedbackMarkdown,
  buildElementMarkdown,
  copyText,
  copyElementScreenshot,
  resolveBySelector,
} from "../../state/annotations";
import { useT } from "../../i18n";
import { Icon } from "../Icon";

/**
 * Annotations tray (dev-only). Always expanded when mounted; visibility is
 * owned by DevToolsHost (shared bottom-left dock with Theme Lab).
 */
export function AnnotationTray({ onHide }: { onHide?: () => void }) {
  const t = useT();
  const mode = useAnnotations((s) => s.mode);
  const annotations = useAnnotations((s) => s.annotations);
  const lastElement = useAnnotations((s) => s.lastElement);
  const remove = useAnnotations((s) => s.remove);
  const clear = useAnnotations((s) => s.clear);
  const toggleMode = useAnnotations((s) => s.toggleMode);
  const setMode = useAnnotations((s) => s.setMode);

  const [status, setStatus] = useState("");
  const [busy, setBusy] = useState(false);

  const flash = (msg: string) => {
    setStatus(msg);
    window.setTimeout(() => setStatus(""), 2600);
  };

  const handleHide = () => {
    // Leaving pick-mode when the tray is dismissed avoids a stuck crosshair.
    if (mode) setMode(false);
    onHide?.();
  };

  const handleCopyAll = async () => {
    if (annotations.length) {
      await copyText(buildFeedbackMarkdown(annotations));
      flash(t("annotations.copiedFeedback", { n: annotations.length }));
    } else if (lastElement) {
      await copyText(buildElementMarkdown(lastElement));
      flash(t("annotations.copiedElement"));
    }
  };

  const handleCopyScreenshot = async () => {
    if (!lastElement || busy) return;
    setBusy(true);
    try {
      const el = resolveBySelector(lastElement.selector);
      if (!el) {
        flash(t("annotations.elementGone"));
        return;
      }
      const kind = await copyElementScreenshot(el);
      flash(
        kind === "image"
          ? t("annotations.shotImage")
          : kind === "dataurl"
            ? t("annotations.shotDataUrl")
            : t("annotations.shotFailed")
      );
    } finally {
      setBusy(false);
    }
  };

  const tray = (
    <div className="annot-tray open" data-annot-ui>
      <div className="annot-tray-head">
        <Icon name="message-square" size={13} />
        <span className="annot-tray-title">{t("annotations.tray")}</span>
        <button
          type="button"
          className={`annot-tray-toggle${mode ? " active" : ""}`}
          onClick={(e) => {
            e.stopPropagation();
            toggleMode();
          }}
          title={mode ? t("annotations.exitMode") : t("annotations.enterMode")}
        >
          <Icon name={mode ? "x" : "pencil"} size={11} />
          {mode ? t("annotations.exit") : t("annotations.annotate")}
        </button>
        {annotations.length > 0 && (
          <span className="annot-tray-count">{annotations.length}</span>
        )}
        {onHide && (
          <button
            type="button"
            className="annot-tray-hide"
            onClick={handleHide}
            title={t("annotations.hide")}
            aria-label={t("annotations.hide")}
          >
            ×
          </button>
        )}
      </div>

      <div className="annot-tray-body">
        {mode && <div className="annot-tray-hint">{t("annotations.hint")}</div>}

        {annotations.map((a, i) => (
          <div className="annot-tray-item" key={a.id}>
            <span className={`annot-tray-num annot-num-${a.intent}`}>{i + 1}</span>
            <div className="annot-tray-item-main">
              <div className="annot-tray-item-text">
                {a.text || t("annotations.noDescription")}
              </div>
              <div className="annot-tray-item-el">
                {a.element.component ? `${a.element.component} · ` : ""}
                {a.element.tag}
                {a.element.classes.length ? `.${a.element.classes[0]}` : ""}
              </div>
            </div>
            <button
              type="button"
              className="annot-tray-del"
              title={t("annotations.deleteOne")}
              onClick={() => remove(a.id)}
            >
              ×
            </button>
          </div>
        ))}

        {annotations.length === 0 && (
          <div className="annot-tray-empty">
            <div className="annot-tray-empty-hint">{t("annotations.empty")}</div>
            {lastElement && (
              <div className="annot-tray-lastel">
                <div className="annot-tray-item-el">
                  {lastElement.component ? `${lastElement.component} · ` : ""}
                  {lastElement.tag}
                  {lastElement.classes.length
                    ? `.${lastElement.classes[0]}`
                    : ""}
                  <span className="annot-tray-item-sel">
                    {lastElement.selector}
                  </span>
                </div>
              </div>
            )}
          </div>
        )}

        <div className="annot-tray-actions">
          <button
            type="button"
            className="annot-tray-copy"
            onClick={handleCopyAll}
          >
            {t("annotations.copyAll")}
          </button>
          {annotations.length === 0 && lastElement && (
            <button
              type="button"
              className="annot-tray-shot"
              onClick={handleCopyScreenshot}
            >
              {t("annotations.copyScreenshot")}
            </button>
          )}
          {annotations.length > 0 && (
            <button
              type="button"
              className="annot-tray-clear"
              onClick={() => {
                if (window.confirm(t("annotations.clearConfirm"))) clear();
              }}
            >
              {t("annotations.clear")}
            </button>
          )}
        </div>

        {status && <div className="annot-tray-status">{status}</div>}
      </div>
    </div>
  );

  if (typeof document === "undefined") return tray;
  return createPortal(tray, document.body);
}
