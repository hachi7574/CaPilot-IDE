import { useEffect, useMemo, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

/** Matches Tauri's `startResizeDragging` direction enum (not re-exported). */
type ResizeDirection =
  | "East"
  | "North"
  | "NorthEast"
  | "NorthWest"
  | "South"
  | "SouthEast"
  | "SouthWest"
  | "West";

/**
 * Frameless-window edge/corner hit targets.
 *
 * `tauri.conf.json` runs with `decorations: false`, so the OS chrome (and its
 * native resize grips) is gone. Production packages sometimes still pick up
 * compositor-side edge resize; `pnpm tauri dev` on Wayland/X11 typically does
 * not. These 4-edge + 4-corner strips call `startResizeDragging` so both paths
 * can stretch the window. Hidden while maximized (no free edges to drag).
 */
const EDGES: { dir: ResizeDirection; className: string }[] = [
  { dir: "North", className: "win-edge win-edge-n" },
  { dir: "South", className: "win-edge win-edge-s" },
  { dir: "West", className: "win-edge win-edge-w" },
  { dir: "East", className: "win-edge win-edge-e" },
  { dir: "NorthWest", className: "win-edge win-edge-nw" },
  { dir: "NorthEast", className: "win-edge win-edge-ne" },
  { dir: "SouthWest", className: "win-edge win-edge-sw" },
  { dir: "SouthEast", className: "win-edge win-edge-se" },
];

export function WindowResizeEdges() {
  const appWindow = useMemo(() => getCurrentWindow(), []);
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | undefined;
    const refresh = () => {
      appWindow
        .isMaximized()
        .then((m) => {
          if (alive) setMaximized(m);
        })
        .catch(() => {});
    };
    refresh();
    appWindow
      .onResized(refresh)
      .then((u) => {
        if (alive) unlisten = u;
        else u();
      })
      .catch(() => {});
    return () => {
      alive = false;
      unlisten?.();
    };
  }, [appWindow]);

  if (maximized) return null;

  return (
    <div className="win-edges" aria-hidden>
      {EDGES.map(({ dir, className }) => (
        <div
          key={dir}
          className={className}
          onMouseDown={(e) => {
            // Only primary button starts a resize; right/middle clicks fall
            // through to whatever is under the strip.
            if (e.button !== 0) return;
            e.preventDefault();
            e.stopPropagation();
            void appWindow.startResizeDragging(dir);
          }}
        />
      ))}
    </div>
  );
}
