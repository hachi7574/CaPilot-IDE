import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { useStore } from "../../state/store";
import {
  THEMES,
  getTheme,
  notifyThemeVarsChanged,
  type Theme,
} from "../../state/themes";
import {
  DEFAULT_BODY_HEIGHT,
  MAX_BODY_HEIGHT,
  MIN_BODY_HEIGHT,
  loadThemeLabBodyHeight,
  loadThemeLabCollapsed,
  loadThemeLabPos,
  saveThemeLabBodyHeight,
  saveThemeLabCollapsed,
  saveThemeLabPos,
  type ThemeLabPos,
} from "../../state/themeLab";
import { useT, themeLabel } from "../../i18n";
import {
  LAB_GROUPS,
  applyInlineOverrides,
  clearInlineOverrides,
  derivedVarsFor,
  snapshotLabVars,
  toColorInputValue,
  type LabToken,
} from "./tokens";

/** Prefer keeping the full panel on-screen; never let it leave the viewport. */
function clampPos(
  pos: ThemeLabPos,
  size: { w: number; h: number } = { w: 388, h: 200 }
): ThemeLabPos {
  const margin = 8;
  // If the panel is larger than the window, pin to the top-left margin so the
  // head/title stays reachable. Otherwise require the full box to fit.
  const maxX = Math.max(margin, window.innerWidth - size.w - margin);
  const maxY = Math.max(margin, window.innerHeight - size.h - margin);
  const x = Number.isFinite(pos.x) ? pos.x : margin;
  const y = Number.isFinite(pos.y) ? pos.y : margin;
  return {
    x: Math.min(Math.max(margin, x), maxX),
    y: Math.min(Math.max(margin, y), maxY),
  };
}

function resolveDefaultPos(
  stored: ThemeLabPos,
  bodyHeight: number
): ThemeLabPos {
  // Head (~34) + body height ≈ panel total.
  const panelW = Math.min(388, window.innerWidth - 16);
  const panelH = Math.min(window.innerHeight - 16, bodyHeight + 40);
  const looksUnset = stored.y < 0 || !Number.isFinite(stored.y);
  if (looksUnset) {
    // Bottom-left default (annotations tray sits bottom-right).
    return clampPos(
      { x: 12, y: Math.max(8, window.innerHeight - panelH - 8) },
      { w: panelW, h: panelH }
    );
  }
  return clampPos(stored, { w: panelW, h: panelH });
}

function clampBodyHeight(h: number): number {
  const maxByViewport = Math.max(
    MIN_BODY_HEIGHT,
    window.innerHeight - 80
  );
  const hardMax = Math.min(MAX_BODY_HEIGHT, maxByViewport);
  if (!Number.isFinite(h)) return DEFAULT_BODY_HEIGHT;
  return Math.min(hardMax, Math.max(MIN_BODY_HEIGHT, Math.round(h)));
}

function buildExportPayload(
  base: Theme,
  draft: Record<string, string>
): Record<string, unknown> {
  const veil = parseFloat(draft["--term-veil"] ?? "1");
  const vars = { ...base.vars, ...draft };
  // Prefer numeric top-level termVeil for authors.
  const out: Record<string, unknown> = {
    id: base.id,
    name: base.name,
    note: base.note,
    swatches: base.swatches,
    colorScheme: base.colorScheme,
    termVeil: Number.isFinite(veil) ? Math.min(1, Math.max(0, veil)) : 1,
    vars,
  };
  if (base.wallpaper) out.wallpaper = base.wallpaper;
  return out;
}

