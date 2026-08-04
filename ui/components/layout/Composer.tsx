import { useRef, useCallback, KeyboardEvent, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Channel } from "@tauri-apps/api/core";
import { useStore } from "../../state/store";

const PERMISSION_MODES = ["ask", "auto", "yolo"] as const;
const SPEEDS = ["high", "mid", "fast", "auto"] as const;
const SPEED_LABELS: Record<string, string> = {
  high: "high",
  mid: "mid",
  fast: "fast",
  auto: "auto",
};

export function Composer() {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const composerOpen = useStore((s) => s.composerOpen);
  const composerTarget = useStore((s) => s.composerTarget);
  const permissionMode = useStore((s) => s.permissionMode);
  const speed = useStore((s) => s.speed);
  const activeTabId = useStore((s) => s.activeTabId);
  const tabs = useStore((s) => s.tabs);

  const toggleComposer = useStore((s) => s.toggleComposer);
  const setComposerTarget = useStore((s) => s.setComposerTarget);
  const setPermissionMode = useStore((s) => s.setPermissionMode);
  const setSpeed = useStore((s) => s.setSpeed);
  const pushDraft = useStore((s) => s.pushDraft);
  const navigateDraft = useStore((s) => s.navigateDraft);
  const addAgent = useStore((s) => s.addAgent);
  const addTab = useStore((s) => s.addTab);

  // Get the target agent ID (from active tab or master)
  const activeTab = tabs.find((t) => t.id === activeTabId);
  const targetAgentId =
    composerTarget === "agent" ? activeTab?.agentId : undefined;

  const handleSend = useCallback(async () => {
    const text = textareaRef.current?.value.trim();
    if (!text) return;

    // Save to draft history
    pushDraft(text);

    // If no agent exists yet, spawn one
    let agentId = targetAgentId;
    if (!agentId) {
      try {
        // Create a Channel to receive PTY output
        const channel = new Channel<number[]>();

        // Determine CWD (use home dir for now)
        const cwd = "/home/hachi";

        const info = await invoke("agent_spawn", {
          runtime: "claude",
          cwd,
          role: "master",
          onData: channel,
        });

        // Type assertion for the returned info
        const agentInfo = info as import("../../state/store").AgentInfo;
        agentId = agentInfo.id;

        addAgent(agentInfo, channel);
        addTab({
          id: `agent-${agentId}`,
          type: "agent",
          agentId,
          title: `claude@master`,
        });

        // Write the message to the PTY
        setTimeout(() => {
          invoke("agent_write", { id: agentId, data: text });
        }, 500);
      } catch (err) {
        console.error("Failed to spawn agent:", err);
        return;
      }
    } else {
      // Write to existing agent PTY
      try {
        await invoke("agent_write", { id: agentId, data: text });
      } catch (err) {
        console.error("Failed to write to agent:", err);
      }
    }

    // Clear input
    if (textareaRef.current) {
      textareaRef.current.value = "";
      textareaRef.current.style.height = "auto";
    }
  }, [targetAgentId, pushDraft, addAgent, addTab]);

  const handleKeyDown = useCallback(
    (e: KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        handleSend();
      } else if (e.key === "Tab") {
        e.preventDefault();
        if (e.shiftKey) {
          // Cycle permission mode
          const idx = PERMISSION_MODES.indexOf(permissionMode);
          setPermissionMode(PERMISSION_MODES[(idx + 1) % 3]);
        } else {
          // Toggle send target
          setComposerTarget(composerTarget === "agent" ? "master" : "agent");
        }
      } else if (e.key === "ArrowUp" && !e.currentTarget.value) {
        e.preventDefault();
        const draft = navigateDraft(1);
        if (draft !== null) {
          e.currentTarget.value = draft;
        }
      } else if (e.key === "ArrowDown" && !e.currentTarget.value) {
        e.preventDefault();
        const draft = navigateDraft(-1);
        if (draft !== null) {
          e.currentTarget.value = draft;
        }
      }
      // Esc: no-op (do not respond)
    },
    [
      handleSend,
      permissionMode,
      composerTarget,
      setPermissionMode,
      setComposerTarget,
      navigateDraft,
    ]
  );

  const handleInput = () => {
    const el = textareaRef.current;
    if (el) {
      el.style.height = "auto";
      el.style.height = Math.min(el.scrollHeight, 200) + "px";
    }
  };

  // Focus textarea when composer opens
  useEffect(() => {
    if (composerOpen && textareaRef.current) {
      textareaRef.current.focus();
    }
  }, [composerOpen]);

  return (
    <div className={`composer ${!composerOpen ? "composer-collapsed" : ""}`}>
      {/* Function bar */}
      <div className="composer-toolbar">
        <span className="composer-target">
          → {composerTarget === "master" ? "master" : activeTab?.title || "agent"}
        </span>

        <button className="composer-btn" title="Attach file/reference/skill">
          +file
        </button>

        <button className="composer-btn" title="Switch model">
          model↑
        </button>

        <button
          className="composer-btn"
          onClick={() => {
            const idx = SPEEDS.indexOf(speed);
            setSpeed(SPEEDS[(idx + 1) % 4]);
          }}
          title={`Speed: ${speed}`}
        >
          {SPEED_LABELS[speed]}↑
        </button>

        <button className="composer-btn" title="Toggle worker">
          🤖worker
        </button>

        <button
          className={`composer-btn ${permissionMode === "yolo" ? "active" : ""}`}
          onClick={() => {
            const idx = PERMISSION_MODES.indexOf(permissionMode);
            setPermissionMode(PERMISSION_MODES[(idx + 1) % 3]);
          }}
        >
          [{permissionMode}]
        </button>

        <button
          className="composer-btn toggle"
          onClick={toggleComposer}
          style={{ marginLeft: "auto" }}
          title={composerOpen ? "Collapse" : "Expand"}
        >
          {composerOpen ? "▼" : "▲"}
        </button>
      </div>

      {/* Input area */}
      <div className="composer-input-wrap">
        <textarea
          ref={textareaRef}
          className="composer-input"
          placeholder={`→ ${composerTarget}  Type a message… (/cmd · @file · Enter send · Tab toggle)`}
          rows={2}
          onKeyDown={handleKeyDown}
          onInput={handleInput}
        />
        <button
          className="composer-btn send"
          onClick={handleSend}
          title="Send (Enter)"
        >
          ↑ send
        </button>
      </div>
    </div>
  );
}
