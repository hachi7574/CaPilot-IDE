import { useStore, Tab, AgentInfo } from "../../state/store";
import { spawnAgent } from "../../state/agentActions";

function projectOf(cwd: string): string {
  const m = cwd.match(/workspaces\/([^/]+)/);
  if (m) return m[1];
  const parts = cwd.split("/").filter(Boolean);
  return parts[parts.length - 1] || cwd;
}

/** Longest-matching project root for an editor file path (or undefined). */
function projectRootOfPath(
  filePath: string,
  projectRoots: Record<string, string>
): string | undefined {
  let best: string | undefined;
  let bestLen = -1;
  for (const [name, root] of Object.entries(projectRoots)) {
    const prefix = root.endsWith("/") ? root : `${root}/`;
    if (filePath.startsWith(prefix) && prefix.length > bestLen) {
      best = name;
      bestLen = prefix.length;
    }
  }
  return best;
}

/** Map a tab to its owning project, or undefined when it can't be determined.
 *  - agent tab → the agent's cwd via `projectOf` (master-group cwd → "master")
 *  - editor tab → longest matching `projectRoots` prefix, else `projectOf` on
 *    the file path's dirname */
function tabProject(
  tab: Tab,
  agents: Map<string, AgentInfo>,
  projectRoots: Record<string, string>
): string | undefined {
  if (tab.type === "agent") {
    // Pinned master placeholder tab (no live agent yet).
    if (tab.id === "master") return "master";
    if (tab.agentId) {
      const agent = agents.get(tab.agentId);
      if (agent?.cwd) return projectOf(agent.cwd);
    }
    return undefined;
  }
  if (tab.type === "editor" && tab.filePath) {
    const byRoot = projectRootOfPath(tab.filePath, projectRoots);
    if (byRoot) return byRoot;
    const dir = tab.filePath.split("/").slice(0, -1).join("/");
    return projectOf(dir || tab.filePath);
  }
  return undefined;
}

export function TabBar() {
  const tabs = useStore((s) => s.tabs);
  const activeTabId = useStore((s) => s.activeTabId);
  const agents = useStore((s) => s.agents);
  const projectRoots = useStore((s) => s.projectRoots);
  const focusedProject = useStore((s) => s.focusedProject);
  const workerMode = useStore((s) => s.workerMode);
  const draggedTabId = useStore((s) => s.draggedTabId);
  const setActiveTab = useStore((s) => s.setActiveTab);
  const setDraggedTabId = useStore((s) => s.setDraggedTabId);
  const toggleLeftSidebar = useStore((s) => s.toggleLeftSidebar);
  const closeTab = useStore((s) => s.closeTab);

  // Project-scoped view: when a project is focused, show only its tabs. Tabs
  // whose project can't be determined (e.g. mid-spawn) stay visible, and tabs
  // of other projects remain in the store — hidden, NOT closed.
  const visibleTabs = focusedProject
    ? tabs.filter((t) => {
        const tp = tabProject(t, agents, projectRoots);
        return tp === undefined || tp === focusedProject;
      })
    : tabs;

  // Project badge shows the focused project; fall back to the active tab's
  // project when nothing is focused.
  const activeTab = tabs.find((t) => t.id === activeTabId);
  const projectName = focusedProject
    ? focusedProject
    : activeTab
      ? (tabProject(activeTab, agents, projectRoots) ?? "Agent")
      : "";

  const handleNew = async () => {
    try {
      await spawnAgent(workerMode ? "worker" : "standalone");
    } catch (err) {
      console.error("Failed to spawn agent:", err);
    }
  };

  return (
    <div className="tab-bar">
      <button className="tab-btn-icon" onClick={toggleLeftSidebar} title="折叠侧栏">
        ☰
      </button>
      {projectName && (
        <span className="tab-proj-badge">
          {projectName}
        </span>
      )}
      {visibleTabs.map((tab) => {
        const agent = tab.agentId ? agents.get(tab.agentId) : undefined;
        const status = agent?.status || "idle";
        const roleBadge = agent?.role && agent.role !== "standalone" ? ` · ${agent.role}` : "";
        return (
          <div
            key={tab.id}
            className={`tab-item${tab.id === activeTabId ? " active" : ""}${draggedTabId === tab.id ? " dragging" : ""}`}
            draggable
            onDragStart={(e) => {
              e.dataTransfer.setData("text/plain", tab.id);
              e.dataTransfer.effectAllowed = "copy";
              setDraggedTabId(tab.id);
            }}
            onDragEnd={() => setDraggedTabId(null)}
            onClick={() => setActiveTab(tab.id)}
          >
            <span className={`tab-dot ${status}`} />
            <span>
              {tab.type === "agent" ? "🤖" : "📄"}{tab.title}
            </span>
            {agent?.runtime && (
              <span className="tab-runtime">{agent.runtime}{roleBadge}</span>
            )}
            <button
              className="tab-close"
              onClick={(e) => {
                e.stopPropagation();
                closeTab(tab.id);
              }}
              title="关闭标签"
            >
              ×
            </button>
          </div>
        );
      })}
      <button className="tab-add" title="新建终端" onClick={handleNew}>
        +
      </button>
    </div>
  );
}
