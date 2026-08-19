/**
 * Dev-only Theme Lab chrome state (position / collapsed / visible / size).
 * Kept out of the main session store — production never imports this module
 * (dynamic import behind import.meta.env.DEV).
 */

const POS_KEY = "capilot.themeLab.pos";
const COLLAPSED_KEY = "capilot.themeLab.collapsed";
const VISIBLE_KEY = "capilot.themeLab.visible";
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

export function loadThemeLabCollapsed(): boolean {
  return loadJson<boolean>(COLLAPSED_KEY, false) === true;
}

export function saveThemeLabCollapsed(v: boolean): void {
  try {
    localStorage.setItem(COLLAPSED_KEY, JSON.stringify(v));
  } catch {
    // ignore
  }
}

export function loadThemeLabVisible(): boolean {
  // Default visible in dev so the tool is discoverable.
  const v = loadJson<boolean | null>(VISIBLE_KEY, null);
  return v === null ? true : v === true;
}

export function saveThemeLabVisible(v: boolean): void {
  try {
    localStorage.setItem(VISIBLE_KEY, JSON.stringify(v));
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
