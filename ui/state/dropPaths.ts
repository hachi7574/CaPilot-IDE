// Path extraction for DOM drag-and-drop.
//
// On Tauri v2 + WebKitGTK the legacy `File.path` extension is gone and the
// `getCurrentWebview().onDragDropEvent` listener can be unreliable on Linux, so
// the OS file manager's `text/uri-list` payload is the robust source for the
// absolute path of a dropped file.

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
