import { useEffect, useMemo, useState, type ComponentType, type CSSProperties } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { LeftSidebar } from "./components/layout/LeftSidebar";
import { MainArea } from "./components/layout/MainArea";
import { RightSidebar } from "./components/layout/RightSidebar";
import { StatusBar } from "./components/layout/StatusBar";
import { UpdatePrompt } from "./components/layout/UpdatePrompt";
import { WindowResizeEdges } from "./components/layout/WindowResizeEdges";
import { Onboarding } from "./components/onboarding/Onboarding";
import { useResourceSync } from "./state/resource";
import { useRuntimeSync } from "./state/runtime";
import { useSessionRestore, useAgentEvents } from "./state/session";
import { useCloneEvents } from "./state/clone";
import { useWorktreeEvents } from "./state/worktree";
import { useUsageSync } from "./state/usage";
import { useContextUsageSync } from "./state/usageContext";
import { useUpdateSync } from "./state/update";
import { useStore } from "./state/store";
import {
  getTheme,
  DEFAULT_WALLPAPER_OPACITY,
} from "./state/themes";
import "./App.css";

/**
 * Design-feedback annotation UI is a `tauri dev` tool only.
 *
 * Production builds (`pnpm tauri build` / tagged releases) must not ship the
 * floating tray or pick layer. Vite replaces `import.meta.env.DEV` with the
 * literal `false` at build time, so the dynamic import below is eliminated
 * from the production module graph (a static import would still pull the
 * annotation modules in even behind a dead `&&` branch).
 */
function DevAnnotationsGate() {
  const [Comp, setComp] = useState<ComponentType | null>(null);
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    let cancelled = false;
    void import("./components/annotations/DevAnnotations").then((m) => {
      if (!cancelled) setComp(() => m.DevAnnotations);
    });
    return () => {
      cancelled = true;
    };
  }, []);
  if (!import.meta.env.DEV || !Comp) return null;
  return <Comp />;
}

/**
 * Theme Lab mixer — dev only (same elimination rules as DevAnnotationsGate).
 * Live-edits CSS theme tokens + terminal veil; never ships in production.
 */
function DevThemeLabGate() {
  const [Comp, setComp] = useState<ComponentType | null>(null);
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    let cancelled = false;
    void import("./components/theme-lab/DevThemeLab")
      .then((m) => {
        if (!cancelled) setComp(() => m.DevThemeLab);
      })
      .catch((err) => {
        console.error("[ThemeLab] failed to load", err);
      });
    return () => {
      cancelled = true;
    };
  }, []);
  if (!import.meta.env.DEV || !Comp) return null;
  return <Comp />;
}

/**
 * Resolve the active wallpaper URL + paint params.
 * Priority: mode off → none; custom path → asset URL; auto → theme cartridge.
 * Opacity comes from the single user preference (defaults match themes.ts).
 */
function useWallpaperLayer() {
  const themeId = useStore((s) => s.themeId);
  const mode = useStore((s) => s.wallpaperMode);
  const path = useStore((s) => s.wallpaperPath);
  const opacity = useStore((s) => s.wallpaperOpacity);

  return useMemo(() => {
    const theme = getTheme(themeId);
    let url: string | null = null;
    let size = theme?.wallpaper?.size ?? "cover";
    let position = theme?.wallpaper?.position ?? "center";

    if (mode === "custom" && path) {
      try {
        url = convertFileSrc(path);
      } catch {
        url = null;
      }
      // Custom picks always cover the viewport; theme size/position only apply
      // to the cartridge's own art.
      size = "cover";
      position = "center";
    } else if (mode === "auto") {
      url = theme?.wallpaperUrl ?? null;
    }

    if (!url) return null;

    const imgOpacity = Number.isFinite(opacity) ? opacity : DEFAULT_WALLPAPER_OPACITY;

    const style: CSSProperties = {
      ["--wallpaper-image" as string]: `url("${url.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}")`,
      ["--wallpaper-size" as string]: size,
      ["--wallpaper-position" as string]: position,
      ["--wallpaper-opacity" as string]: String(imgOpacity),
    };
    return style;
  }, [themeId, mode, path, opacity]);
}

function App() {
  useResourceSync();
  useRuntimeSync();
  useSessionRestore();
  useAgentEvents();
  useCloneEvents();
  useWorktreeEvents();
  useUsageSync();
  useContextUsageSync();
  useUpdateSync();
  const onboarded = useStore((s) => s.onboarded);
  const fontScale = useStore((s) => s.fontScale);
  const themeId = useStore((s) => s.themeId);
  const wallpaperStyle = useWallpaperLayer();
  // Reflect the chosen font-size preset on <html> so the CSS `html[data-fs=…]`
  // rules can rescale every `--fs-*` token.
  document.documentElement.dataset.fs = fontScale;
  // Theme tokens live in CSS; reflecting the persisted preset here updates the
  // whole shell (and CodeMirror) without changing component structure.
  document.documentElement.dataset.theme = themeId;
  // Let CSS know a wallpaper is active so shell surfaces can go translucent.
  if (wallpaperStyle) document.documentElement.dataset.wallpaper = "on";
  else delete document.documentElement.dataset.wallpaper;

  return (
    <div className="app">
      {wallpaperStyle && (
        <div className="app-wallpaper" style={wallpaperStyle} aria-hidden="true" />
      )}
      <div className="app-body">
        <RightSidebar />
        <MainArea />
        <LeftSidebar />
      </div>
      <StatusBar />
      <UpdatePrompt />
      {/* Frameless window (`decorations: false`): OS edge-resize is missing in
          tauri dev on many Linux compositors — overlay grips restore it. */}
      <WindowResizeEdges />
      <DevAnnotationsGate />
      <DevThemeLabGate />
      {!onboarded && <Onboarding />}
    </div>
  );
}

export default App;
