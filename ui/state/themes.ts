/**
 * Theme catalog — one JSON file per theme, bundled at build time from the
 * repo-root `themes/` folder via Vite `import.meta.glob`. Each file is a
 * complete visual cartridge: color tokens (plus per-theme radius/shadow)
 * mapped to CSS custom properties, with display metadata for the Settings
 * picker. Layout and type roles stay in `ui/App.css` `:root` so switching a
 * theme never moves the UI.
 */

export interface Theme {
  id: string;
  name: string;
  note: string;
  swatches: [string, string, string, string];
  colorScheme: "dark" | "light";
  vars: Record<string, string>;
}

const themeModules = import.meta.glob<Theme>("../../themes/*.json", {
  eager: true,
  import: "default",
});

/** All known themes, sorted by id for a stable dropdown order. */
export const THEMES: Theme[] = Object.values(themeModules).sort((a, b) =>
  a.id.localeCompare(b.id),
);

export const DEFAULT_THEME_ID = "quantum";

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