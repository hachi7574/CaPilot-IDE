import { useStore } from "../../state/store";

export function TabBar() {
  const tabs = useStore((s) => s.tabs);
  const activeTabId = useStore((s) => s.activeTabId);
  const agents = useStore((s) => s.agents);
  const closeTab = useStore((s) => s.closeTab);
  const setActiveTab = useStore((s) => s.setActiveTab);
  const toggleLeftSidebar = useStore((s) => s.toggleLeftSidebar);

  const activeTab = tabs.find((t) => t.id === activeTabId);

  return (
    <div className="tab-bar">
      <button className="tab-btn-icon" onClick={toggleLeftSidebar} title="折叠侧栏">
        ☰
      </button>
      {activeTab && (
        <span className="tab-proj-badge">
          {activeTab.type === "agent" ? (activeTab.title || "Agent") : "Editor"}
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
              {tab.type === "agent" ? "🤖" : "📄"} {tab.title}
            </span>
            {agent?.runtime && (
              <span className="tab-runtime">{agent.runtime}</span>
            )}
            <button
              className="tab-close"
              onClick={(e) => {
                e.stopPropagation();
                closeTab(tab.id);
              }}
            >
              ×
            </button>
          </div>
        );
      })}
      <button className="tab-add" title="新建终端">
        +
      </button>
    </div>
  );
}
