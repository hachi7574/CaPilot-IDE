import { useState, useEffect, useCallback, useRef } from "react";
import { invoke, Channel } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import { useStore, AgentInfo } from "../../state/store";
import { setAgentRole } from "../../state/orchestration";
import { spawnAgent, closeAgent as closeAgentAction, MASTER_PROJECT } from "../../state/agentActions";
import { SettingsModal } from "./SettingsModal";

/** Derive the workspace project name from an agent cwd. */
function projectOf(cwd: string): string {
  const m = cwd.match(/workspaces\/([^/]+)/);
  if (m) return m[1];
  const parts = cwd.split("/").filter(Boolean);
  return parts[parts.length - 1] || cwd;
}

/** Derive a project name from a git clone URL (last path segment, strip .git).
 *  e.g. `https://github.com/owner/repo.git` → `repo`; query/fragment stripped. */
function repoBaseName(url: string): string {
  const clean = url.split(/[?#]/)[0];
  const seg = clean.split("/").filter(Boolean).pop() ?? "";
  return seg.replace(/\.git$/i, "");
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
  const setProjectRoots = useStore((s) => s.setProjectRoots);
  const focusedProject = useStore((s) => s.focusedProject);
  const setFocusedProject = useStore((s) => s.setFocusedProject);
  const addProject = useStore((s) => s.addProject);
  const setActiveTab = useStore((s) => s.setActiveTab);
  const addTab = useStore((s) => s.addTab);

  const [collapsedProjs, setCollapsedProjs] = useState<Set<string>>(new Set());
  const [masterExpanded, setMasterExpanded] = useState(true);
  const [ctx, setCtx] = useState<CtxState | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [nprojOpen, setNprojOpen] = useState(false);
  const [nprojError, setNprojError] = useState<string | null>(null);
  const leftWidth = useStore((s) => s.leftWidth);
  const setLeftWidth = useStore((s) => s.setLeftWidth);

  // On mount, pull the on-disk workspace list so empty projects render too.
  // Rust `list_projects` returns `{name, root}` entries — feed both the name
  // list (tree grouping) and the root map (tab-bar editor-file resolution).
  // Then default the focus to the first project so the very first view is
  // already project-scoped.
  useEffect(() => {
    invoke<{ name: string; root: string }[]>("list_projects")
      .then((entries) => {
        if (Array.isArray(entries) && entries.length) {
          const names = entries.map((e) => e.name);
          const roots: Record<string, string> = {};
          for (const e of entries) roots[e.name] = e.root;
          setProjects(names);
          setProjectRoots(roots);
          if (useStore.getState().focusedProject === null) {
            setFocusedProject(names[0]);
          }
        }
      })
      .catch(console.error);
  }, [setProjects, setProjectRoots, setFocusedProject]);

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
    setFocusedProject(id === focusedProject ? null : id);
    setCollapsedProjs((prev) => {
      const next = new Set(prev);
      next.delete(id);
      return next;
    });
  };

  // Spawn a terminal under a project, expanding the project first so the new
  // terminal is immediately visible (hover "+" button + project context menu).
  const spawnInProject = (proj: string) => {
    setCollapsedProjs((prev) => {
      const next = new Set(prev);
      next.delete(proj);
      return next;
    });
    spawnAgent("standalone", proj).catch(console.error);
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
    setFocusedProject(MASTER_PROJECT);
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

  // Open a terminal AND single-select its project (terminal clicks focus the
  // owning project — any other project loses focus).
  const openProjectTerminal = (proj: string, id: string) => {
    setFocusedProject(proj);
    openAgentTab(id);
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
  // The Master group's project ("master") is pinned and rendered separately, so
  // it is always excluded here.
  const projectNames = [...projects];
  for (const name of agentsByProject.keys()) {
    if (!projectNames.includes(name)) projectNames.push(name);
  }
  for (let i = projectNames.length - 1; i >= 0; i--) {
    if (projectNames[i] === MASTER_PROJECT) projectNames.splice(i, 1);
  }

  // Terminals living in the Master group: standalone agents spawned via the
  // group's "＋ 新建终端" (cwd maps to the "master" project). The master session
  // row itself is always rendered separately, so it is excluded here.
  const masterTerminals = (agentsByProject.get(MASTER_PROJECT)?.agents ?? []).filter(
    (a) => a.id !== masterAgentId
  );

  const toggleMaster = () => setMasterExpanded((v) => !v);

  // [☰] collapses / expands ALL project groups (sidebar stays visible).
  // Empty projects (zero terminals) are never collapsible — they have no
  // triangle and their header doesn't toggle — so they are excluded here.
  const expandableProjects = projectNames.filter(
    (n) => (agentsByProject.get(n)?.agents?.length ?? 0) > 0
  );
  // [☰] now covers the Master group too: "all collapsed" means the master group
  // is collapsed AND every expandable project is collapsed. Toggling collapses /
  // expands both (`.every` on an empty list is vacuously true, so with no
  // expandable projects the button still reflects / toggles the master).
  const allCollapsed =
    !masterExpanded && expandableProjects.every((n) => collapsedProjs.has(n));
  const toggleAllProjects = () => {
    if (allCollapsed) {
      // Expand everything: expand master + clear collapsed projects.
      setMasterExpanded(true);
      setCollapsedProjs(new Set());
    } else {
      // Collapse everything: collapse master + all expandable projects.
      setMasterExpanded(false);
      setCollapsedProjs(new Set(expandableProjects));
    }
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
        const root = await invoke<string>("create_project", { name: trimmed, path });
        // Rooted at the user-picked folder — record the canonical path so the
        // tab bar can map editor files under it to this project.
        addProject(trimmed, root);
      } else {
        await invoke<string>("create_project", { name: trimmed });
        addProject(trimmed);
      }
      // Default-new projects are expanded.
      setCollapsedProjs((prev) => {
        const next = new Set(prev);
        next.delete(trimmed);
        return next;
      });
      setFocusedProject(trimmed);
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

  // [🔄 从 Git 克隆] clone a remote repo into a chosen parent dir, then surface
  // it in the tree like any other project (addProject + auto-spawn + expand).
  // The Rust `git_clone` validates the URL + name + parent dir; we only check
  // the fields are present here so the errors surface through `nprojError`.
  const handleGitClone = async (
    url: string,
    name: string,
    parentDir: string
  ): Promise<string | null> => {
    const trimmedUrl = url.trim();
    const trimmedName = name.trim();
    if (!trimmedUrl) {
      setNprojError("请输入 Git 仓库地址");
      return "请输入 Git 仓库地址";
    }
    if (!trimmedName) {
      setNprojError("请输入项目名称");
      return "请输入项目名称";
    }
    if (!parentDir) {
      setNprojError("请选择父目录");
      return "请选择父目录";
    }
    try {
      const root = await invoke<string>("git_clone", {
        url: trimmedUrl,
        name: trimmedName,
        parentDir,
      });
      // Record the clone dir as the project root so the tab bar can resolve
      // editor files opened inside it to this project.
      addProject(trimmedName, root);
      // Newly cloned projects are expanded.
      setCollapsedProjs((prev) => {
        const next = new Set(prev);
        next.delete(trimmedName);
        return next;
      });
      setFocusedProject(trimmedName);
      setNprojError(null);
      // Auto-open a fresh agent terminal in the clone. Best-effort: a failed
      // spawn must not block the modal close or undo the cloned project.
      try {
        await spawnAgent("standalone", trimmedName);
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
                +
              </span>
            </div>

            {/* Zone 3: Tree */}
            <div className="sidebar-tree">
              {/* Master (pinned, always first) — a collapsible purple group. */}
              <div className={`uj-master-group${masterExpanded ? "" : " collapsed"}${focusedProject === MASTER_PROJECT ? " uo-master-focused" : ""}`}>
                <div
                  className={`u9-master-btn${activeTabId === (masterAgentId || "master") ? " active" : ""}`}
                  onClick={toggleMaster}
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
                  <span className="uj-master-arrow">▾</span>
                  <span className="u9-master-icon">🔄</span>
                  <span className="u9-master-name">Master</span>
                  <button
                    className="un-master-new"
                    title="新建终端"
                    onClick={(e) => {
                      e.stopPropagation();
                      setMasterExpanded(true);
                      spawnAgent("standalone", MASTER_PROJECT).catch(console.error);
                    }}
                  >
                    +
                  </button>
                </div>
                {masterExpanded && (
                  <>
                    {/* Master session terminal row — opens the master. */}
                    <div
                      className={`terminal-item uj-master-term${activeTabId === (masterAgentId || "master") ? " active" : ""}`}
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
                      <span className="tm-icon">🤖</span>
                      <span className="tm-name">⭐ master</span>
                      <span className="tm-time">—</span>
                    </div>
                    {/* Standalone terminals spawned into the Master group. */}
                    {masterTerminals.map((a) => (
                      <div
                        key={a.id}
                        className={`terminal-item${activeTabId === a.id ? " active" : ""}`}
                        onClick={() => openProjectTerminal(MASTER_PROJECT, a.id)}
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
                  </>
                )}
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
                  // A project with zero terminals has nothing to collapse: no
                  // triangle, and the header click does not toggle.
                  const hasAgents = projAgents.length > 0;
                  return (
                    <div
                      key={name}
                      className={`proj${hasAgents && collapsedProjs.has(name) ? " collapsed" : ""}${focusedProject === name ? " focused" : ""}`}
                    >
                      <div
                        className="proj-header"
                        onClick={() => hasAgents && toggleProj(name)}
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
                        {hasAgents && <span className="pj-arrow">▾</span>}
                        <span className="pj-name">{name}</span>
                        <button
                          className="um-new-term"
                          title="新建终端"
                          onClick={(e) => {
                            e.stopPropagation();
                            spawnInProject(name);
                          }}
                        >
                          +
                        </button>
                      </div>
                      {projAgents.length === 0 ? (
                        <div className="nproj-empty-row">（空）· 右键新建终端</div>
                      ) : (
                        projAgents.map((a) => (
                          <div
                            key={a.id}
                            className={`terminal-item${activeTabId === a.id ? " active" : ""}`}
                            onClick={() => openProjectTerminal(name, a.id)}
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

      {ctx && (
        <ContextMenu
          ctx={ctx}
          onClose={() => setCtx(null)}
          onSpawnInProject={spawnInProject}
        />
      )}
      {settingsOpen && <SettingsModal onClose={() => setSettingsOpen(false)} />}
      {nprojOpen && (
        <NewProjectModal
          error={nprojError}
          onClose={() => {
            setNprojOpen(false);
            setNprojError(null);
          }}
          onCreate={handleCreateProject}
          onGitClone={handleGitClone}
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
  onGitClone,
}: {
  error: string | null;
  onClose: () => void;
  onCreate: (name: string, path?: string) => Promise<string | null>;
  onGitClone: (url: string, name: string, parentDir: string) => Promise<string | null>;
}) {
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [gitUrl, setGitUrl] = useState("");
  const [gitName, setGitName] = useState("");
  const [parentDir, setParentDir] = useState("");
  const prevAutoName = useRef("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // Auto-fill the project name from the repo URL's base name (editable): it
  // tracks the last auto-derived value so a hand-typed name isn't clobbered.
  useEffect(() => {
    const base = repoBaseName(gitUrl);
    if ((gitName === "" || gitName === prevAutoName.current) && base) {
      setGitName(base);
    }
    prevAutoName.current = base;
  }, [gitUrl, gitName]);

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

  // 📂 Pick the parent directory the repo will be cloned into.
  const pickParentDir = async () => {
    if (busy) return;
    setBusy(true);
    try {
      const selected = await open({ directory: true, multiple: false });
      if (typeof selected === "string" && selected) {
        setParentDir(selected);
      }
    } catch (e) {
      console.error("选择父目录失败:", e);
    } finally {
      setBusy(false);
    }
  };

  // 🔄 Clone the git repo into the chosen parent dir. Validation, store updates
  // and auto-spawn live in the parent's onGitClone; errors surface via the
  // shared `error` (nprojError) display and the modal stays open on failure.
  const cloneSubmit = async () => {
    if (busy) return;
    setBusy(true);
    const err = await onGitClone(gitUrl, gitName, parentDir);
    setBusy(false);
    closeOnSuccess(err);
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

        <div className="ug-nproj-sep" />

        <div className="ug-nproj-label">🔄 从 Git 克隆</div>
        <input
          className="nproj-input"
          placeholder="https://github.com/owner/repo.git"
          value={gitUrl}
          onChange={(e) => setGitUrl(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") cloneSubmit();
            if (e.key === "Escape") onClose();
          }}
        />
        <div className="un-git-row">
          <button
            className="ug-nproj-folder"
            onClick={pickParentDir}
            disabled={busy}
          >
            📂 选择父目录…
          </button>
          {parentDir ? (
            <span className="un-git-parent" title={parentDir}>
              {parentDir}
            </span>
          ) : null}
        </div>
        <input
          className="nproj-input"
          placeholder="项目名称（默认取仓库名）"
          value={gitName}
          onChange={(e) => setGitName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") cloneSubmit();
            if (e.key === "Escape") onClose();
          }}
        />
        <button
          className="nproj-btn primary un-git-clone"
          onClick={cloneSubmit}
          disabled={busy}
        >
          {busy ? "克隆中…" : "克隆并创建"}
        </button>
      </div>
    </div>
  );
}

/* ── Right-click context menu ─────────────────────────────────── */

function ContextMenu({
  ctx,
  onClose,
  onSpawnInProject,
}: {
  ctx: CtxState;
  onClose: () => void;
  onSpawnInProject?: (project: string) => void;
}) {
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
            // Spawn + expand (so a terminal added to an empty/collapsed
            // project is immediately visible).
            if (onSpawnInProject) onSpawnInProject(proj);
            else spawnAgent("standalone", proj);
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
  const allAgents = useStore((s) => s.agents);
  const runtimes = useStore((s) => s.runtimes);
  const addAgent = useStore((s) => s.addAgent);
  const closeTab = useStore((s) => s.closeTab);
  const removeAgent = useStore((s) => s.removeAgent);

  // Count terminals in this project (same `projectOf` grouping as the tree).
  // When it is the project's ONLY terminal, the context menu swaps the normal
  // "关闭并终止" for "关闭并移除项目" (removes the whole project). The Master
  // group is never a deletable project, so its terminals keep the plain close.
  const project = agent ? projectOf(agent.cwd) : undefined;
  const isMasterProject = project === MASTER_PROJECT;
  let projCount = 0;
  if (project && !isMasterProject) {
    allAgents.forEach((a) => {
      if (projectOf(a.cwd) === project) projCount++;
    });
  }

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
        ✕ 终止并关闭
      </div>
      {project && !isMasterProject && projCount === 1 && (
        <div
          className="ctx-item danger"
          onClick={() => {
            // Last terminal of the project — remove the whole project (kills +
            // closes its agents via the store action).
            useStore.getState().removeProject(project);
            onClose();
          }}
        >
          🗑 关闭并移除项目
        </div>
      )}
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
        </>
      )}
    </div>
  );
}
