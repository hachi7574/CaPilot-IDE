/**
 * Theme Lab token catalog — which CSS vars the mixer exposes, and how brand
 * colors derive companion rgb / selection tokens.
 */

export type TokenKind = "color" | "ratio" | "text" | "rgb";

export interface LabToken {
  name: string;
  kind: TokenKind;
  /** i18n key under themeLab.token.* (optional; falls back to bare name). */
  labelKey?: string;
}

export interface LabGroup {
  id: string;
  /** i18n key under themeLab.group.* */
  labelKey: string;
  tokens: LabToken[];
  /** Start collapsed (advanced groups). */
  collapsed?: boolean;
}

export const LAB_GROUPS: LabGroup[] = [
  {
    id: "layers",
    labelKey: "layers",
    tokens: [
      { name: "--term-veil", kind: "ratio", labelKey: "termVeil" },
      {
        name: "--wallpaper-surface-mix",
        kind: "ratio",
        labelKey: "surfaceMix",
      },
      {
        name: "--wallpaper-chrome-mix",
        kind: "ratio",
        labelKey: "chromeMix",
      },
    ],
  },
  {
    id: "surf",
    labelKey: "surf",
    tokens: [
      { name: "--bg", kind: "color" },
      { name: "--bg2", kind: "color" },
      { name: "--bg3", kind: "color" },
      { name: "--bg4", kind: "color" },
      { name: "--term-bg", kind: "color" },
      { name: "--rule", kind: "color" },
      { name: "--rule2", kind: "color" },
      { name: "--search-match-bg", kind: "color" },
    ],
  },
  {
    id: "ink",
    labelKey: "ink",
    tokens: [
      { name: "--ink", kind: "color" },
      { name: "--ink2", kind: "color" },
      { name: "--muted", kind: "color" },
      { name: "--accent-ink", kind: "color" },
    ],
  },
  {
    id: "brand",
    labelKey: "brand",
    tokens: [
      { name: "--brand", kind: "color" },
      { name: "--brand-dim", kind: "color" },
      { name: "--primary", kind: "color" },
      { name: "--primary-dim", kind: "color" },
      { name: "--ai", kind: "color" },
      { name: "--ai-dim", kind: "color" },
      { name: "--success", kind: "color" },
      { name: "--warn", kind: "color" },
      { name: "--danger", kind: "color" },
    ],
  },
  {
    id: "term",
    labelKey: "term",
    tokens: [
      { name: "--pl-fg", kind: "color" },
      { name: "--pl-black", kind: "color" },
      { name: "--pl-red", kind: "color" },
      { name: "--pl-green", kind: "color" },
      { name: "--pl-yellow", kind: "color" },
      { name: "--pl-blue", kind: "color" },
      { name: "--pl-magenta", kind: "color" },
      { name: "--pl-cyan", kind: "color" },
      { name: "--pl-white", kind: "color" },
      { name: "--pl-orange", kind: "color" },
      { name: "--pl-purple", kind: "color" },
      { name: "--pl-comment", kind: "color" },
      { name: "--pl-cursor", kind: "color" },
      { name: "--pl-selection", kind: "color" },
    ],
  },
  {
    id: "termBright",
    labelKey: "termBright",
    collapsed: true,
    tokens: [
      { name: "--pl-bright-black", kind: "color" },
      { name: "--pl-bright-red", kind: "color" },
      { name: "--pl-bright-green", kind: "color" },
      { name: "--pl-bright-yellow", kind: "color" },
      { name: "--pl-bright-blue", kind: "color" },
      { name: "--pl-bright-magenta", kind: "color" },
      { name: "--pl-bright-cyan", kind: "color" },
      { name: "--pl-bright-white", kind: "color" },
      { name: "--pl-blue-purple", kind: "color" },
    ],
  },
  {
    id: "lanes",
    labelKey: "lanes",
    collapsed: true,
    tokens: [
      { name: "--lane-0", kind: "color" },
      { name: "--lane-1", kind: "color" },
      { name: "--lane-2", kind: "color" },
      { name: "--lane-3", kind: "color" },
      { name: "--lane-4", kind: "color" },
      { name: "--lane-5", kind: "color" },
      { name: "--lane-6", kind: "color" },
    ],
  },
  {
    id: "scan",
    labelKey: "scan",
    tokens: [
      { name: "--scan-rgb", kind: "rgb" },
      { name: "--scan-alpha", kind: "ratio" },
    ],
  },
  {
    id: "shape",
    labelKey: "shape",
    collapsed: true,
    tokens: [
      { name: "--control-radius", kind: "text" },
      { name: "--panel-radius", kind: "text" },
      { name: "--control-shadow", kind: "text" },
      { name: "--panel-shadow", kind: "text" },
      { name: "--shadow-hard", kind: "text" },
    ],
  },
];

