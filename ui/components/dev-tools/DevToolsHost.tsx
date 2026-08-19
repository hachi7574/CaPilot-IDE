import { useState, type ReactNode } from "react";
import { DevToolChip, DevToolsDock } from "./DevToolsDock";
import {
  loadAnnotationsVisible,
  saveAnnotationsVisible,
} from "../../state/devTools";
import { useT } from "../../i18n";
import "./dev-tools-host.css";

export type DevToolId = "annotations";

/**
 * Shared host for dev-only floating tools. Renders a bottom-left chip dock
 * with one button per *hidden* tool. Theme Editor is no longer hosted here
 * (it ships in production and is toggled from Settings).
 */
export function DevToolsHost({
  tools,
}: {
  tools: Array<{
    id: DevToolId;
    /** Panel node when visible. Receives an onHide callback. */
    render: (api: { onHide: () => void }) => ReactNode;
    /** Optional badge on the restore chip (e.g. annotation count). */
    badge?: number | string;
    labelKey: string;
    showKey: string;
    /** Shortcut hint shown in the chip title, e.g. "Ctrl+Shift+T". */
    shortcutHint?: string;
  }>;
}) {
  const t = useT();
  const [visible, setVisible] = useState<Record<DevToolId, boolean>>(() => ({
    annotations: loadAnnotationsVisible(),
  }));

  const setToolVisible = (id: DevToolId, next: boolean) => {
    setVisible((prev) => {
      if (prev[id] === next) return prev;
      saveAnnotationsVisible(next);
      return { ...prev, [id]: next };
    });
  };

  const hidden = tools.filter((tool) => !visible[tool.id]);

  return (
    <>
      {tools.map((tool) =>
        visible[tool.id] ? (
          <div key={tool.id} className="dev-tool-slot">
            {tool.render({ onHide: () => setToolVisible(tool.id, false) })}
          </div>
        ) : null
      )}

      {hidden.length > 0 && (
        <DevToolsDock>
          {hidden.map((tool) => (
            <DevToolChip
              key={tool.id}
              label={t(tool.labelKey)}
              ariaLabel={t(tool.showKey)}
              title={
                tool.shortcutHint
                  ? `${t(tool.labelKey)} (${tool.shortcutHint})`
                  : t(tool.labelKey)
              }
              badge={tool.badge}
              onClick={() => setToolVisible(tool.id, true)}
            />
          ))}
        </DevToolsDock>
      )}
    </>
  );
}
