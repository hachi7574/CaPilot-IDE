import { useRef, useCallback, KeyboardEvent } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { useStore } from "../../state/store";

const PERMISSION_MODES = ["ask", "auto", "yolo"] as const;
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

  const activeTab = tabs.find((t) => t.id === activeTabId);
  const targetAgentId =
    composerTarget === "agent" ? activeTab?.agentId : undefined;

  const handleSend = useCallback(async () => {
    const text = textareaRef.current?.value.trim();
    if (!text) return;

    pushDraft(text);

    let agentId = targetAgentId;
    if (!agentId) {
      try {
        const channel = new Channel<number[]>();
        const cwd = "/home/hachi";
        const info = await invoke("agent_spawn", {
          runtime: "claude",
          cwd,
          role: "master",
          onData: channel,
        });
        const agentInfo = info as import("../../state/store").AgentInfo;
        agentId = agentInfo.id;
        addAgent(agentInfo, channel);
        addTab({
          id: agentId,
          type: "agent",
          agentId,
          title: `claude@master`,
        });
        setTimeout(() => {
          invoke("agent_write", { id: agentId, data: text });
        }, 500);
      } catch (err) {
        console.error("Failed to spawn agent:", err);
        return;
      }
    } else {
      try {
        await invoke("agent_write", { id: agentId, data: text });
      } catch (err) {
        console.error("Failed to write to agent:", err);
      }
    }

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
          const idx = PERMISSION_MODES.indexOf(permissionMode);
          setPermissionMode(PERMISSION_MODES[(idx + 1) % 3]);
        } else {
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
    },
    [handleSend, permissionMode, composerTarget, setPermissionMode, setComposerTarget, navigateDraft]
  );

  return (
    <div className={`composer${!composerOpen ? " composer-collapsed" : ""}`}>
      {/* Target line */}
      <div className="composer-target">
        <span>→</span>{" "}
        agent: {composerTarget === "master"
          ? "master"
          : activeTab?.title || "none"}
      </div>

      {/* Input area */}
      <div className="composer-input-wrap">
        <textarea
          ref={textareaRef}
          className="composer-input"
          placeholder="发消息…（/ 命令 · @ 文件 · ! 终端 · 拖入文件）"
          rows={2}
          onKeyDown={handleKeyDown}
          onInput={(e) => {
            const el = e.currentTarget;
            el.style.height = "auto";
            el.style.height = Math.min(el.scrollHeight, 200) + "px";
          }}
        />
      </div>

      {/* Actions */}
      <div className="composer-actions">
        <span className="act-btn">+ 文件/引用</span>
        <span className="act-btn">模型 ↑</span>
        <span
          className="act-btn"
          onClick={() => {
            const s = ["high", "mid", "fast", "auto"] as const;
            const idx = s.indexOf(speed);
            setSpeed(s[(idx + 1) % 4]);
          }}
        >
          速度: {SPEED_LABELS[speed]}
        </span>
        <span className="act-sep" />
        <span className="act-btn accent" title="worker 开关">
          🤖worker 开
        </span>
        <span className="act-sep" />
        <span className="act-mode-group">
          {PERMISSION_MODES.map((m) => (
            <span
              key={m}
              className={`act-mode-btn${permissionMode === m ? " active" : ""}`}
              onClick={() => setPermissionMode(m)}
            >
              {m}
            </span>
          ))}
        </span>
        <button className="collapse-btn" onClick={toggleComposer}>
          {composerOpen ? "▼" : "▲"}
        </button>
      </div>
    </div>
  );
}
