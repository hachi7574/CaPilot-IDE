import { useT } from "../../i18n";

export function CanvasToolbar({
  onAdd,
  onFit,
  onArrange,
  canArrange,
}: {
  onAdd: () => void;
  onFit: () => void;
  onArrange: () => void;
  canArrange: boolean;
}) {
  const t = useT();
  return (
    <div className="canvas-toolbar">
      <button
        type="button"
        className="canvas-toolbar-btn"
        title={t("canvas.addTerminal")}
        onClick={onAdd}
      >
        {t("canvas.addTerminal")}
      </button>
      <button
        type="button"
        className="canvas-toolbar-btn"
        title={t("canvas.fitView")}
        onClick={onFit}
      >
        {t("canvas.fitView")}
      </button>
      <button
        type="button"
        className="canvas-toolbar-btn"
        title={t("canvas.arrangeGrid")}
        onClick={onArrange}
        disabled={!canArrange}
      >
        {t("canvas.arrangeGrid")}
      </button>
    </div>
  );
}
