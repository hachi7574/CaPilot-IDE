#!/usr/bin/env python3
"""WebKitGTK <video> probe — no screenshots, no input injection.

Hidden Gtk/WebKit2 window + one-shot HTML that plays capilot.mp4 from a given
URL scheme. Prints one JSON object per mode with media-element state
(readyState, videoWidth, currentTime, error).

Agent-verifiable substitute for "did the wallpaper paint": WebKitGTK +
GStreamer either produce non-zero videoWidth (decoded frame) or they don't.
Wayland screenshots cannot see a native GTK window on this host.

Modes:
  file    file:///abs/capilot.mp4
  http    http://127.0.0.1:<port>/capilot.mp4   (Range-capable)
  custom  capilot-media://localhost/<encoded>    (mirrors the app protocol)
  blob    fetch(http) → blob: URL
"""
from __future__ import annotations

import argparse
import http.server
import json
import socketserver
import sys
import threading
import time
import urllib.parse
from pathlib import Path

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("WebKit2", "4.1")
gi.require_version("Soup", "3.0")
from gi.repository import Gio, GLib, Gtk, Soup, WebKit2  # noqa: E402

DEFAULT_MP4 = Path("/usr/lib/CaPilot/themes/wallpapers/capilot.mp4")
FALLBACK_MP4 = Path("/home/hachi/Project/CaPilot-Ide/themes/wallpapers/capilot.mp4")

PROBE_JS = r"""
(function () {
  const v = document.getElementById("v");
  if (!v) return JSON.stringify({ ok: false, reason: "no-video-el" });
  const err = v.error;
  return JSON.stringify({
    ok: true,
    src: v.currentSrc || v.src || "",
    readyState: v.readyState,
    networkState: v.networkState,
    paused: v.paused,
    ended: v.ended,
    videoWidth: v.videoWidth,
    videoHeight: v.videoHeight,
    currentTime: v.currentTime,
    duration: Number.isFinite(v.duration) ? v.duration : null,
    error: err ? { code: err.code, message: err.message || "" } : null,
    fetchError: (window.__probe && window.__probe.fetchError) || null,
  });
})();
"""

PAGE = """<!doctype html>
<html>
<head><meta charset="utf-8"><title>wallpaper-probe</title></head>
<body style="margin:0;background:#111">
<video id="v" muted autoplay loop playsinline preload="auto"
       style="width:320px;height:180px;object-fit:cover;background:#222"></video>
<script>
const mode = %MODE%;
const src = %SRC%;
const v = document.getElementById("v");
window.__probe = { mode, src, started: Date.now() };
async function go() {
  try {
    if (mode === "blob") {
      const r = await fetch(src);
      const b = await r.blob();
      const typed = b.type && b.type.startsWith("video/")
        ? b : new Blob([b], { type: "video/mp4" });
      v.src = URL.createObjectURL(typed);
    } else {
      v.src = src;
    }
    const p = v.play();
    if (p && p.catch) p.catch(function (e) { window.__probe.fetchError = String(e); });
  } catch (e) {
    window.__probe.fetchError = String(e);
  }
}
go();
</script>
</body>
</html>
"""


class RangeHandler(http.server.BaseHTTPRequestHandler):
    file_path: Path = FALLBACK_MP4

    def log_message(self, fmt, *args):
        sys.stderr.write("[http] " + (fmt % args) + "\n")

    def do_HEAD(self):
        self._serve(head=True)

    def do_GET(self):
        self._serve(head=False)

    def _serve(self, head: bool):
        path = urllib.parse.unquote(self.path.split("?", 1)[0])
        if path in ("/", "/index.html"):
            port = self.server.server_address[1]
            body = (
                PAGE.replace("%MODE%", json.dumps("http"))
                .replace("%SRC%", json.dumps(f"http://127.0.0.1:{port}/capilot.mp4"))
                .encode()
            )
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Access-Control-Allow-Origin", "*")
            self.end_headers()
            if not head:
                self.wfile.write(body)
            return
        if path != "/capilot.mp4":
            self.send_error(404)
            return
        data_path = self.file_path
        length = data_path.stat().st_size
        rng = self.headers.get("Range")
        start, end, status = 0, length - 1, 200
        if rng and rng.lower().startswith("bytes="):
            spec = rng.split("=", 1)[-1].split(",")[0].strip()
            a, _, b = spec.partition("-")
            if a == "":
                start = max(0, length - int(b or "0"))
            else:
                start = int(a)
                if b:
                    end = min(int(b), length - 1)
            status = 206
        nbytes = end - start + 1
        self.send_response(status)
        self.send_header("Content-Type", "video/mp4")
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(nbytes))
        if status == 206:
            self.send_header("Content-Range", f"bytes {start}-{end}/{length}")
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        if not head:
            with data_path.open("rb") as f:
                f.seek(start)
                self.wfile.write(f.read(nbytes))


