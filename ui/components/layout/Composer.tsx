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
import { open } from "@tauri-apps/plugin-dialog";
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

interface RecentEntry extends FsEntryBrief {
  path: string;
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
  const composerH = useStore((s) => s.composerH);
  const masterReportH = useStore((s) => s.masterReportH);
  const setComposerH = useStore((s) => s.setComposerH);
  const toggleWorkerMode = useStore((s) => s.toggleWorkerMode);
  const pushDraft = useStore((s) => s.pushDraft);
  const navigateDraft = useStore((s) => s.navigateDraft);
  const setWorkerUnlock = useStore((s) => s.setWorkerUnlock);

  const [models, setModels] = useState<ModelInfo[]>([]);
  const [atMenu, setAtMenu] = useState<AtMenuState | null>(null);
  const [dragHover, setDragHover] = useState(false);
  const [isBangInput, setIsBangInput] = useState(false);
  // Non-empty input → enables the send button (`.ul-send-btn`).
  const [hasInput, setHasInput] = useState(false);
  // Root ref + dragging flag for the height-resize divider above the composer.
  const composerRef = useRef<HTMLDivElement>(null);
  const [composerResizing, setComposerResizing] = useState(false);

  // Composer popover menus (向上弹出)：模型选择 + 文件/引用.
  const [modelMenuOpen, setModelMenuOpen] = useState(false);
  const [refMenuOpen, setRefMenuOpen] = useState(false);
  const [recentEntries, setRecentEntries] = useState<RecentEntry[]>([]);
  const modelAnchorRef = useRef<HTMLSpanElement>(null);
  const modelMenuRef = useRef<HTMLDivElement>(null);
  const refAnchorRef = useRef<HTMLSpanElement>(null);
  const refMenuRef = useRef<HTMLDivElement>(null);

  // Stale-response guard for async fs_list fetches in the `@` menu.
  const atReqRef = useRef(0);
  // Guards against double-insert when both the DOM drop handler and the Tauri
  // drag-drop event observe the same drop.
  const dropHandledRef = useRef(false);
  // Nesting counter (dragenter/dragleave fire when crossing child boundaries).
  const dragDepthRef = useRef(0);
  // Guards against double-send on rapid Enter (Bug 3).
  const sendingRef = useRef(false);
  // Auto-relock timer for the composer 解锁 (mirrors XTermPanel's 8s window).
  const unlockTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Clear the composer unlock timer on unmount.
  useEffect(() => {
    return () => {
      if (unlockTimerRef.current) clearTimeout(unlockTimerRef.current);
    };
  }, []);

