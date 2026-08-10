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
import { createBufferedChannel, useStore } from "../../state/store";
import type {
  AgentInfo,
  PermissionMode,
  PermissionModeInfo,
  ThinkingOptionInfo,
} from "../../state/store";
import { spawnAgent, ensureAgentChannel } from "../../state/agentActions";
import { PermissionConfirmationDialog } from "./PermissionConfirmationDialog";

const DEFAULT_RUNTIME = "claude";
type ComposerPermissionMode = PermissionMode;

// Claude Code's Shift+Tab cycle is not the same order as the permission menu:
// manual → acceptEdits → plan → bypassPermissions → auto. Keep this explicit;
// using `list_permission_modes()` here makes auto/bypass transitions land on
// the wrong native mode because the menu intentionally presents safer choices
// before bypass.
const CLAUDE_PERMISSION_CYCLE: readonly ComposerPermissionMode[] = [
  "ask",
  "accept_edits",
  "plan",
  "yolo",
  "auto",
];

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
  const permissionMode = useStore((s) => s.permissionMode);
  const speed = useStore((s) => s.speed);
  const selectedModel = useStore((s) => s.selectedModel);
  const activeTabId = useStore((s) => s.activeTabId);
  const tabs = useStore((s) => s.tabs);
  const agents = useStore((s) => s.agents);
  const runtimes = useStore((s) => s.runtimes);

  const toggleComposer = useStore((s) => s.toggleComposer);
  const composerH = useStore((s) => s.composerH);
  const setComposerH = useStore((s) => s.setComposerH);
  const pushDraft = useStore((s) => s.pushDraft);
  const navigateDraft = useStore((s) => s.navigateDraft);

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
  const [permissionMenuOpen, setPermissionMenuOpen] = useState(false);
  const [pendingPermissionMode, setPendingPermissionMode] =
    useState<PermissionModeInfo | null>(null);
  const [thinkingMenuOpen, setThinkingMenuOpen] = useState(false);
  const [openCodeAgentModes, setOpenCodeAgentModes] =
    useState<Record<string, "Build" | "Plan">>({});
  const [refMenuOpen, setRefMenuOpen] = useState(false);
  const [recentEntries, setRecentEntries] = useState<RecentEntry[]>([]);
  const modelAnchorRef = useRef<HTMLSpanElement>(null);
  const modelMenuRef = useRef<HTMLDivElement>(null);
  const permissionAnchorRef = useRef<HTMLSpanElement>(null);
  const permissionMenuRef = useRef<HTMLDivElement>(null);
  const thinkingAnchorRef = useRef<HTMLSpanElement>(null);
  const thinkingMenuRef = useRef<HTMLDivElement>(null);
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
  // A Claude permission change is a short sequence of PTY key presses. Do not
  // allow two slider clicks to interleave and land on an unintended mode.
  const permissionSwitchingRef = useRef(false);
  const modelSwitchingRef = useRef(false);
  // Keep the textarea in the right mode when the composer switches between
  // auto-height and a fixed user-selected height.
  const fixedHeight = composerH !== null;
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
  const targetAgentId = activeTab?.agentId;

  // ── Per-session composer config ────────────────────────────────
  // The permission/speed/model controls show and edit the CURRENT target
  // session's own values (falling back to the global "next spawn" defaults when
  // no session is targeted). Changing one applies to that session (persisted,
  // takes effect on next resume) and remembers the choice for new sessions.
  const configAgentId = activeTab?.agentId;
  const configAgent = configAgentId ? agents.get(configAgentId) : undefined;
  const configRuntimeId = configAgent?.runtime ?? DEFAULT_RUNTIME;
  const configRuntime = runtimes.find((runtime) => runtime.id === configRuntimeId);
  const models = configRuntime?.models ?? [];
  const permissionModes: PermissionModeInfo[] = configRuntime?.permission_modes ?? [];
  const thinkingOptions: ThinkingOptionInfo[] = configRuntime?.thinking_options ?? [];
  const shownMode =
    (configAgent?.mode as ComposerPermissionMode) ?? permissionMode;
  const shownSpeed = configAgent?.speed ?? speed;
  const defaultModel = models.find((model) => model.is_default) ?? null;
  const preferredModel = configAgent?.model ?? selectedModel;
  const shownModel = models.some((model) => model.id === preferredModel)
    ? preferredModel
    : defaultModel?.id ?? null;
  const currentModel = models.find((m) => m.id === shownModel) ?? null;

  const applyConfig = useCallback(
    (patch: { mode?: string; speed?: string; model?: string | null }) => {
      const s = useStore.getState();
      // Remember for the next spawned session.
      if (patch.mode !== undefined) s.setPermissionMode(patch.mode as never);
      if (patch.speed !== undefined) s.setSpeed(patch.speed as never);
      if (patch.model !== undefined) s.setSelectedModel(patch.model);
      // Apply to the current session (persisted; takes effect on next resume).
      const id = activeTab?.agentId;
      if (!id || !s.agents.has(id)) return;
      s.addAgent({ ...s.agents.get(id)!, ...patch }, null);
      invoke("agent_set_session_config", { id, ...patch }).catch(() => {});
    },
    [activeTab?.agentId]
  );

  const applyPermissionMode = useCallback(
    async (mode: ComposerPermissionMode) => {
      if (permissionSwitchingRef.current) return;
      const s = useStore.getState();
      const id = activeTab?.agentId;
      const agent = id ? s.agents.get(id) : undefined;
      if (!id || !agent) {
        s.setPermissionMode(mode);
        return;
      }

      const previousMode =
        (agent.mode as ComposerPermissionMode | undefined) ?? s.permissionMode;
      if (mode === previousMode) return;

      permissionSwitchingRef.current = true;
      try {
        if (agent.runtime === "omp") {
          // OMP's approval mode is a process-level runtime override; its TUI
          // currently has no live command/keybinding for changing it. Persist
          // first, then restart the same provider session so the terminal's
          // effective mode and the composer selection cannot diverge.
          await invoke("agent_set_session_config", { id, mode });
          const { channel, flush } = createBufferedChannel();
          try {
            const info = await invoke<AgentInfo>("agent_resume", {
              id,
              onData: channel,
            });
            flush(info.id);
            useStore.getState().addAgent(info, channel);
          } catch (error) {
            // Keep persistence aligned with the still-displayed selection if
            // the restart failed before the new OMP process became available.
            await invoke("agent_set_session_config", { id, mode: previousMode }).catch(
              () => {}
            );
            throw error;
          }
        } else {
          // A restored session may not have its PTY until its terminal is first
          // shown. Resume it before applying a live mode change.
          const resumed = !s.agentChannels.has(id)
            ? await ensureAgentChannel(id)
            : false;
          if (resumed) await new Promise((r) => setTimeout(r, 250));
        }

        if (agent.runtime === "codex") {
          // Codex supports changing permissions in the current TUI through the
          // native `/permissions` picker. Drive that picker instead of killing
          // and resuming the process. Its presets are ordered Read Only,
          // Default (workspace access), Full Access.
          const presetIndex = permissionModes.findIndex((item) => item.id === mode);
          if (presetIndex < 0) throw new Error(`Unsupported Codex permission mode: ${mode}`);
          await invoke("agent_write", { id, data: "/permissions", raw: true });
          await new Promise((r) => setTimeout(r, 40));
          await invoke("agent_write", { id, data: "\r", raw: true });
          await new Promise((r) => setTimeout(r, 160));
          await invoke("agent_write", { id, data: "\u001b[H", raw: true });
          for (let i = 0; i < presetIndex; i++) {
            await invoke("agent_write", { id, data: "\u001b[B", raw: true });
          }
          await invoke("agent_write", { id, data: "\r", raw: true });
          if (mode === "yolo") {
            // Codex opens a second "Enable full access?" picker after the
            // preset is selected. The user has already accepted the IDE-owned
            // warning dialog, so accept Codex's default "Yes, continue anyway"
            // choice here as well; terminal focus is never required.
            await new Promise((r) => setTimeout(r, 160));
            await invoke("agent_write", { id, data: "\r", raw: true });
          }
        } else if (agent.runtime === "claude") {
          // Claude Code has no direct in-session command that takes a target
          // permission mode. Its supported live control is Shift+Tab, cycling:
          // manual → acceptEdits → plan → bypassPermissions → auto. Calculate
          // the forward route from the persisted current policy. This
          // changes the running TUI; it does not kill or resume the process.
          const currentMode =
            (agent.mode as ComposerPermissionMode | undefined) ?? s.permissionMode;
          const currentPosition = CLAUDE_PERMISSION_CYCLE.indexOf(currentMode);
          const targetPosition = CLAUDE_PERMISSION_CYCLE.indexOf(mode);
          if (currentPosition < 0 || targetPosition < 0) {
            throw new Error(`Unsupported Claude permission transition: ${currentMode} -> ${mode}`);
          }
          const steps =
            (targetPosition - currentPosition + CLAUDE_PERMISSION_CYCLE.length) %
            CLAUDE_PERMISSION_CYCLE.length;
          for (let i = 0; i < steps; i++) {
            await invoke("agent_write", { id, data: "\u001b[Z", raw: true });
            // Claude redraws its status line after every transition. Giving it
            // a brief turn prevents rapid key presses from being coalesced.
            if (i + 1 < steps) await new Promise((r) => setTimeout(r, 60));
          }
        } else if (agent.runtime === "opencode") {
          // OpenCode only accepts --auto at process startup. For a running TUI,
          // its supported switch lives in the command palette. CaPilot launches
          // OpenCode with a session-scoped TUI config that binds that palette to
          // F12, avoiding dependence on the user's configurable Ctrl+P binding.
          const command =
            mode === "auto"
              ? "Enable auto-approve permissions"
              : "Disable auto-approve permissions";
          // Ctrl+P keeps already-running sessions (started before the private
          // config existed) working. F12 is authoritative for newly launched
          // sessions. Sending both is harmless: an already-open palette ignores
          // F12, while a remapped Ctrl+P is ignored before F12 opens it.
          await invoke("agent_write", { id, data: "\u0010", raw: true });
          await new Promise((r) => setTimeout(r, 120));
          await invoke("agent_write", { id, data: "\u001b[24~", raw: true });
          await new Promise((r) => setTimeout(r, 120));
          await invoke("agent_write", { id, data: command, raw: true });
          await new Promise((r) => setTimeout(r, 80));
          await invoke("agent_write", { id, data: "\r", raw: true });
        }

        // Persist the selected mode so a later resume starts with the same
        // policy. This does not restart or replace the running PTY.
        if (agent.runtime !== "omp") {
          await invoke("agent_set_session_config", { id, mode });
        }
        const latest = useStore.getState().agents.get(id);
        if (latest) useStore.getState().addAgent({ ...latest, mode }, null);
        useStore.getState().setPermissionMode(mode);
      } catch (error) {
        console.error("permission mode switch failed:", error);
      } finally {
        permissionSwitchingRef.current = false;
      }
    },
    [activeTab?.agentId, permissionModes]
  );

  const applyModel = useCallback(
    async (modelId: string) => {
      if (modelSwitchingRef.current) return;
      const s = useStore.getState();
      const id = activeTab?.agentId;
      const agent = id ? s.agents.get(id) : undefined;
      if (!id || !agent) {
        s.setSelectedModel(modelId);
        return;
      }

      modelSwitchingRef.current = true;
      try {
        const resumed = !s.agentChannels.has(id) ? await ensureAgentChannel(id) : false;
        if (resumed) await new Promise((resolve) => setTimeout(resolve, 250));

        if (agent.runtime === "codex") {
          const modelIndex = models.findIndex((model) => model.id === modelId);
          if (modelIndex < 0) throw new Error(`Unsupported Codex model: ${modelId}`);
          await invoke("agent_write", { id, data: "/model", raw: true });
          await new Promise((resolve) => setTimeout(resolve, 40));
          await invoke("agent_write", { id, data: "\r", raw: true });
          await new Promise((resolve) => setTimeout(resolve, 180));
          await invoke("agent_write", { id, data: "\u001b[H", raw: true });
          for (let i = 0; i < modelIndex; i++) {
            await invoke("agent_write", { id, data: "\u001b[B", raw: true });
          }
          await invoke("agent_write", { id, data: "\r", raw: true });
        } else if (agent.runtime === "claude") {
          await invoke("agent_write", { id, data: `/model ${modelId}`, raw: true });
          await new Promise((resolve) => setTimeout(resolve, 30));
          await invoke("agent_write", { id, data: "\r", raw: true });
        }

        await invoke("agent_set_session_config", { id, model: modelId });
        const latest = useStore.getState().agents.get(id);
        if (latest) useStore.getState().addAgent({ ...latest, model: modelId }, null);
        useStore.getState().setSelectedModel(modelId);
      } catch (error) {
        console.error("model switch failed:", error);
      } finally {
        modelSwitchingRef.current = false;
      }
    },
    [activeTab?.agentId, models]
  );

  const cycleOpenCodeAgent = useCallback(async () => {
    const s = useStore.getState();
    const id = activeTab?.agentId;
    const agent = id ? s.agents.get(id) : undefined;
    if (!id || agent?.runtime !== "opencode") return;
    try {
      const resumed = !s.agentChannels.has(id) ? await ensureAgentChannel(id) : false;
      if (resumed) await new Promise((resolve) => setTimeout(resolve, 250));
      // OpenCode owns primary-agent switching in its native TUI. One Tab moves
      // to the next primary agent (Build ⇄ Plan in the default configuration).
      await invoke("agent_write", { id, data: "\t", raw: true });
      setOpenCodeAgentModes((current) => ({
        ...current,
        [id]: current[id] === "Plan" ? "Build" : "Plan",
      }));
    } catch (error) {
      console.error("OpenCode agent switch failed:", error);
    }
  }, [activeTab?.agentId]);

  // ── Esc → abort the target agent's current operation ──────────
  // Sends a raw ESC byte to the agent's PTY — the same path the terminal uses
  // (xterm keydown → agent_write raw:true), so the CLI aborts its in-flight
  // turn exactly like pressing Esc inside the terminal.
  const abortAgentOperation = useCallback(() => {
    const id = activeTab?.agentId;
    if (!id || !agents.has(id)) return;
    invoke("agent_write", { id, data: "\u001b", raw: true }).catch(() => {});
  }, [activeTab?.agentId, agents]);

  // ── Popover open/close (click-outside + Escape) ───────────────
  useEffect(() => {
    if (!modelMenuOpen && !permissionMenuOpen && !thinkingMenuOpen) return;
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node | null;
      if (modelMenuRef.current?.contains(t)) return;
      if (modelAnchorRef.current?.contains(t)) return;
      if (permissionMenuRef.current?.contains(t)) return;
      if (permissionAnchorRef.current?.contains(t)) return;
      if (thinkingMenuRef.current?.contains(t)) return;
      if (thinkingAnchorRef.current?.contains(t)) return;
      setModelMenuOpen(false);
      setPermissionMenuOpen(false);
      setThinkingMenuOpen(false);
    };
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") {
        setModelMenuOpen(false);
        setPermissionMenuOpen(false);
        setThinkingMenuOpen(false);
      }
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [modelMenuOpen, permissionMenuOpen, thinkingMenuOpen]);

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
    // A fixed-height composer fills the input area and scrolls internally. In
    // auto-height mode it grows with its content instead (capped at 200px).
    const s = useStore.getState();
    if (s.composerH !== null) {
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
    const id = s.tabs.find((t) => t.id === s.activeTabId)?.agentId;
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
    // 发送。视觉上由 `.composer-bang` 徽标标注。
    const isBang = raw.startsWith("!");
    const text = isBang ? raw.slice(1).trimStart() : raw;
    const agentInput = text;
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
        agentId = await spawnAgent();
        justSpawned = true;
      }

      // Resumed/restored sessions may not have a channel yet.
      const resumed = await ensureAgentChannel(agentId);
      // Give a freshly-spawned/resumed CLI TUI time to attach its input loop
      // before injecting the message. A fixed 800ms can be too short on slow
      // machines / cold claude starts (first instruction typed before the TUI
      // is reading stdin → dropped or eaten by the shell prompt).
      //
      // Why not "wait until the PTY buffer holds the first output" instead?
      // In exactly the cases this wait applies to (justSpawned || resumed) the
      // target agent's tab is the ACTIVE tab, so its XTermPanel has already
      // attached to the channel and drains the store's agentOutputs buffer
      // (output goes straight to xterm). The buffer is therefore empty at this
      // point, and polling it would either spin until a timeout (adding seconds
      // of latency to every first send) or never fire. Detecting readiness
      // reliably would require wiring into the channel/terminal or a new store
      // flag — both out of scope here. So the fixed heuristic stays.
      if (justSpawned || resumed) {
        await new Promise((r) => setTimeout(r, 800));
      }
      const runtime = useStore.getState().agents.get(agentId)?.runtime;
      if (runtime === "codex") {
        // Codex's TUI detects a text+Enter burst as pasted input. When both are
        // delivered in one PTY write, the trailing CR may remain in the editor
        // instead of submitting the prompt. Send the keystrokes separately,
        // matching what happens when a user types in the terminal directly.
        await invoke("agent_write", {
          id: agentId,
          data: agentInput,
          raw: true,
        });
        await new Promise((r) => setTimeout(r, 30));
        await invoke("agent_write", { id: agentId, data: "\r", raw: true });
      } else {
        await invoke("agent_write", { id: agentId, data: agentInput });
      }
    } catch (err) {
      console.error("Failed to send to agent:", err);
    } finally {
      // Release the in-flight guard so the next Enter can send again.
      sendingRef.current = false;
    }
  }, [
    targetAgentId,
    resizeTextarea,
    pushDraft,
  ]);

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
        handleSend();
      } else if (e.key === "Tab") {
        e.preventDefault();
        if (e.shiftKey) {
          // In OpenCode, Shift+Tab in CaPilot's composer mirrors the native
          // primary-agent switch: Build ⇄ Plan. Permission switching remains
          // available from its dedicated button/menu.
          if (configRuntimeId === "opencode") {
            void cycleOpenCodeAgent();
            return;
          }
          if (permissionModes.length === 0) return;
          const idx = permissionModes.findIndex((mode) => mode.id === shownMode);
          const next = permissionModes[(idx + 1 + permissionModes.length) % permissionModes.length];
          if (next.requires_confirmation && shownMode !== next.id) {
            setPendingPermissionMode(next);
          } else {
            void applyPermissionMode(next.id);
          }
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
        // 终端式中断：向目标 agent 的 PTY 发原始 ESC 字节。模型/文件弹出
        // 菜单打开时，这次 Esc 只负责关菜单（窗口级监听），不中断。
        e.preventDefault();
        if (!modelMenuOpen && !permissionMenuOpen && !thinkingMenuOpen && !refMenuOpen) abortAgentOperation();
      }
    },
    [
      atMenu,
      insertAtItem,
      handleSend,
      shownMode,
      permissionModes,
      configRuntimeId,
      cycleOpenCodeAgent,
      applyConfig,
      applyPermissionMode,
      navigateDraft,
      modelMenuOpen,
      permissionMenuOpen,
      thinkingMenuOpen,
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

  const effH = composerH;

  return (
    <div
      ref={composerRef}
      className={`composer${!composerOpen ? " composer-collapsed" : ""}`}
      style={composerOpen && effH ? { height: effH } : undefined}
    >
      {/* Height divider: drag to resize, double-click to reset. */}
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
        <span>
          → agent:{" "}
          {activeTab?.type === "agent" && activeTab.agentId
            ? activeTab.title || "agent"
            : "(无标签)"}
        </span>
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
        <div className="ul-composer-input-row">
          <textarea
            ref={textareaRef}
            className="composer-input"
            placeholder="发消息…（/ 命令 · @ 文件 · ! 终端 · 拖入文件）"
            rows={2}
            onKeyDown={handleKeyDown}
            onInput={handleInput}
          />
          <button
            className="ul-send-btn"
            title="发送消息（Enter）"
            onClick={() => handleSend()}
            disabled={sendingRef.current || !hasInput}
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
              setPermissionMenuOpen(false);
              setThinkingMenuOpen(false);
              setRefMenuOpen((o) => !o);
            }}
          >
            +
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
              setPermissionMenuOpen(false);
              setThinkingMenuOpen(false);
              setModelMenuOpen((o) => !o);
            }}
            title={`选择模型（当前：${currentModel ? currentModel.name : "runtime 默认"}）`}
          >
            {currentModel ? currentModel.name : "选择模型"}
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
                    applyModel(m.id);
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

        {configRuntimeId === "opencode" && (
          <span
            className="act-btn"
            title="切换 OpenCode Build / Plan"
            onClick={() => {
              setRefMenuOpen(false);
              setModelMenuOpen(false);
              setPermissionMenuOpen(false);
              setThinkingMenuOpen(false);
              void cycleOpenCodeAgent();
            }}
          >
            {configAgentId ? openCodeAgentModes[configAgentId] ?? "Build" : "Build"}
          </span>
        )}

        {thinkingOptions.length > 0 && (
          <span className="cmp-pop" ref={thinkingAnchorRef}>
            <span
              className="act-btn"
              title="选择思考强度"
              onClick={() => {
                setRefMenuOpen(false);
                setModelMenuOpen(false);
                setPermissionMenuOpen(false);
                setThinkingMenuOpen((open) => !open);
              }}
            >
              ⚡ {thinkingOptions.find((option) => option.id === shownSpeed)?.label ?? "思考强度"}
            </span>
            {thinkingMenuOpen && (
              <div className="cmp-menu" ref={thinkingMenuRef} role="menu">
                <div className="cmp-menu-label">思考强度</div>
                {thinkingOptions.map((option) => (
                  <div
                    key={option.id}
                    className={`cmp-menu-item${option.id === shownSpeed ? " current" : ""}`}
                    title={option.description}
                    onClick={() => {
                      applyConfig({ speed: option.id });
                      setThinkingMenuOpen(false);
                    }}
                  >
                    <span className="cmp-menu-name">{option.label}</span>
                    {option.id === shownSpeed && <span className="cmp-menu-check">✓</span>}
                  </div>
                ))}
              </div>
            )}
          </span>
        )}
        <span className="act-sep" />
        {permissionModes.length > 0 && (
          <span className="cmp-pop" ref={permissionAnchorRef}>
            <span
              className="act-btn"
              title="选择权限模式"
              onClick={() => {
                setRefMenuOpen(false);
                setModelMenuOpen(false);
                setThinkingMenuOpen(false);
                setPermissionMenuOpen((open) => !open);
              }}
            >
              🛡 {permissionModes.find((mode) => mode.id === shownMode)?.label ?? "权限"}
            </span>
            {permissionMenuOpen && (
              <div className="cmp-menu" ref={permissionMenuRef} role="menu">
                <div className="cmp-menu-label">权限模式</div>
                {permissionModes.map((mode) => (
                  <div
                    key={mode.id}
                    className={`cmp-menu-item${shownMode === mode.id ? " current" : ""}`}
                    title={mode.description}
                    onClick={() => {
                      setPermissionMenuOpen(false);
                      if (mode.requires_confirmation && shownMode !== mode.id) {
                        setPendingPermissionMode(mode);
                      } else {
                        void applyPermissionMode(mode.id);
                      }
                    }}
                  >
                    <span className="cmp-menu-name">{mode.label}</span>
                    {shownMode === mode.id && <span className="cmp-menu-check">✓</span>}
                  </div>
                ))}
              </div>
            )}
          </span>
        )}
        <button className="collapse-btn" onClick={toggleComposer}>
          {composerOpen ? "▼" : "▲"}
        </button>
      </div>
      {pendingPermissionMode && (
        <PermissionConfirmationDialog
          modeLabel={pendingPermissionMode.label}
          onCancel={() => setPendingPermissionMode(null)}
          onConfirm={() => {
            const mode = pendingPermissionMode.id;
            setPendingPermissionMode(null);
            void applyPermissionMode(mode);
          }}
        />
      )}
    </div>
  );
}
