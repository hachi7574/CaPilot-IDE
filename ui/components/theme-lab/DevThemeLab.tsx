import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { ThemeLabPanel } from "./ThemeLabPanel";
import {
  loadThemeLabVisible,
  saveThemeLabVisible,
} from "../../state/themeLab";
import { useT } from "../../i18n";
import "./theme-lab.css";

/**
 * Dev-only Theme Lab root. Mounted via dynamic import from App.tsx so
 * production builds never ship this module graph.
 *
 * Ctrl+Shift+T toggles visibility (capture phase). When hidden, a small
 * bottom-left chip stays mounted so the tool remains discoverable.
 */
export function DevThemeLab() {
  const t = useT();
  const [visible, setVisible] = useState(loadThemeLabVisible);

  const setVisiblePersist = (next: boolean | ((v: boolean) => boolean)) => {
    setVisible((v) => {
      const resolved = typeof next === "function" ? next(v) : next;
      saveThemeLabVisible(resolved);
      return resolved;
    });
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "T" && e.key !== "t") return;
      if (!e.ctrlKey || !e.shiftKey || e.altKey || e.metaKey) return;
      // Don't steal from editable fields when the user is typing a capital T
      // chord inside an input — still allow toggle from anywhere else.
      e.preventDefault();
      e.stopPropagation();
      setVisible((v) => {
        const next = !v;
        saveThemeLabVisible(next);
        return next;
      });
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  if (visible) {
    return <ThemeLabPanel onHide={() => setVisiblePersist(false)} />;
  }

  const chip = (
    <button
      type="button"
      className="tl-fab"
      data-theme-lab
      title={`${t("themeLab.title")} (Ctrl+Shift+T)`}
      aria-label={t("themeLab.show")}
      onClick={() => setVisiblePersist(true)}
    >
      {t("themeLab.title")}
    </button>
  );

  if (typeof document === "undefined") return chip;
  return createPortal(chip, document.body);
}
