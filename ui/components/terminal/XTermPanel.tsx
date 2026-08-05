import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { useStore } from "../../state/store";
import "@xterm/xterm/css/xterm.css";

interface XTermPanelProps {
  agentId: string;
}

export function XTermPanel({ agentId }: XTermPanelProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const agentChannels = useStore((s) => s.agentChannels);

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

    // Connect to the PTY channel for this agent
    const channel = agentChannels.get(agentId);
    if (channel) {
      channel.onmessage = (data: number[]) => {
        term.write(new Uint8Array(data));
      };
    }

    // Forward user input to the PTY
    term.onData((data) => {
      // TODO: invoke agent_write command
      console.debug("term input:", data);
    });

    // Resize handler
    const handleResize = () => {
      if (fitAddonRef.current) {
        fitAddonRef.current.fit();
      }
    };

    const observer = new ResizeObserver(handleResize);
    observer.observe(containerRef.current);

    return () => {
      observer.disconnect();
      term.dispose();
    };
  }, [agentId]);

  return (
    <div
      ref={containerRef}
      style={{
        flex: 1,
        padding: "10px 14px",
        background: "#05070D",
      }}
    />
  );
}
