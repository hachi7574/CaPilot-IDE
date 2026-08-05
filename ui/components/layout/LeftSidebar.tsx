import { useState } from "react";
import { useStore } from "../../state/store";

export function LeftSidebar() {
  const leftSidebarOpen = useStore((s) => s.leftSidebarOpen);
  const toggleLeftSidebar = useStore((s) => s.toggleLeftSidebar);
  const agents = useStore((s) => s.agents);
  const tabs = useStore((s) => s.tabs);
  const activeTabId = useStore((s) => s.activeTabId);
  const setActiveTab = useStore((s) => s.setActiveTab);
  const addTab = useStore((s) => s.addTab);

  const [masterCollapsed, setMasterCollapsed] = useState(false);
  const [focusedProj, setFocusedProj] = useState<string | null>(null);
  const [collapsedProjs, setCollapsedProjs] = useState<Set<string>>(new Set());

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

  return (
    <>
      <div className={`left-sidebar${!leftSidebarOpen ? " collapsed" : ""}`}>
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
              <span className="sidebar-btn" title="设置">
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
                  className={`terminal-item${activeTabId === "master" ? " active" : ""}`}
                  onClick={() => {
                    if (!tabs.find((t) => t.id === "master")) {
                      addTab({
                        id: "master",
                        type: "agent",
                        agentId: undefined,
                        title: "⭐master",
                      });
                    }
                    setActiveTab("master");
                  }}
                >
                  <span className="tm-icon">🔄</span>
                  <span className="tm-name">Master</span>
                  <span className="tm-time">—</span>
                </div>
              </div>

              {/* Dynamic projects from agents */}
              {(() => {
                // Group agents by cwd (projects)
                const projects = new Map<
                  string,
                  { cwd: string; agents: { id: string; title: string }[] }
                >();
                agents.forEach((a, id) => {
                  const projName = a.cwd.split("/").pop() || a.cwd;
                  if (!projects.has(projName)) {
                    projects.set(projName, { cwd: a.cwd, agents: [] });
                  }
                  projects.get(projName)!.agents.push({ id, title: a.title || `agent-${id.slice(0, 4)}` });
                });

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
                    >
                      <span className="pj-icon">📁</span>
                      <span className="pj-name">{name}</span>
                      <span className="pj-arrow">▲</span>
                    </div>
                    {proj.agents.map((a) => (
                      <div
                        key={a.id}
                        className={`terminal-item${activeTabId === a.id ? " active" : ""}`}
                        onClick={() => {
                          if (!tabs.find((t) => t.id === a.id)) {
                            const agentInfo = agents.get(a.id);
                            addTab({
                              id: a.id,
                              type: "agent",
                              agentId: a.id,
                              title: agentInfo?.title || `agent-${a.id.slice(0, 6)}`,
                            });
                          }
                          setActiveTab(a.id);
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

      {/* Resize handle */}
      <div className="resize-handle" id="resize-left" />
    </>
  );
}
