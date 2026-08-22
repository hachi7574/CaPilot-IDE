/** Persisted canvas layout prefs (gap + default expanded card size + click sync). */

export interface CanvasLayoutPrefs {
  gap: number;
  cardW: number;
  cardH: number;
  /** Clicking a canvas card pins Composer send target to that session. Default on. */
  selectSyncsSendTarget: boolean;
}

export const CANVAS_LAYOUT_LIMITS = {
  gap: { min: 0, max: 400 },
  cardW: { min: 240, max: 2400 },
  cardH: { min: 160, max: 2400 },
} as const;

export const CANVAS_LAYOUT_DEFAULTS: CanvasLayoutPrefs = {
  gap: 24,
  cardW: 700,
  cardH: 600,
  selectSyncsSendTarget: true,
};

const KEY = "capilot.canvasLayout";
const DEFAULTS = CANVAS_LAYOUT_DEFAULTS;

const listeners = new Set<() => void>();

function clampPrefs(p: Partial<CanvasLayoutPrefs>): CanvasLayoutPrefs {
  const n = (v: unknown, lo: number, hi: number, fallback: number) => {
    const x = typeof v === "number" ? v : Number(v);
    if (!Number.isFinite(x)) return fallback;
    return Math.min(hi, Math.max(lo, Math.round(x)));
  };
  return {
    gap: n(p.gap, CANVAS_LAYOUT_LIMITS.gap.min, CANVAS_LAYOUT_LIMITS.gap.max, DEFAULTS.gap),
    cardW: n(p.cardW, CANVAS_LAYOUT_LIMITS.cardW.min, CANVAS_LAYOUT_LIMITS.cardW.max, DEFAULTS.cardW),
    cardH: n(p.cardH, CANVAS_LAYOUT_LIMITS.cardH.min, CANVAS_LAYOUT_LIMITS.cardH.max, DEFAULTS.cardH),
    selectSyncsSendTarget:
      typeof p.selectSyncsSendTarget === "boolean"
        ? p.selectSyncsSendTarget
        : DEFAULTS.selectSyncsSendTarget,
  };
}

function read(): CanvasLayoutPrefs {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return { ...DEFAULTS };
    return clampPrefs(JSON.parse(raw) as Partial<CanvasLayoutPrefs>);
  } catch {
    return { ...DEFAULTS };
  }
}

let current = read();

export function getCanvasLayoutPrefs(): CanvasLayoutPrefs {
  return current;
}

export function setCanvasLayoutPrefs(next: Partial<CanvasLayoutPrefs>): CanvasLayoutPrefs {
  current = clampPrefs({ ...current, ...next });
  try {
    localStorage.setItem(KEY, JSON.stringify(current));
  } catch {
    /* ignore */
  }
  for (const l of listeners) l();
  return current;
}

export function subscribeCanvasLayoutPrefs(fn: () => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}
