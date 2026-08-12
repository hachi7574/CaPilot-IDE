import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  useStore,
  Tab,
  AgentInfo,
  HookStatus,
  effectiveAgentStatus,
  ACTIVE_WINDOW_MS,
} from "../../state/store";
import { closeAgent as closeAgentAction } from "../../state/agentActions";
import { TerminalTemplatePicker } from "./TerminalTemplatePicker";
import { RenameAgentModal } from "./RenameAgentModal";
import { Icon, runtimeIcon } from "../Icon";

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
  busy: "运行中",
  done: "待验收",
  failed: "异常",
  dormant: "休眠中",
} as const;

/** Runtimes whose backend adapter installs lifecycle status hooks (claude's
 *  `--settings`, codex's per-session config profile, opencode's status
 *  plugin). The tab strip polls the sidecar these hooks write; other runtimes
 *  keep PTY-activity heuristics. */
const HOOK_STATUS_RUNTIMES = new Set(["claude", "codex", "opencode"]);

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
  const hookStatus = useStore((s) => s.hookStatus);
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
  const dragWRef = useRef(0);
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
  // terminated via closeAgentAction; ended sessions stay recoverable.
  const closeTabById = (id: string) => {
    const tab = tabs.find((t) => t.id === id);
    if (!tab) return;
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

  // hook-status poll: the backend sidecar (`~/CaPilot/status/<id>.json`,
  // written by lifecycle hooks — claude's `--settings`, codex's per-session
  // config profile) is the authoritative 运行中/空闲 source. Poll once a second
  // for connected hook-enabled agents. The store's setter is
  // reference-preserving, so an unchanged status doesn't re-render the strip —
  // and a removed agent is skipped (no stale re-add).
  useEffect(() => {
    const t = setInterval(() => {
      const s = useStore.getState();
      const targets: string[] = [];
      for (const [id, agent] of s.agents) {
        if (HOOK_STATUS_RUNTIMES.has(agent.runtime) && s.agentChannels.has(id)) {
          targets.push(id);
        }
      }
      if (targets.length === 0) return;
      Promise.allSettled(
        targets.map((id) => invoke<HookStatus | null>("agent_status_read", { id }))
      ).then((results) => {
        const st = useStore.getState();
        targets.forEach((id, i) => {
          const r = results[i];
          if (r.status === "fulfilled" && st.agents.has(id)) {
            st.setHookStatus(id, r.value);
          }
        });
      });
    }, 1000);
    return () => clearInterval(t);
  }, []);

  // New-terminal template picker (the "+" button), anchored at the button.
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
    // same (content) coordinate space the transforms use.
    let acc = barRect.left - bar.scrollLeft + 6;
    // Natural slot midpoints of the non-dragged tabs.
    const slots: { tabIndex: number; mid: number }[] = [];
    for (const tv of visibleTabs) {
      if (tv.id === draggedTabId) continue;
      const w = tabElsRef.current.get(tv.id)?.offsetWidth ?? 120;
      slots.push({ tabIndex: tabs.indexOf(tv), mid: acc + w / 2 });
      acc += w + TAB_GAP;
    }

    // Candidate slot: the first whose midpoint is right of the cursor, else
    // the last slot (drop after everything). The dragged tab's own index is a
    // no-op (drop in place).
    let cand = slots.findIndex((s) => e.clientX < s.mid);
    if (cand === -1) cand = slots.length - 1;
    const candidate = slots[cand].tabIndex;
    const next = candidate === fromIndex ? null : candidate;

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
        const status = effectiveAgentStatus(
          agent,
          connected,
          active,
          tab.agentId ? hookStatus.get(tab.agentId) : null
        );
        // Agent records are the live source of truth for terminal names. A
        // tab's title is only the snapshot taken when it was opened, so it can
        // become stale after restoring/resuming a session (or any runtime
        // update that replaces AgentInfo). The sidebar already renders from
        // `agents`; doing the same here keeps both labels in lockstep.
        const title = tab.type === "agent" ? agent?.title || tab.title : tab.title;
        const isDragging = draggedTabId === tab.id;
        const fromIndex = draggedTabId
          ? tabs.findIndex((t) => t.id === draggedTabId)
          : -1;
        const idx = tabs.indexOf(tab);
        // Live position during a drag: the dragged tab's slot rides the hover
        // index while siblings slide out of its path (empty-space = original
        // slot, so dropping there is a no-op).
        let dx = 0;
        if (draggedTabId && dragTargetIdx !== null && fromIndex >= 0) {
          const step = dragWRef.current + TAB_GAP;
          if (idx === fromIndex) dx = (dragTargetIdx - fromIndex) * step;
          else if (dragTargetIdx > fromIndex && idx > fromIndex && idx <= dragTargetIdx)
            dx = -step;
          else if (dragTargetIdx < fromIndex && idx >= dragTargetIdx && idx < fromIndex)
            dx = step;
        }
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
              dragWRef.current = (e.currentTarget as HTMLElement).offsetWidth;
              lastSwitchXRef.current = 0;
              setDragHidden(false);
              setDraggedTabId(tab.id);
            }}
            onDragEnd={endTabDrag}
            onClick={() => setActiveTab(tab.id)}
            onContextMenu={(e) => {
              e.preventDefault();
              e.stopPropagation();
              setTabMenu({ x: e.clientX, y: e.clientY, tabId: tab.id });
            }}
          >
            <span>
              <Icon
                name={tab.type === "agent" ? runtimeIcon(agent?.runtime ?? "") : "file-text"}
                size={12}
                style={{ marginRight: 5 }}
              />
              {title}
            </span>
            {tab.type === "agent" && (
              <span className={`tab-status st-${status}`}>
                {STATUS_TEXT[status as keyof typeof STATUS_TEXT] ?? status}
              </span>
            )}
            <button
              className="tab-close"
              onClick={(e) => {
                e.stopPropagation();
                closeTabById(tab.id);
              }}
              title={
                tab.type === "agent" && tab.agentId
                  ? agent?.status === "done"
                    ? "关闭（已结束，可从侧栏找回）"
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
