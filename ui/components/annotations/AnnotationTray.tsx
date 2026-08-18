import { useEffect, useState } from "react";
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

export function AnnotationTray() {
  const t = useT();
  const mode = useAnnotations((s) => s.mode);
  const annotations = useAnnotations((s) => s.annotations);
  const lastElement = useAnnotations((s) => s.lastElement);
  const remove = useAnnotations((s) => s.remove);
  const clear = useAnnotations((s) => s.clear);
  const toggleMode = useAnnotations((s) => s.toggleMode);

  const [open, setOpen] = useState(false);
  const [status, setStatus] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (mode) setOpen(true);
  }, [mode]);

  const flash = (msg: string) => {
    setStatus(msg);
    window.setTimeout(() => setStatus(""), 2600);
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

  return (
    <div className={`annot-tray${open ? " open" : ""}`} data-annot-ui>
      <div
        className="annot-tray-head"
        onClick={() => setOpen((o) => !o)}
        title={open ? t("annotations.collapse") : t("annotations.expand")}
      >
        <Icon name="message-square" size={13} />
        <span className="annot-tray-title">{t("annotations.tray")}</span>
        <button
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
        <span className="annot-tray-chevron">{open ? "▾" : "▴"}</span>
      </div>

      {open && (
        <div className="annot-tray-body" onClick={(e) => e.stopPropagation()}>
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
                    {lastElement.classes.length ? `.${lastElement.classes[0]}` : ""}
                    <span className="annot-tray-item-sel">{lastElement.selector}</span>
                  </div>
                </div>
              )}
            </div>
          )}

          <div className="annot-tray-actions">
            <button className="annot-tray-copy" onClick={handleCopyAll}>
              {t("annotations.copyAll")}
            </button>
            {annotations.length === 0 && lastElement && (
              <button className="annot-tray-shot" onClick={handleCopyScreenshot}>
                {t("annotations.copyScreenshot")}
              </button>
            )}
            {annotations.length > 0 && (
              <button
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
      )}
    </div>
  );
}
