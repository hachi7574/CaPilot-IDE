import { useEffect, useRef, useState } from "react";
import {
  useAnnotations,
  pickElement,
  elementInfo,
  resolveBySelector,
  AnnotElementInfo,
  AnnotIntent,
} from "../../state/annotations";
import { useT } from "../../i18n";

/** uid for a new annotation (crypto.randomUUID with a fallback). */
function uid(): string {
  if (typeof crypto !== "undefined" && crypto.randomUUID) return crypto.randomUUID();
  return `ann-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

interface Pending {
  info: AnnotElementInfo;
  el: Element;
}

/**
 * Annotation selection layer. Mounted once at the app root. When `mode` is on it
 * shows a crosshair + hover highlight over the whole IDE, lets the user click an
 * element to attach a comment, and keeps the numbered markers of every existing
 * annotation pinned to their (selector-resolved) element.
 */
export function AnnotationLayer() {
  const mode = useAnnotations((s) => s.mode);
  const annotations = useAnnotations((s) => s.annotations);
  const setMode = useAnnotations((s) => s.setMode);
  const add = useAnnotations((s) => s.add);
  const setLastElement = useAnnotations((s) => s.setLastElement);

  const [pending, setPending] = useState<Pending | null>(null);
  const [popupPos, setPopupPos] = useState<{ x: number; y: number }>({ x: 0, y: 0 });
  const [markerPos, setMarkerPos] = useState<Record<string, { left: number; top: number } | null>>({});

  const highlightRef = useRef<HTMLDivElement>(null);
  const nameRef = useRef<HTMLDivElement>(null);
  const hoverElRef = useRef<Element | null>(null);

  // Crosshair cursor over the whole window while selecting.
  useEffect(() => {
    document.body.classList.toggle("annot-mode", mode);
    return () => document.body.classList.remove("annot-mode");
  }, [mode]);

  // Selection interaction (mouse move / click / Escape).
  useEffect(() => {
    if (!mode) return;

    /** Pick the element under the pointer (element + its box). */
    const unitAt = (x: number, y: number): { el: Element; rect: DOMRect } | null => {
      const el = pickElement(x, y);
      if (!el) return null;
      const rect = el.getBoundingClientRect();
      if (rect.width < 1 || rect.height < 1) return null;
      return { el, rect };
    };

    let raf = 0;
    let lastX = 0;
    let lastY = 0;

    const onMove = (e: MouseEvent) => {
      lastX = e.clientX;
      lastY = e.clientY;
      if (raf) return; // one hit-test + repaint per frame (same as Orca)
      raf = requestAnimationFrame(() => {
        raf = 0;
        const hl = highlightRef.current;
        const nameEl = nameRef.current;
        if (!hl || !nameEl) return;
        // While a comment popup is open, keep the purple fill pinned to the
        // clicked element instead of following the cursor.
        const pinned = pendingRef.current;
        let unit: { el: Element; rect: DOMRect } | null = null;
        if (pinned) {
          const rect = pinned.el.getBoundingClientRect();
          unit = rect.width > 1 && rect.height > 1 ? { el: pinned.el, rect } : null;
        } else {
          unit = unitAt(lastX, lastY);
        }
        if (!unit) {
          hl.style.display = "none";
          nameEl.style.display = "none";
          return;
        }
        const r = unit.rect;
        hl.style.display = "block";
        hl.style.left = `${r.left}px`;
        hl.style.top = `${r.top}px`;
        hl.style.width = `${r.width}px`;
        hl.style.height = `${r.height}px`;
        nameEl.style.display = "block";
        nameEl.style.left = `${r.left}px`;
        nameEl.style.top = `${r.top < 26 ? r.top + 4 : r.top - 26}px`;
        if (hoverElRef.current !== unit.el) {
          hoverElRef.current = unit.el;
          const info = elementInfo(unit.el);
          // Write the label straight into the chip (imperative, like the rest of
          // the overlay) so it appears in the same frame as the fill.
          nameEl.textContent = `${info.component ? `${info.component} · ` : ""}${info.tag}${info.classes.length ? `.${info.classes[0]}` : ""}`;
        }
      });
    };

    const onClick = (e: MouseEvent) => {
      const t = e.target as HTMLElement | null;
      if (!t) return;
      if (t.closest("[data-annot-ui]")) return; // our chrome handles its own clicks
      // First-run onboarding sits above the app chrome and must stay clickable
      // even if annotation mode is left on — let its own buttons handle clicks.
      if (t.closest(".onboarding-overlay")) return;
      e.preventDefault();
      e.stopPropagation();
      if (pendingRef.current) {
        setPending(null);
        return;
      }
      const unit = unitAt(e.clientX, e.clientY);
      if (!unit) return;
      const info = elementInfo(unit.el);
      setLastElement(info);
      setPending({ info, el: unit.el });
      setPopupPos({
        x: Math.min(e.clientX + 14, window.innerWidth - 360),
        y: Math.min(e.clientY + 18, window.innerHeight - 330),
      });
    };

    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      if (pendingRef.current) setPending(null);
      else setMode(false);
    };

    window.addEventListener("mousemove", onMove);
    window.addEventListener("click", onClick, true);
    window.addEventListener("keydown", onKey);
    return () => {
      if (raf) cancelAnimationFrame(raf);
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("click", onClick, true);
      window.removeEventListener("keydown", onKey);
      document.body.classList.remove("annot-mode");
    };
  }, [mode, setMode, setLastElement]);

  // Keep the pending reference reachable from the capture-phase click handler.
  const pendingRef = useRef<Pending | null>(null);
  pendingRef.current = pending;

  // Numbered markers: re-resolve each selector so badges follow elements across
  // re-renders (tabs opening/closing, split resize, …).
  useEffect(() => {
    if (!annotations.length) {
      setMarkerPos({});
      return;
    }
    const update = () => {
      const next: Record<string, { left: number; top: number } | null> = {};
      for (const a of annotations) {
        const el = resolveBySelector(a.selector);
        if (!el) {
          next[a.id] = null;
          continue;
        }
        const r = el.getBoundingClientRect();
        if (r.bottom < 0 || r.top > window.innerHeight || r.right < 0 || r.left > window.innerWidth) {
          next[a.id] = null;
          continue;
        }
        next[a.id] = { left: r.left - 7, top: r.top - 7 };
      }
      setMarkerPos(next);
    };
    update();
    const iv = setInterval(update, 600);
    window.addEventListener("resize", update);
    return () => {
      clearInterval(iv);
      window.removeEventListener("resize", update);
    };
  }, [annotations]);

  return (
    <>
      {mode && (
        <div className="annot-layer" data-annot-overlay>
          <div className="annot-highlight" ref={highlightRef} />
          {/* Always mounted (content set imperatively in the rAF handler); the
              ref must be valid or the hover highlight would never render. */}
          <div className="annot-name" ref={nameRef} />
          {pending && (
            <CommentPopup
              x={popupPos.x}
              y={popupPos.y}
              info={pending.info}
              onAdd={(text, intent) => {
                add({
                  id: uid(),
                  selector: pending.info.selector,
                  element: pending.info,
                  text,
                  intent,
                  createdAt: Date.now(),
                });
                setPending(null);
              }}
              onCancel={() => setPending(null)}
            />
          )}
        </div>
      )}
      {Object.keys(markerPos).length > 0 && (
        <div className="annot-markers" data-annot-overlay>
          {annotations.map((a, i) => {
            const p = markerPos[a.id];
            if (!p) return null;
            return (
              <div
                key={a.id}
                className={`annot-marker annot-marker-${a.intent}`}
                style={{ left: p.left, top: p.top }}
                data-annot-overlay
              >
                {i + 1}
              </div>
            );
          })}
        </div>
      )}
    </>
  );
}

/* ── Comment popup ────────────────────────────────────────────── */

function CommentPopup({
  x,
  y,
  info,
  onAdd,
  onCancel,
}: {
  x: number;
  y: number;
  info: AnnotElementInfo;
  onAdd: (text: string, intent: AnnotIntent) => void;
  onCancel: () => void;
}) {
  const t = useT();
  const [text, setText] = useState("");
  const [intent, setIntent] = useState<AnnotIntent>("change");
  // Local position so dragging can move the popup; seeded from the click point.
  const [pos, setPos] = useState<{ x: number; y: number }>(() => ({ x, y }));
  const dragRef = useRef<{ dx: number; dy: number } | null>(null);

  const cls = info.classes.slice(0, 4).join(" ");

  // Drag anywhere on the popup (the textarea and buttons keep their own
  // pointer semantics — grabbing those doesn't start a drag). Pointer capture
  // keeps the drag alive even when the cursor leaves the box.
  const startDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    const target = e.target as HTMLElement | null;
    if (target?.closest("textarea, button")) return;
    e.stopPropagation();
    e.preventDefault();
    const el = e.currentTarget;
    el.setPointerCapture(e.pointerId);
    const r = el.getBoundingClientRect();
    dragRef.current = { dx: e.clientX - r.left, dy: e.clientY - r.top };
  };

  const onDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = dragRef.current;
    if (!d) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const w = Math.max(rect.width, 200);
    const h = Math.max(rect.height, 100);
    setPos({
      x: Math.max(8, Math.min(e.clientX - d.dx, window.innerWidth - w - 8)),
      y: Math.max(8, Math.min(e.clientY - d.dy, window.innerHeight - h - 8)),
    });
  };

  const endDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!dragRef.current) return;
    dragRef.current = null;
    try {
      e.currentTarget.releasePointerCapture(e.pointerId);
    } catch {
      // capture may already be released
    }
  };

  return (
    <div
      className="annot-popup"
      style={{ left: pos.x, top: pos.y }}
      data-annot-ui
      onPointerDown={startDrag}
      onPointerMove={onDrag}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
    >
      <div className="annot-popup-head" title={t("annotations.popupDrag")}>
        <span className="annot-popup-title">{t("annotations.popupTitle")}</span>
        <span className="annot-popup-el" title={info.selector}>
          {info.component ? `${info.component} · ` : ""}
          {info.tag}
          {cls ? `.${cls.replace(/\s+/g, ".")}` : ""}
        </span>
      </div>
      <textarea
        className="annot-popup-text"
        autoFocus
        placeholder={t("annotations.popupPlaceholder")}
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            onAdd(text, intent);
          }
          if (e.key === "Escape") onCancel();
        }}
      />
      <div className="annot-popup-intents">
        <button
          className={`annot-intent-btn${intent === "change" ? " active" : ""}`}
          onClick={() => setIntent("change")}
        >
          {t("annotations.change")}
        </button>
        <button
          className={`annot-intent-btn question${intent === "question" ? " active" : ""}`}
          onClick={() => setIntent("question")}
        >
          {t("annotations.question")}
        </button>
        <button
          className={`annot-intent-btn error${intent === "error" ? " active" : ""}`}
          onClick={() => setIntent("error")}
        >
          {t("annotations.error")}
        </button>
      </div>
      <div className="annot-popup-actions">
        <button className="annot-popup-cancel" onClick={onCancel}>
          {t("common.cancel")}
        </button>
        <button
          className="annot-popup-add"
          onClick={() => onAdd(text, intent)}
          disabled={!text.trim()}
        >
          {t("annotations.add")}
        </button>
      </div>
    </div>
  );
}
