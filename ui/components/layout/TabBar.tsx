import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  useStore,
  Tab,
  AgentInfo,
  HookStatus,
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
import { ExitDaemonDialog } from "./ExitDaemonDialog";
import { handleTitlebarClose } from "../../state/exitDaemon";
import { Icon, runtimeIcon } from "../Icon";
import { useT } from "../../i18n";

function projectOf(cwd: string): string {
  const m = cwd.match(/workspaces\/([^/]+)/);
  if (m) return m[1];
  const parts = cwd.split("/").filter(Boolean);
  return parts[parts.length - 1] || cwd;
}

/** Agent status keys shown in the tab bar (the runtime logo replaces the old
 *  claude/codex text). `st-<key>` classes carry the color. `dormant` is derived
 *  (no live PTY — restored after restart / sleepProject / killed); it is never
 *  persisted. Labels come from `status.*` via `t()` so they follow locale. */
const STATUS_KEYS = [
  "idle",
  "running",
  "waiting_input",
  "awaiting_choice",
  "busy",
  "done",
  "failed",
  "dormant",
] as const;

/** Runtimes whose backend adapter installs lifecycle status hooks (claude's
 *  `--settings`, codex's per-session config profile, opencode's status
 *  plugin). The tab strip polls the sidecar these hooks write; other runtimes
 *  keep PTY-activity heuristics. */
// Runtimes whose live status comes from the backend sidecar poll. dsh has no
// hook system, but the backend synthesizes an equivalent status for it from the
// session log (方案 B) — same `agent_status_read` path, same 运行中/空闲 display.
const HOOK_STATUS_RUNTIMES = new Set(["claude", "codex", "opencode", "dsh"]);

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
  if ((tab.type === "editor" || tab.type === "diff" || tab.type === "image") && tab.filePath) {
    const byRoot = projectRootOfPath(tab.filePath, projectRoots);
    if (byRoot) return byRoot;
    const dir = tab.filePath.split("/").slice(0, -1).join("/");
    return projectOf(dir || tab.filePath);
  }
  return undefined;
}

