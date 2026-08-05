import { useState, useEffect, useCallback, useRef } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import { useStore, AgentInfo } from "../../state/store";
import { setAgentRole } from "../../state/orchestration";
import { spawnAgent, closeAgent as closeAgentAction } from "../../state/agentActions";
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
  /** Right-click was on the pinned Master terminal (not a deletable project agent). */
  isMaster?: boolean;
}

export function LeftSidebar() {
  const leftSidebarOpen = useStore((s) => s.leftSidebarOpen);
  const agents = useStore((s) => s.agents);
  const tabs = useStore((s) => s.tabs);
  const activeTabId = useStore((s) => s.activeTabId);
  const masterAgentId = useStore((s) => s.masterAgentId);
  const projects = useStore((s) => s.projects);
  const setProjects = useStore((s) => s.setProjects);
  const addProject = useStore((s) => s.addProject);
  const setActiveTab = useStore((s) => s.setActiveTab);
  const addTab = useStore((s) => s.addTab);

  const [focusedProj, setFocusedProj] = useState<string | null>(null);
  const [collapsedProjs, setCollapsedProjs] = useState<Set<string>>(new Set());
  const [ctx, setCtx] = useState<CtxState | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [nprojOpen, setNprojOpen] = useState(false);
  const [nprojError, setNprojError] = useState<string | null>(null);
  const leftWidth = useStore((s) => s.leftWidth);
  const setLeftWidth = useStore((s) => s.setLeftWidth);

  // On mount, pull the on-disk workspace list so empty projects render too.
  useEffect(() => {
    invoke<string[]>("list_projects")
      .then((names) => {
        if (Array.isArray(names) && names.length) setProjects(names);
      })
      .catch(console.error);
  }, [setProjects]);

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

  // Group agents by workspace project (keyed by project name → first cwd + agents).
  const agentsByProject = new Map<
    string,
    { cwd: string; agents: { id: string; title: string }[] }
  >();
  agents.forEach((a, id) => {
    const projName = projectOf(a.cwd);
    if (!agentsByProject.has(projName)) {
      agentsByProject.set(projName, { cwd: a.cwd, agents: [] });
    }
    agentsByProject.get(projName)!.agents.push({
      id,
      title: a.title || `agent-${id.slice(0, 4)}`,
    });
  });

  // The tree is DRIVEN BY the store's project list (which includes empty
  // projects). Any project that only exists via an agent's cwd — e.g. before
  // `list_projects` resolves on mount — is merged in so nothing regresses.
  const projectNames = [...projects];
  for (const name of agentsByProject.keys()) {
    if (!projectNames.includes(name)) projectNames.push(name);
  }

  // [☰] collapses / expands ALL project groups (sidebar stays visible).
  const allCollapsed =
    projectNames.length > 0 && projectNames.every((n) => collapsedProjs.has(n));
  const toggleAllProjects = () => {
    if (projectNames.length === 0) return;
    setCollapsedProjs(allCollapsed ? new Set() : new Set(projectNames));
  };

  // [📁+] create a new workspace project and surface it in the tree.
  // With `path`, the project is rooted at an existing local folder the user
  // picked (create_project creates context/ + agents/ inside it + git init and
  // returns the canonical path); without it, the existing `~/CaPilot/workspaces`
  // behavior is kept. The store key is always the project *name* (base name for
  // the folder flow), never the canonical path.
  const handleCreateProject = async (
    name: string,
    path?: string
  ): Promise<string | null> => {
    const trimmed = name.trim();
    if (!trimmed) return "请输入项目名称";
    try {
      if (path) {
        await invoke<string>("create_project", { name: trimmed, path });
      } else {
        await invoke<string>("create_project", { name: trimmed });
      }
      addProject(trimmed);
      // Default-new projects are expanded.
      setCollapsedProjs((prev) => {
        const next = new Set(prev);
        next.delete(trimmed);
        return next;
      });
      setFocusedProj((cur) => (cur === trimmed ? cur : trimmed));
      setNprojError(null);
      // New project auto-opens a fresh agent terminal (spawnAgent adds +
      // activates the tab). Best-effort: a failed spawn must not block the
      // modal close or undo the created project.
      try {
        await spawnAgent("standalone", trimmed);
      } catch (e) {
        console.error("自动打开终端失败:", e);
        setNprojError(`项目已创建，但自动打开终端失败：${String(e)}`);
      }
      return null;
    } catch (e) {
      setNprojError(String(e));
      return String(e);
    }
  };

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
              <span className="sidebar-btn" onClick={() => setSettingsOpen(true)} title="设置">
                ⚙
              </span>
              <span
                className={`sidebar-btn${allCollapsed ? " active" : ""}`}
                onClick={toggleAllProjects}
                title={allCollapsed ? "展开全部项目" : "收起全部项目"}
              >
                ☰
              </span>
              <span className="sidebar-btn" onClick={() => setNprojOpen(true)} title="新建项目">
                📁+
              </span>
            </div>

            {/* Zone 3: Tree */}
            <div className="sidebar-tree">
              {/* Master (pinned, always first) — a single purple button. */}
              <div
                className={`u9-master-btn${activeTabId === (masterAgentId || "master") ? " active" : ""}`}
                onClick={openMaster}
                onContextMenu={(e) => {
                  e.preventDefault();
                  e.stopPropagation();
                  setCtx({
                    x: e.clientX,
                    y: e.clientY,
                    agentId: masterAgentId ?? undefined,
                    isMaster: true,
                  });
                }}
              >
                <span className="u9-master-icon">🔄</span>
                <span className="u9-master-name">Master 会话</span>
              </div>

              {/* Dynamic projects (driven by store.projects, includes empties) */}
              {(() => {
                if (projectNames.length === 0) {
                  return <div className="nproj-empty-row">暂无项目</div>;
                }

                return projectNames.map((name) => {
                  const proj = agentsByProject.get(name);
                  const projAgents = proj?.agents ?? [];
                  const projCwd = proj?.cwd;
                  return (
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
                            cwd: projCwd,
                          });
                        }}
                      >
                        <span className="pj-icon">📁</span>
                        <span className="pj-name">{name}</span>
                        <span className="pj-arrow">▲</span>
                      </div>
                      {projAgents.length === 0 ? (
                        <div className="nproj-empty-row">（空）· 右键新建终端</div>
                      ) : (
                        projAgents.map((a) => (
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
                            <button
                              className="tm-close"
                              title="关闭并终止"
                              onClick={(e) => {
                                e.stopPropagation();
                                closeAgentAction(a.id);
                              }}
                            >
                              ×
                            </button>
                          </div>
                        ))
                      )}
                    </div>
                  );
                });
              })()}
            </div>
          </>
        )}
      </div>

      {ctx && <ContextMenu ctx={ctx} onClose={() => setCtx(null)} />}
      {settingsOpen && <SettingsModal onClose={() => setSettingsOpen(false)} />}
      {nprojOpen && (
        <NewProjectModal
          error={nprojError}
          onClose={() => {
            setNprojOpen(false);
            setNprojError(null);
          }}
          onCreate={handleCreateProject}
        />
      )}

      {/* Resize handle */}
      <div className="resize-handle" id="resize-left" onMouseDown={startLeftResize} />
    </>
  );
}

