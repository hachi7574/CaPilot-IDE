// Path extraction for DOM drag-and-drop + in-app pointer path-drag.
//
// On Tauri v2 + WebKitGTK the legacy `File.path` extension is gone and the
// `getCurrentWebview().onDragDropEvent` listener can be unreliable on Linux, so
// the OS file manager's `text/uri-list` payload is the robust source for the
// absolute path of a dropped file.
//
// Windows WebView2 also breaks HTML5 DnD for in-app sources (file tree →
// composer/terminal): the cursor sticks on 🚫. File-tree rows therefore use a
// pointer-based drag (same approach as todo tags) and drop via
// `data-path-drop` markers + the helpers below.

/** Movement (px) before a press becomes a path pointer-drag. */
export const PATH_POINTER_DRAG_THRESHOLD = 5;

/** Active in-app path drag (file tree). Null when idle. */
let activePathDrag: { path: string; name: string } | null = null;

export function beginPathDrag(path: string, name?: string): void {
  activePathDrag = {
    path,
    name: name || path.replace(/\\/g, "/").split("/").pop() || path,
  };
}

export function endPathDrag(): void {
  activePathDrag = null;
}

export function getActivePathDrag(): { path: string; name: string } | null {
  return activePathDrag;
}

/**
 * Resolve a path drop at screen coordinates.
 * - `[data-path-drop="composer"]` → composer
 * - `[data-path-drop="terminal"]` (optional `data-todo-drop-agent`) → terminal
 */
export function resolvePathDropTarget(
  clientX: number,
  clientY: number
):
  | { kind: "composer" }
  | { kind: "terminal"; agentId: string | null }
  | null {
  const under = document.elementFromPoint(clientX, clientY);
  if (!under) return null;
  const el = under.closest("[data-path-drop]") as HTMLElement | null;
  if (!el) return null;
  const kind = el.getAttribute("data-path-drop");
  if (kind === "composer") return { kind: "composer" };
  if (kind === "terminal") {
    return {
      kind: "terminal",
      agentId:
        el.getAttribute("data-todo-drop-agent") ||
        el.getAttribute("data-path-drop-agent"),
    };
  }
  return null;
}

/** Decode a `file://` URI into an absolute filesystem path (mirrors wry's
 *  `path_buf_from_uri`). Handles percent-encoding and `localhost`; on Windows
 *  strips the extra leading slash before the drive letter. */
function fileUriToPath(uri: string): string {
  let p = uri.trim();
  if (!p.startsWith("file://")) return "";
  p = p.slice("file://".length);
  if (p.startsWith("localhost/")) p = p.slice("localhost/".length);
  if (/^\/[A-Za-z]:\//.test(p)) p = p.slice(1);
  try {
    p = decodeURIComponent(p);
  } catch {
    // Malformed percent-encoding — keep the raw URI rather than throwing.
  }
  return p;
}

/** Absolute filesystem paths from a DOM drop `DataTransfer`.
 *
 *  Sources, in priority order:
 *  1. `File.path` — Tauri v1 heritage / webviews that still inject it.
 *  2. `text/uri-list` — the GTK drag payload (`file://` URIs) for files dragged
 *     from the OS file manager.
 *  3. `text/plain` — a single line that is not a URI (application-internal
 *     drags, e.g. a file-tree row that stored the absolute path as plain text).
 */
export function pathsFromDataTransfer(
  dt: DataTransfer | null | undefined
): string[] {
  if (!dt) return [];

  // 1. Legacy `File.path` extension.
  const filePaths = Array.from(dt.files)
    .map((f) => (f as File & { path?: string }).path)
    .filter((p): p is string => !!p && p.trim().length > 0);
  if (filePaths.length) return filePaths;

  // 2. OS file-manager drag: `text/uri-list`.
  const uris = dt.getData("text/uri-list");
  if (uris) {
    const paths = uris
      .split(/\r?\n/)
      .map(fileUriToPath)
      .filter((p) => p.length > 0);
    if (paths.length) return paths;
  }

  // 3. Application-internal drag (e.g. a file-tree row).
  const plain = dt.getData("text/plain");
  if (
    plain &&
    plain.trim() &&
    !plain.includes("\n") &&
    !plain.trim().startsWith("file://")
  ) {
    return [plain.trim()];
  }

  return [];
}
