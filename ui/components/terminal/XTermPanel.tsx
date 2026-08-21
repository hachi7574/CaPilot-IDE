import { useEffect, useRef, useState, useCallback } from "react";
import { createPortal } from "react-dom";
import { invoke, Channel } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { useStore, AgentInfo, getTodoDragId, isTodoDrag } from "../../state/store";
import { assignTodoAndSend } from "../../state/agentActions";
import { pathsFromDataTransfer } from "../../state/dropPaths";
import { useT } from "../../i18n";
import { Icon } from "../Icon";
import {
  canForwardSgrMouse,
  isMouseTuiRuntime,
  sgrWheelReport,
} from "./mouseProtocol";
import "@xterm/xterm/css/xterm.css";

interface XTermPanelProps {
  agentId: string;
  /** True when this panel belongs to the active tab. Only an active terminal may
   *  steal focus on the F1 input↔terminal toggle; hidden resident Claude /
   *  OpenCode panels must not. Defaults to true for standalone use. */
  active?: boolean;
  /** Canvas-embedded terminals: paint an opaque --term-bg cell instead of
   *  transparent cells. WebKitGTK otherwise leaves per-row compositor garbage
   *  (a solid blue band behind glyphs) when the canvas is stretched. */
  opaqueBg?: boolean;
  /** Override cell font size in CSS px. Canvas zoom passes baseSize * zoom so
   *  selection hit-testing stays aligned (no CSS scale on the terminal). */
  fontSizePx?: number;
}

/** Terminal PTY font size (px) per UI font-size preset. Base preset "s" keeps
 *  the historic 13px; larger presets scale it step-for-step with the CSS tokens. */
const TERMINAL_FONT_SIZES: Record<string, number> = {
  s: 13,
  m: 14,
  l: 15,
  xl: 16,
  xxl: 17,
};

const CLAUDE_PERMISSION_MARKERS: ReadonlyArray<[string, string]> = [
  // Claude renders manual without the cycle hint; the other four include it.
  ["manual mode on", "ask"],
  ["accept edits on (shift+tab to cycle)", "accept_edits"],
  ["plan mode on (shift+tab to cycle)", "plan"],
  ["bypass permissions on (shift+tab to cycle)", "yolo"],
  ["auto mode on (shift+tab to cycle)", "auto"],
];

/** Return the last Claude permission status rendered in a PTY fragment.
 * A held Shift+Tab can redraw several modes in one packet, so first-match
 * semantics would leave the composer one or more modes behind the terminal. */
function detectClaudePermissionMode(text: string): string | null {
  const normalized = text.toLowerCase();
  let detected: string | null = null;
  let detectedAt = -1;
  for (const [marker, mode] of CLAUDE_PERMISSION_MARKERS) {
    const at = normalized.lastIndexOf(marker);
    if (at > detectedAt) {
      detectedAt = at;
      detected = mode;
    }
  }
  return detected;
}

