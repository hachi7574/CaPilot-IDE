import { useStore } from "../../state/store";
import { XTermPanel } from "../terminal/XTermPanel";
import { EditorPanel } from "../editor/EditorPanel";

export function ContentArea() {
  const tabs = useStore((s) => s.tabs);
  const activeTabId = useStore((s) => s.activeTabId);
  const agents = useStore((s) => s.agents);
  const activeTab = tabs.find((t) => t.id === activeTabId);
  const activeAgent = activeTab?.agentId ? agents.get(activeTab.agentId) : undefined;

  if (!activeTab) {
    return (
      <div className="content-area">
        <div className="empty-state">
          <img src="/logo.png" alt="CaPilot" />
          <h3>CaPilot IDE</h3>
          <p style={{ fontFamily: "var(--mono)", fontSize: 12, color: "var(--muted)" }}>
            Press + to start a new agent session
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="content-area">
      <div className="content-panel">
        <div className="panel-header">
          PANEL · {activeTab.title}{" "}
          {activeTab.type === "agent"
            ? `(${activeAgent?.runtime || "claude"})`
            : ""}
        </div>
        {activeTab.type === "agent" && (
          <XTermPanel agentId={activeTab.agentId || "master"} />
        )}
        {activeTab.type === "editor" && activeTab.filePath && (
          <EditorPanel filePath={activeTab.filePath} />
        )}
      </div>
    </div>
  );
}
