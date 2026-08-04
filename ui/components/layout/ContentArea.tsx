import { useStore } from "../../state/store";
import { XTermPanel } from "../terminal/XTermPanel";

export function ContentArea() {
  const tabs = useStore((s) => s.tabs);
  const activeTabId = useStore((s) => s.activeTabId);
  const activeTab = tabs.find((t) => t.id === activeTabId);

  if (!activeTab) {
    return (
      <div className="content-area">
        <div className="empty-state">
          <img src="/logo.png" alt="CaPilot" />
          <h3>CaPilot IDE</h3>
          <p>Press + to start a new agent session</p>
        </div>
      </div>
    );
  }

  return (
    <div className="content-area">
      <div className="content-panel">
        {activeTab.type === "agent" && activeTab.agentId && (
          <XTermPanel agentId={activeTab.agentId} />
        )}
        {activeTab.type === "editor" && (
          <div className="empty-state">
            <p>📄 Editor: {activeTab.filePath}</p>
          </div>
        )}
      </div>
    </div>
  );
}
