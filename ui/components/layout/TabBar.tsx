import { useStore } from "../../state/store";

export function TabBar() {
  const tabs = useStore((s) => s.tabs);
  const activeTabId = useStore((s) => s.activeTabId);
  const agents = useStore((s) => s.agents);
  const setActiveTab = useStore((s) => s.setActiveTab);
  const toggleLeftSidebar = useStore((s) => s.toggleLeftSidebar);

  // Derive project name from current active tab's agent cwd
  const activeTab = tabs.find((t) => t.id === activeTabId);
  const activeAgent = activeTab?.agentId ? agents.get(activeTab.agentId) : undefined;
  const projectName = activeAgent?.cwd.split("/").pop() || (activeTab ? "Agent" : "");

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
              <span className="tab-runtime">{agent.runtime}</span>
            )}
          </div>
        );
      })}
      <button className="tab-add" title="新建终端">
        +
      </button>
    </div>
  );
}
