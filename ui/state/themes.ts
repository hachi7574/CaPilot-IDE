/**
 * Theme catalog — one JSON file per theme, bundled at build time from the
 * repo-root `themes/` folder via Vite `import.meta.glob`. Each file is a
 * complete visual cartridge: color tokens (plus per-theme radius/shadow)
 * mapped to CSS custom properties, with display metadata for the Settings
 * picker.
 *
 * Terminal transparency is first-class:
 *   - `--term-veil` (0–1, default 1) fades the terminal default-cell fill so
 *     wallpaper can show through; xterm reads it in XTermPanel.
 *   - optional top-level `termVeil` number is merged into that var at hydrate.
 * Wallpaper shell mixes (when `data-wallpaper=on`):
 *   - `--wallpaper-surface-mix` (default 0.72) / `--wallpaper-chrome-mix` (0.78)
 * Layout and type roles stay in `ui/App.css` `:root` so switching a theme
 * never moves the UI.
 *
 * Optional `wallpaper` on a cartridge points at a file under
 * `themes/wallpapers/`; the image is resolved to a bundled URL at load time.
 * User-picked wallpapers (Settings) live outside this catalog and override
 * the cartridge image when enabled — see `ui/state/store.ts`.
 *
 * Dev-only live overrides (Theme Lab) write inline styles on <html> and
 * dispatch `capilot:theme-vars` so xterm re-samples without a full reload.
 */

export interface ThemeWallpaper {
  /** Basename under `themes/wallpapers/` (e.g. `"whale.jpg"`). */
  file: string;
  /**
   * Default image opacity 0–1 when the user hasn't overridden it.
   * Defaults to 0.55 when omitted.
   */
  opacity?: number;
  /** CSS background-size. Defaults to `"cover"`. */
  size?: "cover" | "contain";
  /** CSS background-position. Defaults to `"center"`. */
  position?: string;
}

export interface Theme {
  id: string;
  name: string;
  note: string;
  swatches: [string, string, string, string];
  colorScheme: "dark" | "light";
  vars: Record<string, string>;
  /**
   * Optional author-facing terminal veil (0–1). When set, written into
   * `vars["--term-veil"]` at hydrate (clamped). Prefer this over a raw var
   * string when exporting from Theme Lab.
   */
  termVeil?: number;
  /** Optional built-in backdrop art for this cartridge. */
  wallpaper?: ThemeWallpaper;
  /**
   * Resolved at module load from `themes/wallpapers/<file>`. Undefined when the
   * cartridge has no wallpaper or the file is missing from the bundle.
   */
  wallpaperUrl?: string;
}

/** Event name Theme Lab / live overrides dispatch so xterm re-reads CSS vars. */
export const THEME_VARS_EVENT = "capilot:theme-vars";

export function notifyThemeVarsChanged(): void {
  if (typeof window === "undefined") return;
  window.dispatchEvent(new Event(THEME_VARS_EVENT));
}

/** Clamp a veil/mix ratio into [0, 1]; non-finite → fallback. */
export function clamp01(n: number, fallback = 1): number {
  return Number.isFinite(n) ? Math.min(1, Math.max(0, n)) : fallback;
}

const themeModules = import.meta.glob<Theme>("../../themes/*.json", {
  eager: true,
  import: "default",
});

/** Bundled wallpaper assets — keyed by the Vite module path. */
const wallpaperModules = import.meta.glob<string>("../../themes/wallpapers/*", {
  eager: true,
  query: "?url",
  import: "default",
});

function resolveWallpaperUrl(file: string): string | undefined {
  const needle = file.replace(/\\/g, "/").replace(/^.*\//, "");
  if (!needle) return undefined;
  for (const [path, url] of Object.entries(wallpaperModules)) {
    const base = path.replace(/\\/g, "/").split("/").pop();
    if (base === needle) return url;
  }
  return undefined;
}

function hydrateTheme(raw: Theme): Theme {
  const vars = { ...raw.vars };

  // Terminal veil — top-level termVeil wins, else vars, else opaque default.
  if (typeof raw.termVeil === "number") {
    vars["--term-veil"] = String(clamp01(raw.termVeil, 1));
  } else {
    const parsed = parseFloat(vars["--term-veil"] ?? "1");
    vars["--term-veil"] = String(clamp01(parsed, 1));
  }

  // Wallpaper shell mixes — fill defaults so lab/CSS always have a number.
  if (vars["--wallpaper-surface-mix"] == null) {
    vars["--wallpaper-surface-mix"] = "0.72";
  } else {
    vars["--wallpaper-surface-mix"] = String(
      clamp01(parseFloat(vars["--wallpaper-surface-mix"]), 0.72)
    );
  }
  if (vars["--wallpaper-chrome-mix"] == null) {
    vars["--wallpaper-chrome-mix"] = "0.78";
  } else {
    vars["--wallpaper-chrome-mix"] = String(
      clamp01(parseFloat(vars["--wallpaper-chrome-mix"]), 0.78)
    );
  }

  let theme: Theme = { ...raw, vars };

  const file = raw.wallpaper?.file;
  if (!file) return theme;
  const wallpaperUrl = resolveWallpaperUrl(file);
  if (!wallpaperUrl) {
    // Keep the declaration so authors notice a missing asset, but don't
    // advertise a broken URL to the runtime applicator.
    console.warn(`[themes] wallpaper missing for "${raw.id}": ${file}`);
    return theme;
  }
  return { ...theme, wallpaperUrl };
}

/** All known themes, sorted by id for a stable dropdown order. */
export const THEMES: Theme[] = Object.values(themeModules)
  .map(hydrateTheme)
  .sort((a, b) => a.id.localeCompare(b.id));

export const DEFAULT_THEME_ID = "quantum";

export const DEFAULT_WALLPAPER_OPACITY = 0.55;

export function getTheme(id: string): Theme | undefined {
  return THEMES.find((t) => t.id === id);
}

/**
 * Emit one `html[data-theme=…]` rule per theme so the existing selector-based
 * CSS — including the `html[data-theme="handheld"] …` overrides — keeps
 * working, and `color-scheme` flips with the theme. Runs once at module load,
 * before the first React render, so there is no unstyled flash.
 */
const THEME_STYLE_ID = "capilot-theme-css";
if (typeof document !== "undefined" && !document.getElementById(THEME_STYLE_ID)) {
  const style = document.createElement("style");
  style.id = THEME_STYLE_ID;
  style.textContent = THEMES.map((theme) => {
    const decls = Object.entries(theme.vars)
      .map(([name, value]) => `${name}: ${value};`)
      .join(" ");
    return `html[data-theme="${theme.id}"] { ${decls} color-scheme: ${theme.colorScheme}; }`;
  }).join("\n");
  document.head.appendChild(style);
}
