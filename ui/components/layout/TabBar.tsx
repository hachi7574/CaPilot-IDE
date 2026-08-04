import { useStore } from "../../state/store";

export function TabBar() {
  const tabs = useStore((s) => s.tabs);
  const activeTabId = useStore((s) => s.activeTabId);
  const closeTab = useStore((s) => s.closeTab);
  const setActiveTab = useStore((s) => s.setActiveTab);

  return (
    <div className="tab-bar">
      {tabs.map((tab) => (
        <div
          key={tab.id}
          className={`tab ${tab.id === activeTabId ? "active" : ""}`}
          onClick={() => setActiveTab(tab.id)}
        >
          <span>
            {tab.type === "agent" ? "🤖 " : "📄 "}
            {tab.title}
          </span>
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
      ))}
      <button className="tab-add" title="New agent terminal">
        +
      </button>
    </div>
  );
}
