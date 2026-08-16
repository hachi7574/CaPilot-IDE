import type { Tab } from "./store";

/**
 * Image file extensions opened in the dedicated image viewer instead of the
 * text editor. Browsers (WebKitGTK) can decode these natively in an <img>.
 * Lowercased before matching.
 */
const IMAGE_EXTENSIONS = new Set([
  "png",
  "apng",
  "jpg",
  "jpeg",
  "gif",
  "webp",
  "bmp",
  "ico",
  "avif",
  "svg",
]);

/** Whether a file path points at a viewable image (by extension). */
export function isImagePath(path: string): boolean {
  const ext = (path.split(".").pop() ?? "").toLowerCase();
  return IMAGE_EXTENSIONS.has(ext);
}

/**
 * Build a file tab for `path`, routing images to the image viewer
 * (`type: "image"`) and everything else to the text editor.
 */
export function fileTab(path: string, name: string): Tab {
  return {
    id: `file:${path}`,
    type: isImagePath(path) ? "image" : "editor",
    filePath: path,
    title: name,
  };
}
