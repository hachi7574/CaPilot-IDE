/**
 * Theme Editor chrome state (position / visible / size).
 * Visibility is owned by the store (`themeLabEnabled`) so Settings can toggle
 * it; this module only remembers panel geometry.
 */

const POS_KEY = "capilot.themeLab.pos";
const HEIGHT_KEY = "capilot.themeLab.height";

export interface ThemeLabPos {
  x: number;
  y: number;
}

/** Default expanded body height (px). Panel total ≈ head + body. */
export const DEFAULT_BODY_HEIGHT = 420;
export const MIN_BODY_HEIGHT = 180;
export const MAX_BODY_HEIGHT = 900;

const DEFAULT_POS: ThemeLabPos = { x: 12, y: -1 }; // y=-1 → bottom-left at mount

function loadJson<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return fallback;
    return JSON.parse(raw) as T;
  } catch {
    return fallback;
  }
}

export function loadThemeLabPos(): ThemeLabPos {
  const p = loadJson<ThemeLabPos>(POS_KEY, DEFAULT_POS);
  if (typeof p?.x !== "number" || typeof p?.y !== "number") return DEFAULT_POS;
  if (!Number.isFinite(p.x) || !Number.isFinite(p.y)) return DEFAULT_POS;
  return p;
}

export function saveThemeLabPos(pos: ThemeLabPos): void {
  try {
    localStorage.setItem(POS_KEY, JSON.stringify(pos));
  } catch {
    // ignore
  }
}

export function loadThemeLabBodyHeight(): number {
  const v = loadJson<number | null>(HEIGHT_KEY, null);
  if (typeof v !== "number" || !Number.isFinite(v)) return DEFAULT_BODY_HEIGHT;
  return Math.min(MAX_BODY_HEIGHT, Math.max(MIN_BODY_HEIGHT, Math.round(v)));
}

export function saveThemeLabBodyHeight(h: number): void {
  try {
    const clamped = Math.min(
      MAX_BODY_HEIGHT,
      Math.max(MIN_BODY_HEIGHT, Math.round(h))
    );
    localStorage.setItem(HEIGHT_KEY, JSON.stringify(clamped));
  } catch {
    // ignore
  }
}
