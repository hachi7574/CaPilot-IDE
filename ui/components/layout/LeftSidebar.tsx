import { useState, useEffect, useCallback } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { openPath } from "@tauri-apps/plugin-opener";
import { useStore, AgentInfo } from "../../state/store";
import { setAgentRole } from "../../state/orchestration";
import { spawnAgent } from "../../state/agentActions";
import { SettingsModal } from "./SettingsModal";

/** Derive the workspace project name from an agent cwd. */
function projectOf(cwd: string): string {
  const m = cwd.match(/workspaces\/([^/]+)/);
  if (m) return m[1];
  const parts = cwd.split("/").filter(Boolean);
  return parts[parts.length - 1] || cwd;
}

interface CtxState {
  x: number;
  y: number;
  agentId?: string;
  project?: string;
  cwd?: string;
}

export function LeftSidebar() {
  const leftSidebarOpen = useStore((s) => s.leftSidebarOpen);
  const toggleLeftSidebar = useStore((s) => s.toggleLeftSidebar);
  const agents = useStore((s) => s.agents);
  const tabs = useStore((s) => s.tabs);
  const activeTabId = useStore((s) => s.activeTabId);
  const masterAgentId = useStore((s) => s.masterAgentId);
  const setActiveTab = useStore((s) => s.setActiveTab);
  const addTab = useStore((s) => s.addTab);

  const [masterCollapsed, setMasterCollapsed] = useState(false);
  const [focusedProj, setFocusedProj] = useState<string | null>(null);
  const [collapsedProjs, setCollapsedProjs] = useState<Set<string>>(new Set());
  const [ctx, setCtx] = useState<CtxState | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const leftWidth = useStore((s) => s.leftWidth);
  const setLeftWidth = useStore((s) => s.setLeftWidth);

  // Close the context menu on outside click / Escape.
  useEffect(() => {
    if (!ctx) return;
    const close = () => setCtx(null);
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && close();
    window.addEventListener("click", close);
    window.addEventListener("contextmenu", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("contextmenu", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [ctx]);

  const toggleProj = (id: string) => {
    setCollapsedProjs((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const focusProject = (id: string) => {
    setFocusedProj(id === focusedProj ? null : id);
    setCollapsedProjs((prev) => {
      const next = new Set(prev);
      next.delete(id);
      return next;
    });
  };

  const openAgentTab = useCallback(
    (id: string, title?: string) => {
      if (!tabs.find((t) => t.id === id)) {
        const agentInfo = agents.get(id);
        addTab({
          id,
          type: "agent",
          agentId: id,
          title: title || agentInfo?.title || `agent-${id.slice(0, 6)}`,
        });
      }
      setActiveTab(id);
    },
    [tabs, agents, addTab, setActiveTab]
  );

  const openMaster = () => {
    const masterId = masterAgentId;
    if (masterId && agents.has(masterId)) {
      openAgentTab(masterId, "⭐master");
    } else {
      if (!tabs.find((t) => t.id === "master")) {
        addTab({ id: "master", type: "agent", agentId: undefined, title: "⭐master" });
      }
      setActiveTab("master");
    }
  };

  // Draggable left sidebar resize.
  const startLeftResize = (e: React.MouseEvent) => {
    e.preventDefault();
    const startX = e.clientX;
    const startWidth = leftWidth;
    const onMove = (ev: MouseEvent) => {
      const w = Math.min(420, Math.max(180, startWidth + (ev.clientX - startX)));
      setLeftWidth(w);
    };
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  // Group agents by workspace project.
  const projects = new Map<string, { cwd: string; agents: { id: string; title: string }[] }>();
  agents.forEach((a, id) => {
    const projName = projectOf(a.cwd);
    if (!projects.has(projName)) {
      projects.set(projName, { cwd: a.cwd, agents: [] });
    }
    projects.get(projName)!.agents.push({
      id,
      title: a.title || `agent-${id.slice(0, 4)}`,
    });
  });

  return (
    <>
      <div
        className={`left-sidebar${!leftSidebarOpen ? " collapsed" : ""}`}
        style={leftSidebarOpen ? { width: leftWidth } : undefined}
      >
        {leftSidebarOpen && (
          <>
            {/* Zone 1: Brand */}
            <div className="sidebar-brand">CaPilot</div>

            {/* Zone 2: Op bar */}
            <div className="sidebar-actions">
              <span className="sidebar-btn active" title="全部显示">
                👁
              </span>
              <span className="sidebar-btn" onClick={toggleLeftSidebar} title="收起侧栏">
                ☰
              </span>
              <span className="sidebar-btn" title="新建项目">
                📁+
              </span>
              <span className="sidebar-btn" onClick={() => setSettingsOpen(true)} title="设置">
                ⚙
              </span>
            </div>

            {/* Zone 3: Tree */}
            <div className="sidebar-tree">
              {/* Master (pinned, always first) */}
              <div className={`master-pinned${masterCollapsed ? " collapsed" : ""}`}>
                <div
                  className="master-header"
                  onClick={() => setMasterCollapsed(!masterCollapsed)}
                >
                  <span className="m-icon">⭐</span>
                  <span className="m-name">Master 会话</span>
                  <span className="m-arrow">▲</span>
                </div>
                <div
                  className={`terminal-item${activeTabId === (masterAgentId || "master") ? " active" : ""}`}
                  onClick={openMaster}
                >
                  <span className="tm-icon">🔄</span>
                  <span className="tm-name">Master</span>
                  <span className="tm-time">—</span>
                </div>
              </div>

              {/* Dynamic projects from agents */}
              {(() => {
                if (projects.size === 0) {
                  return (
                    <div className="proj">
                      <div className="proj-header">
                        <span className="pj-icon">📁</span>
                        <span className="pj-name">No projects</span>
                        <span className="pj-arrow">▲</span>
                      </div>
                    </div>
                  );
                }

                return [...projects.entries()].map(([name, proj]) => (
                  <div
                    key={name}
                    className={`proj${collapsedProjs.has(name) ? " collapsed" : ""}${focusedProj === name ? " focused" : ""}`}
                  >
                    <div
                      className="proj-header"
                      onClick={() => toggleProj(name)}
                      onDoubleClick={() => focusProject(name)}
                      onContextMenu={(e) => {
                        e.preventDefault();
                        e.stopPropagation();
                        setCtx({
                          x: e.clientX,
                          y: e.clientY,
                          project: name,
                          cwd: proj.cwd,
                        });
                      }}
                    >
                      <span className="pj-icon">📁</span>
                      <span className="pj-name">{name}</span>
                      <span className="pj-arrow">▲</span>
                    </div>
                    {proj.agents.map((a) => (
                      <div
                        key={a.id}
                        className={`terminal-item${activeTabId === a.id ? " active" : ""}`}
                        onClick={() => openAgentTab(a.id)}
                        onContextMenu={(e) => {
                          e.preventDefault();
                          e.stopPropagation();
                          setCtx({ x: e.clientX, y: e.clientY, agentId: a.id });
                        }}
                      >
                        <span className="tm-icon">🤖</span>
                        <span className="tm-name">{a.title}</span>
                        <span className="tm-time">—</span>
                      </div>
                    ))}
                  </div>
                ));
              })()}
            </div>
          </>
        )}
      </div>

      {ctx && <ContextMenu ctx={ctx} onClose={() => setCtx(null)} />}
      {settingsOpen && <SettingsModal onClose={() => setSettingsOpen(false)} />}

      {/* Resize handle */}
      <div className="resize-handle" id="resize-left" onMouseDown={startLeftResize} />
    </>
  );
}

/* ── Right-click context menu ─────────────────────────────────── */

function ContextMenu({ ctx, onClose }: { ctx: CtxState; onClose: () => void }) {
  // ── Project context ───────────────────────────────────────────
  if (ctx.project) {
    return (
      <div
        className="ctx-menu"
        style={{ position: "fixed", left: ctx.x, top: ctx.y, zIndex: 1000 }}
        onClick={(e) => e.stopPropagation()}
        onContextMenu={(e) => e.stopPropagation()}
      >
        <div className="ctx-label">{ctx.project}</div>
        <div
          className="ctx-item"
          onClick={() => {
            spawnAgent("standalone");
            onClose();
          }}
        >
          🖥 新建终端
        </div>
        <div
          className="ctx-item"
          onClick={() => {
            if (ctx.cwd) openPath(ctx.cwd).catch(console.error);
            onClose();
          }}
        >
          📁 在文件管理器中显示
        </div>
        <div className="ctx-sep" />
        <div className="ctx-item" onClick={onClose}>
          ✏ 重命名项目（待开发）
        </div>
      </div>
    );
  }

  // ── Agent context ─────────────────────────────────────────────
  const agent = useStore((s) => s.agents.get(ctx.agentId ?? ""));
  const runtimes = useStore((s) => s.runtimes);
  const addAgent = useStore((s) => s.addAgent);
  const closeTab = useStore((s) => s.closeTab);
  const removeAgent = useStore((s) => s.removeAgent);

  const switchRuntime = async (runtime: string) => {
    try {
      const channel = new Channel<number[]>();
      channel.onmessage = (data) =>
        useStore.getState().appendAgentOutput(ctx.agentId ?? "", data);
      const info = (await invoke("agent_switch_runtime", {
        id: ctx.agentId,
        runtime,
        onData: channel,
      })) as AgentInfo;
      addAgent(info, channel);
    } catch (e) {
      console.error("runtime switch failed:", e);
    }
    onClose();
  };

  const setRole = async (role: "worker" | "standalone") => {
    if (!ctx.agentId) return;
    await setAgentRole(ctx.agentId, role);
    onClose();
  };

  const closeAgent = async () => {
    if (!ctx.agentId) return;
    try {
      // sessions_delete kills the PTY, removes the agent dir + DB session row,
      // and unregisters the worker — so a killed agent won't resurrect on
      // restart (Bug 7).
      await invoke("sessions_delete", { id: ctx.agentId });
    } catch {
      // Fall back to a plain kill so the terminal still closes even if session
      // cleanup failed.
      try {
        await invoke("agent_kill", { id: ctx.agentId });
      } catch {
        // ignore
      }
    }
    closeTab(ctx.agentId);
    removeAgent(ctx.agentId);
    onClose();
  };

  const isWorker = agent?.role === "worker";

  return (
    <div
      className="ctx-menu"
      style={{ position: "fixed", left: ctx.x, top: ctx.y, zIndex: 1000 }}
      onClick={(e) => e.stopPropagation()}
      onContextMenu={(e) => e.stopPropagation()}
    >
      {!isWorker && (
        <div className="ctx-item" onClick={() => setRole("worker")}>
          🤖 设为 worker
        </div>
      )}
      {isWorker && (
        <div className="ctx-item" onClick={() => setRole("standalone")}>
          🔓 取消 worker
        </div>
      )}
      <div className="ctx-sep" />
      <div className="ctx-label">切换 runtime</div>
      {runtimes.map((rt) => (
        <div
          key={rt.id}
          className={`ctx-item${rt.id === agent?.runtime ? " current" : ""}`}
          onClick={() => switchRuntime(rt.id)}
        >
          {rt.name}
          {!rt.available && " (未安装)"}
          {rt.id === agent?.runtime && " · 当前"}
        </div>
      ))}
      <div className="ctx-sep" />
      <div className="ctx-item danger" onClick={closeAgent}>
        ✕ 关闭并终止
      </div>
    </div>
  );
}
