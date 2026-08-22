import { useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  useStore,
  TodoTag,
  beginTodoDrag,
  endTodoDrag,
  splitLeafTabIds,
} from "../../state/store";
import { assignTodoAndSend } from "../../state/agentActions";
import { Icon } from "../Icon";
import { useT } from "../../i18n";

/** Backend settings KV key holding the persisted todo list (JSON array). */
const TODOS_KEY = "todos";

/** Movement (px) before a press becomes a pointer-drag instead of a click. */
const POINTER_DRAG_THRESHOLD = 5;

/** Same grouping as the left-sidebar tree: persisted project, else cwd. */
function agentOwningProject(
  agent: { workspace_id?: string | null; project?: string; cwd: string },
  roots: Record<string, string>
): string {
  if (agent.workspace_id && agent.project) return agent.project;
  const m = agent.cwd.match(/workspaces\/([^/]+)/);
  if (m) return m[1]!;
  for (const [name, root] of Object.entries(roots)) {
    if (!root) continue;
    const base = root.endsWith("/") ? root : `${root}/`;
    if (agent.cwd === root || agent.cwd.startsWith(base)) return name;
  }
  const parts = agent.cwd.split("/").filter(Boolean);
  return parts[parts.length - 1] || agent.cwd;
}

interface TodoSendSession {
  agentId: string;
  title: string;
}

/** Hover-revealed ⋯ on the left of a 待分配 tag: clone, or send to composer /
 *  the unique visible terminal / a session in the current overview scope. */
function TodoTagActions({
  uniqueVisible,
  sessions,
  onClone,
  onSendComposer,
  onSendAgent,
}: {
  uniqueVisible: TodoSendSession | null;
  sessions: TodoSendSession[];
  onClone: () => void;
  onSendComposer: () => void;
  onSendAgent: (agentId: string) => void;
}) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLSpanElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (wrapRef.current?.contains(e.target as Node)) return;
      setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const close = () => setOpen(false);

  return (
    <span ref={wrapRef} className={`todo-more${open ? " open" : ""}`}>
      <button
        type="button"
        className="todo-more-btn"
        title={t("todo.moreActions")}
        onPointerDown={(e) => {
          e.stopPropagation();
          e.preventDefault();
        }}
        onClick={(e) => {
          e.stopPropagation();
          setOpen((v) => !v);
        }}
      >
        <Icon name="ellipsis" size={12} />
      </button>
      {open && (
        <div className="todo-more-menu" role="menu">
          <div
            className="todo-more-item"
            role="menuitem"
            onClick={() => {
              onClone();
              close();
            }}
          >
            <Icon name="copy" size={12} /> {t("todo.clone")}
          </div>
          <div className="todo-more-sep" />
          <div className="todo-more-label">{t("todo.sendTo")}</div>
          <div
            className="todo-more-item"
            role="menuitem"
            onClick={() => {
              onSendComposer();
              close();
            }}
          >
            <Icon name="message-square" size={12} /> {t("todo.sendToComposer")}
          </div>
          {uniqueVisible && (
            <div
              className="todo-more-item"
              role="menuitem"
              onClick={() => {
                onSendAgent(uniqueVisible.agentId);
                close();
              }}
            >
              <Icon name="square-terminal" size={12} /> {t("todo.sendToCurrent")}
            </div>
          )}
          {sessions.map((session) => (
            <div
              key={session.agentId}
              className="todo-more-item"
              role="menuitem"
              onClick={() => {
                onSendAgent(session.agentId);
                close();
              }}
            >
              <Icon name="monitor" size={12} /> {session.title}
            </div>
          ))}
        </div>
      )}
    </span>
  );
}

/* ── Collapsible Section ─────────────────────────────────────────
 * Moved here from RightSidebar so TodoPanel owns its only remaining consumer
 * (the old OverviewDashboard that shared it was replaced). Headers use the
 * left-sidebar project-row look: a left collapse triangle (chevron-down,
 * rotated to chevron-right when collapsed) + title, with an optional
 * hover-revealed action on the right. */