export function ThemeLabPanel({ onHide }: { onHide?: () => void }) {
  const t = useT();
  const themeId = useStore((s) => s.themeId);
  const setThemeId = useStore((s) => s.setThemeId);
  const wallpaperOpacity = useStore((s) => s.wallpaperOpacity);
  const setWallpaperOpacity = useStore((s) => s.setWallpaperOpacity);
  const wallpaperMode = useStore((s) => s.wallpaperMode);
  const locale = useStore((s) => s.locale);

  const [bodyHeight, setBodyHeight] = useState(() =>
    clampBodyHeight(loadThemeLabBodyHeight())
  );
  const [pos, setPos] = useState<ThemeLabPos>(() =>
    resolveDefaultPos(loadThemeLabPos(), clampBodyHeight(loadThemeLabBodyHeight()))
  );
  const [collapsed, setCollapsed] = useState(loadThemeLabCollapsed);
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [baseline, setBaseline] = useState<Record<string, string>>({});
  const [openGroups, setOpenGroups] = useState<Record<string, boolean>>(() => {
    const init: Record<string, boolean> = {};
    for (const g of LAB_GROUPS) init[g.id] = !g.collapsed;
    return init;
  });
  const [status, setStatus] = useState<{
    kind: "ok" | "err" | "";
    text: string;
  }>({ kind: "", text: "" });
  const [dragging, setDragging] = useState(false);

  const panelRef = useRef<HTMLDivElement>(null);
  const posRef = useRef(pos);
  posRef.current = pos;
  const bodyHeightRef = useRef(bodyHeight);
  bodyHeightRef.current = bodyHeight;
  // Track keys we've ever written so Reset can clear them all.
  const touchedKeysRef = useRef<Set<string>>(new Set());
  // Active pointer sessions — kept on window so release outside the head
  // always ends the gesture (the old head-only handlers left dragRef sticky).
  const moveSessionRef = useRef<{
    kind: "move";
    pointerId: number;
    dx: number;
    dy: number;
  } | null>(null);
  const resizeSessionRef = useRef<{
    kind: "resize";
    pointerId: number;
    startY: number;
    startH: number;
  } | null>(null);

  const theme = getTheme(themeId) ?? THEMES[0];

  const resnap = useCallback((id: string) => {
    const cart = getTheme(id);
    if (!cart) return;
    // Clear previous inline overrides before reading computed styles.
    clearInlineOverrides([...touchedKeysRef.current]);
    touchedKeysRef.current.clear();
    // Double-rAF so the browser applies the new data-theme rule first.
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        const snap = snapshotLabVars(cart.vars);
        setBaseline(snap);
        setDraft(snap);
        notifyThemeVarsChanged();
      });
    });
  }, []);

  // Seed / re-seed when the active theme cartridge changes.
  useEffect(() => {
    resnap(themeId);
  }, [themeId, resnap]);

  // Live-paint draft onto <html>.
  useEffect(() => {
    if (!Object.keys(baseline).length) return;
    applyInlineOverrides(draft, baseline);
    for (const k of Object.keys(draft)) {
      if (draft[k] !== baseline[k]) touchedKeysRef.current.add(k);
    }
    notifyThemeVarsChanged();
  }, [draft, baseline]);

  // Cleanup inline styles if the panel unmounts.
  useEffect(() => {
    return () => {
      clearInlineOverrides([...touchedKeysRef.current]);
      touchedKeysRef.current.clear();
      notifyThemeVarsChanged();
    };
  }, []);

  // Keep the panel on-screen after window resize / DPI changes.
  useEffect(() => {
    const reclamp = () => {
      setBodyHeight((h) => {
        const next = clampBodyHeight(h);
        return next === h ? h : next;
      });
      const el = panelRef.current;
      const w = el?.offsetWidth || 388;
      const h = el?.offsetHeight || bodyHeightRef.current + 40;
      setPos((p) => {
        const next = clampPos(p, { w, h });
        if (next.x === p.x && next.y === p.y) return p;
        return next;
      });
    };
    window.addEventListener("resize", reclamp);
    const raf = requestAnimationFrame(reclamp);
    return () => {
      window.removeEventListener("resize", reclamp);
      cancelAnimationFrame(raf);
    };
  }, []);

  const setToken = useCallback((name: string, value: string) => {
    setDraft((prev) => {
      const next = { ...prev, [name]: value };
      const derived = derivedVarsFor(name, value);
      for (const [k, v] of Object.entries(derived)) next[k] = v;
      return next;
    });
  }, []);

  const resetToken = useCallback(
    (name: string) => {
      setDraft((prev) => {
        const next = { ...prev };
        const baseVal = baseline[name];
        if (baseVal != null) next[name] = baseVal;
        else delete next[name];
        // Also reset derived companions of this token.
        const derived = derivedVarsFor(name, baseVal ?? "");
        for (const k of Object.keys(derived)) {
          if (baseline[k] != null) next[k] = baseline[k];
          else delete next[k];
        }
        return next;
      });
    },
    [baseline]
  );

  const flash = useCallback((kind: "ok" | "err", text: string) => {
    setStatus({ kind, text });
    window.setTimeout(() => setStatus({ kind: "", text: "" }), 2400);
  }, []);

  const resetAll = useCallback(() => {
    clearInlineOverrides([...touchedKeysRef.current]);
    touchedKeysRef.current.clear();
    setDraft({ ...baseline });
    notifyThemeVarsChanged();
    flash("ok", t("themeLab.resetDone"));
  }, [baseline, t, flash]);

  // ── Drag / resize via window listeners ──────────────────────
  // Capture on window so pointerup outside the head (or lost capture) always
  // clears the session. Head-only handlers previously left dragRef sticky:
  // release → move back over panel → onPointerMove re-fired with leftover dx/dy.
  useEffect(() => {
    const onMove = (e: PointerEvent) => {
      const move = moveSessionRef.current;
      if (move && e.pointerId === move.pointerId) {
        const panel = panelRef.current;
        const w = panel?.offsetWidth ?? 320;
        const h = panel?.offsetHeight ?? bodyHeightRef.current + 40;
        const next = clampPos(
          { x: e.clientX - move.dx, y: e.clientY - move.dy },
          { w, h }
        );
        setPos(next);
        return;
      }
      const resize = resizeSessionRef.current;
      if (resize && e.pointerId === resize.pointerId) {
        const next = clampBodyHeight(
          resize.startH + (e.clientY - resize.startY)
        );
        setBodyHeight(next);
      }
    };

    const endSession = (e: PointerEvent) => {
      const move = moveSessionRef.current;
      if (move && e.pointerId === move.pointerId) {
        moveSessionRef.current = null;
        setDragging(false);
        saveThemeLabPos(posRef.current);
        document.body.classList.remove("tl-dragging");
        return;
      }
      const resize = resizeSessionRef.current;
      if (resize && e.pointerId === resize.pointerId) {
        resizeSessionRef.current = null;
        saveThemeLabBodyHeight(bodyHeightRef.current);
        // After height change, keep panel inside the viewport.
        const panel = panelRef.current;
        if (panel) {
          const next = clampPos(posRef.current, {
            w: panel.offsetWidth,
            h: panel.offsetHeight,
          });
          setPos(next);
          saveThemeLabPos(next);
        }
        document.body.classList.remove("tl-resizing");
      }
    };

    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", endSession);
    window.addEventListener("pointercancel", endSession);
    // Safety: if the window loses focus mid-drag, drop the session.
    const onBlur = () => {
      if (moveSessionRef.current) {
        moveSessionRef.current = null;
        setDragging(false);
        saveThemeLabPos(posRef.current);
        document.body.classList.remove("tl-dragging");
      }
      if (resizeSessionRef.current) {
        resizeSessionRef.current = null;
        saveThemeLabBodyHeight(bodyHeightRef.current);
        document.body.classList.remove("tl-resizing");
      }
    };
    window.addEventListener("blur", onBlur);
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", endSession);
      window.removeEventListener("pointercancel", endSession);
      window.removeEventListener("blur", onBlur);
      document.body.classList.remove("tl-dragging", "tl-resizing");
    };
  }, []);

  // Persist pos whenever it changes outside an active drag (resize reclamp etc.).
  useEffect(() => {
    if (moveSessionRef.current) return;
    saveThemeLabPos(pos);
  }, [pos]);

  useEffect(() => {
    if (resizeSessionRef.current) return;
    saveThemeLabBodyHeight(bodyHeight);
  }, [bodyHeight]);

  const startDrag = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    const target = e.target as HTMLElement | null;
    if (target?.closest("button, input, textarea, select, a")) return;
    e.preventDefault();
    e.stopPropagation();
    const el = panelRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    moveSessionRef.current = {
      kind: "move",
      pointerId: e.pointerId,
      dx: e.clientX - r.left,
      dy: e.clientY - r.top,
    };
    setDragging(true);
    document.body.classList.add("tl-dragging");
  };

  const startResize = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    e.preventDefault();
    e.stopPropagation();
    resizeSessionRef.current = {
      kind: "resize",
      pointerId: e.pointerId,
      startY: e.clientY,
      startH: bodyHeightRef.current,
    };
    document.body.classList.add("tl-resizing");
  };

  const toggleCollapsed = () => {
    setCollapsed((c) => {
      const next = !c;
      saveThemeLabCollapsed(next);
      return next;
    });
  };

  // ── Export helpers ──────────────────────────────────────────
  const exportJson = useMemo(() => {
    if (!theme) return "{}";
    return JSON.stringify(buildExportPayload(theme, draft), null, 2);
  }, [theme, draft]);

  const copyJson = async () => {
    try {
      await navigator.clipboard.writeText(exportJson);
      flash("ok", t("themeLab.copied"));
    } catch {
      flash("err", t("themeLab.copyFailed"));
    }
  };

  const downloadJson = () => {
    const blob = new Blob([exportJson], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${theme?.id ?? "theme"}-lab.json`;
    a.click();
    URL.revokeObjectURL(url);
    flash("ok", t("themeLab.downloaded"));
  };

  const saveToDisk = async (saveAs: boolean) => {
    try {
      let path: string | null = null;
      if (saveAs) {
        const picked = await save({
          defaultPath: `${theme?.id ?? "theme"}-lab.json`,
          filters: [{ name: "Theme JSON", extensions: ["json"] }],
        });
        path = typeof picked === "string" ? picked : null;
      } else {
        // Overwrite the cartridge in the repo if we can resolve a path via dialog
        // defaulting to themes/<id>.json name — still goes through save() so the
        // user confirms the location (dev machines differ).
        const picked = await save({
          defaultPath: `themes/${theme?.id ?? "theme"}.json`,
          filters: [{ name: "Theme JSON", extensions: ["json"] }],
        });
        path = typeof picked === "string" ? picked : null;
      }
      if (!path) return;
      await invoke("fs_write", { path, content: exportJson });
      flash("ok", t("themeLab.saved", { path }));
    } catch (e) {
      flash("err", t("themeLab.saveFailed", { err: String(e) }));
    }
  };

  const dirty = useMemo(() => {
    for (const k of Object.keys(draft)) {
      if (draft[k] !== baseline[k]) return true;
    }
    return false;
  }, [draft, baseline]);

  const renderTokenRow = (tok: LabToken) => {
    const value = draft[tok.name] ?? "";
    const isDirty = value !== (baseline[tok.name] ?? "");
    const label =
      (tok.labelKey && t(`themeLab.token.${tok.labelKey}`)) || tok.name;

    return (
      <div className="tl-row" key={tok.name}>
        <div className="tl-row-label" title={tok.name}>
          {label}
        </div>
        <div className="tl-row-controls">
          {tok.kind === "color" && (
            <input
              className="tl-color"
              type="color"
              value={toColorInputValue(value)}
              onChange={(e) => setToken(tok.name, e.target.value)}
              aria-label={label}
            />
          )}
          {tok.kind === "ratio" ? (
            <>
              <input
                className="tl-range"
                type="range"
                min={0}
                max={1}
                step={0.01}
                value={Number.isFinite(parseFloat(value)) ? parseFloat(value) : 1}
                onChange={(e) => setToken(tok.name, e.target.value)}
              />
              <span className="tl-ratio-val">
                {Number.isFinite(parseFloat(value))
                  ? parseFloat(value).toFixed(2)
                  : "—"}
              </span>
            </>
          ) : (
            <input
              className="tl-text"
              type="text"
              value={value}
              spellCheck={false}
              onChange={(e) => setToken(tok.name, e.target.value)}
            />
          )}
          <button
            type="button"
            className="tl-reset"
            disabled={!isDirty}
            title={t("themeLab.resetOne")}
            onClick={() => resetToken(tok.name)}
          >
            ↺
          </button>
        </div>
      </div>
    );
  };

  const panel = (
    <div
      ref={panelRef}
      className={`tl-panel${collapsed ? " collapsed" : ""}${
        dragging ? " dragging" : ""
      }`}
      style={{ left: pos.x, top: pos.y }}
      data-theme-lab
    >
      <div
        className="tl-head"
        onPointerDown={startDrag}
        title={t("themeLab.dragHint")}
      >
        <span className="tl-title">{t("themeLab.title")}</span>
        <button
          type="button"
          className="tl-head-btn"
          onClick={toggleCollapsed}
          title={collapsed ? t("themeLab.expand") : t("themeLab.collapse")}
        >
          {collapsed ? "+" : "–"}
        </button>
        {onHide && (
          <button
            type="button"
            className="tl-head-btn"
            onClick={onHide}
            title={t("themeLab.hide")}
            aria-label={t("themeLab.hide")}
          >
            ×
          </button>
        )}
      </div>

      {!collapsed && (
        <>
          <div className="tl-body" style={{ height: bodyHeight }}>
            <div className="tl-chips">
              {THEMES.map((th) => {
                const label = themeLabel(locale, th.id) ?? {
                  name: th.name,
                  note: th.note,
                };
                return (
                  <button
                    key={th.id}
                    type="button"
                    className={`tl-chip${th.id === themeId ? " active" : ""}`}
                    title={label.note}
                    onClick={() => setThemeId(th.id)}
                  >
                    <span className="tl-chip-swatches">
                      {th.swatches.map((c, i) => (
                        <i key={i} style={{ background: c }} />
                      ))}
                    </span>
                    <span className="tl-chip-name">{label.name}</span>
                  </button>
                );
              })}
            </div>

            <div className="tl-wallpaper-row">
              <label htmlFor="tl-wp-opacity">
                {t("themeLab.wallpaperOpacity")}
                {wallpaperMode === "off"
                  ? ` (${t("themeLab.wallpaperOff")})`
                  : ""}
              </label>
              <input
                id="tl-wp-opacity"
                className="tl-range"
                type="range"
                min={0}
                max={1}
                step={0.01}
                value={wallpaperOpacity}
                onChange={(e) =>
                  setWallpaperOpacity(parseFloat(e.target.value))
                }
              />
              <span className="tl-ratio-val">
                {wallpaperOpacity.toFixed(2)}
              </span>
            </div>

            {LAB_GROUPS.map((g) => {
              const open = openGroups[g.id] !== false;
              return (
                <div className="tl-group" key={g.id}>
                  <button
                    type="button"
                    className="tl-group-head"
                    onClick={() =>
                      setOpenGroups((s) => ({ ...s, [g.id]: !open }))
                    }
                  >
                    <span>{open ? "▾" : "▸"}</span>
                    <span>{t(`themeLab.group.${g.labelKey}`)}</span>
                  </button>
                  {open && (
                    <div className="tl-group-body">
                      {g.tokens.map(renderTokenRow)}
                    </div>
                  )}
                </div>
              );
            })}

            <div className="tl-actions">
              <button type="button" onClick={resetAll} disabled={!dirty}>
                {t("themeLab.resetAll")}
              </button>
              <button type="button" onClick={() => void copyJson()}>
                {t("themeLab.copy")}
              </button>
              <button type="button" onClick={downloadJson}>
                {t("themeLab.download")}
              </button>
              <button
                type="button"
                className="primary"
                onClick={() => void saveToDisk(false)}
              >
                {t("themeLab.save")}
              </button>
              <button type="button" onClick={() => void saveToDisk(true)}>
                {t("themeLab.saveAs")}
              </button>
            </div>
            <div className={`tl-status${status.kind ? ` ${status.kind}` : ""}`}>
              {status.text}
            </div>
            <div className="tl-hint">{t("themeLab.hint")}</div>
          </div>

          <div
            className="tl-resize"
            onPointerDown={startResize}
            title={t("themeLab.resizeHint")}
            role="separator"
            aria-orientation="horizontal"
            aria-label={t("themeLab.resizeHint")}
          />
        </>
      )}
    </div>
  );

  // Portal out of `.app` (position:relative + z-index) so fixed positioning and
  // stacking are always relative to the viewport, not a nested context.
  if (typeof document === "undefined") return panel;
  return createPortal(panel, document.body);
}
