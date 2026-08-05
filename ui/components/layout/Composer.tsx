import {
  useRef,
  useState,
  useEffect,
  useCallback,
  KeyboardEvent,
  DragEvent,
  FormEvent,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useStore } from "../../state/store";
import { spawnAgent, ensureAgentChannel } from "../../state/agentActions";

const DEFAULT_RUNTIME = "claude";
const PERMISSION_MODES = ["ask", "auto", "yolo"] as const;
const SPEED_LABELS: Record<string, string> = {
  high: "high",
  mid: "mid",
  fast: "fast",
  auto: "auto",
};

interface ModelInfo {
  id: string;
  name: string;
  provider: string;
}

interface FsEntryBrief {
  name: string;
  is_dir: boolean;
}

interface AtMenuState {
  /** Index of the `@` in the textarea value (replaced on insert). */
  anchor: number;
  /** Text typed after `@` (may be a partial path like `src/mai`). */
  query: string;
  items: FsEntryBrief[];
  idx: number;
}

export function Composer() {
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const composerOpen = useStore((s) => s.composerOpen);
  const composerTarget = useStore((s) => s.composerTarget);
  const permissionMode = useStore((s) => s.permissionMode);
  const speed = useStore((s) => s.speed);
  const selectedModel = useStore((s) => s.selectedModel);
  const workerMode = useStore((s) => s.workerMode);
  const activeTabId = useStore((s) => s.activeTabId);
  const tabs = useStore((s) => s.tabs);
  const agents = useStore((s) => s.agents);
  const masterAgentId = useStore((s) => s.masterAgentId);
  const workerUnlockId = useStore((s) => s.workerUnlockId);

  const toggleComposer = useStore((s) => s.toggleComposer);
  const setComposerTarget = useStore((s) => s.setComposerTarget);
  const setPermissionMode = useStore((s) => s.setPermissionMode);
  const setSpeed = useStore((s) => s.setSpeed);
  const setSelectedModel = useStore((s) => s.setSelectedModel);
  const toggleWorkerMode = useStore((s) => s.toggleWorkerMode);
  const pushDraft = useStore((s) => s.pushDraft);
  const navigateDraft = useStore((s) => s.navigateDraft);
  const setWorkerUnlock = useStore((s) => s.setWorkerUnlock);

  const [models, setModels] = useState<ModelInfo[]>([]);
  const [atMenu, setAtMenu] = useState<AtMenuState | null>(null);
  const [dragHover, setDragHover] = useState(false);
  const [isBangInput, setIsBangInput] = useState(false);

  // Stale-response guard for async fs_list fetches in the `@` menu.
  const atReqRef = useRef(0);
  // Guards against double-insert when both the DOM drop handler and the Tauri
  // drag-drop event observe the same drop.
  const dropHandledRef = useRef(false);
  // Nesting counter (dragenter/dragleave fire when crossing child boundaries).
  const dragDepthRef = useRef(0);

  const activeTab = tabs.find((t) => t.id === activeTabId);
  const targetAgentId =
    composerTarget === "agent" ? activeTab?.agentId : undefined;
  const currentModel = models.find((m) => m.id === selectedModel) ?? null;

  // DevPlan §4.6: worker terminals lock the composer input to prevent
  // orchestration conflicts. Locked → readOnly + 仍然发送/解锁 affordance.
  const activeAgent = activeTab?.agentId ? agents.get(activeTab.agentId) : undefined;
  const workerLocked = composerTarget === "agent" && activeAgent?.role === "worker";
  const unlocked = workerUnlockId === activeTab?.agentId;
  const locked = workerLocked && !unlocked;

  // ── Model list (composer `[模型↑]`) ────────────────────────────
  useEffect(() => {
    let cancelled = false;
    invoke<ModelInfo[]>("runtime_models", { runtime: DEFAULT_RUNTIME })
      .then((m) => {
        if (cancelled) return;
        setModels(m ?? []);
        const s = useStore.getState();
        if (!s.selectedModel && m && m.length) s.setSelectedModel(m[0].id);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  const cycleModel = useCallback(() => {
    if (!models.length) return;
    const idx = models.findIndex((m) => m.id === selectedModel);
    const next = models[(idx + 1) % models.length];
    setSelectedModel(next.id);
  }, [models, selectedModel, setSelectedModel]);

  // ── Text helpers ──────────────────────────────────────────────
  const resizeTextarea = useCallback((el: HTMLTextAreaElement) => {
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, 200) + "px";
  }, []);

  const insertText = useCallback(
    (text: string, pos?: number) => {
      const el = textareaRef.current;
      if (!el) return;
      const at = pos ?? el.selectionStart ?? el.value.length;
      el.value = el.value.slice(0, at) + text + el.value.slice(at);
      const newPos = at + text.length;
      el.selectionStart = el.selectionEnd = newPos;
      el.focus();
      resizeTextarea(el);
    },
    [resizeTextarea]
  );

  /** Append `@<path> ` chips at the end of the message (drag & drop). */
  const appendPaths = useCallback(
    (paths: string[]) => {
      if (!paths.length) return;
      const text = paths.map((p) => `@${p}`).join(" ");
      const end = textareaRef.current?.value.length ?? 0;
      insertText(text + " ", end);
    },
    [insertText]
  );

  // ── Drag & drop → `@path` chip (DevPlan §3.2) ─────────────────
  const composerWrapRef = useRef<HTMLDivElement>(null);

  /** Tauri drag-drop positions are physical pixels; CSS rects are CSS pixels. */
  const isPointInComposer = useCallback((pos: { x: number; y: number }) => {
    const wrap = composerWrapRef.current;
    if (!wrap) return false;
    const r = wrap.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    const x = pos.x / dpr;
    const y = pos.y / dpr;
    return x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
  }, []);

  const handleDragEnter = useCallback((e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    dragDepthRef.current += 1;
    setDragHover(true);
  }, []);

  const handleDragOver = useCallback((e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
  }, []);

  const handleDragLeave = useCallback((e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    dragDepthRef.current = Math.max(0, dragDepthRef.current - 1);
    if (dragDepthRef.current === 0) setDragHover(false);
  }, []);

  const handleDrop = useCallback(
    (e: DragEvent<HTMLDivElement>) => {
      e.preventDefault();
      // Don't reset dragDepthRef here: the Tauri drag-drop event (which carries
      // the real paths) fires after this, and still needs to know the drop
      // happened over the composer. Only reset when a path was read directly.
      // Some webviews still expose `.path` on File (Tauri v1 heritage).
      const f = e.dataTransfer.files?.[0] as
        | (File & { path?: string })
        | undefined;
      if (f?.path) {
        appendPaths([f.path]);
        dropHandledRef.current = true;
        dragDepthRef.current = 0;
        setDragHover(false);
      }
    },
    [appendPaths]
  );

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        const p = event.payload;
        if (p.type === "enter" || p.type === "over") {
          setDragHover(isPointInComposer(p.position));
        } else if (p.type === "leave") {
          setDragHover(false);
        } else if (p.type === "drop") {
          // Scope to the composer: the drop must have landed on it (DOM counter
          // or Tauri position). Fall back to position in case Tauri suppresses
          // DOM drag events.
          const overComposer =
            dragDepthRef.current > 0 || isPointInComposer(p.position);
          if (overComposer && !dropHandledRef.current) {
            appendPaths(p.paths);
          }
          dropHandledRef.current = false;
          dragDepthRef.current = 0;
          setDragHover(false);
        }
      })
      .then((un) => {
        unlisten = un;
      });
    return () => {
      unlisten?.();
    };
  }, [appendPaths, isPointInComposer]);

  // ── `@` file autocomplete (DevPlan §3.2) ──────────────────────
  const resolveTargetCwd = useCallback((): string | null => {
    const s = useStore.getState();
    const id =
      s.composerTarget === "agent"
        ? s.tabs.find((t) => t.id === s.activeTabId)?.agentId
        : s.masterAgentId;
    return id ? s.agents.get(id)?.cwd ?? null : null;
  }, []);

  const handleAtAuto = useCallback(
    async (el: HTMLTextAreaElement) => {
      const pos = el.selectionStart ?? el.value.length;
      const before = el.value.slice(0, pos);
      const lastAt = before.lastIndexOf("@");
      if (lastAt < 0) {
        setAtMenu(null);
        return;
      }
      const query = before.slice(lastAt + 1);
      // A space / newline ends the `@` mention.
      if (/\s/.test(query)) {
        setAtMenu(null);
        return;
      }

      const cwd = resolveTargetCwd();
      if (!cwd) return;

      const req = ++atReqRef.current;
      const slashIdx = query.lastIndexOf("/");
      const dirPart = slashIdx >= 0 ? query.slice(0, slashIdx) : "";
      const filePart = slashIdx >= 0 ? query.slice(slashIdx + 1) : query;
      const listDir = dirPart ? `${cwd}/${dirPart}` : cwd;

      let items: FsEntryBrief[] = [];
      try {
        items = (await invoke<FsEntryBrief[]>("fs_list", { dir: listDir })) ?? [];
      } catch {
        try {
          items = (await invoke<FsEntryBrief[]>("fs_list", { dir: cwd })) ?? [];
        } catch {
          items = [];
        }
      }
      if (req !== atReqRef.current) return; // stale response

      const filtered = filePart
        ? items.filter((it) =>
            it.name.toLowerCase().startsWith(filePart.toLowerCase())
          )
        : items;
      if (!filtered.length) {
        setAtMenu(null);
        return;
      }
      setAtMenu({ anchor: lastAt, query, items: filtered.slice(0, 20), idx: 0 });
    },
    [resolveTargetCwd]
  );

  const insertAtItem = useCallback(
    (item: FsEntryBrief) => {
      if (!atMenu || !textareaRef.current) return;
      const el = textareaRef.current;
      const { anchor, query } = atMenu;
      const slashIdx = query.lastIndexOf("/");
      const dirPart = slashIdx >= 0 ? query.slice(0, slashIdx + 1) : "";
      const insert = `@${dirPart}${item.name} `;
      el.value =
        el.value.slice(0, anchor) +
        insert +
        el.value.slice(anchor + query.length + 1);
      const newPos = anchor + insert.length;
      el.selectionStart = el.selectionEnd = newPos;
      el.focus();
      resizeTextarea(el);
      setAtMenu(null);
    },
    [atMenu, resizeTextarea]
  );

  // ── Send ──────────────────────────────────────────────────────
  const handleSend = useCallback(async () => {
    const el = textareaRef.current;
    if (!el) return;
    const raw = el.value.trim();
    if (!raw) return;
    // `!命令` 直发终端（绕过 agent 会话，DevPlan §4.3）：去掉 `!` 标记，其余原样
    // 发送（不做 smart 包装）。视觉上由 `.composer-bang` 徽标标注。
    const isBang = raw.startsWith("!");
    const text = isBang ? raw.slice(1).trimStart() : raw;
    pushDraft(raw);

    let agentId = targetAgentId;
    let justSpawned = false;
    try {
      if (!agentId) {
        if (composerTarget === "master") {
          // Reuse the existing master session instead of spawning a new one.
          if (masterAgentId && agents.has(masterAgentId)) {
            agentId = masterAgentId;
          }
        }
        if (!agentId) {
          const role =
            composerTarget === "master"
              ? "master"
              : workerMode
              ? "worker"
              : "standalone";
          agentId = await spawnAgent(role);
          justSpawned = true;
        }
      }

      // Resumed/restored sessions may not have a channel yet.
      const resumed = await ensureAgentChannel(agentId);
      // Give a freshly-spawned/resumed CLI TUI time to attach its input loop
      // before injecting the message.
      if (justSpawned || resumed) {
        await new Promise((r) => setTimeout(r, 800));
      }
      await invoke("agent_write", { id: agentId, data: text });
    } catch (err) {
      console.error("Failed to send to agent:", err);
      return;
    }

    // Relock worker input after a successful send (解锁 until next send).
    setWorkerUnlock(null);

    if (el) {
      el.value = "";
      el.style.height = "auto";
    }
    setIsBangInput(false);
  }, [
    targetAgentId,
    composerTarget,
    workerMode,
    masterAgentId,
    agents,
    pushDraft,
    setWorkerUnlock,
  ]);

  const handleStillSend = useCallback(async () => {
    if (!activeTab?.agentId) return;
    setWorkerUnlock(activeTab.agentId); // allow a single send…
    await handleSend(); // …which relocks after sending.
  }, [activeTab?.agentId, handleSend, setWorkerUnlock]);

  // ── Keyboard ──────────────────────────────────────────────────
  const handleKeyDown = useCallback(
    (e: KeyboardEvent<HTMLTextAreaElement>) => {
      if (atMenu) {
        if (e.key === "ArrowDown") {
          e.preventDefault();
          setAtMenu({
            ...atMenu,
            idx: (atMenu.idx + 1) % atMenu.items.length,
          });
          return;
        }
        if (e.key === "ArrowUp") {
          e.preventDefault();
          setAtMenu({
            ...atMenu,
            idx: (atMenu.idx - 1 + atMenu.items.length) % atMenu.items.length,
          });
          return;
        }
        if (e.key === "Enter") {
          e.preventDefault();
          const item = atMenu.items[atMenu.idx];
          if (item) insertAtItem(item);
          return;
        }
        if (e.key === "Escape") {
          e.preventDefault();
          setAtMenu(null);
          return;
        }
        // Tab closes the menu, then falls through to target switching below.
        if (e.key === "Tab") setAtMenu(null);
      }

      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        if (locked) return; // 锁定期间不静默发送，走 banner 二选一
        handleSend();
      } else if (e.key === "Tab") {
        e.preventDefault();
        if (e.shiftKey) {
          const idx = PERMISSION_MODES.indexOf(permissionMode);
          setPermissionMode(PERMISSION_MODES[(idx + 1) % 3]);
        } else {
          setComposerTarget(composerTarget === "agent" ? "master" : "agent");
        }
      } else if (e.key === "ArrowUp" && !e.currentTarget.value) {
        e.preventDefault();
        const draft = navigateDraft(1);
        if (draft !== null) {
          e.currentTarget.value = draft;
        }
      } else if (e.key === "ArrowDown" && !e.currentTarget.value) {
        e.preventDefault();
        const draft = navigateDraft(-1);
        if (draft !== null) {
          e.currentTarget.value = draft;
        }
      }
    },
    [
      atMenu,
      insertAtItem,
      handleSend,
      permissionMode,
      composerTarget,
      setPermissionMode,
      setComposerTarget,
      navigateDraft,
      locked,
    ]
  );

  const handleInput = useCallback(
    (e: FormEvent<HTMLTextAreaElement>) => {
      const el = e.currentTarget;
      resizeTextarea(el);
      setIsBangInput(el.value.trimStart().startsWith("!"));
      handleAtAuto(el);
    },
    [resizeTextarea, handleAtAuto]
  );

  return (
    <div className={`composer${!composerOpen ? " composer-collapsed" : ""}`}>
      {/* Target line */}
      <div className="composer-target">
        <span>→</span>{" "}
        agent:{" "}
        {composerTarget === "master"
          ? "master"
          : activeTab?.title || "none"}
        {workerMode && composerTarget !== "master" ? " · worker" : ""}
        {workerLocked ? " · 🔒worker" : ""}
        {isBangInput && <span className="composer-bang">⚡ 终端直发</span>}
      </div>

      {/* Input area */}
      <div
        ref={composerWrapRef}
        className={`composer-input-wrap${dragHover ? " drop-hint" : ""}`}
        onDragEnter={handleDragEnter}
        onDragOver={handleDragOver}
        onDragLeave={handleDragLeave}
        onDrop={handleDrop}
      >
        {locked && (
          <div className="lock-banner">
            <span className="lock-banner-text">🔒 此 agent 是 worker，输入会被编排结果覆盖</span>
            <button className="lock-btn" onClick={handleStillSend}>
              仍然发送
            </button>
            <button
              className="lock-btn"
              onClick={() => activeTab?.agentId && setWorkerUnlock(activeTab.agentId)}
            >
              解锁
            </button>
          </div>
        )}
        {unlocked && workerLocked && (
          <div className="lock-banner unlocked">
            <span className="lock-banner-text">🔓 已解锁（发送后重新锁定）</span>
          </div>
        )}
        <textarea
          ref={textareaRef}
          className={`composer-input${locked ? " locked" : ""}`}
          placeholder={
            locked
              ? "🔒 worker — 输入被编排锁定"
              : "发消息…（/ 命令 · @ 文件 · ! 终端 · 拖入文件）"
          }
          rows={2}
          readOnly={locked}
          onKeyDown={handleKeyDown}
          onInput={handleInput}
        />
      </div>

      {/* `@` file autocomplete menu */}
      {atMenu && (
        <div className="composer-at-menu" role="listbox">
          {atMenu.items.map((item, i) => (
            <div
              key={item.name}
              role="option"
              aria-selected={i === atMenu.idx}
              className={`composer-at-item${i === atMenu.idx ? " active" : ""}`}
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => insertAtItem(item)}
            >
              <span className="composer-at-name">
                {item.name}
                {item.is_dir ? "/" : ""}
              </span>
            </div>
          ))}
        </div>
      )}

      {/* Actions */}
      <div className="composer-actions">
        <span className="act-btn">+ 文件/引用</span>
        <span
          className="act-btn"
          onClick={cycleModel}
          title={`模型切换（当前：${currentModel ? currentModel.name : "runtime 默认"}）`}
        >
          模型{currentModel ? `: ${currentModel.name}` : " ↑"}
        </span>
        <span
          className="act-btn"
          onClick={() => {
            const s = ["high", "mid", "fast", "auto"] as const;
            const idx = s.indexOf(speed);
            setSpeed(s[(idx + 1) % 4]);
          }}
        >
          速度: {SPEED_LABELS[speed]}
        </span>
        <span className="act-sep" />
        <span
          className={`act-btn accent${workerMode ? " active" : ""}`}
          title="worker 开关：开启后新终端进编排池"
          onClick={toggleWorkerMode}
        >
          🤖worker {workerMode ? "开" : "关"}
        </span>
        <span className="act-sep" />
        <span className="act-mode-group">
          {PERMISSION_MODES.map((m) => (
            <span
              key={m}
              className={`act-mode-btn${permissionMode === m ? " active" : ""}`}
              onClick={() => setPermissionMode(m)}
            >
              {m}
            </span>
          ))}
        </span>
        <button className="collapse-btn" onClick={toggleComposer}>
          {composerOpen ? "▼" : "▲"}
        </button>
      </div>
    </div>
  );
}
