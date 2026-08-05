import { useEffect, useRef, useState } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { useStore, AgentInfo } from "../../state/store";
import "@xterm/xterm/css/xterm.css";

interface XTermPanelProps {
  agentId: string;
}

/**
 * xterm panel bound to an agent's PTY channel.
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

  useEffect(() => {
    return () => {
      if (unlockTimerRef.current) clearTimeout(unlockTimerRef.current);
    };
  }, []);

  useEffect(() => {
    if (!containerRef.current) return;

    const term = new Terminal({
      cursorBlink: true,
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
    } else {
      // Restored session with no live PTY → resume it.
      const resumeChannel = new Channel<number[]>();
      resumeChannel.onmessage = (data) =>
        useStore.getState().appendAgentOutput(agentId, data);
      invoke<AgentInfo>("agent_resume", { id: agentId, onData: resumeChannel })
        .then((info) => {
          if (disposed) return;
          useStore.getState().addAgent(info, resumeChannel);
          attachChannel(resumeChannel);
        })
        .catch((err) => {
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
      style={{
        flex: 1,
        padding: "10px 14px",
        background: "#05070D",
        position: "relative",
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
