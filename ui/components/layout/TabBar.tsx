import { useEffect, useRef, useState } from "react";
import {
  useStore,
  Tab,
  AgentInfo,
  effectiveAgentStatus,
  ACTIVE_WINDOW_MS,
  TODO_DRAG_MIME,
} from "../../state/store";
import {
  closeAgent as closeAgentAction,
  assignTodoAndSend,
} from "../../state/agentActions";
import { TerminalTemplatePicker } from "./TerminalTemplatePicker";
import { RenameAgentModal } from "./RenameAgentModal";
import { Icon, runtimeIcon } from "../Icon";
import {
  useStructuredStore,
  closeStructuredAgent,
  AGENT_STATUS_TEXT,
} from "../../state/structuredAgent";

function projectOf(cwd: string): string {
  const m = cwd.match(/workspaces\/([^/]+)/);
  if (m) return m[1];
  const parts = cwd.split("/").filter(Boolean);
  return parts[parts.length - 1] || cwd;
}

/** Agent status → colored text shown in the tab bar (the runtime logo replaces
 *  the old claude/codex text). `st-<key>` classes carry the color. `dormant`
 *  is derived (no live PTY — restored after restart / sleepProject / killed);
 *  it is never persisted. */
const STATUS_TEXT = {
  idle: "空闲",
  running: "运行中",
  waiting_input: "待确认",
  awaiting_choice: "待选择",
  busy: "运行中",
  done: "待处理",
  failed: "异常",
  dormant: "休眠中",
} as const;

/** Structured Agent Runtime statuses → the existing tab-strip color palette
 *  (the canonical `AgentStatus` set has no direct 1:1 for every PTY label). */
const STRUCTURED_STATUS_CLASS: Record<string, string> = {
  initializing: "st-running",
  idle: "st-idle",
  running: "st-running",
  waiting_permission: "st-waiting_input",
  waiting_input: "st-waiting_input",
  error: "st-failed",
  closed: "st-dormant",
};

/** Runtimes whose sessions are legacy PTY Agent sessions (EOL, read-only in
 *  Phase 5). Their tab strip status falls back to PTY-activity heuristics like
 *  bash — the lifecycle-hook sidecar was retired. */
const LEGACY_AGENT_RUNTIMES = new Set(["claude", "codex", "opencode"]);

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
 *  - agent tab → the agent's cwd via `projectOf`
 *  - editor tab → longest matching `projectRoots` prefix, else `projectOf` on
 *    the file path's dirname */
