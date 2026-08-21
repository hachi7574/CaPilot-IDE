/** App-level keyboard shortcuts. Defaults are the current hard-coded chords;
 *  Settings → Shortcuts can remap them. Persist under `capilot.shortcuts`. */

export type ShortcutId =
  | "newTerminal"
  | "search"
  | "focusToggle"
  | "themeLab";

export interface Chord {
  key: string;
  ctrl: boolean;
  shift: boolean;
  alt: boolean;
  meta: boolean;
}

export const DEFAULT_SHORTCUTS: Record<ShortcutId, Chord> = {
  newTerminal: { key: "t", ctrl: true, shift: false, alt: false, meta: false },
  search: { key: "f", ctrl: true, shift: false, alt: false, meta: false },
  focusToggle: { key: "F1", ctrl: false, shift: false, alt: false, meta: false },
  themeLab: { key: "t", ctrl: true, shift: true, alt: false, meta: false },
};

const STORAGE_KEY = "capilot.shortcuts";

function normalizeKey(key: string): string {
  if (key.length === 1) return key.toLowerCase();
  return key;
}

export function chordFromEvent(e: KeyboardEvent): Chord {
  return {
    key: normalizeKey(e.key),
    ctrl: e.ctrlKey,
    shift: e.shiftKey,
    alt: e.altKey,
    meta: e.metaKey,
  };
}

export function chordsEqual(a: Chord, b: Chord): boolean {
  return (
    normalizeKey(a.key) === normalizeKey(b.key) &&
    a.ctrl === b.ctrl &&
    a.shift === b.shift &&
    a.alt === b.alt &&
    a.meta === b.meta
  );
}

export function formatChord(c: Chord): string {
  const parts: string[] = [];
  if (c.ctrl) parts.push("Ctrl");
  if (c.shift) parts.push("Shift");
  if (c.alt) parts.push("Alt");
  if (c.meta) parts.push("Meta");
  const k = c.key.length === 1 ? c.key.toUpperCase() : c.key;
  parts.push(k);
  return parts.join("+");
}

export function isEditableTarget(e: KeyboardEvent): boolean {
  const el = e.target as HTMLElement | null;
  if (!el) return false;
  if (el.isContentEditable) return true;
  const tag = el.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT";
}

function isModifierOnly(key: string): boolean {
  return (
    key === "Control" ||
    key === "Shift" ||
    key === "Alt" ||
    key === "Meta" ||
    key === "OS"
  );
}

export function isRecordableKey(e: KeyboardEvent): boolean {
  return !isModifierOnly(e.key);
}

export function loadShortcuts(): Record<ShortcutId, Chord> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULT_SHORTCUTS };
    const parsed = JSON.parse(raw) as Partial<Record<ShortcutId, Chord>>;
    const out = { ...DEFAULT_SHORTCUTS };
    for (const id of Object.keys(DEFAULT_SHORTCUTS) as ShortcutId[]) {
      const c = parsed[id];
      if (
        c &&
        typeof c.key === "string" &&
        typeof c.ctrl === "boolean" &&
        typeof c.shift === "boolean" &&
        typeof c.alt === "boolean" &&
        typeof c.meta === "boolean"
      ) {
        out[id] = {
          key: normalizeKey(c.key),
          ctrl: c.ctrl,
          shift: c.shift,
          alt: c.alt,
          meta: c.meta,
        };
      }
    }
    return out;
  } catch {
    return { ...DEFAULT_SHORTCUTS };
  }
}

export function saveShortcuts(map: Record<ShortcutId, Chord>) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(map));
  } catch {
    /* ignore */
  }
}

export function matchesShortcut(
  e: KeyboardEvent,
  id: ShortcutId,
  map: Record<ShortcutId, Chord>
): boolean {
  return chordsEqual(chordFromEvent(e), map[id]);
}
