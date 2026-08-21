/** Persisted canvas layout prefs (gap + default expanded card size). */

export interface CanvasLayoutPrefs {
  gap: number;
  cardW: number;
  cardH: number;
}

const KEY = "capilot.canvasLayout";
const DEFAULTS: CanvasLayoutPrefs = { gap: 24, cardW: 700, cardH: 700 };

const listeners = new Set<() => void>();

function clampPrefs(p: Partial<CanvasLayoutPrefs>): CanvasLayoutPrefs {
  const n = (v: unknown, lo: number, hi: number, fallback: number) => {
    const x = typeof v === "number" ? v : Number(v);
    if (!Number.isFinite(x)) return fallback;
    return Math.min(hi, Math.max(lo, Math.round(x)));
  };
  return {
    gap: n(p.gap, 0, 400, DEFAULTS.gap),
    cardW: n(p.cardW, 240, 2400, DEFAULTS.cardW),
    cardH: n(p.cardH, 160, 2400, DEFAULTS.cardH),
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
