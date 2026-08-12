import { useEffect, useState } from "react";
import {
  useAnnotations,
  buildFeedbackMarkdown,
  buildElementMarkdown,
  copyText,
  copyElementScreenshot,
  resolveBySelector,
} from "../../state/annotations";
import { Icon } from "../Icon";

export function AnnotationTray() {
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
      flash(`已复制 Design Feedback（${annotations.length} 条）`);
    } else if (lastElement) {
      await copyText(buildElementMarkdown(lastElement));
      flash("已复制元素组件信息");
    }
  };

  const handleCopyScreenshot = async () => {
    if (!lastElement || busy) return;
    setBusy(true);
    try {
      const el = resolveBySelector(lastElement.selector);
      if (!el) {
        flash("元素已不在页面中，无法截图");
        return;
      }
      const kind = await copyElementScreenshot(el);
      flash(
        kind === "image"
          ? "截图已复制（图片）"
          : kind === "dataurl"
            ? "截图已复制（data URL 文本）"
            : "截图失败"
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
        title={open ? "收起注释托盘" : "展开注释托盘"}
      >
        <Icon name="message-square" size={13} />
        <span className="annot-tray-title">注释</span>
        <button
          className={`annot-tray-toggle${mode ? " active" : ""}`}
          onClick={(e) => {
            e.stopPropagation();
            toggleMode();
          }}
          title={mode ? "退出标注模式 (Esc)" : "进入标注模式 — 悬停预览组件"}
        >
          <Icon name={mode ? "x" : "pencil"} size={11} />
          {mode ? "退出" : "标注"}
        </button>
        {annotations.length > 0 && (
          <span className="annot-tray-count">{annotations.length}</span>
        )}
        <span className="annot-tray-chevron">{open ? "▾" : "▴"}</span>
      </div>

      {open && (
        <div className="annot-tray-body" onClick={(e) => e.stopPropagation()}>
          {mode && <div className="annot-tray-hint">点击页面元素添加评论 · Esc 退出</div>}

          {annotations.map((a, i) => (
            <div className="annot-tray-item" key={a.id}>
              <span className={`annot-tray-num annot-num-${a.intent}`}>{i + 1}</span>
              <div className="annot-tray-item-main">
                <div className="annot-tray-item-text">{a.text || "（无描述）"}</div>
                <div className="annot-tray-item-el">
                  {a.element.component ? `${a.element.component} · ` : ""}
                  {a.element.tag}
                  {a.element.classes.length ? `.${a.element.classes[0]}` : ""}
                </div>
              </div>
              <button
                className="annot-tray-del"
                title="删除该注释"
                onClick={() => remove(a.id)}
              >
                ×
              </button>
            </div>
          ))}

          {annotations.length === 0 && (
            <div className="annot-tray-empty">
              <div className="annot-tray-empty-hint">
                还没有评论。点击元素添加后，这里会列出全部注释。
              </div>
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
              复制全部
            </button>
            {annotations.length === 0 && lastElement && (
              <button className="annot-tray-shot" onClick={handleCopyScreenshot}>
                复制截图
              </button>
            )}
            {annotations.length > 0 && (
              <button
                className="annot-tray-clear"
                onClick={() => {
                  if (window.confirm("清空全部注释？")) clear();
                }}
              >
                清空
              </button>
            )}
          </div>

          {status && <div className="annot-tray-status">{status}</div>}
        </div>
      )}
    </div>
  );
}