/* ── New-project modal ────────────────────────────────────────── */

function NewProjectModal({
  error,
  onClose,
  onCreate,
}: {
  error: string | null;
  onClose: () => void;
  onCreate: (name: string, path?: string) => Promise<string | null>;
}) {
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const closeOnSuccess = (err: string | null) => {
    if (err) return;
    setName("");
    onClose();
  };

  const submit = async () => {
    const trimmed = name.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    const err = await onCreate(trimmed);
    setBusy(false);
    closeOnSuccess(err);
  };

  // 📂 Choose an existing local folder → root the new project there. The project
  // name is derived from the folder's base name (last path segment).
  const pickFolder = async () => {
    if (busy) return;
    setBusy(true);
    try {
      const selected = await open({ directory: true, multiple: false });
      if (typeof selected === "string" && selected) {
        const createdName =
          selected.split(/[\\/]/).filter(Boolean).pop() ?? "项目";
        const err = await onCreate(createdName, selected);
        closeOnSuccess(err);
      }
    } catch (e) {
      console.error("选择文件夹失败:", e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="nproj-overlay" onClick={onClose}>
      <div className="nproj-card" onClick={(e) => e.stopPropagation()}>
        <div className="nproj-title">📁+ 新建项目</div>

        <div className="ug-nproj-label">新建文件夹</div>
        <input
          ref={inputRef}
          className="nproj-input"
          placeholder="项目名称（如 my-project）"
          value={name}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") submit();
            if (e.key === "Escape") onClose();
          }}
        />
        {error && <div className="nproj-error">{error}</div>}
        <div className="nproj-actions">
          <button className="nproj-btn" onClick={onClose}>
            取消
          </button>
          <button
            className="nproj-btn primary"
            onClick={submit}
            disabled={busy || !name.trim()}
          >
            {busy ? "创建中…" : "创建"}
          </button>
        </div>

        <div className="ug-nproj-sep" />

        <div className="ug-nproj-label">或选择现有文件夹</div>
        <button className="ug-nproj-folder" onClick={pickFolder} disabled={busy}>
          📂 选择现有文件夹…
        </button>
      </div>
    </div>
  );
}

