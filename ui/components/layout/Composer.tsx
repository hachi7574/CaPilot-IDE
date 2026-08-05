import { useRef, useCallback, KeyboardEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useStore } from "../../state/store";
import { spawnAgent, ensureAgentChannel } from "../../state/agentActions";

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
  const workerMode = useStore((s) => s.workerMode);
  const activeTabId = useStore((s) => s.activeTabId);
  const tabs = useStore((s) => s.tabs);
  const agents = useStore((s) => s.agents);
  const masterAgentId = useStore((s) => s.masterAgentId);

  const toggleComposer = useStore((s) => s.toggleComposer);
  const setComposerTarget = useStore((s) => s.setComposerTarget);
  const setPermissionMode = useStore((s) => s.setPermissionMode);
  const setSpeed = useStore((s) => s.setSpeed);
  const toggleWorkerMode = useStore((s) => s.toggleWorkerMode);
  const pushDraft = useStore((s) => s.pushDraft);
  const navigateDraft = useStore((s) => s.navigateDraft);

  const activeTab = tabs.find((t) => t.id === activeTabId);
  const targetAgentId =
    composerTarget === "agent" ? activeTab?.agentId : undefined;

  const handleSend = useCallback(async () => {
    const text = textareaRef.current?.value.trim();
    if (!text) return;

    pushDraft(text);

    let agentId = targetAgentId;
    let justSpawned = false;
    try {
      if (!agentId) {
        if (composerTarget === "master") {
          // Reuse the existing master session instead of spawning a new one.
          if (masterAgentId && agents.has(masterAgentId)) {
            agentId = masterAgentId;
          }
        }
        if (!agentId) {
          const role =
            composerTarget === "master"
              ? "master"
              : workerMode
              ? "worker"
              : "standalone";
          agentId = await spawnAgent(role);
          justSpawned = true;
        }
      }

      // Resumed/restored sessions may not have a channel yet.
      const resumed = await ensureAgentChannel(agentId);
      // Give a freshly-spawned/resumed CLI TUI time to attach its input loop
      // before injecting the message.
      if (justSpawned || resumed) {
        await new Promise((r) => setTimeout(r, 800));
      }
      await invoke("agent_write", { id: agentId, data: text });
    } catch (err) {
      console.error("Failed to send to agent:", err);
      return;
    }

    if (textareaRef.current) {
      textareaRef.current.value = "";
      textareaRef.current.style.height = "auto";
    }
  }, [
    targetAgentId,
    composerTarget,
    workerMode,
    masterAgentId,
    agents,
    pushDraft,
  ]);

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
    [
      handleSend,
      permissionMode,
      composerTarget,
      setPermissionMode,
      setComposerTarget,
      navigateDraft,
    ]
  );

  return (
    <div className={`composer${!composerOpen ? " composer-collapsed" : ""}`}>
      {/* Target line */}
      <div className="composer-target">
        <span>→</span>{" "}
        agent:{" "}
        {composerTarget === "master"
          ? "master"
          : activeTab?.title || "none"}
        {workerMode && composerTarget !== "master" ? " · worker" : ""}
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
        <span
          className={`act-btn accent${workerMode ? " active" : ""}`}
          title="worker 开关：开启后新终端进编排池"
          onClick={toggleWorkerMode}
        >
          🤖worker {workerMode ? "开" : "关"}
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
