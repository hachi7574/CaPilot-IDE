import { memo } from "react";
import {
  effectiveAgentStatus,
  ACTIVE_WINDOW_MS,
  useStore,
} from "../../state/store";
import { Icon, runtimeIcon } from "../Icon";
import { useT } from "../../i18n";
import { XTermPanel } from "../terminal/XTermPanel";

export const CanvasNodeCard = memo(function CanvasNodeCard({
  agentId,
  kind,
  selected,
  marked,
  showPty,
  mountPty = true,
  onSelect,
  onDoubleClick,
  onPointerDownDrag,
  onConnectPointerDown,
  onCardContextMenu,
}: {
  agentId: string;
  kind: "terminal" | "console";
  selected: boolean;
  /** Marquee / multi-select mark — distinct from keyboard/click focus. */
  marked?: boolean;
  showPty: boolean;
  /** False while a create/drop appear spring is still scaling the card.
   *  WebView2 drops the xterm canvas backing store if we mount under CSS scale(0). */
  mountPty?: boolean;
  onSelect: (e: React.MouseEvent) => void;
  onDoubleClick: () => void;
  onPointerDownDrag: (e: React.PointerEvent<HTMLDivElement>) => void;
  onConnectPointerDown?: (e: React.PointerEvent<HTMLDivElement>) => void;
  onHide?: () => void;
  onCardContextMenu?: (e: React.MouseEvent) => void;
}) {
  const t = useT();
  const agent = useStore((s) => s.agents.get(agentId));
  const connected = useStore((s) => s.agentChannels.has(agentId));
  const activeAt = useStore((s) => s.agentActiveAt.get(agentId) ?? 0);
  const hook = useStore((s) => s.hookStatus.get(agentId) ?? null);
  const submittedAt = useStore((s) => s.agentSubmittedAt.get(agentId));
  const unread = useStore((s) => s.unreadCompletion.has(agentId));
  const active = Date.now() - activeAt < ACTIVE_WINDOW_MS;
  if (!agent) {
    if (!agentId.startsWith("pending:")) return null;
    const runtime = agentId.split(":")[1] ?? "";
    return (
      <div className={`canvas-card expanded${kind === "console" ? " console" : ""}`}>
        <div className="canvas-card-title canvas-card-drag">
          <Icon name={runtimeIcon(runtime)} size={12} />
          <span className="canvas-card-name">{t("common.loading")}</span>
        </div>
        <div className="canvas-card-pty" />
      </div>
    );
  }
  const status = effectiveAgentStatus(agent, connected, active, hook, submittedAt);
  const completed = status === "idle" && unread;
  const statusLabel = completed
    ? t("status.completed")
    : t(`status.${status}`);
  const statusClass = completed ? "st-completed" : `st-${status}`;

  return (
    <div
      className={`canvas-card expanded${selected ? " selected" : ""}${marked ? " marked" : ""}${kind === "console" ? " console" : ""}`}
      draggable={false}
      onDragStart={(e) => e.preventDefault()}
      onPointerDown={(e) => {
        if ((e.target as HTMLElement).closest("button, .canvas-connect-handle, .canvas-card-pty")) return;
        e.preventDefault();
        onPointerDownDrag(e);
      }}
      onClick={(e) => {
        e.stopPropagation();
        onSelect(e);
      }}
      onDoubleClick={(e) => {
        e.stopPropagation();
        onDoubleClick();
      }}
      onContextMenu={(e) => {
        e.preventDefault();
        e.stopPropagation();
        onCardContextMenu?.(e);
      }}
    >
      <div className="canvas-card-title canvas-card-drag">
        <Icon name={runtimeIcon(agent.runtime)} size={12} />
        <span className="canvas-card-name">
          {agent.title} <span className={`tab-status ${statusClass}`}>{statusLabel}</span>
        </span>
      </div>
      {showPty && (
        <div
          className="canvas-card-pty"
          onPointerDown={(e) => {
            e.stopPropagation();
            if (e.button !== 0) return;
            const card = e.currentTarget.closest(".canvas-card") as HTMLElement | null;
            const surface = e.currentTarget.closest(".canvas-surface") as HTMLElement | null;
            card?.classList.add("selecting-text");
            surface?.classList.add("selecting");
            const clear = () => {
              card?.classList.remove("selecting-text");
              surface?.classList.remove("selecting");
              window.removeEventListener("pointerup", clear, true);
              window.removeEventListener("pointercancel", clear, true);
            };
            window.addEventListener("pointerup", clear, true);
            window.addEventListener("pointercancel", clear, true);
          }}
          onMouseDown={(e) => e.stopPropagation()}
          onWheel={(e) => e.stopPropagation()}
          onContextMenuCapture={(e) => {
            e.preventDefault();
            e.stopPropagation();
            onCardContextMenu?.(e);
          }}
        >
          {mountPty && <XTermPanel agentId={agentId} active={selected} opaqueBg />}
        </div>
      )}
      <div
        className="canvas-card-grip canvas-card-grip-l"
        onPointerDown={(e) => {
          e.preventDefault();
          e.stopPropagation();
          onPointerDownDrag(e);
        }}
      />
      <div
        className="canvas-card-grip canvas-card-grip-r"
        onPointerDown={(e) => {
          e.preventDefault();
          e.stopPropagation();
          onPointerDownDrag(e);
        }}
      />
      <div
        className="canvas-card-grip canvas-card-grip-b"
        onPointerDown={(e) => {
          e.preventDefault();
          e.stopPropagation();
          onPointerDownDrag(e);
        }}
      />
      {kind === "terminal" && onConnectPointerDown && (
        <div
          className="canvas-connect-handle"
          title={t("canvas.connectHint")}
          onPointerDown={onConnectPointerDown}
          onClick={(e) => e.stopPropagation()}
          onDoubleClick={(e) => e.stopPropagation()}
        />
      )}
    </div>
  );
});
