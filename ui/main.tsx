import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";

/**
 * Document-level drag guards.
 *
 * History: we used unconditional `dragover`/`drop` preventDefault so WebKitGTK
 * would not turn OS file drops into text-selection. On Windows WebView2 that
 * unconditional preventDefault leaves dropEffect at "none" for *in-app* HTML5
 * drags (file tree, tabs, todo tags) — the cursor sticks on 🚫 from the first
 * pixel of movement.
 *
 * Fix: only cancel the browser default for **external file** drags (types
 * includes "Files"). In-app drags rely on each drop target's own
 * onDragOver/onDrop preventDefault, matching file-tree → composer etc.
 */
function isExternalFileDrag(dt: DataTransfer | null): boolean {
  if (!dt) return false;
  try {
    const types = Array.from(dt.types as unknown as ArrayLike<string>);
    return types.some(
      (t) => t === "Files" || t === "application/x-moz-file" || t === "public.file-url"
    );
  } catch {
    return false;
  }
}

document.addEventListener("dragover", (e) => {
  if (isExternalFileDrag(e.dataTransfer)) e.preventDefault();
});
document.addEventListener("drop", (e) => {
  if (isExternalFileDrag(e.dataTransfer)) e.preventDefault();
});

/**
 * Kill the WebView's native context menu (Reload / Print / Paste / Inspect)
 * everywhere. Custom menus (tabs, file tree, projects, composer/terminal paste)
 * call preventDefault themselves and still open — this is the backstop so a
 * right-click on chrome that has no handler never surfaces the browser menu.
 */
document.addEventListener(
  "contextmenu",
  (e) => {
    e.preventDefault();
  },
  true
);

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