/** Parse #rgb / #rrggbb / rgb() into [r,g,b] 0–255. */
export function parseColorToRgb(
  value: string
): [number, number, number] | null {
  const v = value.trim();
  const hex = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(v);
  if (hex) {
    let h = hex[1];
    if (h.length === 3) {
      h = h
        .split("")
        .map((c) => c + c)
        .join("");
    }
    return [
      parseInt(h.slice(0, 2), 16),
      parseInt(h.slice(2, 4), 16),
      parseInt(h.slice(4, 6), 16),
    ];
  }
  const rgb =
    /^rgba?\(\s*([\d.]+)\s*[, ]\s*([\d.]+)\s*[, ]\s*([\d.]+)/i.exec(v);
  if (rgb) {
    return [
      Math.round(Number(rgb[1])),
      Math.round(Number(rgb[2])),
      Math.round(Number(rgb[3])),
    ];
  }
  // "r g b" space-separated (our --*-rgb tokens)
  const parts = v.split(/\s+/).map(Number);
  if (parts.length >= 3 && parts.every((n) => Number.isFinite(n))) {
    return [parts[0], parts[1], parts[2]];
  }
  return null;
}

/** Best-effort #rrggbb for <input type="color">. */
export function toColorInputValue(value: string): string {
  const rgb = parseColorToRgb(value);
  if (!rgb) return "#000000";
  const [r, g, b] = rgb;
  const h = (n: number) =>
    Math.max(0, Math.min(255, n)).toString(16).padStart(2, "0");
  return `#${h(r)}${h(g)}${h(b)}`;
}

/**
 * When a brand/status color changes, also update companion rgb tokens so
 * `rgb(var(--brand-rgb) / …)` surfaces stay in sync.
 */
export function derivedVarsFor(
  name: string,
  value: string
): Record<string, string> {
  const out: Record<string, string> = {};
  const rgb = parseColorToRgb(value);
  if (!rgb) return out;
  const [r, g, b] = rgb;
  const triple = `${r} ${g} ${b}`;

  const map: Record<string, string[]> = {
    "--brand": ["--brand-rgb"],
    "--success": ["--success-rgb"],
    "--warn": ["--warn-rgb"],
    "--danger": ["--danger-rgb"],
  };
  for (const key of map[name] ?? []) {
    out[key] = triple;
  }
  if (name === "--brand") {
    out["--brand-selection"] = `rgb(${r} ${g} ${b} / 0.28)`;
    out["--scan-rgb"] = triple;
  }
  return out;
}

/** Snapshot current computed values for every lab token from the active theme. */
export function snapshotLabVars(
  cartridgeVars: Record<string, string>
): Record<string, string> {
  const root =
    typeof document !== "undefined" ? document.documentElement : null;
  const cs = root ? getComputedStyle(root) : null;
  const out: Record<string, string> = {};
  for (const g of LAB_GROUPS) {
    for (const t of g.tokens) {
      const fromCartridge = cartridgeVars[t.name];
      const live = cs?.getPropertyValue(t.name).trim() || "";
      out[t.name] = live || fromCartridge || defaultFor(t);
    }
  }
  // Also keep derived companions so export is complete.
  for (const key of [
    "--brand-rgb",
    "--success-rgb",
    "--warn-rgb",
    "--danger-rgb",
    "--brand-selection",
    "--white-rgb",
    "--black-rgb",
  ]) {
    const fromCartridge = cartridgeVars[key];
    const live = cs?.getPropertyValue(key).trim() || "";
    if (live || fromCartridge) out[key] = live || fromCartridge;
  }
  return out;
}

function defaultFor(t: LabToken): string {
  if (t.kind === "ratio") return "1";
  if (t.kind === "rgb") return "0 0 0";
  if (t.kind === "text") return "";
  return "#000000";
}

/** Apply draft overrides as inline styles on <html>; clear keys not in draft. */
export function applyInlineOverrides(
  draft: Record<string, string>,
  baseline: Record<string, string>
): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  const allKeys = new Set([...Object.keys(draft), ...Object.keys(baseline)]);
  for (const key of allKeys) {
    const d = draft[key];
    const b = baseline[key];
    if (d != null && d !== b) {
      root.style.setProperty(key, d);
    } else {
      root.style.removeProperty(key);
    }
  }
}

export function clearInlineOverrides(keys: string[]): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  for (const key of keys) root.style.removeProperty(key);
}
