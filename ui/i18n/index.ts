import { useCallback, useSyncExternalStore } from "react";
import { zh, type ZhMessages } from "./zh";
import { en } from "./en";

export type Locale = "zh" | "en";

export const LOCALES: { id: Locale; nativeLabel: string }[] = [
  { id: "zh", nativeLabel: "中文" },
  { id: "en", nativeLabel: "English" },
];

export const DEFAULT_LOCALE: Locale = "zh";

const catalogs: Record<Locale, ZhMessages> = { zh, en };

/** Dot-path into the message tree, e.g. `"settings.title"`. */
export type MessageKey = string;

type Dict = Record<string, unknown>;

function lookup(dict: Dict, path: string): string | undefined {
  const parts = path.split(".");
  let cur: unknown = dict;
  for (const p of parts) {
    if (cur == null || typeof cur !== "object") return undefined;
    cur = (cur as Dict)[p];
  }
  return typeof cur === "string" ? cur : undefined;
}

export type Vars = Record<string, string | number | null | undefined>;

/** Replace `{name}` placeholders. Missing vars become empty strings. */
function interpolate(template: string, vars?: Vars): string {
  if (!vars) return template;
  return template.replace(/\{(\w+)\}/g, (_, key: string) => {
    const v = vars[key];
    return v == null ? "" : String(v);
  });
}

/**
 * Translate a key for the given locale. Falls back to Chinese, then the key
 * itself, so a missing English string never blanks the UI.
 */
export function translate(
  locale: Locale,
  key: MessageKey,
  vars?: Vars
): string {
  const primary = lookup(catalogs[locale] as unknown as Dict, key);
  if (primary != null) return interpolate(primary, vars);
  if (locale !== "zh") {
    const fallback = lookup(catalogs.zh as unknown as Dict, key);
    if (fallback != null) return interpolate(fallback, vars);
  }
  return key;
}

// ── Live locale (kept outside zustand so i18n never imports the store) ──

let currentLocale: Locale = DEFAULT_LOCALE;
const listeners = new Set<() => void>();

export function getLocale(): Locale {
  return currentLocale;
}

/** Called by the store whenever `locale` changes (and once at boot). */
export function setI18nLocale(locale: Locale): void {
  if (currentLocale === locale) return;
  currentLocale = locale;
  if (typeof document !== "undefined") {
    document.documentElement.lang = locale === "zh" ? "zh-CN" : "en";
  }
  for (const l of listeners) l();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Imperative helper for non-React code (notifications, actions, etc.). */
export function t(key: MessageKey, vars?: Vars): string {
  return translate(currentLocale, key, vars);
}

export function isLocale(value: unknown): value is Locale {
  return value === "zh" || value === "en";
}

/**
 * React hook: returns a stable `t` that re-renders the component when the
 * language changes. Does not depend on the zustand store — only on the
 * i18n module's own locale bus (fed by `setI18nLocale` from the store).
 */
export function useT(): (key: MessageKey, vars?: Vars) => string {
  const locale = useSyncExternalStore(subscribe, getLocale, getLocale);
  return useCallback(
    (key: MessageKey, vars?: Vars) => translate(locale, key, vars),
    [locale]
  );
}

/** Localized display name / note for a theme cartridge. */
export function themeLabel(
  locale: Locale,
  themeId: string
): { name: string; note: string } | null {
  const name = translate(locale, `themes.${themeId}.name`);
  const note = translate(locale, `themes.${themeId}.note`);
  if (name === `themes.${themeId}.name`) return null;
  return { name, note };
}
