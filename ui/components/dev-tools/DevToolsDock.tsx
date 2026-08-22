import { useEffect, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import "./dev-tools-dock.css";

/**
 * Bottom-left chip row for hidden tools (Theme Editor / Annotations).
 * Multiple callers share one dock so chips sit in a single row instead of
 * stacking on top of each other.
 */
let sharedDock: HTMLDivElement | null = null;

function ensureDock(): HTMLDivElement {
  if (sharedDock && document.body.contains(sharedDock)) return sharedDock;
  const el = document.createElement("div");
  el.className = "dev-tools-dock";
  el.setAttribute("data-dev-tools-dock", "");
  document.body.appendChild(el);
  sharedDock = el;
  return el;
}

export function DevToolsDock({ children }: { children: ReactNode }) {
  const [slot, setSlot] = useState<HTMLElement | null>(null);
  useEffect(() => {
    const dock = ensureDock();
    const host = document.createElement("div");
    host.style.display = "contents";
    dock.appendChild(host);
    setSlot(host);
    return () => {
      host.remove();
      if (dock.childElementCount === 0) {
        dock.remove();
        if (sharedDock === dock) sharedDock = null;
      }
    };
  }, []);
  if (!slot) return null;
  return createPortal(children, slot);
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
