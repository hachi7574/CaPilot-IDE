import { useState } from "react";
import { useStore, Tab, AgentInfo } from "../../state/store";
import { closeAgent as closeAgentAction, MASTER_PROJECT } from "../../state/agentActions";
import { TerminalTemplatePicker } from "./TerminalTemplatePicker";

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
      if (agent?.workspace_id && agent.project) return agent.project;
      if (agent?.cwd) return projectOf(agent.cwd);
    }
    return undefined;
  }
  if ((tab.type === "editor" || tab.type === "diff") && tab.filePath) {
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
  const leftSidebarOpen = useStore((s) => s.leftSidebarOpen);
  const rightSidebarOpen = useStore((s) => s.rightSidebarOpen);
  const toggleRightSidebar = useStore((s) => s.toggleRightSidebar);
  const closeTab = useStore((s) => s.closeTab);
  const dropAgentChannel = useStore((s) => s.dropAgentChannel);

  // Project-scoped view: when a project is focused, show only its tabs. Tabs
  // whose project can't be determined (e.g. mid-spawn) stay visible, and tabs
  // of other projects remain in the store — hidden, NOT closed.
  const visibleTabs = focusedProject
    ? tabs.filter((t) => {
        const tp = tabProject(t, agents, projectRoots);
        return tp === undefined || tp === focusedProject;
      })
    : tabs;

  // New-terminal template picker (the "+" button), anchored at the button.
  const [termPicker, setTermPicker] = useState<{ x: number; y: number } | null>(
    null
  );
  const openPicker = (e: React.MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    setTermPicker({ x: r.right, y: r.bottom });
  };

  return (
    <div className="tab-bar">
      <button
        className="tab-btn-icon"
        onClick={toggleLeftSidebar}
        title={leftSidebarOpen ? "折叠左侧栏" : "展开左侧栏"}
      >
        {leftSidebarOpen ? "«" : "»"}
      </button>
      {visibleTabs.map((tab) => {
        const agent = tab.agentId ? agents.get(tab.agentId) : undefined;
        const status = agent?.status || "idle";
        const roleBadge = agent?.role && agent.role !== "standalone" ? ` · ${agent.role}` : "";
        // Agent records are the live source of truth for terminal names. A
        // tab's title is only the snapshot taken when it was opened, so it can
        // become stale after restoring/resuming a session (or any runtime
        // update that replaces AgentInfo). The sidebar already renders from
        // `agents`; doing the same here keeps both labels in lockstep. Master
        // is intentionally named "master" in the pinned sidebar row.
        const title = tab.type === "agent"
          ? agent?.role === "master"
            ? "master"
            : agent?.title || tab.title
          : tab.title;
        return (
          <div
            key={tab.id}
            className={`tab-item${tab.id === activeTabId ? " active" : ""}${draggedTabId === tab.id ? " dragging" : ""}${status === "done" ? " tab-done" : ""}`}
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
            {agent?.requires_attention && (
              <span
                className={`tab-attention ${agent.attention_reason ?? "finished"}`}
                title={agent.attention_reason === "error" ? "需要处理：异常退出" : "任务已完成"}
              >
                !
              </span>
            )}
            <span>
              {tab.type === "agent" ? "🤖" : "📄"}{title}
            </span>
            {agent?.runtime && (
              <span className="tab-runtime">{agent.runtime}{roleBadge}</span>
            )}
            <button
              className="tab-close"
              onClick={(e) => {
                e.stopPropagation();
                // A worker's home is the sidebar worker track: closing its tab
                // only hides the view and leaves the task/PTY alive. Explicit
                // archive/delete remains available from the sidebar. Ended
                // sessions likewise stay recoverable.
                if (tab.type === "agent" && tab.agentId) {
                  if (agent?.status === "done" || agent?.role === "worker") {
                    closeTab(tab.id);
                    if (agent?.status === "done") dropAgentChannel(tab.agentId);
                  } else {
                    closeAgentAction(tab.agentId);
                  }
                } else {
                  closeTab(tab.id);
                }
              }}
              title={
                tab.type === "agent" && tab.agentId
                  ? agent?.status === "done"
                    ? "关闭（已结束，可从侧栏找回）"
                    : agent?.role === "worker"
                      ? "关闭视图（worker 继续运行）"
                    : "关闭并终止"
                  : "关闭标签"
              }
            >
              ×
            </button>
          </div>
        );
      })}
      <button className="tab-add" title="新建终端" onClick={openPicker}>
        +
      </button>
      {/* Collapse / expand the right sidebar (mirrors the ☰ left-toggle, pinned
          to the right edge; glyph flips to show the sidebar's state). */}
      <button
        className="tab-btn-icon tab-btn-right"
        onClick={toggleRightSidebar}
        title={rightSidebarOpen ? "折叠右侧栏" : "展开右侧栏"}
      >
        {rightSidebarOpen ? "»" : "«"}
      </button>
      {termPicker && (
        <TerminalTemplatePicker
          project={MASTER_PROJECT}
          anchor={termPicker}
          role={workerMode ? "worker" : "standalone"}
          onClose={() => setTermPicker(null)}
        />
      )}
    </div>
  );
}
