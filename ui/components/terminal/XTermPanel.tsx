import { useEffect, useRef, useState, useCallback } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { SearchAddon } from "@xterm/addon-search";
import { useStore, AgentInfo } from "../../state/store";
import { pathsFromDataTransfer } from "../../state/dropPaths";
import { Icon } from "../Icon";
import "@xterm/xterm/css/xterm.css";

interface XTermPanelProps {
  agentId: string;
  /** True when this panel belongs to the active tab. Only an active terminal may
   *  steal focus on the F1 input↔terminal toggle; hidden resident OpenCode
   *  panels must not. Defaults to true for standalone use. */
  active?: boolean;
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

/** Resolve a `:root` CSS custom property to its concrete value. xterm.js theme
 *  and decoration options need literal color strings, so instead of duplicating
 *  the palette we read it from the CSS (single source of truth). */
const cssVar = (name: string, fallback: string): string => {
  const root = typeof document !== "undefined" ? document.documentElement : null;
  return (root ? getComputedStyle(root).getPropertyValue(name).trim() : "") || fallback;
};

/** Terminal search match highlight colors (xterm decorations need `#RRGGBB`). */
const SEARCH_MATCH_BG = cssVar("--search-match-bg", "#2E2A4A");
const SEARCH_ACTIVE_BG = cssVar("--brand", "#8B5CF6");

/** xterm panel bound to an agent's PTY channel.
 *
 * Race handling: the Composer starts buffering channel output into
 * `store.agentOutputs` from the instant the PTY spawns. This component drains
 * that buffer on mount, then redirects the channel straight to the terminal.
 * On unmount it routes output back to the buffer so nothing is lost if the tab
 * is reopened. When the channel object changes (e.g. runtime switch), the
 * effect re-runs and attaches the new channel.
 */
export function XTermPanel({ agentId, active = true }: XTermPanelProps) {
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
  const focusRequest = useStore((s) => s.focusRequest);
  const searchRequest = useStore((s) => s.searchRequest);

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
        matchBackground: SEARCH_MATCH_BG,
        matchOverviewRuler: SEARCH_MATCH_BG,
        activeMatchBackground: SEARCH_ACTIVE_BG,
        activeMatchColorOverviewRuler: SEARCH_ACTIVE_BG,
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
      fontSize: TERMINAL_FONT_SIZES[fontScale] ?? 13,
      fontFamily: "'JetBrainsMono', ui-monospace, monospace",
      theme: {
        background: cssVar("--term-bg", "#05070D"),
        foreground: cssVar("--ink", "#E8ECF1"),
        cursor: cssVar("--brand", "#8B5CF6"),
        selectionBackground: cssVar("--brand-selection", "rgba(139, 92, 246, 0.3)"),
        black: cssVar("--bg3", "#161B22"),
        red: cssVar("--danger", "#F87171"),
        green: cssVar("--success", "#4ADE80"),
        yellow: cssVar("--warn", "#FACC15"),
        blue: cssVar("--primary", "#A78BFA"),
        magenta: cssVar("--lane-3", "#E84BA5"),
        cyan: cssVar("--lane-1", "#47E8D4"),
        white: cssVar("--ink", "#E8ECF1"),
        brightBlack: cssVar("--rule2", "#30363D"),
        brightRed: cssVar("--danger", "#F87171"),
        brightGreen: cssVar("--success", "#4ADE80"),
        brightYellow: cssVar("--warn", "#FACC15"),
        brightBlue: cssVar("--primary", "#A78BFA"),
        brightMagenta: cssVar("--lane-3", "#E84BA5"),
        brightCyan: cssVar("--lane-1", "#47E8D4"),
        brightWhite: cssVar("--ink", "#E8ECF1"),
      },
      allowProposedApi: true,
    });

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(containerRef.current);

    // Keyboard copy: xterm.js maps Ctrl+C to a PTY SIGINT (^C), so the browser's
    // default Ctrl+Shift+C — which terminals use for copy — is never produced,
    // and WebKitGTK has no native menu fallback. Intercept the chord and copy the
    // selection (matches the sidebar's clipboard helper). Plain Ctrl+C (no shift)
    // is left alone so the PTY still receives SIGINT. Returning false stops the
    // event reaching xterm's own key handling.
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
      if (
        ev.type === "keydown" &&
        ev.ctrlKey &&
        ev.shiftKey &&
        !ev.altKey &&
        !ev.metaKey &&
        ev.key.toLowerCase() === "c"
      ) {
        const text = term.hasSelection() ? term.getSelection() : "";
        if (text) copyText(text);
        return false; // swallow — don't send ^C
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
    let redrawPulseRequested = false;
    let pendingClaudeMode: string | null = null;
    let modePersistTimer: ReturnType<typeof setTimeout> | null = null;

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
        term.write(merged, syncClaudeModeFromScreen);
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

    /** OpenCode's alternate-screen TUI may stay idle after this xterm component
     *  is recreated. The PTY is still alive, but xterm has no screen snapshot to
     *  paint and an unchanged resize is normally suppressed. A one-column resize
     *  pulse makes the native TUI redraw without sending it an input command. */
    const requestOpenCodeRedraw = () => {
      if (redrawPulseRequested) return;
      if (useStore.getState().agents.get(agentId)?.runtime !== "opencode") return;
      redrawPulseRequested = true;
      redrawPulseTimer = setTimeout(() => {
        redrawPulseTimer = null;
        if (disposed) return;
        const rows = term.rows || 24;
        const cols = term.cols || 80;
        const pulseCols = cols > 2 ? cols - 1 : cols + 1;
        invoke("agent_resize", { id: agentId, rows, cols: pulseCols }).catch(() => {});
        redrawRestoreTimer = setTimeout(() => {
          redrawRestoreTimer = null;
          if (disposed) return;
          invoke("agent_resize", { id: agentId, rows, cols }).catch(() => {});
          lastResize = { rows, cols };
        }, 32);
      }, 80);
    };

    /** Fit the terminal to its container and force a repaint. A terminal opened
     *  during a tab switch can land in a 0×0 / not-yet-laid-out container, which
     *  paints a blank canvas until something resizes it — fit() alone won't redraw
     *  when the size didn't change, so refresh() forces the paint. */
    const fitAndRefresh = () => {
      if (disposed) return;
      try {
        fitAddon.fit();
      } catch {
        // Container has no dimensions yet — the deferred retry handles it.
      }
      if (term.rows > 0 && term.cols > 0) {
        sendResize();
        term.refresh(0, term.rows - 1);
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
      requestOpenCodeRedraw();
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
        invoke<AgentInfo>("agent_resume", { id: agentId, onData: resumeChannel })
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

    // Forward user input to the PTY (raw keystroke passthrough).
    term.onData((data) => {
      invoke("agent_write", { id: agentId, data, raw: true }).catch(() => {});
    });

    // Resize handler
    const handleResize = () => {
      fitAndRefresh();
    };

    const observer = new ResizeObserver(handleResize);
    observer.observe(containerRef.current);

    return () => {
      if (redrawPulseTimer) clearTimeout(redrawPulseTimer);
      if (redrawRestoreTimer) clearTimeout(redrawRestoreTimer);
      if (modePersistTimer) clearTimeout(modePersistTimer);
      persistClaudeMode();
      // Do not strand the final packet behind a cancelled animation frame.
      if (flushRaf !== null) cancelAnimationFrame(flushRaf);
      flushPending();
      disposed = true;
      cancelAnimationFrame(raf1);
      cancelAnimationFrame(raf2);
      observer.disconnect();
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

  // F1 focus toggle (Composer → terminal). Only the active tab's terminal
  // responds, and only to requests the shared counter hasn't consumed yet — so
  // neither a freshly-mounted panel nor a reactivated resident OpenCode panel
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
    <div
      ref={containerRef}
      className={dragHover ? "ug-xterm-drophint" : undefined}
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
        background: "var(--term-bg)",
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
    >
      {searchOpen && (
        <div className="term-search-bar">
          <input
            ref={searchInputRef}
            className="term-search-input"
            type="text"
            placeholder="搜索终端…"
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
                ? "无结果"
                : ""}
          </span>
          <button
            className="term-search-btn"
            title="上一个匹配 (Shift+Enter / Shift+F3)"
            onClick={() => runFind(searchQuery, "prev")}
            disabled={!searchQuery}
          >
            <Icon name="arrow-up" size={12} />
          </button>
          <button
            className="term-search-btn"
            title="下一个匹配 (Enter / F3)"
            onClick={() => runFind(searchQuery, "next")}
            disabled={!searchQuery}
          >
            <Icon name="arrow-down" size={12} />
          </button>
          <button
            className="term-search-close"
            title="关闭搜索 (Esc)"
            onClick={closeSearch}
          >
            ×
          </button>
        </div>
      )}
    </div>
  );
}
