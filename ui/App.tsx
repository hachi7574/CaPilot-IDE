import { useEffect, useRef, useState, type ComponentType, type CSSProperties } from "react";
import { join, resourceDir } from "@tauri-apps/api/path";
import { resolveWallpaperSrc } from "./state/wallpaperSrc";
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
import { formatChord, matchesShortcut } from "./state/shortcuts";
import {
  getTheme,
  DEFAULT_WALLPAPER_OPACITY,
  isWallpaperVideo,
} from "./state/themes";
import { ThemeLabPanel } from "./components/theme-lab/ThemeLabPanel";
import { DevToolChip, DevToolsDock } from "./components/dev-tools/DevToolsDock";
import { useT } from "./i18n";
import "./App.css";

/** Absolute path of a cartridge wallpaper under `$RESOURCE/themes/wallpapers/`. */
async function bundledWallpaperPath(file: string): Promise<string | null> {
  const base = file.replace(/\\/g, "/").replace(/^.*\//, "");
  if (!base) return null;
  try {
    const root = await resourceDir();
    return await join(root, "themes", "wallpapers", base);
  } catch (err) {
    console.warn("[wallpaper] resourceDir failed", err);
    return null;
  }
}

/**
 * Theme Editor ships in production. Settings → Appearance toggles
 * `themeLabEnabled` (default off): that is the bottom-left entry chip, not
 * the mixer. Clicking the chip (or Ctrl+Shift+T while the chip is on) opens
 * the panel; hiding the panel returns the chip.
 *
 * The annotations tray stays a `tauri dev` tool — Vite drops that dynamic
 * import from the production module graph.
 */
function ThemeLabGate() {
  const t = useT();
  const enabled = useStore((s) => s.themeLabEnabled);
  const open = useStore((s) => s.themeLabOpen);
  const setThemeLabOpen = useStore((s) => s.setThemeLabOpen);
  const shortcut = useStore((s) => s.shortcuts.themeLab);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!matchesShortcut(e, "themeLab", useStore.getState().shortcuts)) return;
      const s = useStore.getState();
      if (!s.themeLabEnabled) return;
      e.preventDefault();
      e.stopPropagation();
      s.setThemeLabOpen(!s.themeLabOpen);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  if (!enabled) return null;
  if (open) return <ThemeLabPanel onHide={() => setThemeLabOpen(false)} />;
  return (
    <DevToolsDock>
      <DevToolChip
        label={t("themeLab.title")}
        ariaLabel={t("themeLab.show")}
        title={`${t("themeLab.title")} (${formatChord(shortcut)})`}
        onClick={() => setThemeLabOpen(true)}
      />
    </DevToolsDock>
  );
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
 * Priority: mode off → none; custom path → asset/blob; auto → theme cartridge
 * under `$RESOURCE/themes/wallpapers/` (Vite URL fallback).
 * Videos: Vite HTTP in tauri dev; asset:// on Windows/macOS; loopback HTTP
 * on packaged Linux WebKitGTK.
 */
function useWallpaperLayer(): WallpaperLayer | null {
  const themeId = useStore((s) => s.themeId);
  const mode = useStore((s) => s.wallpaperMode);
  const path = useStore((s) => s.wallpaperPath);
  const opacity = useStore((s) => s.wallpaperOpacity);
  const [layer, setLayer] = useState<WallpaperLayer | null>(null);

  useEffect(() => {
    let cancelled = false;
    let revoke: (() => void) | undefined;

    const build = async () => {
      if (mode === "off") {
        if (!cancelled) setLayer(null);
        return;
      }

      const theme = getTheme(themeId);
      let absPath: string | null = null;
      let fallbackUrl: string | undefined;
      let source = "";
      let size = theme?.wallpaper?.size ?? "cover";
      let position = theme?.wallpaper?.position ?? "center";

      if (mode === "custom" && path) {
        absPath = path;
        source = path;
        // Custom picks always cover the viewport; theme size/position only apply
        // to the cartridge's own art.
        size = "cover";
        position = "center";
      } else if (mode === "auto") {
        source = theme?.wallpaper?.file ?? "";
        if (source) {
          absPath = await bundledWallpaperPath(source);
          fallbackUrl = theme?.wallpaperUrl;
        }
      }

      if (cancelled) return;
      if (!source && !absPath && !fallbackUrl) {
        setLayer(null);
        return;
      }

      const video = isWallpaperVideo(source);
      const resolved = await resolveWallpaperSrc(absPath, fallbackUrl, video);
      if (cancelled) {
        resolved?.revoke?.();
        return;
      }
      if (!resolved) {
        setLayer(null);
        return;
      }
      revoke = resolved.revoke;

      const imgOpacity = Number.isFinite(opacity) ? opacity : DEFAULT_WALLPAPER_OPACITY;
      const style: CSSProperties & Record<string, string> = {
        "--wallpaper-size": size,
        "--wallpaper-position": position,
        "--wallpaper-opacity": String(imgOpacity),
      };
      if (!video) {
        style["--wallpaper-image"] =
          `url("${resolved.url.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}")`;
      }
      setLayer({ kind: video ? "video" : "image", url: resolved.url, style });
    };

    void build();
    return () => {
      cancelled = true;
      revoke?.();
    };
  }, [themeId, mode, path, opacity]);

  return layer;
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
