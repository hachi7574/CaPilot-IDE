import { useStore } from "../../state/store";

export function LeftSidebar() {
  const leftSidebarOpen = useStore((s) => s.leftSidebarOpen);
  const toggleLeftSidebar = useStore((s) => s.toggleLeftSidebar);

  if (!leftSidebarOpen) {
    return (
      <div className="left-sidebar collapsed">
        <button
          className="sidebar-btn"
          onClick={toggleLeftSidebar}
          title="Expand sidebar"
          style={{ margin: 4 }}
        >
          ☰
        </button>
      </div>
    );
  }

  return (
    <div className="left-sidebar">
      {/* Zone 1: Brand */}
      <div className="sidebar-brand">
        <img src="/logo.png" alt="CaPilot" />
        <span>CaPilot</span>
      </div>

      {/* Zone 2: Actions */}
      <div className="sidebar-actions">
        <button className="sidebar-btn" title="Filter workers">
          👁
        </button>
        <button className="sidebar-btn" onClick={toggleLeftSidebar} title="Collapse">
          ☰
        </button>
        <button className="sidebar-btn" title="New project">
          📁+
        </button>
        <button className="sidebar-btn" title="Settings">
          ⚙
        </button>
      </div>

      {/* Zone 3: Project tree */}
      <div className="sidebar-tree">
        <div style={{ padding: "8px 0", color: "var(--muted)", fontSize: 11 }}>
          ⭐ Master Session
        </div>
        <div style={{ padding: "4px 16px", color: "var(--muted)", fontSize: 11 }}>
          No projects yet
        </div>
      </div>
    </div>
  );
}