function CollapsibleSection({
  title,
  children,
  headerAction,
}: {
  title: React.ReactNode;
  children: React.ReactNode;
  /** Node rendered at the far right of the header, shown on hover/focus. */
  headerAction?: React.ReactNode;
}) {
  const [collapsed, setCollapsed] = useState(false);

  return (
    <div className={`ov-section uk-section${collapsed ? " collapsed" : ""}`}>
      <div
        className="ov-section-header ov-arrowed"
        onClick={() => setCollapsed(!collapsed)}
      >
        <span className="ov-arrow" aria-hidden>
          <Icon name="chevron-down" size={10} />
        </span>
        <span className="ov-title">{title}</span>
        {headerAction && (
          <span
            className="ov-header-action"
            onClick={(e) => {
              e.stopPropagation();
              // An action opened from a collapsed header reveals body content,
              // so always expand the section first.
              setCollapsed(false);
            }}
          >
            {headerAction}
          </span>
        )}
      </div>
      {!collapsed && <div className="ov-section-body">{children}</div>}
    </div>
  );
}

/** Inline rename input (double-click a tag to edit). Enter commits, Esc/blur
 *  cancels an empty value. */
function TodoEdit({
  initial,
  onSubmit,
  onCancel,
}: {
  initial: string;
  onSubmit: (text: string) => void;
  onCancel: () => void;
}) {
  const [value, setValue] = useState(initial);
  const commit = () => {
    if (value.trim()) onSubmit(value);
    else onCancel();
  };
  return (
    <input
      className="todo-edit"
      autoFocus
      value={value}
      onChange={(e) => setValue(e.target.value)}
      onFocus={(e) => e.target.select()}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          commit();
        } else if (e.key === "Escape") {
          e.preventDefault();
          onCancel();
        }
      }}
      onBlur={commit}
    />
  );
}

/** Inline "add" input revealed by the 待分配 header "+". Enter commits, Esc
 *  cancels, blur commits non-empty text / cancels an empty one (same semantics
 *  as TodoEdit). The empty-list variant (`row`) renders as the original
 *  `.todo-add-row`: leading plus icon, borderless input, dashed rule below —
 *  and stays mounted without stealing focus (`autoFocus={false}`). */
function TodoAdd({
  onSubmit,
  onCancel,
  autoFocus = true,
  row = false,
  inputRef,
}: {
  onSubmit: (text: string) => void;
  onCancel: () => void;
  autoFocus?: boolean;
  /** Render as the original add-row (plus icon + borderless input). */
  row?: boolean;
  inputRef?: React.Ref<HTMLInputElement>;
}) {
  const t = useT();
  const [value, setValue] = useState("");
  const commit = () => {
    if (value.trim()) onSubmit(value);
    else onCancel();
  };
  return (
    <div className={row ? "todo-add-row" : undefined}>
      {row && <Icon name="plus" size={12} />}
      <input
        ref={inputRef}
        className={row ? undefined : "todo-edit"}
        autoFocus={autoFocus}
        value={value}
        placeholder={t("todo.placeholder")}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            commit();
          } else if (e.key === "Escape") {
            e.preventDefault();
            onCancel();
          }
        }}
        onBlur={commit}
      />
    </div>
  );
}

