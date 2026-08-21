/**
 * Theme catalog — one JSON file per theme, bundled at build time from the
 * repo-root `themes/` folder via Vite `import.meta.glob`. Each file is a
 * complete visual cartridge: color tokens (plus per-theme radius/shadow)
 * mapped to CSS custom properties, with display metadata for the Settings
 * picker.
 *
 * Terminal transparency is first-class:
 *   - `--term-veil` (0–1, default 0) fades the terminal default-cell fill so
 *     wallpaper can show through; xterm reads it in XTermPanel.
 *   - optional top-level `termVeil` number is merged into that var at hydrate.
 * Wallpaper shell mixes (when `data-wallpaper=on`):
 *   - `--wallpaper-surface-mix` (default 0.72) / `--wallpaper-chrome-mix` (0.78)
 *     control alpha only; the final `--wallpaper-surface` / `--wallpaper-chrome`
 *     colors are owned by the shared CSS recipe (themes must not hardcode them).
 *   - optional `--wallpaper-surface-base` / `--wallpaper-chrome-base` retint the
 *     mix (default `--bg2`; bilibili chrome uses `--bg3`).
 * Layout and type roles stay in `ui/App.css` `:root` so switching a theme
 * never moves the UI.
 *
 * Optional `wallpaper` on a cartridge points at a file under
 * `themes/wallpapers/` (still image or looping video). Vite also embeds the
 * file as a hashed `/assets/…` URL for catalog hydration. At runtime the
 * wallpaper layer resolves `$RESOURCE/themes/wallpapers/<file>`. Videos on
 * packaged Linux go through `http://127.0.0.1:<port>/wallpaper/…` (WebKitGTK
 * GStreamer cannot play custom-protocol / blob MP4); `tauri dev` uses the Vite
 * HTTP URL; Windows / macOS use `asset://`.
 * User-picked wallpapers (Settings) live outside this catalog and override the
 * cartridge file when enabled — see `ui/state/store.ts`.
 *
 * Live overrides (Theme Editor) write inline styles on <html> and
 * dispatch `capilot:theme-vars` so xterm re-samples without a full reload.
 */

export interface ThemeWallpaper {
  /** Basename under `themes/wallpapers/` (e.g. `"whale.jpg"` / `"loop.mp4"`). */
  file: string;
  /**
   * Default image/video opacity 0–1 when the user hasn't overridden it.
   * Defaults to 0.55 when omitted.
   */
  opacity?: number;
  /** CSS background-size / object-fit. Defaults to `"cover"`. */
  size?: "cover" | "contain";
  /** CSS background-position / object-position. Defaults to `"center"`. */
  position?: string;
}

/** Still-image extensions the wallpaper layer paints via CSS `background-image`. */
export const WALLPAPER_IMAGE_EXTS = [
  "png",
  "jpg",
  "jpeg",
  "webp",
  "gif",
  "bmp",
] as const;

/** Looping-video extensions the wallpaper layer plays via `<video>`. */
export const WALLPAPER_VIDEO_EXTS = ["mp4", "webm", "mov", "m4v"] as const;

function wallpaperExt(file: string): string {
  const base = file.replace(/\\/g, "/").split("/").pop() ?? file;
  const dot = base.lastIndexOf(".");
  return dot >= 0 ? base.slice(dot + 1).toLowerCase() : "";
}

export function isWallpaperVideo(file: string | null | undefined): boolean {
  if (!file) return false;
  return (WALLPAPER_VIDEO_EXTS as readonly string[]).includes(wallpaperExt(file));
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

  // Terminal veil — top-level termVeil wins, else vars, else transparent default.
  if (typeof raw.termVeil === "number") {
    vars["--term-veil"] = String(clamp01(raw.termVeil, 0));
  } else {
    const parsed = parseFloat(vars["--term-veil"] ?? "0");
    vars["--term-veil"] = String(clamp01(parsed, 0));
  }

  // Wallpaper shell mixes — fill defaults so lab/CSS always have a number.
  // Themes may set the mix ratios, but must NOT set the final mixed colors
  // (`--wallpaper-surface` / `--wallpaper-chrome`): those are owned by the
  // shared CSS recipe so Theme Editor sliders always win.
  delete vars["--wallpaper-surface"];
  delete vars["--wallpaper-chrome"];
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