def start_http(mp4: Path) -> tuple[socketserver.TCPServer, int]:
    RangeHandler.file_path = mp4
    httpd = socketserver.TCPServer(("127.0.0.1", 0), RangeHandler)
    httpd.allow_reuse_address = True
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return httpd, httpd.server_address[1]


def parse_range(rng: str | None, length: int) -> tuple[int, int, int]:
    start, end, status = 0, length - 1, 200
    if rng and rng.lower().startswith("bytes="):
        spec = rng.split("=", 1)[1].split(",")[0].strip()
        a, _, b = spec.partition("-")
        if a == "":
            start = max(0, length - int(b or "0"))
        else:
            start = int(a)
            if b:
                end = min(int(b), length - 1)
        status = 206
    return start, end, status


def register_custom_scheme(ctx: WebKit2.WebContext, mp4: Path) -> None:
    """Mirrors src-tauri wallpaper_media_response: always Accept-Ranges, full range."""
    allowed = mp4.resolve()

    def cb(request: WebKit2.URISchemeRequest) -> None:
        try:
            uri = request.get_uri()
            parsed = urllib.parse.urlparse(uri)
            path = urllib.parse.unquote(parsed.path.lstrip("/"))
            target = Path(path).resolve()
            if target != allowed:
                request.finish_error(GLib.Error(f"forbidden {target}"))
                return
            length = target.stat().st_size
            rng = None
            headers_in = request.get_http_headers()
            if headers_in is not None:
                rng = headers_in.get_one("Range")
            start, end, status = parse_range(rng, length)
            nbytes = end - start + 1
            blob = target.read_bytes()[start : start + nbytes]
            stream = Gio.MemoryInputStream.new_from_bytes(GLib.Bytes.new(blob))
            resp = WebKit2.URISchemeResponse.new(stream, nbytes)
            resp.set_status(status, None)
            resp.set_content_type("video/mp4")
            hdrs = Soup.MessageHeaders.new(Soup.MessageHeadersType.RESPONSE)
            hdrs.append("Accept-Ranges", "bytes")
            hdrs.append("Content-Length", str(nbytes))
            if status == 206:
                hdrs.append("Content-Range", f"bytes {start}-{end}/{length}")
            resp.set_http_headers(hdrs)
            request.finish_with_response(resp)
        except Exception as e:
            sys.stderr.write(f"[custom] {type(e).__name__}: {e}\n")
            request.finish_error(GLib.Error(str(e)))

    ctx.register_uri_scheme("capilot-media", cb)


def js_string(view: WebKit2.WebView, script: str) -> str | None:
    holder: dict = {}
    loop = GLib.MainLoop()

    def on_js(_view, task):
        try:
            jsres = view.run_javascript_finish(task)
            holder["s"] = jsres.get_js_value().to_string()
        except Exception as e:
            holder["e"] = str(e)
        loop.quit()

    view.run_javascript(script, None, on_js)
    # Nested loop: drain until this JS finishes.
    loop.run()
    if "e" in holder:
        raise RuntimeError(holder["e"])
    return holder.get("s")