export function TabBar() {
  const t = useT();
  // Built inside the component so status labels re-render on locale change.
  const STATUS_TEXT = useMemo(
    () =>
      Object.fromEntries(
        STATUS_KEYS.map((k) => [k, t(`status.${k}`)])
      ) as Record<(typeof STATUS_KEYS)[number], string>,
    [t]
  );
  const tabs = useStore((s) => s.tabs);
  const activeTabId = useStore((s) => s.activeTabId);
  const agents = useStore((s) => s.agents);
  const agentChannels = useStore((s) => s.agentChannels);
  const agentActiveAt = useStore((s) => s.agentActiveAt);
  const hookStatus = useStore((s) => s.hookStatus);
  const agentSubmittedAt = useStore((s) => s.agentSubmittedAt);
  const unreadCompletion = useStore((s) => s.unreadCompletion);
  const projectRoots = useStore((s) => s.projectRoots);
  const focusedProject = useStore((s) => s.focusedProject);
  const draggedTabId = useStore((s) => s.draggedTabId);
  const leftSidebarOpen = useStore((s) => s.leftSidebarOpen);
  const setActiveTab = useStore((s) => s.setActiveTab);
  const setDraggedTabId = useStore((s) => s.setDraggedTabId);
  const closeTab = useStore((s) => s.closeTab);
  const dropAgentChannel = useStore((s) => s.dropAgentChannel);
  const reorderTabs = useStore((s) => s.reorderTabs);
  // Tab-label flash requests: a running → other transition bumps the seq, and
  // the effect below re-triggers the `.tab-flash` CSS animation on the tab.
  const tabFlash = useStore((s) => s.tabFlash);

  // When the left sidebar is collapsed its op-bar (and the window controls
  // it hosts) is gone. Host min/max/close on the tab bar instead so the
  // frameless window stays operable without a 44px rail strip.
  const appWindow = useMemo(() => getCurrentWindow(), []);
  const [winMaximized, setWinMaximized] = useState(false);
  const [exitDialogOpen, setExitDialogOpen] = useState(false);
  useEffect(() => {
    if (leftSidebarOpen) return;
    let alive = true;
    let unlisten: (() => void) | undefined;
    const refresh = () => {
      appWindow.isMaximized().then((m) => alive && setWinMaximized(m)).catch(() => {});
    };
    refresh();
    appWindow
      .onResized(refresh)
      .then((u) => {
        if (alive) unlisten = u;
        else u();
      })
      .catch(() => {});
    return () => {
      alive = false;
      unlisten?.();
    };
  }, [appWindow, leftSidebarOpen]);

  const onTitlebarClose = () => {
    void handleTitlebarClose(() => setExitDialogOpen(true));
  };

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

  // "关闭所有文件": close editor/image/diff tabs only — terminals stay running.
  const closeAllFiles = () => {
    visibleTabs.forEach((t) => {
      if (t.type === "editor" || t.type === "diff" || t.type === "image") closeTab(t.id);
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

  // Tab-label flash: when `tabFlash` bumps an agent's seq, re-trigger the
  // `.tab-flash` CSS animation (two pulses) on that tab's element. Imperative
  // so the flash never causes a React re-render of the strip.
  const flashSeenRef = useRef<Map<string, number>>(new Map());
  const flashTimerRef = useRef<Map<string, number>>(new Map());
  useEffect(() => {
    for (const [id, seq] of tabFlash) {
      const el = tabElsRef.current.get(id);
      if (!el) continue;
      if (flashSeenRef.current.get(id) === seq) continue;
      flashSeenRef.current.set(id, seq);
      el.classList.remove("tab-flash");
      // Forcing a reflow lets the class re-add restart the animation cleanly.
      void el.offsetWidth;
      el.classList.add("tab-flash");
      window.clearTimeout(flashTimerRef.current.get(id));
      flashTimerRef.current.set(
        id,
        window.setTimeout(() => el.classList.remove("tab-flash"), 800)
      );
    }
  }, [tabFlash]);
  useEffect(
    () => () => {
      for (const t of flashTimerRef.current.values()) window.clearTimeout(t);
    },
    []
  );

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
      data-tauri-drag-region
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
          tab.agentId ? hookStatus.get(tab.agentId) : null,
          tab.agentId ? agentSubmittedAt.get(tab.agentId) : undefined
        );
        // An idle agent whose last completed turn hasn't been viewed reads as
        // 已完成 (user sent content, agent finished, result unread); otherwise
        // plain 空闲. Only hook-reporting runtimes can detect a turn boundary.
        const hasUnread = tab.agentId
          ? unreadCompletion.has(tab.agentId)
          : false;
        const completed = status === "idle" && hasUnread;
        const statusLabel = completed
          ? t("status.completed")
          : (STATUS_TEXT[status as keyof typeof STATUS_TEXT] ?? status);
        const statusClass = completed ? "st-completed" : `st-${status}`;
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
            // Todo-tag drop target (agent tabs only): assign the task + send its
            // text to the session, then focus the tab.
            onDragOver={(e) => {
              if (e.dataTransfer.types.includes(TODO_DRAG_MIME)) {
                e.preventDefault();
                e.stopPropagation();
              }
            }}
            onDrop={(e) => {
              if (!e.dataTransfer.types.includes(TODO_DRAG_MIME)) return;
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
                    : tab.type === "image"
                      ? "image"
                      : "file-text"
                }
                size={12}
                style={{ marginRight: 5 }}
              />
              {title}
            </span>
            {tab.type === "agent" && (
              <span className={`tab-status ${statusClass}`}>{statusLabel}</span>
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
                    ? t("tabBar.closeEnded")
                    : t("tabBar.closeAndKill")
                  : t("tabBar.closeTab")
              }
            >
              ×
            </button>
          </div>
        );
      })}
      <button className="tab-add" title={t("tabBar.newTerminal")} onClick={openPicker}>
        +
      </button>
      {/* Spacer keeps window controls pinned to the trailing edge while tabs
          scroll independently under overflow. Only shown when the sidebar is
          collapsed (open sidebar already hosts these buttons in its op bar). */}
      {!leftSidebarOpen && (
        <div className="tab-win-controls">
          <span className="win-sep" aria-hidden />
          <span
            className="sidebar-btn win-btn"
            onClick={() => void appWindow.minimize()}
            title={t("tabBar.minimize")}
          >
            <Icon name="minus" size={14} />
          </span>
          <span
            className="sidebar-btn win-btn"
            onClick={() => void appWindow.toggleMaximize()}
            title={winMaximized ? t("tabBar.restore") : t("tabBar.maximize")}
          >
            <Icon name={winMaximized ? "copy" : "square"} size={13} />
          </span>
          <span
            className="sidebar-btn win-btn win-close"
            onClick={onTitlebarClose}
            title={t("common.close")}
          >
            <Icon name="x" size={14} />
          </span>
        </div>
      )}
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
              <Icon name="pencil" size={13} /> {t("tabBar.rename")}
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
            <Icon name="x" size={13} /> {t("common.close")}
          </div>
          <div
            className="ctx-item"
            onClick={() => {
              closeOtherTabs(tabMenu.tabId);
              setTabMenu(null);
            }}
          >
            <Icon name="x" size={13} /> {t("tabBar.closeOther")}
          </div>
          <div
            className="ctx-item"
            onClick={() => {
              closeAllFiles();
              setTabMenu(null);
            }}
          >
            <Icon name="file-text" size={13} /> {t("tabBar.closeAllFiles")}
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
      {exitDialogOpen && (
        <ExitDaemonDialog onCancel={() => setExitDialogOpen(false)} />
      )}
    </div>
  );
}
