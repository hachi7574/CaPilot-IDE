import { useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import { Icon } from "../Icon";

interface ImageViewerPanelProps {
  filePath: string;
}

/** Zoom bounds — relative to the image's natural size. */
const MIN_SCALE = 0.05;
const MAX_SCALE = 64;
/** Multiplicative step per ctrl+wheel tick / toolbar click. */
const ZOOM_STEP = 1.2;
/** Pointer must travel this far before a press becomes a pan drag (so a simple
 *  click on a fitted image doesn't kick it out of fit mode). */
const DRAG_THRESHOLD_PX = 3;

interface ViewState {
  natural: { w: number; h: number } | null;
  box: { w: number; h: number };
  scale: number;
  pan: { x: number; y: number };
  fit: boolean;
}

/** Loaded-image metadata, keyed by the path it belongs to. A stale meta (from a
 *  previously viewed file) is ignored as soon as `filePath` changes — the pane
 *  shows nothing rather than the old file's dimensions. */
interface ImgMeta {
  path: string;
  natural: { w: number; h: number } | null;
  loaded: boolean;
  error: boolean;
}

/** Scale that fits the image inside the viewport (1 when unknown). */
function fitScaleOf(v: ViewState): number {
  if (!v.natural || v.box.w <= 0 || v.box.h <= 0) return 1;
  return Math.min(v.box.w / v.natural.w, v.box.h / v.natural.h);
}

/** Clamp a pan offset on one axis so the scaled image always covers the
 *  viewport when it is larger than the viewport along that axis; otherwise it
 *  stays centered (offset 0). */
function clampAxis(halfImage: number, halfView: number, value: number): number {
  const max = Math.max(0, halfImage - halfView);
  return Math.max(-max, Math.min(max, value));
}

/** Loads a local image via Tauri's asset protocol (`convertFileSrc`) and shows
 *  it fitted to the pane with zoom / pan controls:
 *  - toolbar: zoom out / zoom in / 1:1 (actual size) / 适应 (fit window)
 *  - ctrl+wheel (or ⌘+wheel) zooms around the cursor; plain wheel pans
 *  - drag pans once the image is larger than the viewport (a drag on a fitted
 *    image leaves fit mode at the fitted scale first)
 */
export function ImageViewerPanel({ filePath }: ImageViewerPanelProps) {
  const t = useT();
  const src = convertFileSrc(filePath);
  const viewportRef = useRef<HTMLDivElement>(null);

  const [meta, setMeta] = useState<ImgMeta>({
    path: filePath,
    natural: null,
    loaded: false,
    error: false,
  });
  const metaValid = meta.path === filePath;
  const natural = metaValid ? meta.natural : null;
  const loaded = metaValid && meta.loaded;
  const error = metaValid && meta.error;

  const [box, setBox] = useState({ w: 0, h: 0 });
  const [scale, setScale] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [fit, setFit] = useState(true);
  const [dragging, setDragging] = useState(false);

  // Mirrors the reactive state so the stable wheel listener and pointer
  // handlers always read fresh values without re-subscribing.
  const viewRef = useRef<ViewState>({ natural, box, scale, pan, fit });
  viewRef.current = { natural, box, scale, pan, fit };

  const fitScale = fitScaleOf(viewRef.current);
  const displayScale = fit ? fitScale : scale;

  // Switching files in the same pane (a split leaf swaps tabs without
  // remounting the panel): reset the view transform for the new image. The
  // image meta is keyed by path, so it invalidates on its own.
  useEffect(() => {
    setScale(1);
    setPan({ x: 0, y: 0 });
    setFit(true);
    setDragging(false);
    dragRef.current = null;
  }, [filePath]);

  // Track the viewport box so fit mode centers the image precisely.
  useEffect(() => {
    const el = viewportRef.current;
    if (!el) return;
    const update = () => setBox({ w: el.clientWidth, h: el.clientHeight });
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  /** Clamp both pan axes for a given scale. */
  const clampPan = useCallback((x: number, y: number, s: number) => {
    const v = viewRef.current;
    if (!v.natural) return { x: 0, y: 0 };
    return {
      x: clampAxis((v.natural.w * s) / 2, v.box.w / 2, x),
      y: clampAxis((v.natural.h * s) / 2, v.box.h / 2, y),
    };
  }, []);

  /** Zoom by `factor` around the viewport point `(cx, cy)` (CSS px). Exits fit
   *  mode and keeps the image point under the cursor stationary. */
  const zoomAt = useCallback(
    (cx: number, cy: number, factor: number) => {
      const v = viewRef.current;
      if (!v.natural) return;
      const prev = v.fit ? fitScaleOf(v) : v.scale;
      const imgX = (cx - v.box.w / 2 - v.pan.x) / prev;
      const imgY = (cy - v.box.h / 2 - v.pan.y) / prev;
      const next = Math.max(MIN_SCALE, Math.min(MAX_SCALE, prev * factor));
      const panX = cx - v.box.w / 2 - imgX * next;
      const panY = cy - v.box.h / 2 - imgY * next;
      setFit(false);
      setScale(next);
      setPan(clampPan(panX, panY, next));
    },
    [clampPan]
  );

  /** Pan by a viewport-space delta (plain wheel scroll). No-op while fitted —
   *  a fitted image already fills the viewport, so there is no free space. */
  const panBy = useCallback(
    (dx: number, dy: number) => {
      const v = viewRef.current;
      if (!v.natural || v.fit) return;
      setPan(clampPan(v.pan.x + dx, v.pan.y + dy, v.scale));
    },
    [clampPan]
  );

  const reset = useCallback(() => {
    setFit(true);
    setPan({ x: 0, y: 0 });
  }, []);

  const setActualSize = useCallback(() => {
    setFit(false);
    setScale(1);
    setPan(clampPan(0, 0, 1));
  }, [clampPan]);

  // Ctrl+wheel zooms around the cursor; plain wheel pans. Attached as a native
  // non-passive listener (React marks `wheel` passive at the root, which would
  // swallow preventDefault and let the webview zoom).
  const wheelRef = useRef<(e: WheelEvent) => void>(() => {});
  wheelRef.current = (e) => {
    e.preventDefault();
    const el = viewportRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const cx = e.clientX - rect.left;
    const cy = e.clientY - rect.top;
    if (e.ctrlKey || e.metaKey) {
      zoomAt(cx, cy, e.deltaY < 0 ? ZOOM_STEP : 1 / ZOOM_STEP);
    } else {
      panBy(e.deltaX, e.deltaY);
    }
  };
  useEffect(() => {
    const el = viewportRef.current;
    if (!el) return;
    const handler = (e: WheelEvent) => wheelRef.current(e);
    el.addEventListener("wheel", handler, { passive: false });
    return () => el.removeEventListener("wheel", handler);
  }, []);

  // Drag to pan. The baseline (pan + clamp scale) is captured on pointer down
  // so the drag math doesn't chase state updates mid-gesture.
  const dragRef = useRef<{
    startX: number;
    startY: number;
    panX: number;
    panY: number;
    scale: number;
    active: boolean;
  } | null>(null);

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0) return;
    const v = viewRef.current;
    dragRef.current = {
      startX: e.clientX,
      startY: e.clientY,
      panX: v.pan.x,
      panY: v.pan.y,
      scale: v.fit ? 1 : v.scale,
      active: false,
    };
    e.currentTarget.setPointerCapture(e.pointerId);
  };

  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = dragRef.current;
    if (!d) return;
    const dx = e.clientX - d.startX;
    const dy = e.clientY - d.startY;
    if (!d.active) {
      if (Math.abs(dx) < DRAG_THRESHOLD_PX && Math.abs(dy) < DRAG_THRESHOLD_PX) return;
      d.active = true;
      setDragging(true);
      const v = viewRef.current;
      if (v.fit && v.natural) {
        // Leave fit mode at the fitted scale so the drag can actually move the
        // image (a fitted image already fills the viewport).
        d.scale = fitScaleOf(v);
        d.panX = 0;
        d.panY = 0;
        setFit(false);
        setScale(d.scale);
      }
    }
    setPan(clampPan(d.panX + dx, d.panY + dy, d.scale));
  };

  const onPointerUp = () => {
    dragRef.current = null;
    setDragging(false);
  };

  const zoomPct = Math.round(displayScale * 100);

  return (
    <div className="image-viewer">
      <div className="image-viewer-toolbar">
        <button
          className="image-viewer-btn"
          title={t("image.zoomOut")}
          onClick={() => zoomAt(box.w / 2, box.h / 2, 1 / ZOOM_STEP)}
          disabled={!natural}
        >
          <Icon name="zoom-out" size={13} />
        </button>
        <span className="image-viewer-zoom">{zoomPct}%</span>
        <button
          className="image-viewer-btn"
          title={t("image.zoomIn")}
          onClick={() => zoomAt(box.w / 2, box.h / 2, ZOOM_STEP)}
          disabled={!natural}
        >
          <Icon name="zoom-in" size={13} />
        </button>
        <span className="image-viewer-sep" />
        <button
          className={`image-viewer-btn${!fit && scale === 1 ? " active" : ""}`}
          title={t("image.actualSize")}
          onClick={setActualSize}
          disabled={!natural}
        >
          1:1
        </button>
        <button
          className={`image-viewer-btn${fit ? " active" : ""}`}
          title={t("image.fitWindow")}
          onClick={reset}
          disabled={!natural}
        >
          {t("image.fit")}
        </button>
        {natural && (
          <span className="image-viewer-dims">
            {natural.w} × {natural.h}
          </span>
        )}
      </div>
      <div
        ref={viewportRef}
        className="image-viewer-viewport"
        style={{ cursor: dragging ? "grabbing" : natural ? "grab" : "default" }}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onDoubleClick={reset}
      >
        {error ? (
          <div className="image-viewer-error">
            <Icon name="triangle-alert" size={28} />
            <p>{t("image.loadFailed")}</p>
            <code>{filePath}</code>
          </div>
        ) : (
          <img
            key={filePath}
            src={src}
            alt={filePath}
            draggable={false}
            style={{
              width: natural ? natural.w * displayScale : undefined,
              height: natural ? natural.h * displayScale : undefined,
              transform: `translate(calc(-50% + ${pan.x}px), calc(-50% + ${pan.y}px))`,
              opacity: loaded ? 1 : 0,
            }}
            onLoad={(e) => {
              const el = e.currentTarget;
              // Some SVGs report naturalWidth 0 — fall back to the rendered box.
              const w = el.naturalWidth || Math.round(el.getBoundingClientRect().width);
              const h = el.naturalHeight || Math.round(el.getBoundingClientRect().height);
              setMeta({
                path: filePath,
                natural: w > 0 && h > 0 ? { w, h } : null,
                loaded: true,
                error: false,
              });
            }}
            onError={() =>
              setMeta({ path: filePath, natural: null, loaded: true, error: true })
            }
          />
        )}
      </div>
    </div>
  );
}
