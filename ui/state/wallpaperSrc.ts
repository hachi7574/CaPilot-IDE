/**
 * Video wallpaper source resolution.
 *
 * Packaged Linux / WebKitGTK cannot play Tauri custom protocols or blob: URLs:
 * GStreamer only opens `file://` and real `http(s)://` (confirmed 2026-08-21 by
 * `scripts/webkit-video-probe.py`). Those builds ask Rust for
 * `http://127.0.0.1:<port>/wallpaper/<path>` from the in-process loopback
 * server (`wallpaper_http.rs`) which always sends Accept-Ranges and does not
 * truncate Range responses.
 *
 * Everywhere else keeps the protocol URL:
 *   - `tauri dev` → Vite HTTP (`http://localhost:1420/assets/…`, Range works)
 *   - Windows WebView2 / macOS WKWebView → `asset://`
 *
 * Stills always use `asset://` (or the Vite URL).
 */
import { convertFileSrc, invoke } from "@tauri-apps/api/core";

export type WallpaperSrc = { url: string; revoke?: () => void };

/**
 * Resolve a playable/paintable URL.
 * - Images → protocol / Vite URL
 * - Videos → see file header
 */
export async function resolveWallpaperSrc(
  absPath: string | null,
  fallbackUrl: string | undefined,
  video: boolean
): Promise<WallpaperSrc | null> {
  let protocolUrl: string | null = null;
  if (absPath) {
    try {
      protocolUrl = convertFileSrc(absPath);
    } catch (err) {
      console.warn("[wallpaper] convertFileSrc failed", absPath, err);
    }
  }
  const primary = protocolUrl ?? fallbackUrl ?? null;
  if (!primary) return null;

  // `tauri dev`: Vite hashed URL over real HTTP. Prefer it for both stills and
  // videos — that's what used to paint before the Linux packaged workaround.
  if (import.meta.env.DEV && fallbackUrl) {
    return { url: fallbackUrl };
  }

  if (!video) return { url: primary };

  // Linux loopback HTTP (packaged, and tauri-dev custom files with no Vite URL).
  // Non-Linux Rust returns null and we fall through to asset://.
  if (absPath) {
    try {
      const url = await invoke<string | null>("wallpaper_http_url", { path: absPath });
      if (url) return { url };
    } catch (err) {
      console.warn("[wallpaper] loopback http failed", absPath, err);
    }
  }

  // Windows / macOS (dev + packaged): asset://.
  if (protocolUrl) return { url: protocolUrl };
  if (fallbackUrl) return { url: fallbackUrl };
  return null;
}
