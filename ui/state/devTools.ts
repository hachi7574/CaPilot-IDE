/**
 * Shared chrome for dev-only floating tools (Theme Lab, Annotations).
 * Visibility / layout only — production never imports this module.
 */

const ANNOT_VISIBLE_KEY = "capilot.annotations.visible";

function loadJson<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return fallback;
    return JSON.parse(raw) as T;
  } catch {
    return fallback;
  }
}

export function loadAnnotationsVisible(): boolean {
  // Default visible in dev so the tool is discoverable.
  const v = loadJson<boolean | null>(ANNOT_VISIBLE_KEY, null);
  return v === null ? true : v === true;
}

export function saveAnnotationsVisible(v: boolean): void {
  try {
    localStorage.setItem(ANNOT_VISIBLE_KEY, JSON.stringify(v));
  } catch {
    // ignore
  }
}