/* ── Right-click context menu ─────────────────────────────────── */

function ContextMenu({ ctx, onClose }: { ctx: CtxState; onClose: () => void }) {
  // ── Project context ───────────────────────────────────────────
  if (ctx.project) {
    const proj = ctx.project;
    return (
      <div
        className="ctx-menu"
        style={{ position: "fixed", left: ctx.x, top: ctx.y, zIndex: 1000 }}
        onClick={(e) => e.stopPropagation()}
        onContextMenu={(e) => e.stopPropagation()}
      >
        <div className="ctx-label">{proj}</div>
        <div
          className="ctx-item"
          onClick={() => {
            spawnAgent("standalone", proj);
            onClose();
          }}
        >
          🖥 新建终端
        </div>
        {ctx.cwd && (
          <div
            className="ctx-item"
            onClick={() => {
              if (ctx.cwd) openPath(ctx.cwd).catch(console.error);
              onClose();
            }}
          >
            📁 在文件管理器中显示
          </div>
        )}
        <div className="ctx-sep" />
        <div className="ctx-item" onClick={onClose}>
          ✏ 重命名项目（待开发）
        </div>
        <div className="ctx-sep" />
        <div
          className="ctx-item danger"
          onClick={() => {
            // Remove from the list only — disk files are kept (DevPlan §3.3).
            useStore.getState().removeProject(proj);
            onClose();
          }}
        >
          🗑 移除项目
        </div>
      </div>
    );
  }

  // ── Master context ────────────────────────────────────────────
  if (ctx.isMaster) {
    return <MasterContextMenu ctx={ctx} onClose={onClose} />;
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

/* ── Master terminal context menu ────────────────────────────── */

function MasterContextMenu({ ctx, onClose }: { ctx: CtxState; onClose: () => void }) {
  const masterAgentId = useStore((s) => s.masterAgentId);
  const runtimes = useStore((s) => s.runtimes);
  const addAgent = useStore((s) => s.addAgent);
  // The master terminal may be right-clicked before a master agent exists; fall
  // back to the store's masterAgentId when the ctx didn't carry one.
  const id = ctx.agentId ?? masterAgentId;
  const agent = useStore((s) => s.agents.get(id ?? ""));

  const switchRuntime = async (runtime: string) => {
    if (!id) return;
    try {
      const channel = new Channel<number[]>();
      channel.onmessage = (data) => useStore.getState().appendAgentOutput(id, data);
      const info = (await invoke("agent_switch_runtime", {
        id,
        runtime,
        onData: channel,
      })) as AgentInfo;
      addAgent(info, channel);
    } catch (e) {
      console.error("runtime switch failed:", e);
    }
    onClose();
  };

  const closeMaster = async () => {
    if (!id) return;
    try {
      // Kill the master PTY only — the pinned slot is not deletable (DevPlan).
      await invoke("agent_kill", { id });
    } catch {
      // ignore
    }
    onClose();
  };

  return (
    <div
      className="ctx-menu"
      style={{ position: "fixed", left: ctx.x, top: ctx.y, zIndex: 1000 }}
      onClick={(e) => e.stopPropagation()}
      onContextMenu={(e) => e.stopPropagation()}
    >
      <div className="ctx-item disabled">⭐ master 会话</div>
      {id && (
        <>
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
          <div className="ctx-item danger" onClick={closeMaster}>
            ✕ 关闭 master PTY
          </div>
        </>
      )}
    </div>
  );
}