function tabProject(
  tab: Tab,
  agents: Map<string, AgentInfo>,
  projectRoots: Record<string, string>
): string | undefined {
  if (tab.type === "agent") {
    if (tab.agentId) {
      const agent = agents.get(tab.agentId);
      if (agent?.workspace_id && agent.project) return agent.project;
      if (agent?.cwd) return projectOf(agent.cwd);
    }
    return undefined;
  }
  if (tab.type === "structured" && tab.agentId) {
    const view = useStructuredStore.getState().agents.get(tab.agentId);
    if (view?.snapshot.agent.cwd) return projectOf(view.snapshot.agent.cwd);
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
  const agentChannels = useStore((s) => s.agentChannels);
  const agentActiveAt = useStore((s) => s.agentActiveAt);
  const projectRoots = useStore((s) => s.projectRoots);
  const focusedProject = useStore((s) => s.focusedProject);
  const draggedTabId = useStore((s) => s.draggedTabId);
  const setActiveTab = useStore((s) => s.setActiveTab);
  const setDraggedTabId = useStore((s) => s.setDraggedTabId);
  const closeTab = useStore((s) => s.closeTab);
  const dropAgentChannel = useStore((s) => s.dropAgentChannel);
  const reorderTabs = useStore((s) => s.reorderTabs);

  // Drag-reorder state. During a drag we DON'T mutate the store's tabs array
  // (that would re-render ContentArea and its live terminals on every move);
  // instead siblings are shifted with CSS transforms toward the hover slot and
  // `reorderTabs` commits only on drop.
  const [dragTargetIdx, setDragTargetIdx] = useState<number | null>(null);
  const [dragHidden, setDragHidden] = useState(false);
  const tabElsRef = useRef<Map<string, HTMLDivElement | null>>(new Map());
  const lastSwitchXRef = useRef(0);
  const TAB_GAP = 2;
  // The cursor must travel this far past a slot boundary before the target
  // switches, so sub-pixel jitter at a midpoint can't flip-flop the strip.
  const DRAG_DEAD_ZONE = 6;

  // Right-click tab context menu state.
  const [tabMenu, setTabMenu] = useState<{
    x: number;
    y: number;
    tabId: string;
  } | null>(null);
  // The tab being renamed via the context menu (agent tabs only).
  const [renameTabId, setRenameTabId] = useState<string | null>(null);

  // Close the tab context menu on outside click / Escape (same discipline as
  // the sidebar context menu).
  useEffect(() => {
    if (!tabMenu) return;
    const close = () => setTabMenu(null);
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && close();
    window.addEventListener("click", close);
    window.addEventListener("contextmenu", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("contextmenu", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [tabMenu]);

  // Close a single tab exactly like the × button: active/restored sessions get
  // terminated via closeAgentAction; ended sessions stay recoverable. Structured
  // agent tabs close the backend ACP session and drop the view.
  const closeTabById = (id: string) => {
    const tab = tabs.find((t) => t.id === id);
    if (!tab) return;
    if (tab.type === "structured" && tab.agentId) {
      closeStructuredAgent(tab.agentId).catch(() => {});
      return;
    }
    const agent = tab.agentId ? agents.get(tab.agentId) : undefined;
    if (tab.type === "agent" && tab.agentId) {
      if (agent?.status === "done") {
        closeTab(tab.id);
        dropAgentChannel(tab.agentId);
      } else {
        closeAgentAction(tab.agentId);
      }
    } else {
      closeTab(tab.id);
    }
  };

  // "关闭其它": close every *visible* tab except the one right-clicked.
  const closeOtherTabs = (keepId: string) => {
    visibleTabs.forEach((t) => {
      if (t.id !== keepId) closeTabById(t.id);
    });
    setActiveTab(keepId);
  };

  // "关闭所有文件": close editor/diff tabs only — terminals stay running.
  const closeAllFiles = () => {
    visibleTabs.forEach((t) => {
      if (t.type === "editor" || t.type === "diff") closeTab(t.id);
    });
  };

  // Project-scoped view: when a project is focused, show only its tabs. Tabs
  // whose project can't be determined (e.g. mid-spawn) stay visible, and tabs
  // of other projects remain in the store — hidden, NOT closed.
  const visibleTabs = focusedProject
    ? tabs.filter((t) => {
        const tp = tabProject(t, agents, projectRoots);
        return tp === undefined || tp === focusedProject;
      })
    : tabs;

  // The 运行中 → 空闲 flip is time-driven: once the activity window lapses, a
  // connected-but-quiet session reads as idle. Re-render on a 1s tick only
  // while some visible tab is still inside the window (an idle strip doesn't
  // tick).
  const hasActive = visibleTabs.some(
    (t) =>
      t.agentId != null &&
      Date.now() - (agentActiveAt.get(t.agentId) ?? 0) < ACTIVE_WINDOW_MS
  );
  const [, setActivityTick] = useState(0);
  useEffect(() => {
    if (!hasActive) return;
    const t = setInterval(() => setActivityTick((v) => v + 1), 1000);
    return () => clearInterval(t);
  }, [hasActive]);

  // Phase 5: the hook-status sidecar poll was retired. The tab strip derives
  // 运行中/空闲 purely from connectivity + recency (`effectiveAgentStatus`);
  // legacy PTY agent sessions (claude/codex/opencode) are read-only EOL and
  // structured agents report status through the daemon's AgentManager.

  const [termPicker, setTermPicker] = useState<{
    x: number;
    y: number;
    project: string;
  } | null>(null);
  const openPicker = (e: React.MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const activeTab = tabs.find((tab) => tab.id === activeTabId);
    const activeProject = activeTab
      ? tabProject(activeTab, agents, projectRoots)
      : undefined;
    // The focused project defines the currently visible tab set. Prefer the
    // active tab when it belongs to that set (especially editor tabs), then
    // fall back to the focused project or the default workspace.
    const project =
      activeProject && (!focusedProject || activeProject === focusedProject)
        ? activeProject
        : focusedProject ?? "default";
    setTermPicker({ x: r.right, y: r.bottom, project });
  };

  // While dragging over the strip, resolve the hover slot from the natural
  // positions of the non-dragged tabs (the dragged tab's slot collapses, which
  // is exactly what the CSS shifts later reproduce — no feedback loop).
  const handleTabBarDragOver = (e: React.DragEvent) => {
    e.preventDefault();
    // A todo-tag drag is not a tab reorder — tag drags never set draggedTabId,
    // but keep the strip from rendering a reorder preview just in case.
    if (e.dataTransfer.types.includes(TODO_DRAG_MIME)) return;
    if (!draggedTabId) return;
    // The first move past dragstart hides the source tab (its drag image was
    // already snapshotted), leaving only the ghost + the drop-slot gap.
    if (!dragHidden) setDragHidden(true);
    const fromIndex = tabs.findIndex((t) => t.id === draggedTabId);
    if (fromIndex < 0) return;
    if (visibleTabs.length <= 1) {
      if (dragTargetIdx !== null) setDragTargetIdx(null);
      return;
    }
    const bar = e.currentTarget as HTMLElement;
    const barRect = bar.getBoundingClientRect();
    // `.tab-bar` has `padding: 0 6px`, so neighbours start 6px in. The bar
    // content is scrolled by `scrollLeft`, so subtract it to hit-test in the
    // same (content) coordinate space the preview transforms use.
    const originX = barRect.left - bar.scrollLeft + 6;
    const widthOf = (id: string) => tabElsRef.current.get(id)?.offsetWidth ?? 120;
    // Resting left edge of every rendered tab. Transforms don't affect
    // offsetWidth, so this is the geometry the strip settles back to — it must
    // include the dragged tab's own slot (packing only the non-dragged tabs
    // from the left edge shifts every boundary left by the dragged tab's
    // width, which made the drop slot open too early).
    const leftById = new Map<string, number>();
    {
      let acc = 0;
      for (const tv of visibleTabs) {
        leftById.set(tv.id, acc);
        acc += widthOf(tv.id) + TAB_GAP;
      }
    }
    // Midpoints of the non-dragged tabs' resting slots.
    const rest: { id: string; mid: number }[] = [];
    for (const tv of visibleTabs) {
      if (tv.id === draggedTabId) continue;
      rest.push({
        id: tv.id,
        mid: originX + (leftById.get(tv.id) ?? 0) + widthOf(tv.id) / 2,
      });
    }

    // The drop index (reorderTabs `to`) equals how many of those midpoints sit
    // left of the cursor: hover over b's right half / c's left half to put a
    // between b and c, over the source slot to leave it in place.
    let vis = rest.findIndex((r) => e.clientX < r.mid);
    if (vis === -1) vis = rest.length;

    // Map the visible insertion point back to a full-tabs index for reorderTabs
    // (needed when a focused project hides some tabs): insert just before the
    // full-tabs position of the tab now at the insertion point, or after the
    // last rendered tab when the cursor is past every midpoint.
    const atId = vis < rest.length ? rest[vis].id : rest[rest.length - 1].id;
    const atFull = tabs.findIndex((t) => t.id === atId);
    let to: number;
    if (vis < rest.length) {
      to = atFull > fromIndex ? atFull - 1 : atFull;
    } else {
      to = atFull >= fromIndex ? atFull : atFull + 1;
    }
    const next = to === fromIndex ? null : to;

    // Distance hysteresis: once the target changes, don't change it again until
    // the cursor has moved DRAG_DEAD_ZONE px — sub-pixel jitter at a boundary
    // can't make the strip oscillate between two slots.
    if (next !== dragTargetIdx) {
      if (
        dragTargetIdx === null ||
        Math.abs(e.clientX - lastSwitchXRef.current) >= DRAG_DEAD_ZONE
      ) {
        setDragTargetIdx(next);
        lastSwitchXRef.current = e.clientX;
      }
    }
  };

  const handleTabBarDragLeave = (e: React.DragEvent) => {
    // Ignore transitions between child tabs; only clear when leaving the bar.
    if ((e.currentTarget as Node).contains(e.relatedTarget as Node)) return;
    setDragTargetIdx(null);
  };

  const handleTabBarDrop = (e: React.DragEvent) => e.preventDefault();

  // On release the in-strip position is the drop slot; dropping elsewhere
  // (content area → split) left dragTargetIdx cleared, so nothing reorders.
  const endTabDrag = () => {
    if (dragTargetIdx !== null) {
      const fromIndex = tabs.findIndex((t) => t.id === draggedTabId);
      if (fromIndex >= 0 && fromIndex !== dragTargetIdx) {
        reorderTabs(fromIndex, dragTargetIdx);
      }
    }
    setDragHidden(false);
    setDraggedTabId(null);
    setDragTargetIdx(null);
    tabElsRef.current.clear();
  };

  // Preview positions during a drag. Each tab's shift is its exact post-drop
  // flex position minus its resting position, so the preview (and the empty gap
  // that trails the hidden dragged tab) always lines up — even when tab widths
  // differ, which a single `step`-per-slot constant would not.
  const dxById = new Map<string, number>();
  if (draggedTabId && dragTargetIdx !== null) {
    // Positions are relative to the rendered strip (`visibleTabs`), not the
    // full store array, since hidden tabs aren't in the flex layout. We
    // simulate the exact commit on the full array (mirroring reorderTabs) and
    // filter back to the strip, so the preview tracks the commit even when a
    // focused project hides some tabs.
    const order = visibleTabs.map((t) => t.id);
    const fromIndex = order.indexOf(draggedTabId);
    const full = tabs.map((t) => t.id);
    const fullFrom = full.indexOf(draggedTabId);
    if (fromIndex >= 0 && fullFrom >= 0) {
      const [moved] = full.splice(fullFrom, 1);
      full.splice(dragTargetIdx, 0, moved);
      const visibleIds = new Set(order);
      const final = full.filter((id) => visibleIds.has(id));
      if (final.indexOf(draggedTabId) !== fromIndex) {
        const widthOf = (id: string) => tabElsRef.current.get(id)?.offsetWidth ?? 120;
        const origLeft = new Map<string, number>();
        const finalLeft = new Map<string, number>();
        for (let acc = 0, i = 0; i < order.length; i++) {
          origLeft.set(order[i], acc);
          acc += widthOf(order[i]) + TAB_GAP;
        }
        for (let acc = 0, i = 0; i < final.length; i++) {
          finalLeft.set(final[i], acc);
          acc += widthOf(final[i]) + TAB_GAP;
        }
        for (const id of order) {
          const base = origLeft.get(id) ?? 0;
          dxById.set(id, (finalLeft.get(id) ?? base) - base);
        }
      }
    }
  }

  // The tab the context menu is open on (for the rename entry, which only shows
  // for agent tabs — editor/diff titles come from their file paths).
  const tabMenuTab = tabMenu
    ? tabs.find((t) => t.id === tabMenu.tabId)
    : undefined;

  return (
    <div
      className={`tab-bar${draggedTabId ? " tab-drag-active" : ""}`}
      onDragOver={handleTabBarDragOver}
      onDrop={handleTabBarDrop}
      onDragLeave={handleTabBarDragLeave}
      onWheel={(e) => {
        // Vertical wheel rotates the tab strip horizontally when it overflows;
        // horizontal (shift+wheel) delta passes through untouched.
        const el = e.currentTarget;
        if (el.scrollWidth <= el.clientWidth) return;
        const delta = e.deltaY !== 0 ? e.deltaY : e.deltaX;
        if (delta === 0) return;
        const maxScroll = el.scrollWidth - el.clientWidth;
        const next = Math.max(0, Math.min(el.scrollLeft + delta, maxScroll));
        if (next !== el.scrollLeft) {
          el.scrollLeft = next;
          e.preventDefault();
        }
      }}
    >
      {visibleTabs.map((tab) => {
        const agent = tab.agentId ? agents.get(tab.agentId) : undefined;
        // Structured agent tabs derive status from the structured store's
        // snapshot (the PTY agent maps above are empty for them).
        const structured =
          tab.type === "structured" && tab.agentId
            ? useStructuredStore.getState().agents.get(tab.agentId)
            : undefined;
        const structuredStatus = structured?.snapshot.agent.status;
        // A PTY channel is attached on spawn/resume and dropped when a session
        // is killed/removed — it is the "connected" signal. Restored sessions
        // have no channel yet, so they must not display as 运行中.
        const connected = tab.agentId ? agentChannels.has(tab.agentId) : false;
        // Connected-but-quiet sessions read as 空闲; recent output/input (a task
        // in flight) reads as 运行中. `agentActiveAt` is throttled so streaming
        // doesn't re-render the strip per chunk.
        const active = tab.agentId
          ? Date.now() - (agentActiveAt.get(tab.agentId) ?? 0) < ACTIVE_WINDOW_MS
          : false;
        const status = structuredStatus
          ? structuredStatus
          : effectiveAgentStatus(agent, connected, active);
        const statusLabel = structuredStatus
          ? AGENT_STATUS_TEXT[structuredStatus]
          : (STATUS_TEXT[status as keyof typeof STATUS_TEXT] ?? status);
        // Structured statuses map onto the existing tab-strip color palette.
        const statusClass = structuredStatus
          ? (STRUCTURED_STATUS_CLASS[structuredStatus] ?? "st-idle")
          : `st-${status}`;
        // Agent records are the live source of truth for terminal names. A
        // tab's title is only the snapshot taken when it was opened, so it can
        // become stale after restoring/resuming a session (or any runtime
        // update that replaces AgentInfo). The sidebar already renders from
        // `agents`; doing the same here keeps both labels in lockstep.
        const title = tab.type === "agent" ? agent?.title || tab.title : tab.title;
        const isDragging = draggedTabId === tab.id;
        // Live position during a drag: the dragged tab's slot rides the hover
        // index while siblings slide out of its path (empty-space = original
        // slot, so dropping there is a no-op). `dxById` is keyed off the exact
        // post-drop layout, so widths are naturally accounted for.
        const dx = dxById.get(tab.id) ?? 0;
        return (
          <div
            key={tab.id}
            ref={(el) => {
              if (el) tabElsRef.current.set(tab.id, el);
              else tabElsRef.current.delete(tab.id);
            }}
            className={`tab-item${tab.id === activeTabId ? " active" : ""}${isDragging ? " dragging" : ""}${isDragging && dragHidden ? " drag-hidden" : ""}${status === "done" ? " tab-done" : ""}`}
            style={dx ? { transform: `translateX(${dx}px)` } : undefined}
            draggable
            onDragStart={(e) => {
              e.dataTransfer.setData("text/plain", tab.id);
              e.dataTransfer.effectAllowed = "copy";
              lastSwitchXRef.current = 0;
              setDragHidden(false);
              setDraggedTabId(tab.id);
            }}
            onDragEnd={endTabDrag}
            // Todo-tag drop target (PTY agent tabs + structured agent tabs):
            // assign the task, send its text to the session (legacy PTY agent
            // sessions are read-only EOL — the assign still lands, the prompt
            // injection is refused), then focus the tab.
            onDragOver={(e) => {
              if (
                (tab.type === "agent" || tab.type === "structured") &&
                e.dataTransfer.types.includes(TODO_DRAG_MIME)
              ) {
                e.preventDefault();
                e.stopPropagation();
              }
            }}
            onDrop={(e) => {
              if (
                (tab.type !== "agent" && tab.type !== "structured") ||
                !e.dataTransfer.types.includes(TODO_DRAG_MIME)
              )
                return;
              e.preventDefault();
              e.stopPropagation();
              if (!tab.agentId) return;
              const tagId = e.dataTransfer.getData(TODO_DRAG_MIME);
              if (tagId) void assignTodoAndSend(tagId, tab.agentId);
              setActiveTab(tab.id);
            }}
            onClick={() => setActiveTab(tab.id)}
            onContextMenu={(e) => {
              e.preventDefault();
              e.stopPropagation();
              setTabMenu({ x: e.clientX, y: e.clientY, tabId: tab.id });
            }}
          >
            <span>
              <Icon
                name={
                  tab.type === "agent"
                    ? runtimeIcon(agent?.runtime ?? "")
                    : tab.type === "structured"
                      ? runtimeIcon(structured?.snapshot.agent.provider_id ?? "")
                      : "file-text"
                }
                size={12}
                style={{ marginRight: 5 }}
              />
              {title}
            </span>
            {(tab.type === "agent" || tab.type === "structured") && (
              <span className={`tab-status ${statusClass}`}>{statusLabel}</span>
            )}
            <button
              className="tab-close"
              onClick={(e) => {
                e.stopPropagation();
                closeTabById(tab.id);
              }}
              title={
                tab.type === "structured"
                  ? "关闭 Agent"
                  : tab.type === "agent" && tab.agentId
                    ? agent?.status === "done"
                      ? "关闭（已结束，可从侧栏找回）"
                      : LEGACY_AGENT_RUNTIMES.has(agent?.runtime ?? "")
                        ? "关闭（旧版 PTY Agent，EOL 只读）"
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
      {tabMenu && (
        <div
          className="ctx-menu"
          style={{ position: "fixed", left: tabMenu.x, top: tabMenu.y, zIndex: 1000 }}
          onClick={(e) => e.stopPropagation()}
          onContextMenu={(e) => e.stopPropagation()}
        >
          {tabMenuTab?.type === "agent" && (
            <div
              className="ctx-item"
              onClick={() => {
                setRenameTabId(tabMenu.tabId);
                setTabMenu(null);
              }}
            >
              <Icon name="pencil" size={13} /> 重命名
            </div>
          )}
          {tabMenuTab?.type === "agent" && <div className="ctx-sep" />}
          <div
            className="ctx-item"
            onClick={() => {
              closeTabById(tabMenu.tabId);
              setTabMenu(null);
            }}
          >
            <Icon name="x" size={13} /> 关闭
          </div>
          <div
            className="ctx-item"
            onClick={() => {
              closeOtherTabs(tabMenu.tabId);
              setTabMenu(null);
            }}
          >
            <Icon name="x" size={13} /> 关闭其它
          </div>
          <div
            className="ctx-item"
            onClick={() => {
              closeAllFiles();
              setTabMenu(null);
            }}
          >
            <Icon name="file-text" size={13} /> 关闭所有文件
          </div>
        </div>
      )}
      {termPicker && (
        <TerminalTemplatePicker
          project={termPicker.project}
          anchor={termPicker}
          onClose={() => setTermPicker(null)}
        />
      )}
      {renameTabId && (
        <RenameAgentModal
          agentId={renameTabId}
          initial={agents.get(renameTabId)?.title ?? ""}
          onClose={() => setRenameTabId(null)}
        />
      )}
    </div>
  );
}
