import { useEffect, useState, ReactNode } from "react";
import { useStore, Tab, SplitNode, resolveCtrlTRuntime } from "../../state/store";
import { splitLeafTabIds } from "../../state/store";
import { XTermPanel } from "../terminal/XTermPanel";
import { EditorPanel } from "../editor/EditorPanel";
import { DiffPanel } from "../editor/DiffPanel";
import { ImageViewerPanel } from "../editor/ImageViewerPanel";
import { CanvasPanel } from "../canvas/CanvasPanel";
import { Icon } from "../Icon";
import { CaPilotLogo } from "../CaPilotLogo";
import { spawnAgent } from "../../state/agentActions";
import { notify } from "../../state/notify";
import { useT, t as tStatic } from "../../i18n";
import { isMouseTuiRuntime } from "../terminal/mouseProtocol";

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
  const t = useT();
  return (
    <div className="content-panel">
      {tab.type === "agent" && tab.agentId && (
        <XTermPanel agentId={tab.agentId} active={active} />
      )}
      {tab.type === "agent" && !tab.agentId && (
        <div className="panel-placeholder">{t("content.sessionNotStarted")}</div>
      )}
      {tab.type === "editor" && tab.filePath && (
        <EditorPanel filePath={tab.filePath} active={active} />
      )}
      {tab.type === "image" && tab.filePath && (
        <ImageViewerPanel filePath={tab.filePath} />
      )}
      {tab.type === "diff" && (
        <DiffPanel oldText={tab.diffOld ?? ""} newText={tab.diffNew ?? ""} />
      )}
      {tab.type === "canvas" && (
        <CanvasPanel
          scope={{
            projectId: tab.project ?? "default",
            workspaceId: tab.filePath ?? tab.project ?? "default",
          }}
          active={active}
        />
      )}
    </div>
  );
}

/** Which edge band a drop position falls in (left/right first, then top/bottom). */
function computeEdge(clientX: number, clientY: number, el: HTMLElement): DropEdge {
  const rect = el.getBoundingClientRect();
  const x = clientX - rect.left;
  const y = clientY - rect.top;
  if (x < rect.width * 0.3) return "left";
  if (x > rect.width * 0.7) return "right";
  if (y < rect.height * 0.3) return "top";
  if (y > rect.height * 0.7) return "bottom";
  return null;
}

/** A pane box that accepts edge drops: middle drop activates the dragged tab,
 *  an edge drop splits this pane with it. `targetTabId` is the tab this shell
 *  currently holds — the pane that would be split. */
function DropShell({
  targetTabId,
  draggedTabId,
  onSplit,
  children,
}: {
  targetTabId: string;
  draggedTabId: string | null;
  onSplit: (targetTabId: string, draggedTabId: string, edge: DropEdge) => void;
  children: ReactNode;
}) {
  const [dropEdge, setDropEdge] = useState<DropEdge>(null);

  const handleDragOver = (e: React.DragEvent<HTMLDivElement>) => {
    if (!draggedTabId) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
    setDropEdge(computeEdge(e.clientX, e.clientY, e.currentTarget));
  };

  const handleDragLeave = (e: React.DragEvent<HTMLDivElement>) => {
    // Ignore transitions between the shell's own children (xterm canvas etc.).
    if ((e.currentTarget as Node).contains(e.relatedTarget as Node)) return;
    setDropEdge(null);
  };

  const handleDrop = (e: React.DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    const edge = computeEdge(e.clientX, e.clientY, e.currentTarget);
    setDropEdge(null);
    if (!draggedTabId) return;
    onSplit(targetTabId, draggedTabId, edge);
  };

  return (
    <div
      className="pane-shell"
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
    >
      {children}
      {draggedTabId && dropEdge && <div className={`drop-zone ${dropEdge}`} />}
    </div>
  );
}

/** Recursive renderer for the split tree. A leaf renders its tab in a
 *  drop-capable shell; a split renders a flex row/column with a draggable
 *  divider between its two subtrees. */
