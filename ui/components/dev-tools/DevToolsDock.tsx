import { createPortal } from "react-dom";
import type { ReactNode } from "react";
import "./dev-tools-dock.css";

/**
 * Bottom-left chip row for hidden dev tools (Theme Lab / Annotations).
 * Portaled to document.body so it sits above app chrome.
 */
export function DevToolsDock({ children }: { children: ReactNode }) {
  const dock = (
    <div className="dev-tools-dock" data-dev-tools-dock>
      {children}
    </div>
  );
  if (typeof document === "undefined") return dock;
  return createPortal(dock, document.body);
}

export function DevToolChip({
  label,
  title,
  ariaLabel,
  onClick,
  badge,
}: {
  label: string;
  title?: string;
  ariaLabel: string;
  onClick: () => void;
  badge?: number | string;
}) {
  return (
    <button
      type="button"
      className="dev-tool-chip"
      title={title}
      aria-label={ariaLabel}
      onClick={onClick}
    >
      <span className="dev-tool-chip-label">{label}</span>
      {badge != null && badge !== 0 && badge !== "" && (
        <span className="dev-tool-chip-badge">{badge}</span>
      )}
    </button>
  );
}
