import { useEffect, useMemo, useRef, useState, type ComponentType, type CSSProperties } from "react";
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
  isWallpaperVideo,
} from "./state/themes";
import { ThemeLabPanel } from "./components/theme-lab/ThemeLabPanel";
import "./App.css";

/**
 * Theme Editor ships in production. Settings → Appearance toggles
 * `themeLabEnabled` (default off). Ctrl+Shift+T flips the same flag even
 * when the panel is unmounted.
 *
 * The annotations tray stays a `tauri dev` tool — Vite drops that dynamic
 * import from the production module graph.
 */
function ThemeLabGate() {
  const enabled = useStore((s) => s.themeLabEnabled);
  const setThemeLabEnabled = useStore((s) => s.setThemeLabEnabled);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "T" && e.key !== "t") return;
      if (!e.ctrlKey || !e.shiftKey || e.altKey || e.metaKey) return;
      e.preventDefault();
      e.stopPropagation();
      setThemeLabEnabled(!useStore.getState().themeLabEnabled);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [setThemeLabEnabled]);

  if (!enabled) return null;
  return <ThemeLabPanel onHide={() => setThemeLabEnabled(false)} />;
}

function AnnotationsGate() {
  const [Comp, setComp] = useState<ComponentType | null>(null);
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    let cancelled = false;
    void import("./components/dev-tools/DevToolsRoot")
      .then((m) => {
        if (!cancelled) setComp(() => m.DevToolsRoot);
      })
      .catch((err) => {
        console.error("[DevTools] failed to load", err);
      });
    return () => {
      cancelled = true;
    };
  }, []);
  if (!import.meta.env.DEV || !Comp) return null;
  return <Comp />;
}

type WallpaperLayer = {
  kind: "image" | "video";
  url: string;
  style: CSSProperties;
};

/**
 * Resolve the active wallpaper URL + paint params.
 * Priority: mode off → none; custom path → asset URL; auto → theme cartridge.
 * Opacity comes from the single user preference (defaults match themes.ts).
 * Videos use a <video> layer (muted / loop / metadata preload); stills stay
 * on the CSS background-image path so existing themes are unchanged.
 */
function useWallpaperLayer(): WallpaperLayer | null {
  const themeId = useStore((s) => s.themeId);
  const mode = useStore((s) => s.wallpaperMode);
  const path = useStore((s) => s.wallpaperPath);
  const opacity = useStore((s) => s.wallpaperOpacity);

  return useMemo(() => {
    const theme = getTheme(themeId);
    let url: string | null = null;
    let source = "";
    let size = theme?.wallpaper?.size ?? "cover";
    let position = theme?.wallpaper?.position ?? "center";

    if (mode === "custom" && path) {
      try {
        url = convertFileSrc(path);
      } catch {
        url = null;
      }
      source = path;
      // Custom picks always cover the viewport; theme size/position only apply
      // to the cartridge's own art.
      size = "cover";
      position = "center";
    } else if (mode === "auto") {
      url = theme?.wallpaperUrl ?? null;
      source = theme?.wallpaper?.file ?? "";
    }

    if (!url) return null;

    const imgOpacity = Number.isFinite(opacity) ? opacity : DEFAULT_WALLPAPER_OPACITY;
    const video = isWallpaperVideo(source);
    const style: CSSProperties & Record<string, string> = {
      "--wallpaper-size": size,
      "--wallpaper-position": position,
      "--wallpaper-opacity": String(imgOpacity),
    };
    if (!video) {
      style["--wallpaper-image"] =
        `url("${url.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}")`;
    }
    return { kind: video ? "video" : "image", url, style };
  }, [themeId, mode, path, opacity]);
}

/** Looping muted backdrop. Pauses when the window is hidden so a background
 *  CaPilot session does not keep a 1080p decoder hot. On media error (missing
 *  host codec, corrupt file) the element unmounts so `.app-wallpaper` keeps
 *  only its solid `--bg` fill — no empty video over translucent chrome. */
function WallpaperVideo({ src }: { src: string }) {
  const ref = useRef<HTMLVideoElement>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    setFailed(false);
  }, [src]);

  useEffect(() => {
    const el = ref.current;
    if (!el || failed) return;
    const sync = () => {
      if (document.hidden || window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
        el.pause();
        return;
      }
      // Autoplay policy rejections are silent; decode/network failures use onError.
      void el.play().catch(() => {});
    };
    sync();
    document.addEventListener("visibilitychange", sync);
    return () => document.removeEventListener("visibilitychange", sync);
  }, [src, failed]);

  if (failed) return null;

  return (
    <video
      ref={ref}
      className="app-wallpaper-video"
      src={src}
      muted
      loop
      playsInline
      autoPlay
      preload="metadata"
      disablePictureInPicture
      disableRemotePlayback
      onError={(e) => {
        const mediaErr = e.currentTarget.error;
        console.warn(
          "[wallpaper] video failed to load",
          src,
          mediaErr ? `code=${mediaErr.code}` : ""
        );
        setFailed(true);
      }}
    />
  );
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
  const wallpaper = useWallpaperLayer();
  // Reflect the chosen font-size preset on <html> so the CSS `html[data-fs=…]`
  // rules can rescale every `--fs-*` token.
  document.documentElement.dataset.fs = fontScale;
  // Theme tokens live in CSS; reflecting the persisted preset here updates the
  // whole shell (and CodeMirror) without changing component structure.
  document.documentElement.dataset.theme = themeId;
  // Let CSS know a wallpaper is active so shell surfaces can go translucent.
  if (wallpaper) document.documentElement.dataset.wallpaper = "on";
  else delete document.documentElement.dataset.wallpaper;

  return (
    <div className="app">
      {wallpaper && (
        <div className="app-wallpaper" style={wallpaper.style} aria-hidden="true">
          {wallpaper.kind === "video" && <WallpaperVideo src={wallpaper.url} />}
        </div>
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
      <ThemeLabGate />
      <AnnotationsGate />
      {!onboarded && <Onboarding />}
    </div>
  );
}

export default App;