/** Shell-escape a path so spaces / quotes survive (single-quote wrap, `'` → `'\''`). */
function shellEscape(path: string): string {
  return `'${path.replace(/'/g, `'\\''`)}'`;
}

/**
 * xterm.js injects its DOM-renderer `<style>` elements — the per-cell sizing
 * rule (`.xterm-dom-renderer-owner-N .xterm-rows span { display: inline-block;
 * height: 100%; … }`) and the theme rule — directly into `.xterm-screen` (see
 * the renderer's `_injectCss` / `_updateDimensions`). Those rules are
 * document-global, but living inside the screen their CSS text leaks into
 * `.xterm-screen.textContent`: DOM-inspecting tools then flag phantom "letters
 * not displayed", and screen readers read CSS aloud. Relocate them to `<head>`
 * once they appear; xterm keeps the element references and keeps rewriting
 * `textContent` there, so rendering is byte-for-byte unchanged.
 *
 * A terminal creates exactly two such styles (theme on the first render pass,
 * dimensions on the same pass), never re-creates them, so a short-lived
 * observer that detaches once the screen stays clean is enough — it must not
 * remain a per-frame cost on WebKitGTK.
 */
function relocateXtermStyles(container: HTMLElement | null): void {
  const screen = container?.querySelector<HTMLElement>(".xterm-screen");
  if (!screen) return;
  let movedStyles = 0;
  const moveStyles = () => {
    for (const st of Array.from(screen.querySelectorAll("style"))) {
      document.head.appendChild(st);
      movedStyles += 1;
    }
  };
  moveStyles();
  // A terminal creates exactly two such styles (theme + dimensions), both in
  // the first render pass right after open(). If they're already there the
  // sync pass above is the whole fix; otherwise watch the screen's direct
  // children (which rarely change after open — rows live in the inner
  // container) until both are out. A 1s timer bounds the observer so it can
  // never become a permanent per-frame cost on WebKitGTK.
  if (movedStyles >= 2) return;
  const obs = new MutationObserver(() => {
    moveStyles();
    if (movedStyles >= 2) obs.disconnect();
  });
  obs.observe(screen, { childList: true });
  window.setTimeout(() => obs.disconnect(), 1000);
}

/** Copy text to the OS clipboard (navigator API with execCommand fallback —
 *  matches the sidebar's `copyText`; WebKitGTK may reject the async API). */
async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    // fall through to the legacy path
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  try {
    document.execCommand("copy");
  } catch {
    // ignore
  }
  document.body.removeChild(ta);
}

/**
 * agentIds with an agent_resume invoke already in flight. React StrictMode mounts
 * effects twice (mount→cleanup→mount); without this guard each restored terminal
 * spawns TWO claude processes (~300MB waste each). The second mount skips the
 * invoke and picks up the channel once the first mount's resolve stores it.
 */
const resumeInFlight = new Set<string>();

/** Last `focusRequest.seq` an active terminal has consumed for the F1 toggle.
 *  Module-level (not per-instance) so a reactivated resident OpenCode panel
 *  can't re-steal focus for a request a different terminal already handled. */
let lastFocusHandledSeq = 0;

/** Last `searchRequest.seq` an active terminal has consumed for Ctrl+F search. */
let lastSearchHandledSeq = 0;

/** True when an xterm onData payload is a mouse MOTION event (hover / drag
 *  without a fresh button press). The CLIs enable motion tracking (DECSET
 *  1002/1003, SGR 1006), so merely having the mouse over the canvas emits a
 *  continuous stream of `ESC [ < b ; x ; y M` sequences — bit 5 of `b` marks
 *  motion. Forwarding those makes the CLI redraw on every move and stamps the
 *  tab bar's 运行中/空闲 clock, so a hover reads as 运行中. Clicks (`b & 32 == 0`)
 *  and wheel (`b & 64`) are real interactions and pass through. */
function isMouseMotion(data: string): boolean {
  // SGR (DECSET 1006): ESC [ < b ; x ; y (M|m)
  if (data.startsWith("\x1b[<")) {
    const m = /^\x1b\[<(\d+);\d+;\d+[Mm]$/.exec(data);
    return !!m && (Number(m[1]) & 32) !== 0;
  }
  // Legacy (DECSET 1000/1002): ESC [ M b x y (each byte +32).
  if (
    data.length === 6 &&
    data.charCodeAt(0) === 0x1b &&
    data.charCodeAt(1) === 0x5b &&
    data.charCodeAt(2) === 0x4d
  ) {
    return ((data.charCodeAt(3) - 32) & 32) !== 0;
  }
  return false;
}

/**
 * Text selection vs. mouse tracking. The claude/opencode TUIs enable xterm's
 * mouse-reporting modes (DECSET 1000/1002/1003 + SGR 1006), and once any is
 * active xterm routes mouse drags to the PTY instead of selecting — which is
 * exactly why selection works in codex (no tracking) but not in the TUIs.
 *
 * `STRIP_MOUSE_MODES` filters the tracking *enables* out of the incoming stream
 * so xterm stays in its default non-reporting mode and native drag / double-
 * click selection is restored. SGR encoding (1006) is deliberately kept: on its
 * own it makes xterm emit nothing, and the wheel/click forwarders below need
 * the CLI to still accept SGR mouse reports.
 *
 *   1000 button · 1002 button+drag · 1003 any-motion · 1004 focus · 1005 UTF-8
 *   · 1015 urxvt encoding
 */
const STRIP_MOUSE_MODES = new Set([1000, 1002, 1003, 1004, 1005, 1015]);

/** True if `bytes` contains a complete `ESC [ ?` (DECSET/DECRST) marker. */
function hasDecsetMarker(bytes: Uint8Array): boolean {
  for (let i = 0; i + 2 < bytes.length; i++) {
    if (bytes[i] === 0x1b && bytes[i + 1] === 0x5b && bytes[i + 2] === 0x3f) {
      return true;
    }
  }
  return false;
}

/** Length of a trailing `ESC…`, `ESC […` or `ESC [?…` prefix that could become
 *  a DECSET sequence once the rest of the bytes arrive in a later flush. */
function decsetPrefixLength(bytes: Uint8Array): number {
  const n = bytes.length;
  if (n >= 3 && bytes[n - 3] === 0x1b && bytes[n - 2] === 0x5b && bytes[n - 1] === 0x3f) return 3;
  if (n >= 2 && bytes[n - 2] === 0x1b && bytes[n - 1] === 0x5b) return 2;
  if (n >= 1 && bytes[n - 1] === 0x1b) return 1;
  return 0;
}

/** Stateful DECSET filter. Mode sequences can be split across PTY writes, so a
 *  truncated `ESC [ ? <digits…` prefix is carried into the next call. */
function createModeSequenceFilter() {
  let carry: number[] = [];
  let sgr = false;
  return {
    /** True once `CSI ? 1006 h` (SGR mouse encoding) was requested by the CLI. */
    get sgr(): boolean {
      return sgr;
    },
    /** Strip mouse-tracking enables, pass everything else through untouched.
     *  Returns null when the frame contained only stripped sequences. */
    filter(bytes: Uint8Array): Uint8Array | null {
      // Fast path: no ESC byte at all (plain text), or ESC present but nothing
      // here that could become a DECSET sequence — hand the frame through
      // without a copy. (Color-rich TUI frames hit this: they contain ESC but
      // almost never `ESC [?`.)
      if (carry.length === 0) {
        if (bytes.indexOf(0x1b) === -1) return bytes;
        if (!hasDecsetMarker(bytes) && decsetPrefixLength(bytes) === 0) {
          return bytes;
        }
      }
      const src = carry.length ? [...carry, ...bytes] : [...bytes];
      carry = [];
      const out: number[] = [];
      let i = 0;
      while (i < src.length) {
        if (src[i] !== 0x1b) {
          out.push(src[i]);
          i++;
          continue;
        }
        // ESC — only interesting as the start of `ESC [ ? <modes> <h|l>`.
        if (i + 1 >= src.length || src[i + 1] !== 0x5b) {
          if (i + 1 >= src.length) {
            carry = src.slice(i); // trailing ESC may open a marker next flush
            break;
          }
          out.push(src[i]);
          i++;
          continue;
        }
        if (i + 2 >= src.length || src[i + 2] !== 0x3f) {
          if (i + 2 >= src.length) {
            carry = src.slice(i); // trailing `ESC [` — wait for the rest
            break;
          }
          out.push(src[i]);
          i++;
          continue;
        }
        // `ESC [ ?` — parse the (possibly `;`-separated) mode list.
        let k = i + 3;
        const modes: number[] = [];
        let cur = -1;
        while (k < src.length) {
          const c = src[k];
          if (c >= 0x30 && c <= 0x39) {
            cur = cur === -1 ? c - 0x30 : cur * 10 + (c - 0x30);
            k++;
            continue;
          }
          if (c === 0x3b) {
            if (cur !== -1) modes.push(cur);
            cur = -1;
            k++;
            continue;
          }
          break;
        }
        if (k >= src.length) {
          carry = src.slice(i); // digits/params truncated mid-sequence
          break;
        }
        if (cur !== -1) modes.push(cur);
        const final = src[k];
        if (final !== 0x68 && final !== 0x6c) {
          out.push(src[i]);
          i++;
          continue;
        }
        if (modes.length === 0) {
          out.push(src[i]);
          i++;
          continue;
        }
        if (final === 0x68) {
          if (modes.includes(1006)) sgr = true;
          const keep = modes.filter((m) => !STRIP_MOUSE_MODES.has(m));
          if (keep.length !== modes.length) {
            // Rewrite with the tracking modes removed (and drop entirely if none
            // remain). A `;`-combined enable like `?1000;1006h` keeps 1006.
            if (keep.length) {
              out.push(0x1b, 0x5b, 0x3f);
              keep.forEach((m, j) => {
                if (j) out.push(0x3b);
                for (const d of String(m)) out.push(d.charCodeAt(0));
              });
              out.push(0x68);
            }
            i = k + 1;
            continue;
          }
        }
        out.push(...src.slice(i, k + 1));
        i = k + 1;
      }
      return out.length ? new Uint8Array(out) : null;
    },
  };
}

/** Resolve a `:root` CSS custom property to its concrete value. xterm.js theme
 *  and decoration options need literal color strings, so instead of duplicating
 *  the palette we read it from the CSS (single source of truth). */
const cssVar = (name: string, fallback: string): string => {
  const root = typeof document !== "undefined" ? document.documentElement : null;
  return (root ? getComputedStyle(root).getPropertyValue(name).trim() : "") || fallback;
};

/** Theme `--term-veil` (0–1). 0 lets wallpaper show through; 1 is opaque. */
const termVeil = (): number => {
  const n = parseFloat(cssVar("--term-veil", "0"));
  return Number.isFinite(n) ? Math.min(1, Math.max(0, n)) : 0;
};

/** Read the current CSS palette into xterm, whose canvas renderer cannot use
 *  CSS variables directly. Called both at construction and on theme changes.
 *  When `--term-veil` is below 1 the canvas default cell is transparent so the
 *  host's color-mix fill (and wallpaper beneath it) can show through.
 *  allowTransparency stays on so a live theme switch can fade the cell without
 *  reconstructing the PTY. */
const readTerminalTheme = (opaque?: boolean) => ({
  background:
    opaque || termVeil() >= 0.999
      ? cssVar("--term-bg", "#0D1117")
      : "rgba(0,0,0,0)",
  foreground: cssVar("--pl-fg", "#ABB2BF"),
  cursor: cssVar("--pl-cursor", "#5C6370"),
  selectionBackground: cssVar("--pl-selection", "#3A3F4B"),
  black: cssVar("--pl-black", "#1E2127"),
  red: cssVar("--pl-red", "#E06C75"),
  green: cssVar("--pl-green", "#98C379"),
  yellow: cssVar("--pl-yellow", "#D19A66"),
  blue: cssVar("--pl-blue", "#61AFEF"),
  magenta: cssVar("--pl-magenta", "#C678DD"),
  cyan: cssVar("--pl-cyan", "#56B6C2"),
  white: cssVar("--pl-white", "#ABB2BF"),
  brightBlack: cssVar("--pl-bright-black", "#5C6370"),
  brightRed: cssVar("--pl-bright-red", "#E06C75"),
  brightGreen: cssVar("--pl-bright-green", "#98C379"),
  brightYellow: cssVar("--pl-bright-yellow", "#D19A66"),
  brightBlue: cssVar("--pl-bright-blue", "#61AFEF"),
  brightMagenta: cssVar("--pl-bright-magenta", "#C678DD"),
  brightCyan: cssVar("--pl-bright-cyan", "#56B6C2"),
  brightWhite: cssVar("--pl-bright-white", "#FFFFFF"),
});

/** xterm panel bound to an agent's PTY channel.
 *
 * Race handling: the Composer starts buffering channel output into
 * `store.agentOutputs` from the instant the PTY spawns. This component drains
 * that buffer on mount, then redirects the channel straight to the terminal.
 * On unmount it routes output back to the buffer so nothing is lost if the tab
 * is reopened. When the channel object changes (e.g. runtime switch), the
 * effect re-runs and attaches the new channel.
 */
export function XTermPanel({
  agentId,
  active = true,
  opaqueBg = false,
  fontSizePx,
}: XTermPanelProps) {
  const t = useT();
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const channelRef = useRef<Channel<number[]> | null>(null);
  // Precise subscription: subscribe only to THIS agent's channel. Zustand
  // re-renders on reference inequality — `Map.get` returns the same Channel
  // object until it is actually replaced, so this only re-renders when this
  // agent's channel changes. Subscribing to the whole `agentChannels` Map made
  // every mounted XTermPanel re-render whenever ANY agent's channel changed
  // (e.g. a different agent spawning/resuming elsewhere in the app).
  const channel = useStore((s) => s.agentChannels.get(agentId));
  const fontScale = useStore((s) => s.fontScale);
  const themeId = useStore((s) => s.themeId);
  const focusRequest = useStore((s) => s.focusRequest);
  const searchRequest = useStore((s) => s.searchRequest);
  // Immutable per agent; a primitive selection re-renders only when THIS agent's
  // runtime actually changes (never for other agents' updates). Used to give the
  // container a runtime class so CSS can scope xterm rules (e.g. the viewport
  // background) to specific runtimes.
  const runtime = useStore((s) => s.agents.get(agentId)?.runtime);

  // Terminal Ctrl+F search bar state. The SearchAddon instance itself lives in
  // searchAddonRef (created in the terminal init effect); only the UI state and
  // query live here so typing stays on the React side, never the PTY.
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<{
    resultIndex: number;
    resultCount: number;
  } | null>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const searchAddonRef = useRef<SearchAddon | null>(null);

  // DevPlan §4.2 ④ — dragging a file onto the terminal pastes its path
  // (shell-escaped) into the PTY. Guards against double-insert when both the DOM
  // drop handler and the Tauri drag-drop event observe the same physical drop.
  const dropHandledRef = useRef(false);
  // Nesting counter (dragenter/dragleave fire when crossing xterm child nodes).
  const dragDepthRef = useRef(0);
  const [dragHover, setDragHover] = useState(false);
  const [pasteMenu, setPasteMenu] = useState<{
    x: number;
    y: number;
    into: "pty" | "search";
  } | null>(null);

  /** Tauri drag-drop positions are physical px; CSS rects are CSS px. */
  const isPointInTerminal = useCallback((pos: { x: number; y: number }) => {
    const el = containerRef.current;
    if (!el) return false;
    // Resident OpenCode terminals remain mounted while another tab is visible.
    // Do not let the webview-wide Tauri drop listener target a hidden panel.
    if (getComputedStyle(el).visibility === "hidden") return false;
    const r = el.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    const x = pos.x / dpr;
    const y = pos.y / dpr;
    // A few px of tolerance so drops on the container's border still count.
    return (
      x >= r.left - 4 && x <= r.right + 4 && y >= r.top - 4 && y <= r.bottom + 4
    );
  }, []);

  /** Insert shell-escaped path(s) into the PTY (raw keystroke passthrough). */
  const insertPathToPty = useCallback(
    (paths: string[]) => {
      if (!paths.length) return;
      const escaped = paths.map(shellEscape).join(" ");
      // Dropping a path is explicit terminal engagement — end the spawn wake so
      // the command the user runs against it reads as 运行中 in the tab bar.
      useStore.getState().markAgentActive(agentId);
      // Leading space so the path doesn't glue to preceding text (typing a path
      // in a shell); raw:true sends the keystrokes verbatim (no \r appended).
      const payload = ` ${escaped}`;
      invoke("agent_write", { id: agentId, data: payload, raw: true }).catch(
        () => {}
      );
    },
    [agentId]
  );

  /** Run the terminal search. Empty query clears decorations; otherwise jumps to
   *  the next/previous match and paints all matches via the SearchAddon. */
  const runFind = useCallback((query: string, dir: "next" | "prev") => {
    const addon = searchAddonRef.current;
    if (!addon) return;
    if (!query) {
      addon.clearDecorations();
      setSearchResults(null);
      return;
    }
    const opts = {
      decorations: {
        matchBackground: cssVar("--search-match-bg", "#2E2A4A"),
        matchOverviewRuler: cssVar("--search-match-bg", "#2E2A4A"),
        activeMatchBackground: cssVar("--brand", "#8B5CF6"),
        activeMatchColorOverviewRuler: cssVar("--brand", "#8B5CF6"),
      },
    };
    if (dir === "next") addon.findNext(query, opts);
    else addon.findPrevious(query, opts);
  }, []);

  /** Close the search bar: drop decorations, clear the count, return focus. */
  const closeSearch = useCallback(() => {
    setSearchOpen(false);
    searchAddonRef.current?.clearDecorations();
    setSearchResults(null);
    termRef.current?.focus();
  }, []);

  /** Paste clipboard text (right-click menu). PTY path goes through `term.paste`
   *  so bracketed-paste wrapping still applies; search-bar path replaces the
   *  query input's current selection. */
  const pasteClipboard = useCallback(async () => {
    const into = pasteMenu?.into ?? "pty";
    setPasteMenu(null);
    let text = "";
    try {
      text = await navigator.clipboard.readText();
    } catch {
      text = "";
    }
    if (into === "search") {
      const input = searchInputRef.current;
      if (!input) return;
      if (!text) {
        input.focus();
        return;
      }
      const start = input.selectionStart ?? input.value.length;
      const end = input.selectionEnd ?? start;
      const next = input.value.slice(0, start) + text + input.value.slice(end);
      setSearchQuery(next);
      runFind(next, "next");
      requestAnimationFrame(() => {
        input.focus();
        const pos = start + text.length;
        input.setSelectionRange(pos, pos);
      });
      return;
    }
    const term = termRef.current;
    if (!text || !term) {
      term?.focus();
      return;
    }
    term.paste(text);
    term.focus();
  }, [pasteMenu, runFind]);

  // Close the terminal paste menu on outside click / Escape. Capture-phase
  // contextmenu so another custom menu's stopPropagation still dismisses us.
  // Same 150 ms compositor-click guard as the sidebar / tab menus.
  useEffect(() => {
    if (!pasteMenu) return;
    const openedAt = Date.now();
    const close = () => {
      if (Date.now() - openedAt > 150) setPasteMenu(null);
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && close();
    window.addEventListener("click", close);
    window.addEventListener("contextmenu", close, true);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("contextmenu", close, true);
      window.removeEventListener("keydown", onKey);
    };
  }, [pasteMenu]);

  // Opening the bar focuses the query input and selects the old text so typing
  // a new term replaces it (re-search is a single keystroke).
  useEffect(() => {
    if (!searchOpen) return;
    const raf = requestAnimationFrame(() => {
      searchInputRef.current?.focus();
      searchInputRef.current?.select();
    });
    return () => cancelAnimationFrame(raf);
  }, [searchOpen]);

  // While the bar is open, F3/Shift+F3 navigate matches from anywhere in the
  // terminal (the input's own Enter/Shift+Enter cover the input-focused case).
  useEffect(() => {
    if (!searchOpen) return;
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "F3") {
        e.preventDefault();
        runFind(searchQuery, e.shiftKey ? "prev" : "next");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [searchOpen, searchQuery, runFind]);

  useEffect(() => {
    if (!containerRef.current) return;

    const term = new Terminal({
      // Static cursor: xterm implements cursorBlink as an injected infinite CSS
      // keyframe animation, and WebKitGTK's software compositor repaints the
      // whole window at 60 fps for any CSS animation — measured ~1 full core.
      // Claude's TUI draws its own cursor inside the PTY, so a static xterm
      // cursor is barely visible. cursorBlink:false costs nothing perceptible.
      cursorBlink: false,
      fontSize: fontSizePx ?? TERMINAL_FONT_SIZES[fontScale] ?? 13,
      fontFamily: "'JetBrainsMono', ui-monospace, monospace",
      theme: readTerminalTheme(opaqueBg),
      allowTransparency: !opaqueBg,
      allowProposedApi: true,
    });

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(containerRef.current);
    // Belt-and-suspenders for selection: force xterm out of any mouse-reporting
    // mode a PTY may have enabled before this instance existed (e.g. a session
    // started before the tracking strip shipped). The DECSET filter below keeps
    // the modes off for everything that streams in afterwards.
    term.write("\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1004l");
    // Keep .xterm-screen's textContent pure terminal text: xterm appends its
    // renderer <style> elements there, whose CSS text shows up as phantom
    // "missing letters" in DOM text. Relocated to <head> (document-global CSS).
    relocateXtermStyles(containerRef.current);

    // Keyboard copy. xterm.js maps Ctrl+C to PTY SIGINT (^C) and never produces
    // the browser's default copy; WebView2 / WebKitGTK also have no native menu
    // fallback. Returning false stops the event reaching xterm's own key handling.
    term.attachCustomKeyEventHandler((ev) => {
      // F1 is reserved for the composer↔terminal focus toggle (handled by a
      // window-level listener). Swallow it here so the PTY never receives the
      // F1 escape sequence on top of the focus switch — most CLIs would act on
      // it (help panel, etc.), leaving the two areas in an inconsistent state.
      if (
        ev.type === "keydown" &&
        ev.key === "F1" &&
        !ev.ctrlKey &&
        !ev.shiftKey &&
        !ev.altKey &&
        !ev.metaKey
      ) {
        return false;
      }
      // Ctrl+F opens terminal search. Swallow it here so the PTY never receives
      // the `^F` control code — most shells bind Ctrl+F to forward-char.
      if (
        ev.type === "keydown" &&
        ev.ctrlKey &&
        !ev.shiftKey &&
        !ev.altKey &&
        !ev.metaKey &&
        ev.key.toLowerCase() === "f"
      ) {
        setSearchOpen(true);
        return false;
      }
      // Copy shortcuts. xterm maps Ctrl+C to PTY SIGINT (^C) and never produces
      // the browser's default copy, and WebView2 / WebKitGTK have no native menu
      // fallback. Match VS Code / Windows Terminal: Ctrl+C (and Cmd+C / Ctrl+Insert)
      // copies when there is a selection; Ctrl+C without a selection still sends
      // SIGINT. Ctrl+Shift+C is the Linux-terminal copy chord and always copies.
      if (ev.type === "keydown" && !ev.altKey) {
        const key = ev.key.toLowerCase();
        const ctrlOnly = ev.ctrlKey && !ev.metaKey;
        const cmdOnly = ev.metaKey && !ev.ctrlKey;
        const isCtrlShiftC = ctrlOnly && ev.shiftKey && key === "c";
        const isCtrlC = ctrlOnly && !ev.shiftKey && key === "c";
        const isCmdC = cmdOnly && !ev.shiftKey && key === "c";
        const isCtrlInsert =
          ctrlOnly && !ev.shiftKey && ev.key === "Insert";
        if (isCtrlShiftC || isCtrlC || isCmdC || isCtrlInsert) {
          const hasSel = term.hasSelection();
          // Bare Ctrl+C with nothing selected is SIGINT — leave it to xterm.
          if (isCtrlC && !hasSel) return true;
          if (hasSel) {
            const text = term.getSelection();
            if (text) void copyText(text);
          }
          // Stop the window-level file-tree Ctrl+C from stealing this chord.
          ev.preventDefault();
          ev.stopPropagation();
          return false;
        }
      }
      return true;
    });

    termRef.current = term;
    fitAddonRef.current = fitAddon;

    let disposed = false;
    let pendingChunks: Uint8Array[] = [];
    let pendingBytes = 0;
    let flushRaf: number | null = null;
    let redrawPulseTimer: ReturnType<typeof setTimeout> | null = null;
    let redrawRestoreTimer: ReturnType<typeof setTimeout> | null = null;
    // Dims the last TUI redraw pulse used, so a pulse that ran before the
    // terminal settled can re-fire once the real size arrives (the one-shot
    // `redrawPulseRequested` guard left a stale-size frame stuck until a manual
    // window resize — see pulseTuiRedraw).
    let pulseDims = { rows: 0, cols: 0 };
    let pendingClaudeMode: string | null = null;
    let modePersistTimer: ReturnType<typeof setTimeout> | null = null;

    // Selection-vs-tracking fix: strip DECSET mouse-tracking enables from the
    // byte stream before xterm parses it (see STRIP_MOUSE_MODES). One instance
    // per terminal — its carry tracks this PTY's own stream.
    const modeFilter = createModeSequenceFilter();

    // Ctrl+F search: the SearchAddon scans xterm's live buffer and paints match
    // decorations. One instance per terminal; its results callback updates the
    // React-side `n/N` counter.
    const searchAddon = new SearchAddon();
    term.loadAddon(searchAddon);
    searchAddonRef.current = searchAddon;
    const searchResultsSub = searchAddon.onDidChangeResults((results) => {
      if (!disposed) setSearchResults(results);
    });

    const persistClaudeMode = () => {
      modePersistTimer = null;
      const mode = pendingClaudeMode;
      pendingClaudeMode = null;
      if (!mode) return;
      invoke("agent_set_session_config", { id: agentId, mode }).catch(() => {});
    };

    const syncClaudeMode = (text: string) => {
      const state = useStore.getState();
      const agent = state.agents.get(agentId);
      if (agent?.runtime !== "claude") return;

      const mode = detectClaudePermissionMode(text);
      if (!mode || agent.mode === mode) return;

      state.addAgent({ ...agent, mode }, null);
      const activeTab = state.tabs.find((tab) => tab.id === state.activeTabId);
      const composerAgentId = activeTab?.agentId ?? null;
      if (composerAgentId === agentId) state.setPermissionMode(mode);

      // A held key can emit many redraws per second. Reflect every one in the
      // UI immediately, but persist only the final settled mode.
      pendingClaudeMode = mode;
      if (modePersistTimer) clearTimeout(modePersistTimer);
      modePersistTimer = setTimeout(persistClaudeMode, 150);
    };

    const syncClaudeModeFromScreen = () => {
      if (disposed) return;
      // Claude's normal TUI redraws only changed character spans, so its raw
      // PTY packets do not necessarily contain a complete status phrase. Read
      // xterm's post-ANSI visible buffer, where the final line is reconstructed.
      const buffer = term.buffer.active;
      const lines: string[] = [];
      const end = Math.min(buffer.length, buffer.baseY + term.rows);
      for (let row = buffer.baseY; row < end; row++) {
        const line = buffer.getLine(row)?.translateToString(true);
        if (line) lines.push(line);
      }
      // The compact join also covers a status phrase wrapped by a very narrow
      // terminal; the newline join keeps normal rows separated.
      syncClaudeMode(`${lines.join("\n")}\n${lines.join("")}`);
    };

    const flushPending = () => {
      flushRaf = null;
      if (disposed || pendingBytes === 0) return;
      const merged = new Uint8Array(pendingBytes);
      let offset = 0;
      for (const chunk of pendingChunks) {
        merged.set(chunk, offset);
        offset += chunk.byteLength;
      }
      pendingChunks = [];
      pendingBytes = 0;
      try {
        const filtered = modeFilter.filter(merged);
        if (filtered) term.write(filtered, syncClaudeModeFromScreen);
      } catch {
        // terminal disposed
      }
    };

    const writeToTerm = (data: number[]) => {
      if (disposed) return;
      const chunk = new Uint8Array(data);
      pendingChunks.push(chunk);
      pendingBytes += chunk.byteLength;
      // Coalesce all PTY packets arriving in one paint cycle. This keeps the
      // WebKitGTK/xterm hot path to at most one write and repaint per frame.
      if (flushRaf === null) flushRaf = requestAnimationFrame(flushPending);
    };

    let lastResize = { rows: 0, cols: 0 };
    const sendResize = () => {
      const rows = term.rows || 24;
      const cols = term.cols || 80;
      if (rows === lastResize.rows && cols === lastResize.cols) return;
      lastResize = { rows, cols };
      invoke("agent_resize", { id: agentId, rows, cols }).catch(() => {});
    };

    /** Alternate-screen TUIs (Claude / OpenCode) may stay idle after this xterm
     *  component is recreated. The PTY is still alive, but xterm has no screen
     *  snapshot to paint and an unchanged resize is normally suppressed. A
     *  one-column resize pulse makes the native TUI redraw without sending it
     *  an input command.
     *
     *  The pulse is dimension-aware: it records the size it pulsed at and skips
     *  a repeat for the same size, so a pulse fired before the terminal settled
     *  (font still loading, container mid-layout) is retried with the final dims
     *  instead of leaving a wrong-sized frame on screen until the user resizes
     *  the window by hand. */
    const pulseTuiRedraw = () => {
      if (disposed) return;
      if (!isMouseTuiRuntime(useStore.getState().agents.get(agentId)?.runtime)) {
        return;
      }
      const rows = term.rows || 24;
      const cols = term.cols || 80;
      if (pulseDims.rows === rows && pulseDims.cols === cols) return;
      pulseDims = { rows, cols };
      const pulseCols = cols > 2 ? cols - 1 : cols + 1;
      // Drop any pending restore so a re-pulse at a new size can't be cancelled
      // by the previous restore firing after it (out-of-order dims).
      if (redrawRestoreTimer) clearTimeout(redrawRestoreTimer);
      invoke("agent_resize", { id: agentId, rows, cols: pulseCols }).catch(() => {});
      redrawRestoreTimer = setTimeout(() => {
        redrawRestoreTimer = null;
        if (disposed) return;
        invoke("agent_resize", { id: agentId, rows, cols }).catch(() => {});
        lastResize = { rows, cols };
      }, 32);
    };
    /** Debounced request for a TUI redraw pulse — schedule one, replacing any
     *  pending one, so rapid fit passes collapse into a single pulse at the
     *  settled size. */
    const requestTuiRedraw = () => {
      if (!isMouseTuiRuntime(useStore.getState().agents.get(agentId)?.runtime)) {
        return;
      }
      if (redrawPulseTimer) clearTimeout(redrawPulseTimer);
      redrawPulseTimer = setTimeout(() => {
        redrawPulseTimer = null;
        pulseTuiRedraw();
      }, 80);
    };

    /** Fit the terminal to its container and force a repaint. A terminal opened
     *  during a tab switch can land in a 0×0 / not-yet-laid-out container, which
     *  paints a blank canvas until something resizes it — fit() alone won't redraw
     *  when the size didn't change, so refresh() forces the paint. A TUI redraw
     *  pulse is also (re)scheduled so Claude / OpenCode redraw at the settled size. */
    const fitAndRefresh = () => {
      if (disposed) return;
      try {
        const parent = term.element?.parentElement;
        const cell = (term as unknown as { _core?: { _renderService?: { dimensions?: { css?: { cell?: { width: number; height: number } } } } } })
          ._core?._renderService?.dimensions?.css?.cell;
        if (parent && cell && cell.width > 0 && cell.height > 0) {
          const cs = window.getComputedStyle(parent);
          const padX =
            (parseFloat(cs.paddingLeft) || 0) + (parseFloat(cs.paddingRight) || 0);
          const padY =
            (parseFloat(cs.paddingTop) || 0) + (parseFloat(cs.paddingBottom) || 0);
          const cols = Math.max(2, Math.floor((parent.clientWidth - padX) / cell.width));
          const rows = Math.max(1, Math.floor((parent.clientHeight - padY) / cell.height));
          if (term.cols !== cols || term.rows !== rows) {
            term.resize(cols, rows);
          }
        } else {
          fitAddon.fit();
        }
      } catch {
        // Container has no dimensions yet — the deferred retry handles it.
      }
      if (term.rows > 0 && term.cols > 0) {
        sendResize();
        term.refresh(0, term.rows - 1);
        // Alternate-screen TUIs (Claude / OpenCode) also get a resize pulse so
        // they redraw at the settled size after a remount or layout change.
        requestTuiRedraw();
      }
    };
    // Defer the initial fit so the panel has its final size (a tab switch can
    // mount mid-layout). Two frames cover layout/font settling; the ResizeObserver
    // below then keeps it correct. Fonts: the first fit can measure before the
    // pixel/mono font is ready → 0 rows; re-fit once fonts resolve.
    const raf1 = requestAnimationFrame(() => fitAndRefresh());
    const raf2 = requestAnimationFrame(() => fitAndRefresh());
    const fontReady = document.fonts?.ready;
    if (fontReady) {
      fontReady.then(() => {
        if (!disposed) fitAndRefresh();
      });
    }

    /** Attach a channel: drain buffered output, then stream live. */
    const attachChannel = (ch: Channel<number[]>) => {
      channelRef.current = ch;
      ch.onmessage = writeToTerm;
      const buffered = useStore.getState().agentOutputs.get(agentId);
      if (buffered && buffered.length) {
        writeToTerm(buffered);
        useStore.getState().clearAgentOutput(agentId);
      }
      sendResize();
      requestTuiRedraw();
    };

    if (channel) {
      attachChannel(channel);
    } else {
      // Ended (`done`) sessions never auto-resume on their own — only an
      // explicit sidebar "已结束" reopen (which sets resumeOnOpen) brings one
      // back. Otherwise a finished session's tab would silently spawn a fresh
      // process whenever its channel change re-runs this effect.
      const state = useStore.getState();
      const ended = state.agents.get(agentId)?.status === "done";
      const wantsResume = state.resumeOnOpen.has(agentId);
      if (wantsResume) state.consumeResume(agentId);
      if (ended && !wantsResume) {
        // Dead terminal reopened by mistake: render whatever was buffered, do
        // not spawn a new process.
        const buffered = state.agentOutputs.get(agentId);
        if (buffered && buffered.length) {
          writeToTerm(buffered);
          state.clearAgentOutput(agentId);
        }
      } else if (resumeInFlight.has(agentId)) {
        // StrictMode double-mount: the first mount's agent_resume is still running.
        // Skip the invoke (don't spawn a second claude); when it resolves, addAgent
        // stores the channel and this effect re-runs with a live channel to attach.
      } else {
        // Restored session with no live PTY → resume it.
        resumeInFlight.add(agentId);
        const resumeChannel = new Channel<number[]>();
        resumeChannel.onmessage = (data) =>
          useStore.getState().appendAgentOutput(agentId, data);
        invoke<AgentInfo>("agent_resume", {
          id: agentId,
          onData: resumeChannel,
          rows: term.rows || 24,
          cols: term.cols || 80,
        })
          .then((info) => {
            resumeInFlight.delete(agentId);
            // addAgent unconditionally (even if this mount is already disposed): the
            // store write is what hands the channel to any concurrently-mounted tab.
            useStore.getState().addAgent(info, resumeChannel);
            if (disposed) return;
            attachChannel(resumeChannel);
          })
          .catch((err) => {
            resumeInFlight.delete(agentId);
            const bytes = Array.from(new TextEncoder().encode(`[resume failed] ${err}\n`));
            writeToTerm(bytes);
          });
      }
    }

    // Forward user input to the PTY (raw keystroke passthrough). Marking the
    // agent active keeps the tab bar's 运行中/空闲 split honest when a command
    // is submitted from the terminal itself. Mouse MOTION events are dropped
    // before either step — a hover is not input, and neither the PTY nor the
    // activity clock should see it.
    term.onData((data) => {
      if (isMouseMotion(data)) return;
      invoke("agent_write", { id: agentId, data, raw: true }).catch(() => {});
      useStore.getState().markAgentActive(agentId);
    });

    /** Convert a DOM event's clientX/Y into terminal cell coordinates
     *  (1-based, matching xterm's mouse-report encoding). */
    const cellFromEvent = (clientX: number, clientY: number) => {
      const screen = containerRef.current?.querySelector<HTMLElement>(
        ".xterm-screen"
      );
      const cols = term.cols || 80;
      const rows = term.rows || 24;
      let left = clientX;
      let top = clientY;
      let cw = 14;
      let ch = 24;
      if (screen) {
        const r = screen.getBoundingClientRect();
        if (r.width > 0 && r.height > 0) {
          left = clientX - r.left;
          top = clientY - r.top;
          cw = r.width / cols;
          ch = r.height / rows;
        }
      }
      const col = Math.max(1, Math.min(cols, 1 + Math.floor(left / cw)));
      const row = Math.max(1, Math.min(rows, 1 + Math.floor(top / ch)));
      return { col, row };
    };

    // The TUIs scroll their own alternate screen via mouse wheel *reports*.
    // With the tracking modes stripped, xterm would otherwise fall back to
    // emitting ArrowUp/ArrowDown for every wheel event in the alternate buffer.
    // OpenCode interprets those arrows as prompt-history navigation regardless
    // of where the pointer is, in addition to receiving our positioned SGR
    // report below. Disable that xterm fallback for mouse-driven TUIs; codex and
    // bash retain xterm's native scrollback handling.
    term.attachCustomWheelEventHandler(() => {
      const runtime = useStore.getState().agents.get(agentId)?.runtime;
      return !isMouseTuiRuntime(runtime);
    });

    // Forward the wheel to the PTY as an SGR report (the CLI kept SGR encoding
    // 1006 enabled even though the tracking modes were suppressed).
    const onWheel = (ev: WheelEvent) => {
      if (ev.deltaY === 0) return;
      const runtime = useStore.getState().agents.get(agentId)?.runtime;
      if (!canForwardSgrMouse(runtime, modeFilter.sgr)) return;
      ev.preventDefault();
      const { col, row } = cellFromEvent(ev.clientX, ev.clientY);
      invoke("agent_write", {
        id: agentId,
        data: sgrWheelReport(ev.deltaY, col, row),
        raw: true,
      }).catch(() => {});
    };
    // Capture before xterm's nested viewport consumes the bubbling wheel event.
    // A listener on the outer container in the default bubbling phase is not
    // reliable here: xterm's scrollable element can stop propagation first.
    containerRef.current.addEventListener("wheel", onWheel, {
      passive: false,
      capture: true,
    });

    // Preserve the TUIs' click interactions, which tracking used to deliver as
    // mouse reports. Forward a plain left-click as press+release on mouseup —
    // but only if the press never turned into a drag: a drag is a native xterm
    // selection and must not also reach the PTY.
    let mouseDownPos: { x: number; y: number } | null = null;
    const onMouseDown = (ev: MouseEvent) => {
      if (ev.button !== 0) return;
      if (ev.shiftKey || ev.ctrlKey || ev.altKey || ev.metaKey) return;
      const runtime = useStore.getState().agents.get(agentId)?.runtime;
      if (!isMouseTuiRuntime(runtime)) return;
      mouseDownPos = { x: ev.clientX, y: ev.clientY };
    };
    const onMouseMove = (ev: MouseEvent) => {
      if (!mouseDownPos) return;
      const dx = ev.clientX - mouseDownPos.x;
      const dy = ev.clientY - mouseDownPos.y;
      if (dx * dx + dy * dy > 25) mouseDownPos = null; // became a drag → select
    };
    const onMouseUp = (ev: MouseEvent) => {
      if (!mouseDownPos) return;
      mouseDownPos = null;
      if (ev.button !== 0) return;
      const runtime = useStore.getState().agents.get(agentId)?.runtime;
      if (!canForwardSgrMouse(runtime, modeFilter.sgr)) return;
      const { col, row } = cellFromEvent(ev.clientX, ev.clientY);
      // Press + release so the TUI sees a complete click. Deliberately does NOT
      // stamp activity: a click is passive navigation, not the agent working
      // (and with ACTIVE_WINDOW_MS = 0 the stamp would be inert anyway).
      invoke("agent_write", {
        id: agentId,
        data: `\x1b[<0;${col};${row}M\x1b[<0;${col};${row}m`,
        raw: true,
      }).catch(() => {});
    };
    containerRef.current.addEventListener("mousedown", onMouseDown);
    window.addEventListener("mousemove", onMouseMove);
    window.addEventListener("mouseup", onMouseUp);

    // Canvas cards are CSS-scaled. xterm maps mouse with visual offset / unscaled
    // cell size, so selection lands on the wrong row. Rewrite client coords into
    // layout space before xterm sees the event.
    let injectingMouse = false;
    const remapScaledMouse = (e: MouseEvent) => {
      if (!opaqueBg || injectingMouse) return;
      const screen = containerRef.current?.querySelector<HTMLElement>(".xterm-screen");
      if (!screen || screen.offsetWidth < 1) return;
      const visual = screen.getBoundingClientRect();
      const scale = visual.width / screen.offsetWidth;
      if (!Number.isFinite(scale) || Math.abs(scale - 1) < 0.02) return;
      e.stopImmediatePropagation();
      e.preventDefault();
      injectingMouse = true;
      screen.dispatchEvent(
        new MouseEvent(e.type, {
          bubbles: true,
          cancelable: true,
          view: window,
          detail: e.detail,
          button: e.button,
          buttons: e.buttons,
          ctrlKey: e.ctrlKey,
          shiftKey: e.shiftKey,
          altKey: e.altKey,
          metaKey: e.metaKey,
          clientX: visual.left + (e.clientX - visual.left) / scale,
          clientY: visual.top + (e.clientY - visual.top) / scale,
        })
      );
      injectingMouse = false;
    };
    if (opaqueBg) {
      containerRef.current.addEventListener("mousedown", remapScaledMouse, true);
      containerRef.current.addEventListener("mousemove", remapScaledMouse, true);
      containerRef.current.addEventListener("mouseup", remapScaledMouse, true);
    }

    // Resize handler
    let fitTimer: number | null = null;
    const handleResize = () => {
      const screen = containerRef.current?.querySelector<HTMLElement>(".xterm-screen");
      if (screen && screen.offsetWidth > 0) {
        const scale = screen.getBoundingClientRect().width / screen.offsetWidth;
        if (Number.isFinite(scale) && Math.abs(scale - 1) > 0.02) return;
      }
      if (fitTimer != null) window.clearTimeout(fitTimer);
      fitTimer = window.setTimeout(() => {
        fitTimer = null;
        fitAndRefresh();
      }, 50);
    };

    const observer = new ResizeObserver(handleResize);
    observer.observe(containerRef.current);

    return () => {
      if (redrawPulseTimer) clearTimeout(redrawPulseTimer);
      if (redrawRestoreTimer) clearTimeout(redrawRestoreTimer);
      if (modePersistTimer) clearTimeout(modePersistTimer);
      persistClaudeMode();
      if (fitTimer != null) window.clearTimeout(fitTimer);
      // Do not strand the final packet behind a cancelled animation frame.
      if (flushRaf !== null) cancelAnimationFrame(flushRaf);
      flushPending();
      disposed = true;
      cancelAnimationFrame(raf1);
      cancelAnimationFrame(raf2);
      observer.disconnect();
      containerRef.current?.removeEventListener("wheel", onWheel, true);
      containerRef.current?.removeEventListener("mousedown", onMouseDown);
      containerRef.current?.removeEventListener("mousedown", remapScaledMouse, true);
      containerRef.current?.removeEventListener("mousemove", remapScaledMouse, true);
      containerRef.current?.removeEventListener("mouseup", remapScaledMouse, true);
      window.removeEventListener("mousemove", onMouseMove);
      window.removeEventListener("mouseup", onMouseUp);
      searchResultsSub.dispose();
      searchAddonRef.current = null;
      term.dispose();
      // Route output back to the buffer so a reopened tab catches up.
      const ch = channelRef.current;
      if (ch) {
        ch.onmessage = (data) =>
          useStore.getState().appendAgentOutput(agentId, data);
      }
      channelRef.current = null;
      termRef.current = null;
      fitAddonRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agentId, channel]);

  // xterm paints to a canvas and therefore cannot follow CSS variables by
  // itself. Refresh its palette in place so live PTY sessions survive a theme
  // switch (or Theme Lab live overrides via `capilot:theme-vars`) without
  // reconnecting or losing scrollback.
  useEffect(() => {
    const apply = () => {
      const term = termRef.current;
      if (!term) return;
      term.options.theme = readTerminalTheme(opaqueBg);
      if (term.rows > 0) term.refresh(0, term.rows - 1);
      searchAddonRef.current?.clearDecorations();
      setSearchResults(null);
    };
    apply();
    window.addEventListener("capilot:theme-vars", apply);
    return () => window.removeEventListener("capilot:theme-vars", apply);
  }, [themeId, opaqueBg]);

  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    const size = fontSizePx ?? TERMINAL_FONT_SIZES[fontScale] ?? 13;
    if (term.options.fontSize === size) return;
    term.options.fontSize = size;
    requestAnimationFrame(() => {
      try {
        fitAddonRef.current?.fit();
      } catch {
        /* not laid out yet */
      }
    });
  }, [fontScale, fontSizePx]);

  // Tauri drag-drop event — more reliable in the webview than DOM drop (the
  // Composer uses the same fallback). Scoped to the terminal via position so a
  // drop anywhere else in the window doesn't paste into this PTY.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    // StrictMode double-mount guard: onDragDropEvent resolves asynchronously, so
    // cleanup can run before `.then()` assigns unlisten — the late listener must
    // drop itself instead of leaking into the second mount.
    let cancelled = false;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        const p = event.payload;
        if (p.type === "enter") {
          dropHandledRef.current = false; // new drag sequence
          setDragHover(isPointInTerminal(p.position));
        } else if (p.type === "over") {
          setDragHover(isPointInTerminal(p.position));
        } else if (p.type === "leave") {
          dragDepthRef.current = 0;
          setDragHover(false);
        } else if (p.type === "drop") {
          const overTerminal =
            dragDepthRef.current > 0 || isPointInTerminal(p.position);
          if (overTerminal && !dropHandledRef.current) {
            insertPathToPty(p.paths);
            dropHandledRef.current = true;
          }
          dragDepthRef.current = 0;
          setDragHover(false);
        }
      })
      .then((un) => {
        if (cancelled) {
          un();
        } else {
          unlisten = un;
        }
      });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [insertPathToPty, isPointInTerminal]);

  // In-app pointer path-drop from the file tree (WebView2-safe).
  useEffect(() => {
    if (!active) return;
    const onPathDrop = (ev: Event) => {
      const detail = (ev as CustomEvent).detail as
        | { paths?: string[]; kind?: string; agentId?: string | null }
        | undefined;
      if (!detail || detail.kind !== "terminal") return;
      // Prefer the agent id on the event; fall back to this panel when the
      // drop target is this terminal (data-todo-drop-agent matches).
      if (detail.agentId && detail.agentId !== agentId) return;
      if (!detail.agentId) {
        // No agent id — only accept if the point is over this terminal.
        // (resolvePathDropTarget should always set agentId when possible.)
      }
      if (Array.isArray(detail.paths) && detail.paths.length) {
        insertPathToPty(detail.paths);
      }
    };
    window.addEventListener("capilot:path-drop", onPathDrop as EventListener);
    return () =>
      window.removeEventListener("capilot:path-drop", onPathDrop as EventListener);
  }, [active, agentId, insertPathToPty]);

  // A resident Claude / OpenCode panel can sit at `visibility: hidden` while
  // another tab is active. WebView2 / WebKit may drop the canvas backing store
  // in that state; fit + refresh when the tab returns so the last TUI frame
  // (and any packets that arrived while hidden) actually paint.
  useEffect(() => {
    if (!active) return;
    const term = termRef.current;
    const fitAddon = fitAddonRef.current;
    if (!term || !fitAddon) return;
    const raf = requestAnimationFrame(() => {
      try {
        fitAddon.fit();
      } catch {
        // Container has no dimensions yet — ResizeObserver handles the retry.
      }
      if (term.rows > 0) term.refresh(0, term.rows - 1);
    });
    return () => cancelAnimationFrame(raf);
  }, [active]);

  // F1 focus toggle (Composer → terminal). Only the active tab's terminal
  // responds, and only to requests the shared counter hasn't consumed yet — so
  // neither a freshly-mounted panel nor a reactivated resident TUI panel
  // steals focus for a stale request.
  useEffect(() => {
    if (!active || !focusRequest) return;
    if (focusRequest.target !== "terminal") return;
    if (focusRequest.seq <= lastFocusHandledSeq) return;
    lastFocusHandledSeq = focusRequest.seq;
    termRef.current?.focus();
  }, [focusRequest, active]);

  // Ctrl+F routed from the window (when the terminal itself does not have
  // focus). Same seq discipline as the F1 handler above.
  useEffect(() => {
    if (!active || !searchRequest) return;
    if (searchRequest.target !== "terminal") return;
    if (searchRequest.seq <= lastSearchHandledSeq) return;
    lastSearchHandledSeq = searchRequest.seq;
    setSearchOpen(true);
  }, [searchRequest, active]);

  return (
    <>
    <div
      ref={containerRef}
      data-todo-drop-agent={agentId}
      data-path-drop="terminal"
      className={
        [
          dragHover ? "ug-xterm-drophint" : undefined,
          runtime === "opencode" ? "xterm-runtime-opencode" : undefined,
        ].filter(Boolean).join(" ") || undefined
      }
      style={{
        flex: 1,
        // min-height: 0 + overflow hidden give this flex item a definite height
        // that content can't expand. Without it, xterm's screen height feeds back
        // through the ResizeObserver: each fit() adds a row, the container grows a
        // cell height, RO fires again — unbounded (measured rows climbing 634→2734+,
        // renderer compositing a ~50k px page). WebKitGTK software compositor
        // repaints all of it every frame = ~1 core.
        minHeight: 0,
        overflow: "hidden",
        padding: "10px 14px",
        background:
          "color-mix(in srgb, var(--term-bg) calc(var(--term-veil, 0) * 100%), transparent)",
        position: "relative",
      }}
      onDragEnter={(e) => {
        e.preventDefault();
        dragDepthRef.current += 1;
        // A new drag sequence is starting — clear any stale dedupe flag left over
        // from the previous drop so the next drop inserts exactly once.
        dropHandledRef.current = false;
        setDragHover(true);
      }}
      onDragOver={(e) => {
        e.preventDefault(); // allow the drop
        // Todo-tag drop: set matching dropEffect only when we know it's a tag
        // (in-memory session). Don't force dropEffect for file/path drags —
        // WebView2 can stick on 🚫 if effectAllowed is still negotiating.
        if (isTodoDrag(e.dataTransfer)) {
          try {
            if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
          } catch {
            // ignore
          }
        }
      }}
      onDragLeave={(e) => {
        e.preventDefault();
        dragDepthRef.current = Math.max(0, dragDepthRef.current - 1);
        if (dragDepthRef.current === 0) setDragHover(false);
      }}
      onDrop={(e) => {
        e.preventDefault();
        // If the Tauri drag-drop event already inserted the path for this same
        // physical drop, consume the DOM drop without double-inserting.
        if (dropHandledRef.current) {
          dropHandledRef.current = false;
          dragDepthRef.current = 0;
          setDragHover(false);
          return;
        }
        // A todo tag dropped onto the terminal assigns the task to this session
        // and sends its text as a prompt.
        if (isTodoDrag(e.dataTransfer)) {
          const tagId = getTodoDragId(e.dataTransfer);
          if (tagId) {
            void assignTodoAndSend(tagId, agentId);
          }
          dropHandledRef.current = true;
          dragDepthRef.current = 0;
          setDragHover(false);
          return;
        }
        // Extract absolute paths straight from the DOM dataTransfer. On Tauri
        // v2 + WebKitGTK the legacy `File.path` is gone and the Tauri drag-drop
        // event can be unreliable, so this is the primary source (the file
        // manager's `text/uri-list`, plus `text/plain` for app-internal drags).
        const paths = pathsFromDataTransfer(e.dataTransfer);
        if (paths.length) {
          insertPathToPty(paths);
          dropHandledRef.current = true;
          dragDepthRef.current = 0;
          setDragHover(false);
        }
        // No path at all → leave dragDepthRef/dropHandledRef untouched so the
        // Tauri drag-drop event (which fires next) can still detect the terminal.
      }}
      onContextMenu={(e) => {
        const target = e.target as HTMLElement | null;
        if (target?.closest("button, .ctx-menu")) return;
        e.preventDefault();
        e.stopPropagation();
        const fromSearch = !!target?.closest(".term-search-bar");
        setPasteMenu({
          x: Math.min(e.clientX, window.innerWidth - 160),
          y: Math.min(e.clientY, window.innerHeight - 50),
          into: fromSearch ? "search" : "pty",
        });
      }}
    >
      {searchOpen && (
        <div className="term-search-bar">
          <input
            ref={searchInputRef}
            className="term-search-input"
            type="text"
            placeholder={t("terminal.searchPlaceholder")}
            value={searchQuery}
            onChange={(e) => {
              const q = e.target.value;
              setSearchQuery(q);
              // runFind handles the empty query by clearing decorations.
              runFind(q, "next");
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                e.stopPropagation();
                runFind(searchQuery, e.shiftKey ? "prev" : "next");
              } else if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                closeSearch();
              }
            }}
          />
          <span className="term-search-count">
            {searchQuery && searchResults && searchResults.resultCount > 0
              ? `${Math.max(searchResults.resultIndex, 0) + 1}/${searchResults.resultCount}`
              : searchQuery
                ? t("terminal.noResults")
                : ""}
          </span>
          <button
            className="term-search-btn"
            title={t("terminal.prevMatch")}
            onClick={() => runFind(searchQuery, "prev")}
            disabled={!searchQuery}
          >
            <Icon name="arrow-up" size={12} />
          </button>
          <button
            className="term-search-btn"
            title={t("terminal.nextMatch")}
            onClick={() => runFind(searchQuery, "next")}
            disabled={!searchQuery}
          >
            <Icon name="arrow-down" size={12} />
          </button>
          <button
            className="term-search-close"
            title={t("terminal.closeSearch")}
            onClick={closeSearch}
          >
            ×
          </button>
        </div>
      )}
    </div>
    {pasteMenu &&
      createPortal(
        <div
          className="ctx-menu"
          style={{ position: "fixed", left: pasteMenu.x, top: pasteMenu.y, zIndex: 1000 }}
          onClick={(e) => e.stopPropagation()}
          onContextMenu={(e) => e.stopPropagation()}
        >
          <div className="ctx-item" onClick={() => void pasteClipboard()}>
            <Icon name="clipboard" size={13} /> {t("common.paste")}
          </div>
        </div>,
        document.body
      )}
    </>
  );
}