  // Keep the textarea in the right mode when the composer switches between
  // auto-height and a fixed height (e.g. the master report height lands on
  // mount, or the user drags the divider).
  const fixedHeight = composerH !== null || masterReportH > 0;
  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    if (fixedHeight) {
      el.style.height = "100%";
    } else {
      el.style.height = "auto";
      el.style.height = Math.min(el.scrollHeight, 200) + "px";
    }
  }, [fixedHeight]);

  const activeTab = tabs.find((t) => t.id === activeTabId);
  const targetAgentId =
    composerTarget === "agent" ? activeTab?.agentId : undefined;

  // ── Per-session composer config ────────────────────────────────
  // The permission/speed/model controls show and edit the CURRENT target
  // session's own values (falling back to the global "next spawn" defaults when
  // no session is targeted). Changing one applies to that session (persisted,
  // takes effect on next resume) and remembers the choice for new sessions.
  const configAgentId =
    composerTarget === "master" ? masterAgentId ?? undefined : activeTab?.agentId;
  const configAgent = configAgentId ? agents.get(configAgentId) : undefined;
  const shownMode =
    (configAgent?.mode as (typeof PERMISSION_MODES)[number]) ?? permissionMode;
  const shownSpeed =
    (configAgent?.speed as "high" | "mid" | "fast" | "auto") ?? speed;
  const shownModel = configAgent?.model ?? selectedModel;
  const currentModel = models.find((m) => m.id === shownModel) ?? null;

  const applyConfig = useCallback(
    (patch: { mode?: string; speed?: string; model?: string | null }) => {
      const s = useStore.getState();
      // Remember for the next spawned session.
      if (patch.mode !== undefined) s.setPermissionMode(patch.mode as never);
      if (patch.speed !== undefined) s.setSpeed(patch.speed as never);
      if (patch.model !== undefined) s.setSelectedModel(patch.model);
      // Apply to the current session (persisted; takes effect on next resume).
      const id =
        composerTarget === "master" ? s.masterAgentId : activeTab?.agentId;
      if (!id || !s.agents.has(id)) return;
      s.addAgent({ ...s.agents.get(id)!, ...patch }, null);
      invoke("agent_set_session_config", { id, ...patch }).catch(() => {});
    },
    [composerTarget, activeTab?.agentId]
  );

  // DevPlan §4.6: worker terminals lock the composer input to prevent
  // orchestration conflicts. Locked → readOnly + 仍然发送/解锁 affordance.
  const activeAgent = activeTab?.agentId ? agents.get(activeTab.agentId) : undefined;
  const workerLocked = composerTarget === "agent" && activeAgent?.role === "worker";
  const unlocked = workerUnlockId === activeTab?.agentId;
  const locked = workerLocked && !unlocked;

  // ── Esc → abort the target agent's current operation ──────────
  // Sends a raw ESC byte to the agent's PTY — the same path the terminal uses
  // (xterm keydown → agent_write raw:true), so the CLI aborts its in-flight
  // turn exactly like pressing Esc inside the terminal.
  const abortAgentOperation = useCallback(() => {
    const id = composerTarget === "agent" ? activeTab?.agentId : masterAgentId;
    if (!id || !agents.has(id)) return;
    invoke("agent_write", { id, data: "\u001b", raw: true }).catch(() => {});
  }, [composerTarget, activeTab?.agentId, masterAgentId, agents]);

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

  // ── Popover open/close (click-outside + Escape) ───────────────
  useEffect(() => {
    if (!modelMenuOpen) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node | null;
      if (modelMenuRef.current?.contains(t)) return;
      if (modelAnchorRef.current?.contains(t)) return;
      setModelMenuOpen(false);
    };
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") setModelMenuOpen(false);
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [modelMenuOpen]);

  useEffect(() => {
    if (!refMenuOpen) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node | null;
      if (refMenuRef.current?.contains(t)) return;
      if (refAnchorRef.current?.contains(t)) return;
      setRefMenuOpen(false);
    };
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") setRefMenuOpen(false);
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [refMenuOpen]);

  // ── Text helpers ──────────────────────────────────────────────
  const resizeTextarea = useCallback((el: HTMLTextAreaElement) => {
    // Fixed-height composer (default = master report height, or user-dragged):
    // the textarea fills the input area and scrolls internally. In the default
    // auto-height mode it grows with its content instead (capped at 200px).
    const s = useStore.getState();
    if (s.composerH !== null || s.masterReportH > 0) {
      el.style.height = "100%";
      return;
    }
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
      setHasInput(true);
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
    // A few px of tolerance so drops on the wrap's border still count.
    return (
      x >= r.left - 4 && x <= r.right + 4 && y >= r.top - 4 && y <= r.bottom + 4
    );
  }, []);

  const handleDragEnter = useCallback((e: DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    dragDepthRef.current += 1;
    // A new drag sequence is starting — clear any stale dedupe flag left over
    // from the previous drop so the next drop inserts exactly once.
    dropHandledRef.current = false;
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
      // If the Tauri drag-drop event already inserted the paths for this same
      // physical drop, consume the DOM drop without double-inserting.
      if (dropHandledRef.current) {
        dropHandledRef.current = false;
        dragDepthRef.current = 0;
        setDragHover(false);
        return;
      }
      // Some webviews still expose `.path` on File (Tauri v1 heritage / v2
      // dragDropEnabled). If present, insert directly; otherwise defer to the
      // Tauri drag-drop event, which carries the real absolute paths.
      const f = e.dataTransfer.files?.[0] as
        | (File & { path?: string })
        | undefined;
      if (f?.path) {
        appendPaths([f.path]);
        dropHandledRef.current = true;
        dragDepthRef.current = 0;
        setDragHover(false);
      } else {
        // Application-internal drag (e.g. a file tree row): the source's
        // onDragStart stored the full path in text/plain. Consume it so the
        // drop inserts @path instead of the browser's default text selection.
        const textPath = e.dataTransfer.getData("text/plain");
        if (textPath && textPath.trim() && !textPath.includes("\n")) {
          appendPaths([textPath.trim()]);
          dropHandledRef.current = true;
          dragDepthRef.current = 0;
          setDragHover(false);
        }
      }
      // No path at all → leave dragDepthRef/dropHandledRef untouched so the
      // Tauri drag-drop event (which fires next) can still detect the composer.
    },
    [appendPaths]
  );

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    // StrictMode double-mount guard: onDragDropEvent resolves asynchronously,
    // so cleanup can run before `.then()` assigns unlisten — the late listener
    // must drop itself instead of leaking into the second mount.
    let cancelled = false;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        const p = event.payload;
        if (p.type === "enter") {
          dropHandledRef.current = false; // new drag sequence
          setDragHover(isPointInComposer(p.position));
        } else if (p.type === "over") {
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
            dropHandledRef.current = true;
          }
          dragDepthRef.current = 0;
          setDragHover(false);
        }
      })
      .then((un) => {
        if (cancelled) {
          un();
        } else {
          unlisten = un;
        }
      });
    return () => {
      cancelled = true;
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
      setHasInput(true);
      setAtMenu(null);
    },
    [atMenu, resizeTextarea]
  );

  // Load the active agent's cwd listing when the file/ref menu opens.
  useEffect(() => {
    if (!refMenuOpen) return;
    const cwd = resolveTargetCwd();
    if (!cwd) {
      setRecentEntries([]);
      return;
    }
    let cancelled = false;
    invoke<FsEntryBrief[]>("fs_list", { dir: cwd })
      .then((items) => {
        if (cancelled) return;
        const sorted = (items ?? []).slice().sort((a, b) => {
          if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
          return a.name.localeCompare(b.name);
        });
        setRecentEntries(
          sorted
            .slice(0, 12)
            .map((it) => ({ ...it, path: `${cwd}/${it.name}` }))
        );
      })
      .catch(() => {
        if (!cancelled) setRecentEntries([]);
      });
    return () => {
      cancelled = true;
    };
  }, [refMenuOpen, resolveTargetCwd]);

  // ── `+ 文件/引用` menu actions ────────────────────────────────
  const handlePickFile = useCallback(async () => {
    setRefMenuOpen(false);
    try {
      const selected = await open({
        multiple: false,
        directory: false,
        title: "选择文件 — 插入 @路径",
        defaultPath: resolveTargetCwd() ?? undefined,
      });
      if (typeof selected === "string" && selected) {
        appendPaths([selected]);
      }
    } catch (err) {
      console.error("选择文件失败:", err);
    }
  }, [appendPaths, resolveTargetCwd]);

  const handlePasteRef = useCallback(async () => {
    setRefMenuOpen(false);
    let text = "";
    try {
      text = (await navigator.clipboard.readText()).trim();
    } catch {
      text = "";
    }
    if (text) {
      appendPaths([text]);
    } else {
      // 剪贴板为空 → 插入一个裸 `@` 让现有补全菜单接管.
      insertText("@");
    }
  }, [appendPaths, insertText]);

  const handleInsertRecent = useCallback(
    (item: RecentEntry) => {
      setRefMenuOpen(false);
      appendPaths([item.path]);
    },
    [appendPaths]
  );

  // ── Send ──────────────────────────────────────────────────────
  const handleSend = useCallback(async () => {
    if (sendingRef.current) return; // in-flight guard (rapid Enter, Bug 3)
    const el = textareaRef.current;
    if (!el) return;
    const raw = el.value.trim();
    if (!raw) return;
    // `!命令` 直发终端（绕过 agent 会话，DevPlan §4.3）：去掉 `!` 标记，其余原样
    // 发送（不做 smart 包装）。视觉上由 `.composer-bang` 徽标标注。
    const isBang = raw.startsWith("!");
    const text = isBang ? raw.slice(1).trimStart() : raw;
    pushDraft(raw);

    // Clear the textarea synchronously before any await so a second Enter can't
    // read the same value (Bug 3).
    el.value = "";
    // Fixed-height composer keeps the textarea filled; auto-height collapses it.
    resizeTextarea(el);
    setIsBangInput(false);
    setHasInput(false);

    sendingRef.current = true;
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
    } finally {
      // Relock worker input even when the send fails (Bug 6), and release the
      // in-flight guard so the next Enter can send again.
      if (unlockTimerRef.current) {
        clearTimeout(unlockTimerRef.current);
        unlockTimerRef.current = null;
      }
      setWorkerUnlock(null);
      sendingRef.current = false;
    }
  }, [
    targetAgentId,
    composerTarget,
    workerMode,
    masterAgentId,
    agents,
    resizeTextarea,
    pushDraft,
    setWorkerUnlock,
  ]);

  /** 解锁 — allow worker input for 8s (mirrors XTermPanel's auto-relock, Bug 6). */
  const handleUnlock = useCallback(() => {
    const id = activeTab?.agentId;
    if (!id) return;
    setWorkerUnlock(id);
    if (unlockTimerRef.current) clearTimeout(unlockTimerRef.current);
    unlockTimerRef.current = setTimeout(() => {
      const s = useStore.getState();
      if (s.workerUnlockId === id) s.setWorkerUnlock(null);
    }, 8000);
  }, [activeTab?.agentId, setWorkerUnlock]);

  const handleStillSend = useCallback(async () => {
    if (!activeTab?.agentId) return;
    handleUnlock(); // allow a single send…
    await handleSend(); // …which relocks after sending.
  }, [activeTab?.agentId, handleSend, handleUnlock]);

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
        // Tab completes the highlighted `@` mention instead of switching the
        // send target — otherwise Tab would hijack the autocomplete (Bug).
        if (e.key === "Tab") {
          e.preventDefault();
          const item = atMenu.items[atMenu.idx];
          if (item) insertAtItem(item);
          return;
        }
      }

      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        if (locked) return; // 锁定期间不静默发送，走 banner 二选一
        handleSend();
      } else if (e.key === "Tab") {
        e.preventDefault();
        if (e.shiftKey) {
          const idx = PERMISSION_MODES.indexOf(shownMode);
          applyConfig({ mode: PERMISSION_MODES[(idx + 1) % 3] });
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
      } else if (e.key === "Escape") {
        // 终端式中断：向目标 agent 的 PTY 发原始 ESC 字节。worker 锁定期间
        // 与终端一致不转发（终端在 lock 状态下同样吞掉所有按键）；模型/文件
        // 弹出菜单打开时，这次 Esc 只负责关菜单（窗口级监听），不中断。
        e.preventDefault();
        if (!locked && !modelMenuOpen && !refMenuOpen) abortAgentOperation();
      }
    },
    [
      atMenu,
      insertAtItem,
      handleSend,
      shownMode,
      applyConfig,
      composerTarget,
      setComposerTarget,
      navigateDraft,
      locked,
      modelMenuOpen,
      refMenuOpen,
      abortAgentOperation,
    ]
  );

  const handleInput = useCallback(
    (e: FormEvent<HTMLTextAreaElement>) => {
      const el = e.currentTarget;
      resizeTextarea(el);
      setIsBangInput(el.value.trimStart().startsWith("!"));
      setHasInput(el.value.trim().length > 0);
      handleAtAuto(el);
      // Typing dismisses the popover menus (模型选择 / 文件引用).
      setModelMenuOpen(false);
      setRefMenuOpen(false);
    },
    [resizeTextarea, handleAtAuto]
  );

  // ── Height resize (drag the divider above the composer) ───────
  const startComposerResize = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      const startY = e.clientY;
      const startH = composerRef.current?.getBoundingClientRect().height ?? 0;
      setComposerResizing(true);
      const onMove = (ev: MouseEvent) => {
        // Dragging up grows the composer. Clamp so it can't swallow the whole
        // content area or collapse to nothing.
        const h = Math.max(
          80,
          Math.min(window.innerHeight * 0.6, startH + (startY - ev.clientY))
        );
        setComposerH(Math.round(h));
      };
      const onUp = () => {
        setComposerResizing(false);
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
      };
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
    },
    [setComposerH]
  );

  const resetComposerH = useCallback(() => setComposerH(null), [setComposerH]);

  // Effective composer height: user-dragged value wins; otherwise follow the
  // master report's measured height; otherwise stay auto-sized.
  const effH = composerH ?? (masterReportH > 0 ? masterReportH : null);

  return (
    <div
      ref={composerRef}
      className={`composer${!composerOpen ? " composer-collapsed" : ""}`}
      style={composerOpen && effH ? { height: effH } : undefined}
    >
      {/* Height divider: drag to resize, double-click to reset to the default
          (master-report) height. */}
      {composerOpen && (
        <div
          className={`composer-resize${composerResizing ? " active" : ""}`}
          title="拖拽调整高度 · 双击恢复默认高度"
          onMouseDown={startComposerResize}
          onDoubleClick={resetComposerH}
        />
      )}
      {/* Target line */}
      <div className="composer-target">
        {composerTarget === "master" ? (
          <span>→ master</span>
        ) : (
          <span>
            → agent:{" "}
            {activeTab?.type === "agent" && activeTab.agentId
              ? activeTab.title || "agent"
              : "(无标签)"}
          </span>
        )}
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
            <button className="lock-btn" onClick={handleUnlock}>
              解锁
            </button>
          </div>
        )}
        {unlocked && workerLocked && (
          <div className="lock-banner unlocked">
            <span className="lock-banner-text">🔓 已解锁（发送后重新锁定）</span>
          </div>
        )}
        <div className="ul-composer-input-row">
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
          <button
            className="ul-send-btn"
            title="发送消息（Enter）"
            onClick={() => handleSend()}
            disabled={locked || sendingRef.current || !hasInput}
          >
            发送
          </button>
        </div>
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
        <span className="cmp-pop" ref={refAnchorRef}>
          <span
            className="act-btn"
            title="插入文件引用 / 最近文件"
            onClick={() => {
              setModelMenuOpen(false);
              setRefMenuOpen((o) => !o);
            }}
          >
            + 文件/引用
          </span>
          {refMenuOpen && (
            <div className="cmp-menu" ref={refMenuRef} role="menu">
              <div className="cmp-menu-label">插入文件/引用</div>
              <div className="cmp-menu-item" onClick={handlePickFile}>
                <span className="cmp-menu-name">📄 选择文件…</span>
              </div>
              <div className="cmp-menu-item" onClick={handlePasteRef}>
                <span className="cmp-menu-name">🔗 粘贴引用/路径</span>
              </div>
              <div className="cmp-menu-sep" />
              <div className="cmp-menu-label">最近文件（agent cwd）</div>
              {recentEntries.length === 0 && (
                <div className="cmp-menu-empty">暂无文件</div>
              )}
              {recentEntries.map((it) => (
                <div
                  key={it.path}
                  className="cmp-menu-item"
                  onClick={() => handleInsertRecent(it)}
                >
                  <span className="cmp-menu-name">
                    {it.name}
                    {it.is_dir ? "/" : ""}
                  </span>
                </div>
              ))}
            </div>
          )}
        </span>

        <span className="cmp-pop" ref={modelAnchorRef}>
          <span
            className="act-btn"
            onClick={() => {
              setRefMenuOpen(false);
              setModelMenuOpen((o) => !o);
            }}
            title={`选择模型（当前：${currentModel ? currentModel.name : "runtime 默认"}）`}
          >
            模型{currentModel ? `: ${currentModel.name}` : " ↑"}
          </span>
          {modelMenuOpen && (
            <div className="cmp-menu" ref={modelMenuRef} role="menu">
              <div className="cmp-menu-label">选择模型</div>
              {models.length === 0 && (
                <div className="cmp-menu-empty">无可用模型</div>
              )}
              {models.map((m) => (
                <div
                  key={m.id}
                  className={`cmp-menu-item${m.id === shownModel ? " current" : ""}`}
                  onClick={() => {
                    applyConfig({ model: m.id });
                    setModelMenuOpen(false);
                  }}
                >
                  <span className="cmp-menu-name">{m.name}</span>
                  {m.id === shownModel && (
                    <span className="cmp-menu-check">✓</span>
                  )}
                </div>
              ))}
            </div>
          )}
        </span>

        <span
          className="act-btn"
          onClick={() => {
            const s = ["high", "mid", "fast", "auto"] as const;
            const idx = s.indexOf(shownSpeed);
            applyConfig({ speed: s[(idx + 1) % 4] });
          }}
        >
          速度: {SPEED_LABELS[shownSpeed]}
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
              className={`act-mode-btn${shownMode === m ? " active" : ""}`}
              onClick={() => applyConfig({ mode: m })}
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