def run_probe(mode: str, mp4: Path, timeout_s: float) -> dict:
    ctx = WebKit2.WebContext.new()
    ctx.set_cache_model(WebKit2.CacheModel.DOCUMENT_VIEWER)

    httpd = None
    port = 0
    http_src = ""
    if mode in ("http", "blob"):
        httpd, port = start_http(mp4)
        http_src = f"http://127.0.0.1:{port}/capilot.mp4"

    if mode == "custom":
        register_custom_scheme(ctx, mp4)
        src = "capilot-media://localhost/" + urllib.parse.quote(str(mp4.resolve()), safe="")
        html = PAGE.replace("%MODE%", json.dumps("custom")).replace("%SRC%", json.dumps(src))
        origin = "capilot-media://localhost/"
    elif mode == "file":
        src = mp4.resolve().as_uri()
        html = PAGE.replace("%MODE%", json.dumps("file")).replace("%SRC%", json.dumps(src))
        origin = "file:///"
    elif mode == "http":
        src = http_src
        html = PAGE.replace("%MODE%", json.dumps("http")).replace("%SRC%", json.dumps(src))
        origin = f"http://127.0.0.1:{port}/"
    elif mode == "blob":
        src = http_src
        html = PAGE.replace("%MODE%", json.dumps("blob")).replace("%SRC%", json.dumps(src))
        origin = f"http://127.0.0.1:{port}/"
    else:
        raise SystemExit(f"unknown mode {mode}")

    view = WebKit2.WebView.new_with_context(ctx)
    settings = view.get_settings()
    settings.set_property("enable-media", True)
    settings.set_media_playback_requires_user_gesture(False)
    # Hardware decode can hang offscreen; software is slower but probe-stable.
    if hasattr(settings, "set_hardware_acceleration_policy"):
        settings.set_hardware_acceleration_policy(WebKit2.HardwareAccelerationPolicy.NEVER)

    win = Gtk.Window()
    win.set_default_size(320, 180)
    win.set_title(f"webkit-video-probe:{mode}")
    win.add(view)
    win.show_all()

    result: dict = {"mode": mode, "src": src, "mp4": str(mp4)}
    deadline = [time.time() + timeout_s]
    painted = [False]

    def poll():
        if time.time() > deadline[0]:
            result["timeout"] = True
            Gtk.main_quit()
            return False
        try:
            raw = js_string(view, PROBE_JS)
            state = json.loads(raw) if raw else None
        except Exception as e:
            result["js_error"] = str(e)
            return True
        result["state"] = state
        if isinstance(state, dict):
            w = int(state.get("videoWidth") or 0)
            t = float(state.get("currentTime") or 0)
            rs = int(state.get("readyState") or 0)
            if w > 0 and (t > 0 or rs >= 2):
                painted[0] = True
                result["painted"] = True
                Gtk.main_quit()
                return False
            if state.get("error"):
                result["painted"] = False
                Gtk.main_quit()
                return False
        return True

    def load_failed(_view, _event, failing_uri, error):
        result["load_failed"] = {"uri": failing_uri, "error": str(error)}
        Gtk.main_quit()

    view.connect("load-failed", load_failed)
    view.load_html(html, origin)
    GLib.timeout_add(400, poll)
    Gtk.main()
    win.destroy()
    if httpd:
        httpd.shutdown()
    result.setdefault("painted", painted[0])
    return result


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", choices=["file", "http", "custom", "blob", "all"], default="all")
    ap.add_argument("--mp4", type=Path, default=None)
    ap.add_argument("--timeout", type=float, default=10.0)
    args = ap.parse_args()
    mp4 = args.mp4
    if mp4 is None:
        mp4 = DEFAULT_MP4 if DEFAULT_MP4.is_file() else FALLBACK_MP4
    if not mp4.is_file():
        print(json.dumps({"ok": False, "reason": f"missing {mp4}"}))
        return 2
    modes = ["file", "http", "custom", "blob"] if args.mode == "all" else [args.mode]
    reports = []
    painted_any = False
    for mode in modes:
        try:
            r = run_probe(mode, mp4, args.timeout)
        except Exception as e:
            r = {"mode": mode, "error": f"{type(e).__name__}: {e}", "painted": False}
        reports.append(r)
        painted_any = painted_any or bool(r.get("painted"))
        print(json.dumps(r, ensure_ascii=False), flush=True)
    print(
        json.dumps(
            {
                "summary": True,
                "painted_any": painted_any,
                "painted": [x["mode"] for x in reports if x.get("painted")],
                "failed": [x["mode"] for x in reports if not x.get("painted")],
            }
        )
    )
    return 0 if painted_any else 1


if __name__ == "__main__":
    sys.exit(main())
