import { useEffect, useRef, useState, useCallback } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { useStore, AgentInfo } from "../../state/store";
import "@xterm/xterm/css/xterm.css";

interface XTermPanelProps {
  agentId: string;
}

/** Shell-escape a path so spaces / quotes survive (single-quote wrap, `'` → `'\''`). */
function shellEscape(path: string): string {
  return `'${path.replace(/'/g, `'\\''`)}'`;
}

/**
 * agentIds with an agent_resume invoke already in flight. React StrictMode mounts
 * effects twice (mount→cleanup→mount); without this guard each restored terminal
 * spawns TWO claude processes (~300MB waste each). The second mount skips the
 * invoke and picks up the channel once the first mount's resolve stores it.
 */
const resumeInFlight = new Set<string>();

/** xterm panel bound to an agent's PTY channel.
 *
 * Race handling: the Composer starts buffering channel output into
 * `store.agentOutputs` from the instant the PTY spawns. This component drains
 * that buffer on mount, then redirects the channel straight to the terminal.
 * On unmount it routes output back to the buffer so nothing is lost if the tab
 * is reopened. When the channel object changes (e.g. runtime switch), the
 * effect re-runs and attaches the new channel.
 */
export function XTermPanel({ agentId }: XTermPanelProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const channelRef = useRef<Channel<number[]> | null>(null);
  const agentChannels = useStore((s) => s.agentChannels);
  const channel = agentChannels.get(agentId);

  // DevPlan §4.6 — worker lock: input typed while the agent is a worker is
  // intercepted instead of silently forwarded; the user picks 仍然发送/解锁.
  const [lockWarning, setLockWarning] = useState(false);
  const lockedInputRef = useRef("");
  const unlockTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

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
    const r = el.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    const x = pos.x / dpr;
    const y = pos.y / dpr;
    // A few px of tolerance so drops on the container's border still count.
    return (
      x >= r.left - 4 && x <= r.right + 4 && y >= r.top - 4 && y <= r.bottom + 4
    );
  }, []);

  /** Insert shell-escaped path(s) into the PTY (raw keystroke passthrough).
   *  Worker-locked agents route through the same lock banner instead of silently
   *  accepting the input. */
  const insertPathToPty = useCallback(
    (paths: string[]) => {
      if (!paths.length) return;
      const escaped = paths.map(shellEscape).join(" ");
      // Leading space so the path doesn't glue to preceding text (typing a path
      // in a shell); raw:true sends the keystrokes verbatim (no \r appended).
      const payload = ` ${escaped}`;
      const role = useStore.getState().agents.get(agentId)?.role;
      const unlocked = useStore.getState().workerUnlockId === agentId;
      if (role === "worker" && !unlocked) {
        lockedInputRef.current += payload;
        setLockWarning(true);
        return;
      }
      invoke("agent_write", { id: agentId, data: payload, raw: true }).catch(
        () => {}
      );
    },
    [agentId]
  );

  useEffect(() => {
    return () => {
      if (unlockTimerRef.current) clearTimeout(unlockTimerRef.current);
    };
  }, []);

  useEffect(() => {
    if (!containerRef.current) return;

    const term = new Terminal({
      // Static cursor: xterm implements cursorBlink as an injected infinite CSS
      // keyframe animation, and WebKitGTK's software compositor repaints the
      // whole window at 60 fps for any CSS animation — measured ~1 full core.
      // Claude's TUI draws its own cursor inside the PTY, so a static xterm
      // cursor is barely visible. cursorBlink:false costs nothing perceptible.
      cursorBlink: false,
      fontSize: 13,
      fontFamily: "'JetBrainsMono', ui-monospace, monospace",
      theme: {
        background: "#05070D",
        foreground: "#E8ECF1",
        cursor: "#8B5CF6",
        selectionBackground: "rgba(139, 92, 246, 0.3)",
        black: "#161B22",
        red: "#F87171",
        green: "#4ADE80",
        yellow: "#FACC15",
        blue: "#A78BFA",
        magenta: "#C4B5FD",
        cyan: "#8B5CF6",
        white: "#E8ECF1",
        brightBlack: "#30363D",
        brightRed: "#F87171",
        brightGreen: "#4ADE80",
        brightYellow: "#FACC15",
        brightBlue: "#A78BFA",
        brightMagenta: "#C4B5FD",
        brightCyan: "#8B5CF6",
        brightWhite: "#E8ECF1",
      },
      allowProposedApi: true,
    });

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(containerRef.current);
    fitAddon.fit();

    termRef.current = term;
    fitAddonRef.current = fitAddon;

    let disposed = false;

    const writeToTerm = (data: number[]) => {
      if (disposed) return;
      try {
        term.write(new Uint8Array(data));
      } catch {
        // terminal disposed
      }
    };

    const sendResize = () => {
      const rows = term.rows || 24;
      const cols = term.cols || 80;
      invoke("agent_resize", { id: agentId, rows, cols }).catch(() => {});
    };

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
    };

    if (channel) {
      attachChannel(channel);
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

    // Forward user input to the PTY (raw keystroke passthrough). Worker agents
    // are locked (DevPlan §4.6): swallow the keystrokes into a buffer and show a
    // warning instead of silently dropping them.
    term.onData((data) => {
      const role = useStore.getState().agents.get(agentId)?.role;
      const unlocked = useStore.getState().workerUnlockId === agentId;
      if (role === "worker" && !unlocked) {
        lockedInputRef.current += data;
        setLockWarning(true);
        return;
      }
      invoke("agent_write", { id: agentId, data, raw: true }).catch(() => {});
    });

    // Resize handler
    const handleResize = () => {
      if (fitAddonRef.current) {
        fitAddonRef.current.fit();
        sendResize();
      }
    };

    const observer = new ResizeObserver(handleResize);
    observer.observe(containerRef.current);

    return () => {
      disposed = true;
      observer.disconnect();
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

  const handleStillSendTerminal = () => {
    const text = lockedInputRef.current;
    if (text) {
      // Line submission (raw:false appends \r), matching the composer send.
      invoke("agent_write", { id: agentId, data: text, raw: false }).catch(() => {});
    }
    lockedInputRef.current = "";
    setLockWarning(false);
  };

  const handleUnlockTerminal = () => {
    setLockWarning(false);
    lockedInputRef.current = "";
    useStore.getState().setWorkerUnlock(agentId);
    // "解锁 allows input for a bit" — auto-relock after 8s.
    if (unlockTimerRef.current) clearTimeout(unlockTimerRef.current);
    unlockTimerRef.current = setTimeout(() => {
      const s = useStore.getState();
      if (s.workerUnlockId === agentId) s.setWorkerUnlock(null);
    }, 8000);
  };

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
        background: "#05070D",
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
        // Some webviews still expose `.path` on File (Tauri v1 heritage / v2
        // dragDropEnabled). If present, insert directly; otherwise defer to the
        // Tauri drag-drop event, which carries the real absolute paths.
        const f = e.dataTransfer.files?.[0] as
          | (File & { path?: string })
          | undefined;
        if (f?.path) {
          insertPathToPty([f.path]);
          dropHandledRef.current = true;
          dragDepthRef.current = 0;
          setDragHover(false);
        }
      }}
    >
      {lockWarning && (
        <div className="term-lock-banner">
          <span>🔒 此 agent 是 worker，输入会被编排结果覆盖</span>
          <button onClick={handleStillSendTerminal}>仍然发送</button>
          <button onClick={handleUnlockTerminal}>解锁</button>
          <button
            onClick={() => {
              lockedInputRef.current = "";
              setLockWarning(false);
            }}
          >
            知道了
          </button>
        </div>
      )}
    </div>
  );
}