function SplitView({
  node,
  tabs,
  activeTabId,
  draggedTabId,
  resizingId,
  onSplit,
  onResizeStart,
}: {
  node: SplitNode;
  tabs: Tab[];
  activeTabId: string | null;
  draggedTabId: string | null;
  resizingId: string | null;
  onSplit: (targetTabId: string, draggedTabId: string, edge: DropEdge) => void;
  onResizeStart: (
    e: React.MouseEvent<HTMLDivElement>,
    node: Extract<SplitNode, { kind: "split" }>
  ) => void;
}) {
  if (node.kind === "leaf") {
    const tab = tabs.find((t) => t.id === node.tabId);
    if (!tab) return null;
    return (
      <DropShell
        targetTabId={tab.id}
        draggedTabId={draggedTabId}
        onSplit={onSplit}
      >
        <Panel tab={tab} active={tab.id === activeTabId} />
      </DropShell>
    );
  }
  const row = node.direction === "row";
  const childProps = {
    tabs,
    activeTabId,
    draggedTabId,
    resizingId,
    onSplit,
    onResizeStart,
  };
  return (
    <div className={`split-container ${row ? "split-row" : "split-column"}`}>
      <div className="split-pane" style={{ flexBasis: `${node.ratio * 100}%` }}>
        <SplitView node={node.a} {...childProps} />
      </div>
      <div
        className={`split-divider ${row ? "split-divider-col" : "split-divider-row"}${resizingId === node.id ? " active" : ""}`}
        onMouseDown={(e) => onResizeStart(e, node)}
      />
      <div className="split-pane" style={{ flex: 1 }}>
        <SplitView node={node.b} {...childProps} />
      </div>
    </div>
  );
}

/** Splash empty state: logo / title / hint stay up for a few seconds, then
 *  fade out so the main pane is just the wallpaper surface. Remounting (e.g.
 *  closing the last tab) restarts the timer. */
function EmptyState({
  noProjects,
  onNewProject,
}: {
  noProjects: boolean;
  onNewProject: () => void;
}) {
  const t = useT();
  const [faded, setFaded] = useState(false);

  useEffect(() => {
    setFaded(false);
    const timer = window.setTimeout(() => setFaded(true), 2000);
    return () => window.clearTimeout(timer);
  }, []);

  return (
    <div className={`empty-state${faded ? " faded" : ""}`} aria-hidden={faded}>
      <div className="empty-state-content">
        <CaPilotLogo className="empty-state-logo" />
        <h3>CaPilot IDE</h3>
        {noProjects ? (
          <>
            <p className="empty-state-hint">{t("content.noProjects")}</p>
            <button
              className="empty-state-cta"
              onClick={onNewProject}
              tabIndex={faded ? -1 : 0}
            >
              <Icon name="folder-plus" size={13} /> {t("content.newProject")}
            </button>
          </>
        ) : (
          <p className="empty-state-hint">{t("content.ctrlTHint")}</p>
        )}
      </div>
    </div>
  );
}

