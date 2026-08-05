import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";

// Prevent the webview from degrading file drag-and-drop into text selection /
// navigation. Without this, WebKitGTK shows an I-beam cursor and selects text
// instead of firing drop events. Component onDrop/onDragOver handlers still
// run (they call preventDefault themselves) — this only stops the browser's
// default text-selection / navigation behavior at the document level.
document.addEventListener("dragover", (e) => e.preventDefault());
document.addEventListener("drop", (e) => e.preventDefault());

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
