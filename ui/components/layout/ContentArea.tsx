import { useEffect, useState } from "react";
import { useStore, Tab } from "../../state/store";
import { XTermPanel } from "../terminal/XTermPanel";
import { EditorPanel } from "../editor/EditorPanel";
import { DiffPanel } from "../editor/DiffPanel";

type DropEdge = "left" | "right" | "top" | "bottom" | null;

/** A single tab's panel (terminal / editor), reused in split panes. */
function Panel({
  tab,
  active,
}: {
  tab: Tab;
  /** True for the panel of the active tab (drives F1 terminal-focus gating). */
  active?: boolean;
}) {
  return (
    <div className="content-panel">
      {tab.type === "agent" && tab.agentId && (
        <XTermPanel agentId={tab.agentId} active={active} />
      )}
      {tab.type === "agent" && !tab.agentId && (
        <div className="panel-placeholder">会话未启动 — 在输入框发消息自动创建</div>
      )}
      {tab.type === "editor" && tab.filePath && (
        <EditorPanel filePath={tab.filePath} active={active} />
      )}
      {tab.type === "diff" && (
        <DiffPanel oldText={tab.diffOld ?? ""} newText={tab.diffNew ?? ""} />
      )}
    </div>
  );
}

export function ContentArea() {
  const tabs = useStore((s) => s.tabs);
  const agents = useStore((s) => s.agents);
  const activeTabId = useStore((s) => s.activeTabId);
  const splitPaneA = useStore((s) => s.splitPaneA);
  const splitPaneB = useStore((s) => s.splitPaneB);
  const splitDirection = useStore((s) => s.splitDirection);
  const splitRatio = useStore((s) => s.splitRatio);
  const draggedTabId = useStore((s) => s.draggedTabId);
  const setSplit = useStore((s) => s.setSplit);
  const setSplitRatio = useStore((s) => s.setSplitRatio);
  const setActiveTab = useStore((s) => s.setActiveTab);
  const requestSearch = useStore((s) => s.requestSearch);

  const [dropEdge, setDropEdge] = useState<DropEdge>(null);
  const [resizing, setResizing] = useState(false);

  // Global Ctrl+F → search the active tab's content. CodeMirror handles Ctrl+F
  // itself when its editor has focus (bubble phase: CM's own keydown runs first
  // and opens its native panel); any other input/textarea (composer, an already
  // open search bar) keeps its text-editing semantics. Otherwise route by tab
  // type through the store directive, mirroring the F1 focus toggle.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey && e.key.toLowerCase() === "f")) {
        return;
      }
      const el = document.activeElement as HTMLElement | null;
      const inCm = !!el?.closest?.(".cm-editor");
      const inTerm = !!el?.closest?.(".xterm");
      if (inCm) return; // CodeMirror owns Ctrl+F (native search panel)
      if (!inTerm && el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA")) {
        return; // composer / native inputs / an open search bar
      }
      const st = useStore.getState();
      const activeTab = st.tabs.find((t) => t.id === st.activeTabId);
      if (!activeTab) return;
      e.preventDefault();
      if (activeTab.type === "editor" && activeTab.filePath) {
        requestSearch("editor");
      } else if (activeTab.type === "agent" && activeTab.agentId) {
        requestSearch("terminal");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [requestSearch]);

  const activeTab = tabs.find((t) => t.id === activeTabId);
  const paneA = tabs.find((t) => t.id === splitPaneA);
  const paneB = tabs.find((t) => t.id === splitPaneB);
  const splitActive =
    splitPaneA !== null && splitPaneB !== null && splitDirection !== null;
  const openCodeTabs = tabs.filter(
    (tab) => tab.type === "agent" && tab.agentId && agents.get(tab.agentId)?.runtime === "opencode"
  );
  const activeIsOpenCode = openCodeTabs.some((tab) => tab.id === activeTab?.id);

  /** Which edge band a drop position falls in (left/right first, then top/bottom). */
  const computeEdge = (clientX: number, clientY: number, el: HTMLElement): DropEdge => {
    const rect = el.getBoundingClientRect();
    const x = clientX - rect.left;
    const y = clientY - rect.top;
    if (x < rect.width * 0.3) return "left";
    if (x > rect.width * 0.7) return "right";
    if (y < rect.height * 0.3) return "top";
    if (y > rect.height * 0.7) return "bottom";
    return null;
  };

  const handleDragOver = (e: React.DragEvent<HTMLDivElement>) => {
    if (!draggedTabId) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
    setDropEdge(computeEdge(e.clientX, e.clientY, e.currentTarget));
  };

  const handleDrop = (e: React.DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    setDropEdge(null);
    const draggedId = draggedTabId || e.dataTransfer.getData("text/plain");
    const dragged = tabs.find((t) => t.id === draggedId);
    if (!dragged) return;
    const edge = computeEdge(e.clientX, e.clientY, e.currentTarget);
    if (!edge) {
      // Dropped in the middle → just activate the tab.
      setActiveTab(dragged.id);
      return;
    }
    const primary = splitActive ? splitPaneA! : activeTabId;
    if (dragged.id === primary || dragged.id === splitPaneB) return; // already visible
    if (!primary) {
      setActiveTab(dragged.id);
      return;
    }
    const direction = edge === "left" || edge === "right" ? "row" : "column";
    const draggedFirst = edge === "left" || edge === "top";
    const newA = draggedFirst ? dragged.id : primary;
    const newB = draggedFirst ? primary : dragged.id;
    setSplit(newA, newB, direction);
    // Keep the active tab visible: if the replaced pane was focused, move
    // focus onto the tab that just took its place.
    const st = useStore.getState();
    if (st.activeTabId !== newA && st.activeTabId !== newB) {
      setActiveTab(dragged.id);
    }
  };

  const startResize = (e: React.MouseEvent<HTMLDivElement>) => {
    e.preventDefault();
    const row = splitDirection === "row";
    const container = e.currentTarget.parentElement!;
    const rect = container.getBoundingClientRect();
    const start = row ? e.clientX : e.clientY;
    const total = row ? rect.width : rect.height;
    const initial = splitRatio;
    setResizing(true);
    const onMove = (ev: MouseEvent) => {
      const delta = (row ? ev.clientX : ev.clientY) - start;
      setSplitRatio(initial + delta / total);
    };
    const onUp = () => {
      setResizing(false);
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  const dropHandlers = {
    onDragOver: handleDragOver,
    onDrop: handleDrop,
  };

  // Empty state (no tab to show anywhere).
  if (!activeTab && !paneA && !paneB) {
    return (
      <div className="content-area">
        <div className="empty-state">
          <img src="/logo.png" alt="CaPilot" />
          <h3>CaPilot IDE</h3>
          <p style={{ fontFamily: "var(--mono)", fontSize: "var(--fs-sm)", color: "var(--ink2)" }}>
            Press + to start a new agent session
          </p>
        </div>
      </div>
    );
  }

  // Split view: two panes side by side (row) or stacked (column).
  if (splitActive && paneA && paneB) {
    const row = splitDirection === "row";
    return (
      <div className="content-area" {...dropHandlers}>
        <div className={`split-container ${row ? "split-row" : "split-column"}`}>
          <div className="split-pane" style={{ flexBasis: `${splitRatio * 100}%` }}>
            <Panel tab={paneA} active={paneA.id === activeTabId} />
          </div>
          <div
            className={`split-divider ${row ? "split-divider-col" : "split-divider-row"}${resizing ? " active" : ""}`}
            onMouseDown={startResize}
          />
          <div className="split-pane" style={{ flex: 1 }}>
            <Panel tab={paneB} active={paneB.id === activeTabId} />
          </div>
        </div>
        {draggedTabId && dropEdge && <div className={`drop-zone ${dropEdge}`} />}
      </div>
    );
  }

  // Default single-panel view.
  return (
    <div className="content-area" {...dropHandlers}>
      {activeTab && !activeIsOpenCode && <Panel tab={activeTab} active />}
      {/* OpenCode owns an alternate-screen TUI whose complete frame is held by
          xterm, not replayed by the PTY. Keep each opened OpenCode panel mounted
          across tab changes so returning to it cannot produce an empty canvas. */}
      {openCodeTabs.map((tab) => (
        <div
          key={tab.id}
          className={`resident-terminal-panel${tab.id === activeTab?.id ? " active" : " hidden"}`}
          aria-hidden={tab.id !== activeTab?.id}
        >
          <Panel tab={tab} active={tab.id === activeTab?.id} />
        </div>
      ))}
      {draggedTabId && dropEdge && <div className={`drop-zone ${dropEdge}`} />}
    </div>
  );
}