export function ContentArea() {
  const tabs = useStore((s) => s.tabs);
  const agents = useStore((s) => s.agents);
  const activeTabId = useStore((s) => s.activeTabId);
  const splitTree = useStore((s) => s.splitTree);
  const draggedTabId = useStore((s) => s.draggedTabId);
  const setSplitRatio = useStore((s) => s.setSplitRatio);
  const splitPane = useStore((s) => s.splitPane);
  const setActiveTab = useStore((s) => s.setActiveTab);
  const requestSearch = useStore((s) => s.requestSearch);
  const projects = useStore((s) => s.projects);
  const setNprojOpen = useStore((s) => s.setNprojOpen);

  const [resizingId, setResizingId] = useState<string | null>(null);

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
      // canvas / image / diff: no in-panel search in v1
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [requestSearch]);

  // Global Ctrl+T → start a new agent session (the empty-state hint advertises
  // it). Scoped to the composer input so terminal TUIs keep Ctrl+T for their
  // own semantics (the TUI may use it as a tab / next-buffer key). The empty
  // welcome state (no tabs) still spawns from anywhere so the hint works before
  // the user has focused the input.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey && e.key.toLowerCase() === "t")) {
        return;
      }
      const el = document.activeElement as HTMLElement | null;
      const inComposer = !!el?.closest?.(".composer-input");
      const st = useStore.getState();
      const tab = st.tabs.find((tb) => tb.id === st.activeTabId);
      if (tab?.type === "canvas") return;
      if (!inComposer && st.tabs.length > 0) return;
      e.preventDefault();
      // Effective runtime: the configured Ctrl+T preference, else claude, else
      // bash. With no runtime available at all, show the hint instead of
      // spawning a dead terminal.
      const runtime = resolveCtrlTRuntime(st.ctrlTRuntime, st.runtimes);
      if (!runtime) {
        notify(
          tStatic("content.cannotCreateTerminal"),
          tStatic("content.noRuntimeBody")
        );
        return;
      }
      spawnAgent(st.focusedProject ?? undefined, runtime).catch(console.error);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const activeTab = tabs.find((t) => t.id === activeTabId);
  // Claude / OpenCode own an alternate-screen TUI whose complete frame is held
  // by xterm, not replayed by the PTY. Keep each opened panel mounted across
  // tab changes so returning to it cannot produce an empty canvas.
  const residentTabs = tabs.filter(
    (tab) =>
      tab.type === "agent" &&
      tab.agentId &&
      isMouseTuiRuntime(agents.get(tab.agentId)?.runtime)
  );
  const activeIsResident = residentTabs.some((tab) => tab.id === activeTab?.id);

  /** Drop a dragged tab onto a pane: middle → activate it; edge → split that
   *  pane so the dragged tab joins on that side (no-op if it's already shown). */
  const handleSplit = (targetTabId: string, draggedId: string, edge: DropEdge) => {
    if (!edge) {
      setActiveTab(draggedId);
      return;
    }
    if (draggedId === targetTabId) return; // already in this pane
    if (splitTree && splitLeafTabIds(splitTree).includes(draggedId)) return; // already visible
    const direction = edge === "left" || edge === "right" ? "row" : "column";
    const newOnFirst = edge === "left" || edge === "top";
    splitPane(targetTabId, draggedId, direction, newOnFirst);
    setActiveTab(draggedId);
  };

  const startResize = (
    e: React.MouseEvent<HTMLDivElement>,
    node: Extract<SplitNode, { kind: "split" }>
  ) => {
    e.preventDefault();
    const row = node.direction === "row";
    const container = e.currentTarget.parentElement!;
    const rect = container.getBoundingClientRect();
    const start = row ? e.clientX : e.clientY;
    const total = row ? rect.width : rect.height;
    const initial = node.ratio;
    setResizingId(node.id);
    const onMove = (ev: MouseEvent) => {
      const delta = (row ? ev.clientX : ev.clientY) - start;
      setSplitRatio(node.id, initial + delta / total);
    };
    const onUp = () => {
      setResizingId(null);
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  // Empty state (no tab to show anywhere). With no projects yet, guide the
  // user to create one first (the sidebar empty row is the same CTA); with
  // projects but no open tab, keep the Ctrl+T agent-session hint. Brand copy
  // is a brief splash: visible on entry, then fades after a few seconds so
  // the wallpaper surface stays clean.
  if (!activeTab && !splitTree) {
    return (
      <div className="content-area">
        <EmptyState
          noProjects={projects.length === 0}
          onNewProject={() => setNprojOpen(true)}
        />
      </div>
    );
  }

  // Split view: render the recursive pane tree.
  if (splitTree) {
    return (
      <div className="content-area">
        <SplitView
          node={splitTree}
          tabs={tabs}
          activeTabId={activeTabId}
          draggedTabId={draggedTabId}
          resizingId={resizingId}
          onSplit={handleSplit}
          onResizeStart={startResize}
        />
      </div>
    );
  }

  // Default single-panel view.
  return (
    <div className="content-area">
      {activeTab && (
        <DropShell targetTabId={activeTab.id} draggedTabId={draggedTabId} onSplit={handleSplit}>
          {!activeIsResident && <Panel tab={activeTab} active />}
          {activeTab.type !== "canvas" &&
            residentTabs.map((tab) => (
            <div
              key={tab.id}
              className={`resident-terminal-panel${tab.id === activeTab?.id ? " active" : " hidden"}`}
              aria-hidden={tab.id !== activeTab?.id}
            >
              <Panel tab={tab} active={tab.id === activeTab?.id} />
            </div>
          ))}
        </DropShell>
      )}
    </div>
  );
}