export function TodoPanel() {
  const t = useT();
  const todos = useStore((s) => s.todos);
  const todoScope = useStore((s) => s.todoScope);
  const focusedProject = useStore((s) => s.focusedProject);
  const sessionsRestored = useStore((s) => s.sessionsRestored);
  const hookStatus = useStore((s) => s.hookStatus);
  const addTodo = useStore((s) => s.addTodo);
  const cloneTodo = useStore((s) => s.cloneTodo);
  const updateTodoText = useStore((s) => s.updateTodoText);
  const deleteTodo = useStore((s) => s.deleteTodo);
  const agents = useStore((s) => s.agents);
  const tabs = useStore((s) => s.tabs);
  const splitTree = useStore((s) => s.splitTree);
  const activeTabId = useStore((s) => s.activeTabId);
  const projectRoots = useStore((s) => s.projectRoots);

  // Local UI state.
  const [hydrated, setHydrated] = useState(false);
  const [adding, setAdding] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [dragId, setDragId] = useState<string | null>(null);
  // Floating ghost for pointer-based tag drag (avoids broken HTML5 DnD on WebView2).
  const [ghost, setGhost] = useState<{
    id: string;
    text: string;
    x: number;
    y: number;
  } | null>(null);
  // The empty-list add row; the header "+" focuses it instead of stacking a
  // second input when no tags exist.
  const addRowRef = useRef<HTMLInputElement>(null);
  const pointerDragRef = useRef<{
    id: string;
    text: string;
    startX: number;
    startY: number;
    active: boolean;
    pointerId: number;
  } | null>(null);

  // Scope predicate: global view = tags with no project; project view = tags of
  // the focused project.
  const inScope = useMemo(
    () => (t: TodoTag) =>
      todoScope === "global" ? !t.project : t.project === focusedProject,
    [todoScope, focusedProject]
  );
  const todoTags = useMemo(
    // Newest first: creation order is preserved in the array, so reverse it —
    // the latest tag the user added should sit at the top of 待分配.
    () => todos.filter((t) => t.status === "todo" && inScope(t)).reverse(),
    [todos, inScope]
  );
  const doneTags = useMemo(() => {
    const scoped = todos.filter((t) => t.status === "done" && inScope(t));
    const latest = new Map<string, TodoTag>();
    for (const tag of scoped) {
      if (!tag.agentId) continue;
      const prev = latest.get(tag.agentId);
      if (!prev || (tag.doneAt ?? tag.createdAt) >= (prev.doneAt ?? prev.createdAt)) {
        latest.set(tag.agentId, tag);
      }
    }
    // One 待处理 row per session; tags with no session stay as their own rows.
    return scoped.filter((t) => !t.agentId || latest.get(t.agentId) === t);
  }, [todos, inScope]);
  // 待处理 also surfaces in-flight (assigned) tags whose session is currently
  // blocked on a user interaction: `awaiting_choice` (待选择 — a question tool
  // asking for a pick) AND `waiting_input` (待确认 — a permission/approval
  // prompt). Both are states where the task needs the user before the turn can
  // advance, so the tag becomes visible again instead of hiding in the
  // assigned-in-flight state. Derived live from the hook status — nothing is
  // persisted for it; the tag is still `assigned` until the turn ends.
  // Matching `waiting_input` too is deliberate: a question prompt may surface as
  // a PermissionRequest (claude gates AskUserQuestion behind one), so tying the
  // surfacing to exactly one hook status would miss real blocked-on-user cases.
  const waitingTags = useMemo(
    () =>
      todos.filter((t) => {
        if (t.status !== "assigned" || !inScope(t) || t.agentId == null) return false;
        const status = hookStatus.get(t.agentId)?.status;
        return status === "awaiting_choice" || status === "waiting_input";
      }),
    [todos, hookStatus, inScope]
  );

  // Sessions listed under 发送到 follow the overview scope (global = every live
  // agent; project = only the focused project's). The extra "当前打开的唯一终端"
  // row appears only when the content area shows exactly one live agent tab.
  const sendSessions = useMemo(() => {
    const out: TodoSendSession[] = [];
    for (const [id, agent] of agents) {
      if (agent.status === "done" || agent.status === "failed") continue;
      if (
        todoScope === "project" &&
        agentOwningProject(agent, projectRoots) !== focusedProject
      ) {
        continue;
      }
      out.push({ agentId: id, title: agent.title || id });
    }
    return out;
  }, [agents, todoScope, focusedProject, projectRoots]);

  const uniqueVisible = useMemo(() => {
    const visibleIds = splitTree
      ? splitLeafTabIds(splitTree)
      : activeTabId
        ? [activeTabId]
        : [];
    const visibleAgents = visibleIds
      .map((id) => tabs.find((tab) => tab.id === id))
      .filter((tab) => tab && tab.type === "agent" && !!tab.agentId)
      .map((tab) => tab!.agentId as string)
      .filter((id) => {
        const agent = agents.get(id);
        return agent && agent.status !== "done" && agent.status !== "failed";
      });
    if (visibleAgents.length !== 1) return null;
    const agentId = visibleAgents[0]!;
    const agent = agents.get(agentId);
    return { agentId, title: agent?.title || agentId };
  }, [splitTree, activeTabId, tabs, agents]);

  const sendTagToComposer = (tag: TodoTag) => {
    window.dispatchEvent(
      new CustomEvent("capilot:todo-drop", {
        detail: { kind: "composer", tagId: tag.id, text: tag.text },
      })
    );
  };

  const sendTagToAgent = (tagId: string, agentId: string) => {
    void assignTodoAndSend(tagId, agentId);
  };

  // Hydrate from the settings KV once session restore has settled, so an
  // assigned tag whose agent no longer exists (deleted while the app was
  // closed) reverts to 待分配 instead of staying stuck invisible.
  useEffect(() => {
    if (!sessionsRestored || hydrated) return;
    setHydrated(true);
    let cancelled = false;
    invoke<string | null>("setting_get", { key: TODOS_KEY })
      .then((raw) => {
        if (cancelled || !raw) return;
        try {
          const parsed = JSON.parse(raw) as TodoTag[];
          if (!Array.isArray(parsed)) return;
          const st = useStore.getState();
          const alive = new Set(st.agents.keys());
          const fixed = parsed.map((t) =>
            t.status === "assigned" && t.agentId && !alive.has(t.agentId)
              ? {
                  ...t,
                  status: "todo" as const,
                  agentId: null,
                  sessionName: null,
                  doneAt: null,
                }
              : t
          );
          // Merge, don't replace: the user may have added a tag between mount
          // and this hydration resolving. Keeping those would-be-lost additions
          // avoids the persisted list silently overwriting a fresh tag.
          const byId = new Map<string, TodoTag>();
          for (const t of fixed) byId.set(t.id, t);
          for (const t of st.todos) byId.set(t.id, t);
          st.setTodos([...byId.values()]);
        } catch {
          // corrupt payload — leave the store empty
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [sessionsRestored, hydrated]);

  const submitAdd = (text: string) => {
    addTodo(text);
    setAdding(false);
  };

  /**
   * Pointer-based tag drag. HTML5 DnD is unreliable on Windows WebView2
   * (cursor stuck on 🚫 for in-app drags). Tags use pointer events and drop
   * onto:
   * - `[data-todo-drop-agent="<id>"]` → assign + send to that session
   * - `[data-todo-drop="composer"]` → insert tag text into the input (tag stays
   *   in 待分配; same semantics as the old HTML5 composer drop)
   */
  const onTagPointerDown = (
    e: React.PointerEvent<HTMLDivElement>,
    tag: TodoTag
  ) => {
    if (e.button !== 0) return;
    if ((e.target as HTMLElement).closest(".todo-trash, .todo-more")) return;
    e.preventDefault();
    pointerDragRef.current = {
      id: tag.id,
      text: tag.text,
      startX: e.clientX,
      startY: e.clientY,
      active: false,
      pointerId: e.pointerId,
    };
    beginTodoDrag(tag.id);
    setDragId(tag.id);

    const clearHover = () => {
      document
        .querySelectorAll(".todo-drop-hover")
        .forEach((el) => el.classList.remove("todo-drop-hover"));
    };

    /** Prefer agent rows; otherwise the composer input surface. */
    const resolveTodoDropTarget = (clientX: number, clientY: number) => {
      const under = document.elementFromPoint(clientX, clientY);
      if (!under) return null;
      const agentEl = under.closest?.(
        "[data-todo-drop-agent]"
      ) as HTMLElement | null;
      if (agentEl) {
        const agentId = agentEl.getAttribute("data-todo-drop-agent");
        if (agentId) return { kind: "agent" as const, el: agentEl, agentId };
      }
      const composerEl = under.closest?.(
        '[data-todo-drop="composer"]'
      ) as HTMLElement | null;
      if (composerEl) return { kind: "composer" as const, el: composerEl };
      return null;
    };

    const onMove = (ev: PointerEvent) => {
      const st = pointerDragRef.current;
      if (!st || st.id !== tag.id) return;
      const dx = ev.clientX - st.startX;
      const dy = ev.clientY - st.startY;
      if (!st.active) {
        if (Math.hypot(dx, dy) < POINTER_DRAG_THRESHOLD) return;
        st.active = true;
        document.body.classList.add("todo-pointer-dragging");
      }
      setGhost({ id: st.id, text: st.text, x: ev.clientX, y: ev.clientY });
      clearHover();
      const target = resolveTodoDropTarget(ev.clientX, ev.clientY);
      target?.el.classList.add("todo-drop-hover");
    };

    const finish = (ev: PointerEvent) => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", finish);
      window.removeEventListener("pointercancel", finish);
      document.body.classList.remove("todo-pointer-dragging");
      clearHover();

      const st = pointerDragRef.current;
      pointerDragRef.current = null;
      setGhost(null);
      setDragId(null);
      endTodoDrag();
      if (!st?.active) return;

      const target = resolveTodoDropTarget(ev.clientX, ev.clientY);
      if (!target) return;

      if (target.kind === "composer") {
        // Non-destructive: leave the tag in 待分配; Composer inserts the text.
        window.dispatchEvent(
          new CustomEvent("capilot:todo-drop", {
            detail: {
              kind: "composer",
              tagId: st.id,
              text: st.text,
              clientX: ev.clientX,
              clientY: ev.clientY,
            },
          })
        );
        return;
      }

      void assignTodoAndSend(st.id, target.agentId);
    };

    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", finish);
    window.addEventListener("pointercancel", finish);
  };

  /** Open a 待处理 tag's session in the terminal area. Ended sessions reopen
   *  like the sidebar "已结束" click (fresh mount that resumes). */
  const openTodoSession = (agentId: string) => {
    const st = useStore.getState();
    const agent = st.agents.get(agentId);
    if (!agent) return; // agent record gone — nothing to open
    if (agent.status === "done") {
      st.dropAgentChannel(agentId);
      if (st.tabs.some((t) => t.id === agentId)) st.closeTab(agentId);
      st.requestResume(agentId);
    }
    if (!st.tabs.find((t) => t.id === agentId)) {
      st.addTab({
        id: agentId,
        type: "agent",
        agentId,
        title: agent.title || agentId,
      });
    }
    st.setActiveTab(agentId);
  };

  return (
    <div className="tab-panel" id="tab-overview">
      {/* 待分配 — visible unassigned tags, draggable to sessions. An empty list
          shows a persistent add row instead of a "暂无" message; the header "+"
          (hover-revealed) stays available either way. */}
      <CollapsibleSection
        title={t("todo.pending")}
        headerAction={
          <button
            className="ov-add"
            title={t("todo.addTodo")}
            onClick={() => {
              // 空列表时下方已有常驻输入行：`+` 聚焦它，而不是叠出第二个输入框.
              if (todoTags.length === 0) {
                addRowRef.current?.focus();
              } else {
                setAdding((v) => !v);
              }
            }}
          >
            +
          </button>
        }
      >
        {/* Transient "+"-revealed input only applies while rows exist — an empty
            list already shows the persistent add row below, so never both. */}
        {adding && todoTags.length > 0 && (
          <TodoAdd onSubmit={submitAdd} onCancel={() => setAdding(false)} />
        )}
        {todoTags.length === 0 ? (
          <TodoAdd
            row
            autoFocus={false}
            inputRef={addRowRef}
            onSubmit={submitAdd}
            onCancel={() => {}}
          />
        ) : (
          todoTags.map((tag) =>
            editingId === tag.id ? (
              <TodoEdit
                key={tag.id}
                initial={tag.text}
                onSubmit={(text) => {
                  updateTodoText(tag.id, text);
                  setEditingId(null);
                }}
                onCancel={() => setEditingId(null)}
              />
            ) : (
              <div
                key={tag.id}
                className={`todo-item${dragId === tag.id ? " dragging" : ""}`}
                onPointerDown={(e) => onTagPointerDown(e, tag)}
                onDoubleClick={() => setEditingId(tag.id)}
                title={t("todo.dragHint")}
              >
                <TodoTagActions
                  uniqueVisible={uniqueVisible}
                  sessions={sendSessions}
                  onClone={() => cloneTodo(tag.id)}
                  onSendComposer={() => sendTagToComposer(tag)}
                  onSendAgent={(agentId) => sendTagToAgent(tag.id, agentId)}
                />
                <span className="todo-text">{tag.text}</span>
                <span
                  className="todo-trash"
                  title={t("common.delete")}
                  onClick={(e) => {
                    e.stopPropagation();
                    deleteTodo(tag.id);
                  }}
                >
                  <Icon name="trash-2" size={12} />
                </span>
              </div>
            )
          )
        )}
      </CollapsibleSection>

      {/* 待处理 — finished tags + in-flight tags whose session is blocked on a
          user choice (待选择). Click opens the session in the terminal area. */}
      <CollapsibleSection title={t("todo.done")}>
        {waitingTags.length + doneTags.length === 0 ? (
          <div className="todo-empty">{t("todo.emptyDone")}</div>
        ) : (
          <>
            {waitingTags.map((tag) => (
              <div
                key={tag.id}
                className="todo-item todo-waiting"
                onClick={() => tag.agentId && openTodoSession(tag.agentId)}
                title={t("todo.waitingHint")}
              >
                <Icon
                  name="circle-dot"
                  size={12}
                  className="todo-waiting-icon"
                />
                {tag.sessionName && (
                  <span className="todo-session">{tag.sessionName}</span>
                )}
                <span className="todo-text">{tag.text}</span>
                <span
                  className="todo-trash"
                  title={t("common.delete")}
                  onClick={(e) => {
                    e.stopPropagation();
                    deleteTodo(tag.id);
                  }}
                >
                  <Icon name="trash-2" size={12} />
                </span>
              </div>
            ))}
            {doneTags.map((tag) => (
              <div
                key={tag.id}
                className={`todo-item todo-done${tag.agentId ? "" : " todo-disabled"}`}
                onClick={() => tag.agentId && openTodoSession(tag.agentId)}
                title={
                  tag.agentId
                    ? t("todo.openSession")
                    : t("todo.noSession")
                }
              >
                <Icon
                  name="circle-check"
                  size={12}
                  className="todo-done-icon"
                />
                {tag.sessionName && (
                  <span className="todo-session">{tag.sessionName}</span>
                )}
                <span className="todo-text">{tag.text}</span>
                <span
                  className="todo-trash"
                  title={t("common.delete")}
                  onClick={(e) => {
                    e.stopPropagation();
                    deleteTodo(tag.id);
                  }}
                >
                  <Icon name="trash-2" size={12} />
                </span>
              </div>
            ))}
          </>
        )}
      </CollapsibleSection>

      {ghost && (
        <div
          className="todo-drag-ghost"
          style={{ left: ghost.x + 12, top: ghost.y + 12 }}
          aria-hidden
        >
          {ghost.text}
        </div>
      )}
    </div>
  );
}
