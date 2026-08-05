import { useStore } from "../../state/store";
import { spawnAgent } from "../../state/agentActions";

function projectOf(cwd: string): string {
  const m = cwd.match(/workspaces\/([^/]+)/);
  if (m) return m[1];
  const parts = cwd.split("/").filter(Boolean);
  return parts[parts.length - 1] || cwd;
}

export function TabBar() {
  const tabs = useStore((s) => s.tabs);
  const activeTabId = useStore((s) => s.activeTabId);
  const agents = useStore((s) => s.agents);
  const workerMode = useStore((s) => s.workerMode);
  const setActiveTab = useStore((s) => s.setActiveTab);
  const toggleLeftSidebar = useStore((s) => s.toggleLeftSidebar);
  const closeTab = useStore((s) => s.closeTab);

  // Derive project name from current active tab's agent cwd
  const activeTab = tabs.find((t) => t.id === activeTabId);
  const activeAgent = activeTab?.agentId ? agents.get(activeTab.agentId) : undefined;
  const projectName = activeAgent?.cwd ? projectOf(activeAgent.cwd) : activeTab ? "Agent" : "";

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
      {tabs.map((tab) => {
        const agent = tab.agentId ? agents.get(tab.agentId) : undefined;
        const status = agent?.status || "idle";
        const roleBadge = agent?.role && agent.role !== "standalone" ? ` · ${agent.role}` : "";
        return (
          <div
            key={tab.id}
            className={`tab-item${tab.id === activeTabId ? " active" : ""}`}
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
